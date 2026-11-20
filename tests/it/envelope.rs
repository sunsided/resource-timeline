//! Envelopes against exhaustive enumeration of the schedules the windows admit.

use proptest::prelude::*;
use resource_timeline::envelope::{Envelope, Flexible, Verdict};
use resource_timeline::{ActivityId, Bound, Bounds, Impact, Tick, Timeline, Window};

use crate::oracle;
use crate::properties::{owned_impact, with_impacts};

/// A generated flexible activity: window start, window length and impacts relative to start.
pub type Generated = (i64, i64, Vec<Impact>);

/// A battery-like timeline with a few fixed impacts and up to three flexible activities whose
/// windows start at or after its start.
pub fn scenario() -> impl Strategy<Value = (Timeline, Vec<Generated>)> {
    let base = (-3_i64..3, 4_i64..=12, 8_i64..=16).prop_flat_map(|(start, initial, max)| {
        prop::collection::vec(owned_impact(start, false), 0..4).prop_map(move |impacts| {
            let bounds = Bounds::new(0, max).expect("valid");
            let timeline = Timeline::depletable(start, initial, bounds, 64).expect("valid");
            with_impacts(timeline, &impacts)
        })
    });

    base.prop_flat_map(|timeline| {
        let start = timeline.start();
        let activity = (
            start..start + 10,
            0_i64..4,
            prop::collection::vec(owned_impact(0, false).prop_map(|(_, impact)| impact), 1..3),
        );
        (Just(timeline), prop::collection::vec(activity, 1..4))
    })
}

/// The flexible activities described by `generated`, with ids from 10 on.
pub fn flexible(generated: &[Generated]) -> Vec<Flexible<'_>> {
    (10_u32..)
        .zip(generated)
        .map(|(id, (earliest, length, impacts))| Flexible {
            activity: ActivityId(id),
            window: Window::new(*earliest, earliest + length).expect("valid"),
            impacts,
        })
        .collect()
}

/// Every concrete timeline the windows admit: one per combination of starts.
pub fn schedules(timeline: &Timeline, flexible: &[Flexible<'_>]) -> Vec<Timeline> {
    let mut concrete = vec![timeline.clone()];
    for activity in flexible {
        let window = activity.window;
        concrete = concrete
            .into_iter()
            .flat_map(|base| {
                (window.earliest()..=window.latest()).map(move |start| {
                    let mut next = base.clone();
                    for impact in activity.impacts {
                        let shifted = impact.shifted(start).expect("small");
                        next.add(activity.activity, shifted).expect("valid");
                    }

                    next
                })
            })
            .collect();
    }

    concrete
}

/// A tick after which no generated level changes any more.
pub fn horizon(timeline: &Timeline, generated: &[Generated]) -> Tick {
    timeline.start() + 40 + Tick::try_from(generated.len()).expect("small")
}

/// Whether the level of `concrete` is out of its bounds at `t`.
pub fn out_of_bounds(concrete: &Timeline, t: Tick) -> bool {
    let level = oracle::level_at(concrete, t);
    let bounds = concrete.bounds();
    level < i128::from(bounds.min()) || level > i128::from(bounds.max())
}

/// `timeline` and `generated` with every rate impact dropped.
fn without_rates(timeline: &Timeline, generated: &[Generated]) -> (Timeline, Vec<Generated>) {
    let empty = Timeline::depletable(timeline.start(), timeline.initial(), timeline.bounds(), 64)
        .expect("valid");
    let impacts: Vec<_> = timeline
        .impacts()
        .iter()
        .copied()
        .filter(|(_, impact)| !impact.is_rate())
        .collect();
    let generated = generated
        .iter()
        .map(|(earliest, length, impacts)| {
            let impacts = impacts.iter().copied().filter(|impact| !impact.is_rate());
            (*earliest, *length, impacts.collect())
        })
        .collect();

    (with_impacts(empty, &impacts), generated)
}

proptest! {
    /// The envelopes are exactly the pointwise minimum and maximum over all schedules.
    #[test]
    fn envelopes_are_tight((timeline, generated) in scenario()) {
        let flexible = flexible(&generated);
        let envelope = Envelope::new(&timeline, &flexible).expect("valid");
        let all = schedules(&timeline, &flexible);
        for t in timeline.start()..=horizon(&timeline, &generated) {
            let levels = all.iter().map(|concrete| oracle::level_at(concrete, t));
            let lowest = levels.clone().min().expect("at least one schedule");
            let highest = levels.max().expect("at least one schedule");
            prop_assert_eq!(i128::from(envelope.lower_at(t).expect("small")), lowest, "lower at {}", t);
            prop_assert_eq!(i128::from(envelope.upper_at(t).expect("small")), highest, "upper at {}", t);
        }
    }

    /// The verdict is sound for every schedule the windows admit.
    #[test]
    fn verdict_is_sound((timeline, generated) in scenario()) {
        let flexible = flexible(&generated);
        let verdict = Envelope::new(&timeline, &flexible).expect("valid").check().expect("small");
        let all = schedules(&timeline, &flexible);
        match verdict {
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

    /// Without rates the reported tick is the earliest at which the envelopes break a bound.
    #[test]
    fn verdict_tick_is_earliest_without_rates((timeline, generated) in scenario()) {
        let (timeline, generated) = without_rates(&timeline, &generated);
        let flexible = flexible(&generated);
        let envelope = Envelope::new(&timeline, &flexible).expect("valid");
        let bounds = timeline.bounds();
        let breaks = |t: Tick| {
            let (lower, upper) = (envelope.lower_at(t).expect("small"), envelope.upper_at(t).expect("small"));
            let certain = upper < bounds.min() || lower > bounds.max();
            let possible = lower < bounds.min() || upper > bounds.max();
            (certain, possible)
        };
        let ticks = timeline.start()..=horizon(&timeline, &generated);
        let first_certain = ticks.clone().find(|&t| breaks(t).0);
        let first_possible = ticks.clone().find(|&t| breaks(t).1);

        match envelope.check().expect("small") {
            Verdict::Safe => prop_assert_eq!(first_possible, None),
            Verdict::Certain(violation) => prop_assert_eq!(Some(violation.at), first_certain),
            Verdict::Possible(violation) => {
                prop_assert_eq!(first_certain, None);
                prop_assert_eq!(Some(violation.at), first_possible);
            }
        }
    }
}

/// A hand-checked envelope: a flexible claim widens the dip, a fixed charge lifts it.
#[test]
fn hand_checked_claim() {
    let timeline = Timeline::claimable(0, 1, 4).expect("valid");
    let hold = [Impact::Claim {
        start: 0,
        end: 3,
        amount: 1,
    }];
    let flexible = [Flexible {
        activity: ActivityId(1),
        window: Window::new(2, 5).expect("valid"),
        impacts: &hold,
    }];
    let envelope = Envelope::new(&timeline, &flexible).expect("valid");

    // Held at t in [2, 8) for some start; no tick is held for every start.
    let lower: Vec<_> = (0..10)
        .map(|t| envelope.lower_at(t).expect("small"))
        .collect();
    let upper: Vec<_> = (0..10)
        .map(|t| envelope.upper_at(t).expect("small"))
        .collect();
    assert_eq!(lower, [1, 1, 0, 0, 0, 0, 0, 0, 1, 1]);
    assert_eq!(upper, [1; 10]);
    assert_eq!(envelope.check(), Ok(Verdict::Safe));

    let mut busy = timeline.clone();
    busy.add(
        ActivityId(2),
        Impact::Claim {
            start: 4,
            end: 7,
            amount: 1,
        },
    )
    .expect("valid");
    // Every start overlaps the fixed claim, but at different ticks: only possible.
    let envelope = Envelope::new(&busy, &flexible).expect("valid");
    match envelope.check() {
        Ok(Verdict::Possible(violation)) => {
            assert_eq!((violation.at, violation.level), (4, -1));
            assert_eq!(violation.bound, Bound::BelowMin(0));
            assert_eq!(violation.contributors, [ActivityId(1), ActivityId(2)]);
        }
        other => panic!("unexpected {other:?}"),
    }

    // Starts in [3, 4] all hold tick 4, so the overlap there is certain.
    let narrow = [Flexible {
        window: Window::new(3, 4).expect("valid"),
        ..flexible[0]
    }];
    let envelope = Envelope::new(&busy, &narrow).expect("valid");
    match envelope.check() {
        Ok(Verdict::Certain(violation)) => assert_eq!((violation.at, violation.level), (4, -1)),
        other => panic!("unexpected {other:?}"),
    }
}

/// Guards against vacuous properties: all three verdicts occur.
#[test]
fn generators_cover_all_verdicts() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let (mut safe, mut possible, mut certain) = (0, 0, 0);
    for _ in 0..500 {
        let (timeline, generated) = scenario().new_tree(&mut runner).expect("tree").current();
        let flexible = flexible(&generated);
        match Envelope::new(&timeline, &flexible).expect("valid").check() {
            Ok(Verdict::Safe) => safe += 1,
            Ok(Verdict::Possible(_)) => possible += 1,
            Ok(Verdict::Certain(_)) => certain += 1,
            Err(error) => panic!("unexpected {error}"),
        }
    }

    assert!(
        safe > 10 && possible > 10 && certain > 10,
        "{safe} {possible} {certain}"
    );
}

/// End-state targets take part in envelope checks: possibly or certainly missed.
#[test]
fn envelope_checks_targets() {
    use resource_timeline::Target;

    let bounds = Bounds::new(0, 100).expect("valid");
    let mut battery = Timeline::depletable(0, 10, bounds, 8).expect("valid");
    battery
        .add_target(Target { at: 20, min: 5 })
        .expect("valid");
    let heat = [Impact::Step { at: 0, delta: -6 }];
    let flexible = |latest| {
        [Flexible {
            activity: ActivityId(1),
            window: Window::new(0, latest).expect("valid"),
            impacts: &heat,
        }]
    };

    let late = flexible(30);
    match Envelope::new(&battery, &late).expect("valid").check() {
        Ok(Verdict::Possible(violation)) => {
            assert_eq!((violation.at, violation.level), (20, 4));
            assert_eq!(violation.bound, Bound::BelowTarget(5));
            assert_eq!(violation.contributors, [ActivityId(1)]);
        }
        verdict => panic!("unexpected {verdict:?}"),
    }

    let early = flexible(10);
    match Envelope::new(&battery, &early).expect("valid").check() {
        Ok(Verdict::Certain(violation)) => assert_eq!(violation.bound, Bound::BelowTarget(5)),
        verdict => panic!("unexpected {verdict:?}"),
    }
}
