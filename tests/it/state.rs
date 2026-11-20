//! State timelines: hand-checked cases and a tick-by-tick oracle.

use proptest::prelude::*;
use resource_timeline::{ActivityId, Error, StateImpact, StateTimeline, StateViolation, Tick};

/// Activity used where only one is needed.
const A: ActivityId = ActivityId(1);

/// Second activity for conflict scenarios.
const B: ActivityId = ActivityId(2);

/// The latest set tick at or before `t` and the sets there, from the impact list.
fn latest_sets(timeline: &StateTimeline, t: Tick) -> Vec<(ActivityId, u32, Tick)> {
    let sets: Vec<_> = timeline
        .impacts()
        .iter()
        .filter_map(|&(activity, impact)| match impact {
            StateImpact::Set { at, value } if at <= t => Some((activity, value, at)),
            StateImpact::Set { .. } | StateImpact::Require { .. } => None,
        })
        .collect();
    let Some(latest) = sets.iter().map(|&(_, _, at)| at).max() else {
        return Vec::new();
    };

    sets.into_iter()
        .filter(|&(_, _, at)| at == latest)
        .collect()
}

/// The violation at tick `t` according to the definition, if any.
fn violation_at(timeline: &StateTimeline, t: Tick) -> Option<StateViolation> {
    let mut at_t: Vec<_> = latest_sets(timeline, t)
        .into_iter()
        .filter(|&(_, _, at)| at == t)
        .collect();
    if at_t.iter().any(|&(_, value, _)| value != at_t[0].1) {
        at_t.sort_unstable();
        let mut setters: Vec<_> = at_t.into_iter().map(|(activity, _, _)| activity).collect();
        setters.dedup();
        return Some(StateViolation::Conflict { at: t, setters });
    }

    let latest = latest_sets(timeline, t);
    let (actual, setter) = latest
        .iter()
        .min()
        .map_or((timeline.initial(), None), |&(activity, value, _)| {
            (value, Some(activity))
        });
    timeline
        .impacts()
        .iter()
        .filter_map(|&(activity, impact)| match impact {
            StateImpact::Require { start, end, value }
                if start <= t && t < end && value != actual =>
            {
                Some((activity, value))
            }
            StateImpact::Require { .. } | StateImpact::Set { .. } => None,
        })
        .min()
        .map(|(requirer, required)| StateViolation::Mismatch {
            at: t,
            required,
            actual,
            requirer,
            setter,
        })
}

/// The first violation, found by checking every tick up to the last referenced one.
pub fn oracle(timeline: &StateTimeline) -> Option<StateViolation> {
    let last = timeline
        .impacts()
        .iter()
        .map(|&(_, impact)| match impact {
            StateImpact::Set { at, .. } => at,
            StateImpact::Require { end, .. } => end,
        })
        .fold(timeline.start(), Tick::max);
    (timeline.start()..=last).find_map(|t| violation_at(timeline, t))
}

/// A state timeline over three values with up to ten random impacts.
fn timeline() -> impl Strategy<Value = StateTimeline> {
    let set = (0_i64..15, 0_u32..3).prop_map(|(at, value)| StateImpact::Set { at, value });
    let require =
        (0_i64..15, 1_i64..6, 0_u32..3).prop_map(|(start, length, value)| StateImpact::Require {
            start,
            end: start + length,
            value,
        });
    let impact = (0_u32..4, prop_oneof![set, require]);
    (0_u32..3, prop::collection::vec(impact, 0..10)).prop_map(|(initial, impacts)| {
        let mut timeline = StateTimeline::new(0, initial, 64);
        for (id, impact) in impacts {
            timeline.add(ActivityId(id), impact).expect("valid");
        }

        timeline
    })
}

proptest! {
    /// The search agrees with the tick-by-tick definition.
    #[test]
    fn matches_oracle(timeline in timeline()) {
        prop_assert_eq!(timeline.first_violation().expect("memory"), oracle(&timeline));
    }

    /// Removing an activity is the same as never adding it.
    #[test]
    fn remove_matches_rebuild(timeline in timeline(), id in 0_u32..4) {
        let mut incremental = timeline.clone();
        incremental.remove(ActivityId(id));
        let mut rebuilt = StateTimeline::new(timeline.start(), timeline.initial(), 64);
        for &(activity, impact) in timeline.impacts() {
            if activity != ActivityId(id) {
                rebuilt.add(activity, impact).expect("valid");
            }
        }

        prop_assert_eq!(incremental, rebuilt);
    }
}

/// A requirement met by the initial value and then broken by a later set.
#[test]
fn requirement_broken_by_later_set() {
    let mut heater = StateTimeline::new(0, 1, 8);
    heater
        .add(
            A,
            StateImpact::Require {
                start: 0,
                end: 10,
                value: 1,
            },
        )
        .expect("valid");
    heater
        .add(B, StateImpact::Set { at: 10, value: 0 })
        .expect("valid");
    assert_eq!(heater.first_violation(), Ok(None));

    heater
        .add(B, StateImpact::Set { at: 9, value: 0 })
        .expect("valid");
    let expected = StateViolation::Mismatch {
        at: 9,
        required: 1,
        actual: 0,
        requirer: A,
        setter: Some(B),
    };
    assert_eq!(heater.first_violation(), Ok(Some(expected)));
    assert_eq!(heater.value_at(9), Ok(Some(0)));
}

/// Contradicting sets at one tick are a conflict and make the value ambiguous.
#[test]
fn contradicting_sets_conflict() {
    let mut mode = StateTimeline::new(0, 0, 8);
    mode.add(B, StateImpact::Set { at: 4, value: 2 })
        .expect("valid");
    mode.add(A, StateImpact::Set { at: 4, value: 3 })
        .expect("valid");

    let expected = StateViolation::Conflict {
        at: 4,
        setters: vec![A, B],
    };
    assert_eq!(mode.first_violation(), Ok(Some(expected)));
    assert_eq!(mode.value_at(5), Ok(None));
    assert_eq!(mode.value_at(3), Ok(Some(0)));
}

/// Validation errors of state impacts.
#[test]
fn state_errors() {
    let mut mode = StateTimeline::new(5, 0, 1);
    assert_eq!(
        mode.add(A, StateImpact::Set { at: 4, value: 1 }),
        Err(Error::BeforeStart { at: 4, start: 5 })
    );
    assert_eq!(
        mode.add(
            A,
            StateImpact::Require {
                start: 6,
                end: 6,
                value: 1
            }
        ),
        Err(Error::EmptyInterval { start: 6, end: 6 })
    );
    mode.add(A, StateImpact::Set { at: 5, value: 1 })
        .expect("fits");
    assert_eq!(
        mode.add(A, StateImpact::Set { at: 6, value: 1 }),
        Err(Error::BudgetExhausted { limit: 1 })
    );
    assert_eq!(
        mode.value_at(4),
        Err(Error::BeforeStart { at: 4, start: 5 })
    );
}
