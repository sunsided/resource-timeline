//! Precedence-aware envelopes against enumeration of the schedules that respect them.

use proptest::prelude::*;
use resource_timeline::envelope::{Endpoint, Envelope, Flexible, Precedence, Verdict};
use resource_timeline::{ActivityId, Bounds, Error, Impact, Timeline, Window};

use crate::envelope::{Generated, flexible, horizon, out_of_bounds, scenario};
use crate::oracle;

/// Up to three precedences between the generated activities (ids 10 to 12) between any
/// endpoints, gaps in [-2, 6].
fn precedences(count: usize) -> impl Strategy<Value = Vec<Precedence>> {
    let id = 10_u32..10 + u32::try_from(count).expect("small");
    let endpoint = prop_oneof![Just(Endpoint::Start), Just(Endpoint::End)];
    let precedence = (id.clone(), endpoint.clone(), id, endpoint, -2_i64..=6).prop_map(
        |(before, from, after, to, gap)| Precedence {
            before: ActivityId(before),
            from,
            after: ActivityId(after),
            to,
            gap,
        },
    );
    prop::collection::vec(precedence, 0..4)
}

/// The tick of `endpoint` of a flexible activity starting at `start`: its end is the last
/// tick of its impacts.
fn time_of(activity: &Flexible<'_>, start: i64, endpoint: Endpoint) -> i64 {
    match endpoint {
        Endpoint::Start => start,
        Endpoint::End => {
            start
                + activity
                    .impacts
                    .iter()
                    .map(Impact::last_tick)
                    .fold(0, i64::max)
        }
    }
}

/// A scenario with precedences between its flexible activities.
fn ordered() -> impl Strategy<Value = (Timeline, Vec<Generated>, Vec<Precedence>)> {
    scenario().prop_flat_map(|(timeline, generated)| {
        let count = generated.len();
        (Just(timeline), Just(generated), precedences(count))
    })
}

/// Every concrete timeline the windows admit that also respects `precedences`.
fn schedules(
    timeline: &Timeline,
    flexible: &[Flexible<'_>],
    precedences: &[Precedence],
) -> Vec<Timeline> {
    let mut combos: Vec<Vec<i64>> = vec![Vec::new()];
    for activity in flexible {
        let window = activity.window;
        combos = combos
            .into_iter()
            .flat_map(|starts| {
                (window.earliest()..=window.latest()).map(move |start| {
                    let mut next = starts.clone();
                    next.push(start);
                    next
                })
            })
            .collect();
    }

    let at = |starts: &[i64], id: ActivityId, endpoint: Endpoint| {
        let index = flexible
            .iter()
            .position(|a| a.activity == id)
            .expect("known");
        time_of(&flexible[index], starts[index], endpoint)
    };
    combos
        .into_iter()
        .filter(|starts| {
            precedences
                .iter()
                .all(|p| at(starts, p.after, p.to) - at(starts, p.before, p.from) >= p.gap)
        })
        .map(|starts| {
            let mut concrete = timeline.clone();
            for (activity, start) in flexible.iter().zip(starts) {
                for impact in activity.impacts {
                    let shifted = impact.shifted(start).expect("small");
                    concrete.add(activity.activity, shifted).expect("valid");
                }
            }

            concrete
        })
        .collect()
}

proptest! {
    /// Exact for every respecting schedule (the coupled cut covers rates), and never looser
    /// than the independent envelope.
    #[test]
    fn envelopes_are_sound_and_tighter((timeline, generated, precedences) in ordered()) {
        let flexible = flexible(&generated);
        let independent = Envelope::new(&timeline, &flexible).expect("valid");
        let all = schedules(&timeline, &flexible, &precedences);
        let ordered = independent.clone().with_precedences(&precedences);
        let ordered = match ordered {
            Err(Error::InconsistentPrecedences) => {
                prop_assert!(all.is_empty());
                return Ok(());
            }
            result => result.expect("valid"),
        };
        prop_assert!(!all.is_empty());
        for t in timeline.start()..=horizon(&timeline, &generated) {
            let levels = all.iter().map(|concrete| oracle::level_at(concrete, t));
            let lowest = levels.clone().min().expect("schedules");
            let highest = levels.max().expect("schedules");
            let (lower, upper) = (i128::from(ordered.lower_at(t).expect("small")), i128::from(ordered.upper_at(t).expect("small")));
            prop_assert!(lower <= lowest && highest <= upper, "unsound at {}", t);
            prop_assert!(i128::from(independent.lower_at(t).expect("small")) <= lower, "looser at {}", t);
            prop_assert!(upper <= i128::from(independent.upper_at(t).expect("small")), "looser at {}", t);
            prop_assert_eq!((lower, upper), (lowest, highest), "not exact at {}", t);
        }
    }

    /// The verdict is sound for every respecting schedule.
    #[test]
    fn verdict_is_sound((timeline, generated, precedences) in ordered()) {
        let flexible = flexible(&generated);
        let Ok(envelope) = Envelope::new(&timeline, &flexible).expect("valid").with_precedences(&precedences) else {
            return Ok(());
        };
        let all = schedules(&timeline, &flexible, &precedences);
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

/// A hand-checked case where the precedence makes the envelope strictly tighter: two drains of
/// 6 from a battery of 10, charged by 6 at tick 10, may only overlap before the charge without
/// the precedence.
#[test]
fn precedence_is_strictly_tighter() {
    let bounds = Bounds::new(0, 100).expect("valid");
    let mut battery = Timeline::depletable(0, 10, bounds, 8).expect("valid");
    battery
        .add(ActivityId(0), Impact::Step { at: 10, delta: 6 })
        .expect("valid");
    let drain = [Impact::Step { at: 0, delta: -6 }];
    let window = Window::new(0, 20).expect("valid");
    let flexible = [
        Flexible {
            activity: ActivityId(1),
            window,
            impacts: &drain,
        },
        Flexible {
            activity: ActivityId(2),
            window,
            impacts: &drain,
        },
    ];
    let independent = Envelope::new(&battery, &flexible).expect("valid");
    assert_eq!(independent.lower_at(5), Ok(-2));

    // The second drain waits 10 ticks for the first: at tick 5 at most one has happened.
    let precedence = Precedence::starts(ActivityId(1), ActivityId(2), 10);
    let ordered = independent.with_precedences(&[precedence]).expect("valid");
    assert_eq!(ordered.lower_at(5), Ok(4));
    assert_eq!(ordered.window(ActivityId(1)), Window::new(0, 10).ok());
    assert_eq!(ordered.window(ActivityId(2)), Window::new(10, 20).ok());
    assert_eq!(ordered.check(), Ok(Verdict::Safe));
}

/// Unknown activities and contradicting precedences are errors.
#[test]
fn precedence_errors() {
    let timeline = Timeline::claimable(0, 1, 4).expect("valid");
    let hold = [Impact::Claim {
        start: 0,
        end: 2,
        amount: 1,
    }];
    let flexible = [Flexible {
        activity: ActivityId(1),
        window: Window::new(0, 5).expect("valid"),
        impacts: &hold,
    }];
    let envelope = Envelope::new(&timeline, &flexible).expect("valid");
    let unknown = Precedence::starts(ActivityId(1), ActivityId(9), 0);
    assert_eq!(
        envelope.clone().with_precedences(&[unknown]).err(),
        Some(Error::UnknownActivity {
            activity: ActivityId(9)
        })
    );

    let itself = Precedence::starts(ActivityId(1), ActivityId(1), 1);
    assert_eq!(
        envelope.with_precedences(&[itself]).err(),
        Some(Error::InconsistentPrecedences)
    );
}

/// Above the cut budget, rate activities linked by precedences fall back to separate extremes:
/// still sound, never tighter than exact, and here strictly looser. A drain and a charge of the
/// same rate that must start together cancel out exactly.
#[test]
fn budget_fallback_stays_sound() {
    let bounds = Bounds::new(0, 100).expect("valid");
    let battery = Timeline::depletable(0, 20, bounds, 8).expect("valid");
    let rate = |per_tick| {
        [Impact::Rate {
            start: 0,
            end: 5,
            per_tick,
        }]
    };
    let (drain, charge) = (rate(-2), rate(2));
    let window = Window::new(0, 10).expect("valid");
    let flexible = [
        Flexible {
            activity: ActivityId(1),
            window,
            impacts: &drain,
        },
        Flexible {
            activity: ActivityId(2),
            window,
            impacts: &charge,
        },
    ];
    let together = [
        Precedence::starts(ActivityId(1), ActivityId(2), 0),
        Precedence::starts(ActivityId(2), ActivityId(1), 0),
    ];
    let exact = Envelope::new(&battery, &flexible)
        .expect("valid")
        .with_precedences(&together)
        .expect("valid");
    let fallback = exact.clone().with_cut_budget(2).expect("valid");
    for t in 0..20 {
        assert_eq!(exact.lower_at(t), Ok(20));
        assert_eq!(exact.upper_at(t), Ok(20));
        assert!(fallback.lower_at(t).expect("small") <= 20);
        assert!(fallback.upper_at(t).expect("small") >= 20);
    }

    assert_eq!(fallback.lower_at(7), Ok(10));
}
