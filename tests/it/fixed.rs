//! Fixed-capacity storage against heap storage: same operations, same results.
//!
//! The fixed variant shares every algorithm with the heap variant, so the properties here
//! check the storage layer: interleaved adds and removes at full capacity, typed capacity
//! errors that leave the timeline unchanged, and identical checks and searches.

use proptest::prelude::*;
use resource_timeline::storage::{Fixed, FixedPlan, Storage};
use resource_timeline::{
    ActivityId, Bounds, Container, Demand, Error, Impact, Plan, PlanViolation, StateImpact,
    StateTimeline, StateViolation, Timeline, Violation, Window,
};

use crate::plan::{END_OF_SOL, demand, empty_plan};
use crate::properties::owned_impact;

/// Capacity of the fixed timelines under test.
const CAPACITY: usize = 6;

/// The fixed storage under test.
type Small = Fixed<CAPACITY>;

/// One step of a generated edit sequence.
#[derive(Debug, Clone)]
enum Op {
    /// Add an impact of an activity.
    Add(ActivityId, Impact),
    /// Remove every impact of an activity.
    Remove(ActivityId),
}

/// A random edit sequence on a depletable timeline starting at 0; removals are frequent so that
/// the sequence often refills a full timeline.
fn ops() -> impl Strategy<Value = Vec<Op>> {
    let add = owned_impact(0, false).prop_map(|(activity, impact)| Op::Add(activity, impact));
    let remove = (0_u32..4).prop_map(|id| Op::Remove(ActivityId(id)));
    prop::collection::vec(prop_oneof![3 => add, 1 => remove], 0..40)
}

/// A violation with its contributors as a plain vector, for comparing storages.
fn plain<S: Storage>(violation: Option<Violation<S>>) -> Option<(i64, i64, Vec<ActivityId>)> {
    violation.map(|v| (v.at, v.level, v.contributors.to_vec()))
}

/// A plan violation reduced to its tick, timeline index and contributor list.
fn plain_plan<S: Storage>(violation: Option<PlanViolation<S>>) -> Option<(i64, Vec<ActivityId>)> {
    violation.map(|violation| match violation {
        PlanViolation::Level { violation, .. } => (violation.at, violation.contributors.to_vec()),
        PlanViolation::State { violation, .. } => match violation {
            StateViolation::Conflict { at, setters } => (at, setters.to_vec()),
            StateViolation::Mismatch { at, requirer, .. } => (at, vec![requirer]),
        },
    })
}

proptest! {
    /// Adds and removes on a full fixed timeline behave like on a heap timeline with the same
    /// budget, and a budget above the capacity yields typed errors without side effects.
    #[test]
    fn fixed_timeline_matches_heap(ops in ops(), budget in CAPACITY..CAPACITY + 3) {
        let bounds = Bounds::new(0, 20).expect("valid");
        let mut heap = Timeline::depletable(0, 10, bounds, budget).expect("valid");
        let mut fixed = Timeline::<Small>::depletable_in(0, 10, bounds, budget).expect("valid");

        for op in ops {
            match op {
                Op::Add(activity, impact) => {
                    let before = fixed.clone();
                    match fixed.add(activity, impact) {
                        Err(Error::CapacityExceeded { container, capacity }) => {
                            prop_assert_eq!(container, Container::Impacts);
                            prop_assert_eq!(capacity, CAPACITY);
                            prop_assert_eq!(&fixed, &before);
                        }
                        result => prop_assert_eq!(result, heap.add(activity, impact)),
                    }
                }
                Op::Remove(activity) => prop_assert_eq!(fixed.remove(activity), heap.remove(activity)),
            }

            prop_assert_eq!(fixed.impacts(), heap.impacts());
            prop_assert_eq!(plain(fixed.first_violation().expect("room")), plain(heap.first_violation().expect("memory")));
            for t in [0, 7, 15, 30] {
                prop_assert_eq!(fixed.level_at(t), heap.level_at(t));
            }
        }
    }

    /// Fixed plans place, check and search exactly like heap plans, as long as they have room.
    #[test]
    fn fixed_plan_matches_heap(
        activities in prop::collection::vec((0_i64..20, prop::collection::vec(demand(true), 1..4)), 0..6),
        probe in prop::collection::vec(demand(true), 1..4),
    ) {
        let (mut heap, _) = empty_plan();
        let mut fixed = Plan::<FixedPlan<2, 1, 16>>::new_in();
        let bounds = Bounds::new(0, 12).expect("valid");
        let battery = fixed.add_timeline(Timeline::depletable_in(0, 8, bounds, 256).expect("valid")).expect("room");
        fixed.add_target(battery, END_OF_SOL).expect("room");
        fixed.add_timeline(Timeline::claimable_in(0, 2, 256).expect("valid")).expect("room");
        fixed.add_state(StateTimeline::new_in(0, 0, 256)).expect("room");

        for (id, (start, demands)) in (0_u32..).zip(activities) {
            let before = fixed.clone();
            match fixed.place(ActivityId(id), start, &demands) {
                Err(Error::CapacityExceeded { .. }) => prop_assert_eq!(&fixed, &before),
                result => prop_assert_eq!(result, heap.place(ActivityId(id), start, &demands)),
            }
        }

        prop_assert_eq!(plain_plan(fixed.first_violation().expect("room")), plain_plan(heap.first_violation().expect("memory")));
        let window = Window::new(-2, 30).expect("valid");
        prop_assert_eq!(fixed.earliest_start(&probe, window, 10_000), heap.earliest_start(&probe, window, 10_000));
        for start in [-1, 0, 5, 12] {
            prop_assert_eq!(fixed.fits(&probe, start), heap.fits(&probe, start));
        }
    }
}

/// Each fixed container reports its own name and capacity when it is full.
#[test]
fn every_container_reports_its_capacity() {
    let full = |container, capacity| Error::CapacityExceeded {
        container,
        capacity,
    };
    let bounds = Bounds::new(0, 10).expect("valid");

    let mut timeline = Timeline::<Fixed<1>>::depletable_in(0, 5, bounds, 8).expect("valid");
    timeline
        .add(ActivityId(1), Impact::Step { at: 1, delta: 1 })
        .expect("room");
    assert_eq!(
        timeline.add(ActivityId(1), Impact::Step { at: 2, delta: 1 }),
        Err(full(Container::Impacts, 1))
    );

    let mut state = StateTimeline::<Fixed<1>>::new_in(0, 0, 8);
    state
        .add(ActivityId(1), StateImpact::Set { at: 1, value: 1 })
        .expect("room");
    assert_eq!(
        state.add(ActivityId(1), StateImpact::Set { at: 2, value: 1 }),
        Err(full(Container::Impacts, 1))
    );

    let mut plan = Plan::<FixedPlan<1, 1, 1>>::new_in();
    let line = plan.add_timeline(timeline.clone()).expect("room");
    plan.add_state(state.clone()).expect("room");
    assert_eq!(
        plan.add_timeline(timeline),
        Err(full(Container::Timelines, 1))
    );
    assert_eq!(plan.add_state(state), Err(full(Container::States, 1)));

    let two = [
        Demand::Level {
            timeline: line,
            impact: Impact::Step { at: 0, delta: 1 },
        },
        Demand::Level {
            timeline: line,
            impact: Impact::Step { at: 1, delta: 1 },
        },
    ];
    assert_eq!(plan.fits(&two, 0), Err(full(Container::Scratch, 2)));
}

/// A budget below the capacity still limits a fixed timeline.
#[test]
fn budget_applies_below_capacity() {
    let mut timeline = Timeline::<Fixed<4>>::claimable_in(0, 1, 1).expect("valid");
    let claim = Impact::Claim {
        start: 0,
        end: 1,
        amount: 1,
    };
    timeline.add(ActivityId(1), claim).expect("room");
    assert_eq!(
        timeline.add(ActivityId(2), claim),
        Err(Error::BudgetExhausted { limit: 1 })
    );
}
