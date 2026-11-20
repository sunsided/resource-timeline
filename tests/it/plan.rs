//! Plans: placement, atomicity and the earliest-start search against a per-tick scan.

use proptest::prelude::*;
use resource_timeline::{
    ActivityId, Bounds, Demand, Error, Impact, Plan, StateId, StateImpact, StateTimeline, Target,
    Tick, Timeline, TimelineId, Window,
};

use crate::{oracle, state};

/// The id given to the activity whose start is searched.
const PROBE: ActivityId = ActivityId(99);

/// The handles of the battery, channel and state timelines of [`empty_plan`].
pub type Handles = (TimelineId, TimelineId, StateId);

/// The end-state target of the battery in [`empty_plan`]: at least 4 units at tick 25.
pub const END_OF_SOL: Target = Target { at: 25, min: 4 };

/// A plan with a battery (`[0, 12]`, starting at 8, [`END_OF_SOL`]), a capacity-2 channel and a
/// 3-valued state.
pub fn empty_plan() -> (Plan, Handles) {
    let mut plan = Plan::new();
    let bounds = Bounds::new(0, 12).expect("valid");
    let battery = plan
        .add_timeline(Timeline::depletable(0, 8, bounds, 256).expect("valid"))
        .expect("memory");
    plan.add_target(battery, END_OF_SOL).expect("valid");
    let channel = plan
        .add_timeline(Timeline::claimable(0, 2, 256).expect("valid"))
        .expect("memory");
    let mode = plan
        .add_state(StateTimeline::new(0, 0, 256))
        .expect("memory");
    (plan, (battery, channel, mode))
}

/// The handles of the timelines of [`empty_plan`].
fn handles() -> Handles {
    empty_plan().1
}

/// A random demand on one of the three timelines of [`empty_plan`], relative to the start.
pub fn demand(rates: bool) -> impl Strategy<Value = Demand> {
    let step = (0_i64..8, -4_i64..=4).prop_map(|(at, delta)| Impact::Step { at, delta });
    let claim = (0_i64..8, 1_i64..6, 1_i64..=2).prop_map(|(start, length, amount)| Impact::Claim {
        start,
        end: start + length,
        amount,
    });
    let rate =
        (0_i64..8, 1_i64..6, -2_i64..=2).prop_map(|(start, length, per_tick)| Impact::Rate {
            start,
            end: start + length,
            per_tick,
        });
    let battery_impact = if rates {
        prop_oneof![step, claim.clone(), rate].boxed()
    } else {
        prop_oneof![step, claim.clone()].boxed()
    };
    let set = (0_i64..8, 0_u32..3).prop_map(|(at, value)| StateImpact::Set { at, value });
    let require =
        (0_i64..8, 1_i64..6, 0_u32..3).prop_map(|(start, length, value)| StateImpact::Require {
            start,
            end: start + length,
            value,
        });

    prop_oneof![
        battery_impact.prop_map(|impact| Demand::Level {
            timeline: handles().0,
            impact
        }),
        claim.prop_map(|impact| Demand::Level {
            timeline: handles().1,
            impact
        }),
        prop_oneof![set, require].prop_map(|impact| Demand::State {
            timeline: handles().2,
            impact
        }),
    ]
}

/// A demand with long, slow rates on the battery (or a claim on the channel, or a state
/// impact), so that fitting starts often lie inside a piece rather than at its first tick.
fn long_demand() -> impl Strategy<Value = Demand> {
    let rate =
        (0_i64..10, 5_i64..30, -3_i64..=3).prop_map(|(start, length, per_tick)| Demand::Level {
            timeline: handles().0,
            impact: Impact::Rate {
                start,
                end: start + length,
                per_tick,
            },
        });
    prop_oneof![3 => rate, 2 => demand(true)]
}

/// A violation-free plan built from random activities, keeping only those that fit.
fn plan(rates: bool) -> impl Strategy<Value = Plan> {
    let activity = (0_i64..20, prop::collection::vec(demand(rates), 1..4));
    prop::collection::vec(activity, 0..8).prop_map(|activities| {
        let (mut plan, _) = empty_plan();
        for (id, (start, demands)) in (0_u32..).zip(activities) {
            plan.place(ActivityId(id), start, &demands).expect("valid");
            if plan.first_violation().expect("small").is_some() {
                plan.remove(ActivityId(id));
            }
        }

        plan
    })
}

/// Whether `demands` fit at `start`, by placing them on a copy and checking every touched
/// timeline with the brute-force oracles.
fn fits_by_scan(plan: &Plan, demands: &[Demand], start: Tick) -> bool {
    let mut copy = plan.clone();
    match copy.place(PROBE, start, demands) {
        Ok(()) => {}
        Err(Error::BeforeStart { .. }) => return false,
        Err(error) => panic!("unexpected {error}"),
    }

    demands.iter().all(|demand| match *demand {
        Demand::Level { timeline, .. } => {
            oracle::first_violation(copy.timeline(timeline).expect("known")).is_none()
        }
        Demand::State { timeline, .. } => {
            state::oracle(copy.state(timeline).expect("known")).is_none()
        }
    })
}

proptest! {
    /// Without rates the candidate search finds the same start as a per-tick scan.
    #[test]
    fn earliest_start_matches_scan(plan in plan(false), demands in prop::collection::vec(demand(false), 1..4)) {
        let window = Window::new(0, 30).expect("valid");
        let expected = (0..=30).find(|&start| fits_by_scan(&plan, &demands, start));
        prop_assert_eq!(plan.earliest_start(&demands, window, 10_000), Ok(expected));
    }

    /// With rates (in the plan or the demands) the search still matches the scan.
    #[test]
    fn earliest_start_with_rates_matches_scan(plan in plan(true), demands in prop::collection::vec(demand(true), 1..4)) {
        let window = Window::new(-3, 30).expect("valid");
        let expected = (-3..=30).find(|&start| fits_by_scan(&plan, &demands, start));
        prop_assert_eq!(plan.earliest_start(&demands, window, 10_000), Ok(expected));
    }

    /// Long rates and long windows: the closed-form solve per piece finds the same start as a
    /// per-tick scan of `fits` and of the brute-force oracles, also for windows that begin
    /// before the timeline start.
    #[test]
    fn closed_form_matches_scan(plan in plan(true), demands in prop::collection::vec(long_demand(), 1..4), from in -6_i64..4) {
        let window = Window::new(from, 70).expect("valid");
        let by_oracle = (from..=70).find(|&start| fits_by_scan(&plan, &demands, start));
        let by_fits = (from..=70).find(|&start| plan.fits(&demands, start) == Ok(true));
        prop_assert_eq!(by_fits, by_oracle);
        prop_assert_eq!(plan.earliest_start(&demands, window, 10_000), Ok(by_oracle));
    }

    /// `fits` agrees with the oracles at every start.
    #[test]
    fn fits_matches_scan(plan in plan(true), demands in prop::collection::vec(demand(true), 1..4), start in -2_i64..30) {
        prop_assert_eq!(plan.fits(&demands, start), Ok(fits_by_scan(&plan, &demands, start)));
    }
}

/// A failed placement leaves the plan unchanged, even when earlier demands were valid.
#[test]
fn place_is_atomic() {
    let (mut plan, (battery, channel, _)) = empty_plan();
    plan.place(
        ActivityId(1),
        0,
        &[Demand::Level {
            timeline: battery,
            impact: Impact::Step { at: 0, delta: -1 },
        }],
    )
    .expect("valid");
    let before = plan.clone();

    let demands = [
        Demand::Level {
            timeline: battery,
            impact: Impact::Step { at: 1, delta: -1 },
        },
        Demand::Level {
            timeline: channel,
            impact: Impact::Step { at: 1, delta: -1 },
        },
    ];
    assert_eq!(
        plan.place(ActivityId(1), 0, &demands),
        Err(Error::KindMismatch)
    );
    assert_eq!(plan, before);

    let unknown = [Demand::State {
        timeline: handles().2,
        impact: StateImpact::Set { at: 0, value: 1 },
    }];
    assert_eq!(
        Plan::new().place(ActivityId(1), 0, &unknown),
        Err(Error::UnknownResource)
    );
}

/// The search budget caps the pieces examined, and a long window with rates costs a few
/// pieces, not one check per tick.
#[test]
fn search_budget() {
    let (plan, (battery, _, _)) = empty_plan();
    let drain = [Demand::Level {
        timeline: battery,
        impact: Impact::Rate {
            start: 0,
            end: 10,
            per_tick: -1,
        },
    }];
    // Draining 10 from 8 breaks the reserve at any start. The window is clipped to the timeline
    // start; the drain's ticks 0 and 10 meet the start and the end-of-sol target at 25 (or pass
    // them) at starts 0, 1, 15, 16, 25 and 26, so six pieces cover a million ticks.
    let window = Window::new(-100, 1_000_000).expect("valid");
    assert_eq!(
        plan.earliest_start(&drain, window, 2),
        Err(Error::SearchExhausted { checks: 2 })
    );
    assert_eq!(plan.earliest_start(&drain, window, 6), Ok(None));

    // Short enough to keep the end-of-sol reserve.
    let short = [Demand::Level {
        timeline: battery,
        impact: Impact::Rate {
            start: 0,
            end: 4,
            per_tick: -1,
        },
    }];
    assert_eq!(plan.earliest_start(&short, window, 1), Ok(Some(0)));
    assert_eq!(
        Window::new(2, 1),
        Err(Error::InvalidWindow {
            earliest: 2,
            latest: 1
        })
    );
}

/// Guards against vacuous search properties: delayed starts and misses are both frequent.
#[test]
fn search_generators_cover_outcomes() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let strategy = (plan(false), prop::collection::vec(demand(false), 1..4));
    let (mut delayed, mut missed, total) = (0, 0, 300);
    for _ in 0..total {
        let (plan, demands) = strategy.new_tree(&mut runner).expect("tree").current();
        match (0..=30).find(|&start| fits_by_scan(&plan, &demands, start)) {
            Some(start) if start > 0 => delayed += 1,
            Some(_) => {}
            None => missed += 1,
        }
    }

    assert!(delayed > total / 20, "too few delayed starts: {delayed}");
    assert!(missed > total / 20, "too few misses: {missed}");
}

/// The search skips starts that would miss an end-state target, and a plan rejects unknown
/// timelines for targets.
#[test]
fn search_respects_targets() {
    let (plan, (battery, _, _)) = empty_plan();
    // Draining 5 units leaves 3 < 4 at tick 25 unless the drain starts after 25.
    let drain = [Demand::Level {
        timeline: battery,
        impact: Impact::Step { at: 0, delta: -5 },
    }];
    let window = Window::new(0, 40).expect("valid");
    assert_eq!(plan.earliest_start(&drain, window, 100), Ok(Some(26)));
    assert_eq!(plan.fits(&drain, 25), Ok(false));

    let unknown = Plan::new().add_target(battery, END_OF_SOL);
    assert_eq!(unknown, Err(Error::UnknownResource));
}
