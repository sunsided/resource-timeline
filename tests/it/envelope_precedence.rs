//! Precedence-aware envelopes against enumeration of the schedules that respect them.

use proptest::prelude::*;
use resource_timeline::envelope::{Envelope, Flexible, Precedence, Verdict};
use resource_timeline::{ActivityId, Bounds, Error, Impact, Timeline, Window};

use crate::envelope::{Generated, flexible, horizon, out_of_bounds, scenario};
use crate::oracle;

/// Up to three precedences between the generated activities (ids 10 to 12), gaps in [-2, 6].
fn precedences(count: usize) -> impl Strategy<Value = Vec<Precedence>> {
    let id = 10_u32..10 + u32::try_from(count).expect("small");
    let precedence = (id.clone(), id, -2_i64..=6).prop_map(|(before, after, gap)| Precedence {
        before: ActivityId(before),
        after: ActivityId(after),
        gap,
    });
    prop::collection::vec(precedence, 0..4)
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

    let start_of = |starts: &[i64], id: ActivityId| {
        let index = flexible
            .iter()
            .position(|a| a.activity == id)
            .expect("known");
        starts[index]
    };
    combos
        .into_iter()
        .filter(|starts| {
            precedences
                .iter()
                .all(|p| start_of(starts, p.after) - start_of(starts, p.before) >= p.gap)
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
    /// Sound for every respecting schedule, never looser than the independent envelope, and
    /// exact when no activity has a rate.
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

        let rates = generated.iter().any(|(_, _, impacts)| impacts.iter().any(Impact::is_rate));
        for t in timeline.start()..=horizon(&timeline, &generated) {
            let levels = all.iter().map(|concrete| oracle::level_at(concrete, t));
            let lowest = levels.clone().min().expect("schedules");
            let highest = levels.max().expect("schedules");
            let (lower, upper) = (i128::from(ordered.lower_at(t).expect("small")), i128::from(ordered.upper_at(t).expect("small")));
            prop_assert!(lower <= lowest && highest <= upper, "unsound at {}", t);
            prop_assert!(i128::from(independent.lower_at(t).expect("small")) <= lower, "looser at {}", t);
            prop_assert!(upper <= i128::from(independent.upper_at(t).expect("small")), "looser at {}", t);
            if !rates {
                prop_assert_eq!((lower, upper), (lowest, highest), "not exact at {}", t);
            }
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
                // Exact (so the violation is real) only when every activity is stepwise; with
                // rates the separate extremes of precedence-linked activities may add up to a
                // level no single schedule reaches.
                let rates = generated.iter().any(|(_, _, impacts)| impacts.iter().any(Impact::is_rate));
                if !rates {
                    prop_assert!(all.iter().any(|concrete| out_of_bounds(concrete, violation.at)));
                }
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
    let precedence = Precedence {
        before: ActivityId(1),
        after: ActivityId(2),
        gap: 10,
    };
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
    let unknown = Precedence {
        before: ActivityId(1),
        after: ActivityId(9),
        gap: 0,
    };
    assert_eq!(
        envelope.clone().with_precedences(&[unknown]).err(),
        Some(Error::UnknownActivity {
            activity: ActivityId(9)
        })
    );

    let itself = Precedence {
        before: ActivityId(1),
        after: ActivityId(1),
        gap: 1,
    };
    assert_eq!(
        envelope.with_precedences(&[itself]).err(),
        Some(Error::InconsistentPrecedences)
    );
}
