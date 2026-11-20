//! Envelopes of activities with uncertain durations against enumeration of every start and
//! every duration.

use proptest::prelude::*;
use resource_timeline::envelope::{
    Anchor, Contingent, Endpoint, Envelope, Precedence, Uncertain, Verdict,
};
use resource_timeline::{ActivityId, Bounds, Error, Impact, Tick, Timeline, Window};

use crate::envelope::out_of_bounds;
use crate::oracle;
use crate::properties::{owned_impact, with_impacts};

/// A generated contingent activity: window start and length, duration range, impacts.
type Generated = (i64, i64, i64, i64, Vec<Uncertain>);

/// An anchor a few ticks after the start or the end.
fn anchor() -> impl Strategy<Value = Anchor> {
    prop_oneof![
        (0_i64..4).prop_map(Anchor::Start),
        (-1_i64..3).prop_map(Anchor::End),
    ]
}

/// An anchored impact; claims only for claimable timelines.
fn uncertain(claims_only: bool) -> impl Strategy<Value = Uncertain> {
    let claim = (anchor(), anchor(), 1_i64..=3).prop_map(|(start, end, amount)| Uncertain::Claim {
        start,
        end,
        amount,
    });
    let step = (anchor(), -5_i64..=5).prop_map(|(at, delta)| Uncertain::Step { at, delta });
    let rate =
        (anchor(), anchor(), -2_i64..=2).prop_map(|(start, end, per_tick)| Uncertain::Rate {
            start,
            end,
            per_tick,
        });
    if claims_only {
        claim.boxed()
    } else {
        prop_oneof![claim, step, rate].boxed()
    }
}

/// A timeline (depletable or claimable) with a few fixed impacts and up to two contingent
/// activities. Some generated impacts are invalid (inverted intervals); tests skip those.
fn scenario() -> impl Strategy<Value = (Timeline, Vec<Generated>)> {
    prop::bool::ANY.prop_flat_map(|claimable| {
        let timeline =
            prop::collection::vec(owned_impact(0, claimable), 0..3).prop_map(move |impacts| {
                let timeline = if claimable {
                    Timeline::claimable(0, 2, 64).expect("valid")
                } else {
                    Timeline::depletable(0, 8, Bounds::new(0, 14).expect("valid"), 64)
                        .expect("valid")
                };
                with_impacts(timeline, &impacts)
            });
        let activity = (
            0_i64..6,
            0_i64..3,
            0_i64..3,
            0_i64..4,
            prop::collection::vec(uncertain(claimable), 1..3),
        );
        (timeline, prop::collection::vec(activity, 1..3))
    })
}

/// The contingent activities described by `generated`, with ids from 10 on.
fn contingent(generated: &[Generated]) -> Vec<Contingent<'_>> {
    (10_u32..)
        .zip(generated)
        .map(
            |(id, (earliest, length, shortest, spread, impacts))| Contingent {
                activity: ActivityId(id),
                window: Window::new(*earliest, earliest + length).expect("valid"),
                duration: Window::new(*shortest, shortest + spread).expect("valid"),
                impacts,
            },
        )
        .collect()
}

/// Every concrete timeline: one per combination of a start and a duration per activity.
fn outcomes(timeline: &Timeline, contingent: &[Contingent<'_>]) -> Vec<Timeline> {
    outcomes_with_starts(timeline, contingent)
        .into_iter()
        .map(|(_, concrete)| concrete)
        .collect()
}

/// Every concrete timeline with the starts and durations that produced it.
fn outcomes_with_starts(
    timeline: &Timeline,
    contingent: &[Contingent<'_>],
) -> Vec<(Vec<(Tick, Tick)>, Timeline)> {
    let mut concrete = vec![(Vec::new(), timeline.clone())];
    for activity in contingent {
        let (window, duration) = (activity.window, activity.duration);
        concrete = concrete
            .into_iter()
            .flat_map(|(chosen, base)| {
                let pairs = (window.earliest()..=window.latest()).flat_map(move |start| {
                    (duration.earliest()..=duration.latest()).map(move |length| (start, length))
                });
                pairs.map(move |(start, length)| {
                    let mut next = base.clone();
                    for impact in activity.impacts {
                        let relative = impact.resolve(length).expect("small");
                        if !is_empty(relative) {
                            next.add(activity.activity, relative.shifted(start).expect("small"))
                                .expect("valid");
                        }
                    }

                    let mut chosen = chosen.clone();
                    chosen.push((start, length));
                    (chosen, next)
                })
            })
            .collect();
    }

    concrete
}

/// Whether a claim or rate has an empty interval (it then changes nothing).
fn is_empty(impact: Impact) -> bool {
    match impact {
        Impact::Claim { start, end, .. } | Impact::Rate { start, end, .. } => start == end,
        Impact::Step { .. } => false,
    }
}

/// A tick after which nothing changes any more.
const HORIZON: Tick = 30;

proptest! {
    /// The envelopes are exactly the pointwise extremes over every start and duration.
    #[test]
    fn envelopes_are_tight((timeline, generated) in scenario()) {
        let contingent = contingent(&generated);
        let envelope = match Envelope::new(&timeline, &[]).expect("valid").with_contingent(&contingent) {
            Err(Error::EmptyInterval { .. } | Error::BeforeStart { .. }) => return Ok(()),
            result => result.expect("valid"),
        };
        let all = outcomes(&timeline, &contingent);
        for t in timeline.start()..=HORIZON {
            let levels = all.iter().map(|concrete| oracle::level_at(concrete, t));
            prop_assert_eq!(i128::from(envelope.lower_at(t).expect("small")), levels.clone().min().expect("outcomes"), "lower at {}", t);
            prop_assert_eq!(i128::from(envelope.upper_at(t).expect("small")), levels.max().expect("outcomes"), "upper at {}", t);
        }
    }

    /// The verdict is sound for every start and duration.
    #[test]
    fn verdict_is_sound((timeline, generated) in scenario()) {
        let contingent = contingent(&generated);
        let Ok(envelope) = Envelope::new(&timeline, &[]).expect("valid").with_contingent(&contingent) else {
            return Ok(());
        };
        let all = outcomes(&timeline, &contingent);
        match envelope.check().expect("small") {
            Verdict::Safe => {
                for concrete in &all {
                    prop_assert_eq!(oracle::first_violation(concrete), None);
                }
            }
            Verdict::Certain(violation) => {
                for concrete in &all {
                    prop_assert!(out_of_bounds(concrete, violation.at));
                }
            }
            Verdict::Possible(violation) => {
                prop_assert!(all.iter().any(|concrete| out_of_bounds(concrete, violation.at)));
            }
        }
    }
}

proptest! {
    /// With a precedence between any endpoints of the first and the last contingent activity
    /// (their ends included), the envelopes equal the extremes over every start and duration
    /// that respects it, and never get looser.
    #[test]
    fn precedences_are_exact(
        (timeline, generated) in scenario(),
        gap in -2_i64..5,
        from in prop_oneof![Just(Endpoint::Start), Just(Endpoint::End)],
        to in prop_oneof![Just(Endpoint::Start), Just(Endpoint::End)],
    ) {
        let contingent = contingent(&generated);
        let Ok(independent) = Envelope::new(&timeline, &[]).expect("valid").with_contingent(&contingent) else {
            return Ok(());
        };
        let last = ActivityId(9 + u32::try_from(contingent.len()).expect("small"));
        let precedence = Precedence { before: ActivityId(10), from, after: last, to, gap };
        let at = |(start, length): (Tick, Tick), endpoint| match endpoint {
            Endpoint::Start => start,
            Endpoint::End => start + length,
        };
        let respecting: Vec<_> = outcomes_with_starts(&timeline, &contingent)
            .into_iter()
            .filter(|(chosen, _)| at(chosen[chosen.len() - 1], to) - at(chosen[0], from) >= gap)
            .map(|(_, concrete)| concrete)
            .collect();
        let ordered = match independent.clone().with_precedences(&[precedence]) {
            Err(Error::InconsistentPrecedences) => {
                prop_assert!(respecting.is_empty());
                return Ok(());
            }
            result => result.expect("valid"),
        };
        prop_assert!(!respecting.is_empty());
        for t in timeline.start()..=HORIZON {
            let levels = respecting.iter().map(|concrete| oracle::level_at(concrete, t));
            let (lower, upper) = (ordered.lower_at(t).expect("small"), ordered.upper_at(t).expect("small"));
            prop_assert_eq!(i128::from(lower), levels.clone().min().expect("outcomes"), "lower at {}", t);
            prop_assert_eq!(i128::from(upper), levels.max().expect("outcomes"), "upper at {}", t);
            prop_assert!(lower >= independent.lower_at(t).expect("small"));
            prop_assert!(upper <= independent.upper_at(t).expect("small"));
        }
    }
}

/// A hand-checked transmitter hold of 3 to 5 ticks, starting in [0, 2]: held at tick 6 only by
/// the latest start with the longest duration, held at tick 2 by every outcome.
#[test]
fn hand_checked_hold() {
    let transmitter = Timeline::claimable(0, 1, 4).expect("valid");
    let hold = [Uncertain::Claim {
        start: Anchor::Start(0),
        end: Anchor::End(0),
        amount: 1,
    }];
    let contingent = [Contingent {
        activity: ActivityId(1),
        window: Window::new(0, 2).expect("valid"),
        duration: Window::new(3, 5).expect("valid"),
        impacts: &hold,
    }];
    let envelope = Envelope::new(&transmitter, &[])
        .expect("valid")
        .with_contingent(&contingent)
        .expect("valid");
    let lower: Vec<_> = (0..9)
        .map(|t| envelope.lower_at(t).expect("small"))
        .collect();
    let upper: Vec<_> = (0..9)
        .map(|t| envelope.upper_at(t).expect("small"))
        .collect();
    assert_eq!(lower, [0, 0, 0, 0, 0, 0, 0, 1, 1]);
    assert_eq!(upper, [1, 1, 0, 1, 1, 1, 1, 1, 1]);
    assert_eq!(envelope.check(), Ok(Verdict::Safe));

    let negative = [Contingent {
        duration: Window::new(-1, 3).expect("valid"),
        ..contingent[0]
    }];
    assert_eq!(
        Envelope::new(&transmitter, &[])
            .expect("valid")
            .with_contingent(&negative)
            .err(),
        Some(Error::NegativeDuration { shortest: -1 })
    );

    let inverted = [Uncertain::Claim {
        start: Anchor::End(0),
        end: Anchor::Start(3),
        amount: 1,
    }];
    let inverted = [Contingent {
        impacts: &inverted,
        ..contingent[0]
    }];
    assert!(matches!(
        Envelope::new(&transmitter, &[])
            .expect("valid")
            .with_contingent(&inverted),
        Err(Error::EmptyInterval { .. })
    ));
}
