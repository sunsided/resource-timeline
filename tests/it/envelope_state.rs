//! State envelopes against enumeration of every schedule the windows admit.

use proptest::prelude::*;
use resource_timeline::envelope::{
    FlexibleState, StateEnvelope, StateEnvelopeViolation, StateVerdict,
};
use resource_timeline::{ActivityId, StateImpact, StateTimeline, Tick, Window};

/// A generated flexible activity: window start and length, impacts relative to its start.
type Generated = (i64, i64, Vec<StateImpact>);

/// A state impact with small ticks and three values.
fn impact(base: i64) -> impl Strategy<Value = StateImpact> {
    let set = (base..base + 6, 0_u32..3).prop_map(|(at, value)| StateImpact::Set { at, value });
    let length = prop_oneof![4 => 1_i64..5, 1 => Just(1_000_i64)];
    let require = (base..base + 6, length, 0_u32..3).prop_map(|(start, length, value)| {
        StateImpact::Require {
            start,
            end: start + length,
            value,
        }
    });
    prop_oneof![set, require]
}

/// A state timeline with a few fixed impacts and up to three flexible activities.
fn scenario() -> impl Strategy<Value = (StateTimeline, Vec<Generated>)> {
    let fixed = (0_u32..3, prop::collection::vec((0_u32..3, impact(0)), 0..4)).prop_map(
        |(initial, impacts)| {
            let mut timeline = StateTimeline::new(0, initial, 64);
            for (id, impact) in impacts {
                timeline.add(ActivityId(id), impact).expect("valid");
            }

            timeline
        },
    );
    let activity = (0_i64..6, 0_i64..4, prop::collection::vec(impact(0), 1..3));
    (fixed, prop::collection::vec(activity, 1..4))
}

/// The flexible activities described by `generated`, with ids from 10 on.
fn flexible(generated: &[Generated]) -> Vec<FlexibleState<'_>> {
    (10_u32..)
        .zip(generated)
        .map(|(id, (earliest, length, impacts))| FlexibleState {
            activity: ActivityId(id),
            window: Window::new(*earliest, earliest + length).expect("valid"),
            impacts,
        })
        .collect()
}

/// One requirement or set of the scenario: its activity's index (`None` for the fixed
/// impacts) and the impact relative to that activity's start.
type Part = (Option<usize>, ActivityId, StateImpact);

/// Every impact of the scenario.
fn parts(timeline: &StateTimeline, flexible: &[FlexibleState<'_>]) -> Vec<Part> {
    let fixed = timeline
        .impacts()
        .iter()
        .map(|&(id, impact)| (None, id, impact));
    let moving = flexible.iter().enumerate().flat_map(|(index, activity)| {
        activity
            .impacts
            .iter()
            .map(move |&impact| (Some(index), activity.activity, impact))
    });
    fixed.chain(moving).collect()
}

/// Every combination of starts the windows admit.
fn start_combinations(flexible: &[FlexibleState<'_>]) -> Vec<Vec<Tick>> {
    let mut combos: Vec<Vec<Tick>> = vec![Vec::new()];
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

    combos
}

/// `part` moved to absolute ticks for the given starts.
fn placed(part: &Part, starts: &[Tick]) -> StateImpact {
    let shift = part.0.map_or(0, |index| starts[index]);
    part.2.shifted(shift).expect("small")
}

/// The concrete timeline for the given starts.
fn concrete(timeline: &StateTimeline, parts: &[Part], starts: &[Tick]) -> StateTimeline {
    let mut concrete = StateTimeline::new(timeline.start(), timeline.initial(), 64);
    for part in parts {
        concrete.add(part.1, placed(part, starts)).expect("valid");
    }

    concrete
}

/// Whether `concrete` is violated at tick `t`: contradicting sets at `t`, or a requirement in
/// force at `t` that sees another or an ambiguous value.
fn violated_at(concrete: &StateTimeline, t: Tick) -> bool {
    let sets_at_t: Vec<u32> = concrete
        .impacts()
        .iter()
        .filter_map(|&(_, impact)| match impact {
            StateImpact::Set { at, value } if at == t => Some(value),
            StateImpact::Set { .. } | StateImpact::Require { .. } => None,
        })
        .collect();
    if sets_at_t.iter().any(|&value| value != sets_at_t[0]) {
        return true;
    }

    concrete
        .impacts()
        .iter()
        .any(|&(_, impact)| breaks(concrete, impact, t))
}

/// Whether the placed requirement `impact` sees another or an ambiguous value at `t`.
fn breaks(concrete: &StateTimeline, impact: StateImpact, t: Tick) -> bool {
    match impact {
        StateImpact::Require { start, end, value } => {
            start <= t && t < end && concrete.value_at(t).expect("in range") != Some(value)
        }
        StateImpact::Set { .. } => false,
    }
}

/// Whether one requirement, or one pair of contradicting sets, is broken at `t` in every
/// schedule.
fn specific_in_every(
    timeline: &StateTimeline,
    parts: &[Part],
    combos: &[Vec<Tick>],
    t: Tick,
) -> bool {
    let concretes: Vec<_> = combos
        .iter()
        .map(|starts| concrete(timeline, parts, starts))
        .collect();
    let requirement = parts.iter().any(|part| {
        combos
            .iter()
            .zip(&concretes)
            .all(|(starts, c)| breaks(c, placed(part, starts), t))
    });
    let pair = parts.iter().enumerate().any(|(i, a)| {
        parts[i + 1..].iter().any(|b| {
            combos
                .iter()
                .all(|starts| match (placed(a, starts), placed(b, starts)) {
                    (
                        StateImpact::Set {
                            at: first,
                            value: one,
                        },
                        StateImpact::Set {
                            at: second,
                            value: other,
                        },
                    ) => first == t && second == t && one != other,
                    _ => false,
                })
        })
    });
    requirement || pair
}

/// A tick after which no generated impact matters.
const HORIZON: Tick = 20;

proptest! {
    /// The verdict is exact: `Safe` iff every schedule is safe, a possible violation at the
    /// earliest tick some schedule is violated, a certain one at the earliest tick at which one
    /// requirement or one pair of sets is broken in every schedule.
    #[test]
    fn verdict_is_exact((timeline, generated) in scenario()) {
        let flexible = flexible(&generated);
        let verdict = StateEnvelope::new(&timeline, &flexible).expect("valid").check().expect("memory");
        let parts = parts(&timeline, &flexible);
        let combos = start_combinations(&flexible);
        let all: Vec<_> = combos.iter().map(|starts| concrete(&timeline, &parts, starts)).collect();
        let ticks = timeline.start()..=HORIZON;
        let first_some = ticks.clone().find(|&t| all.iter().any(|c| violated_at(c, t)));
        let first_every = ticks.clone().find(|&t| specific_in_every(&timeline, &parts, &combos, t));
        let safe = all.iter().all(|c| c.first_violation().expect("memory").is_none());
        prop_assert_eq!(safe, first_some.is_none());

        match verdict {
            StateVerdict::Safe => prop_assert!(safe),
            StateVerdict::Possible(violation) => {
                prop_assert_eq!(first_every, None);
                prop_assert_eq!(Some(violation.at()), first_some);
            }
            StateVerdict::Certain(violation) => prop_assert_eq!(Some(violation.at()), first_every),
        }
    }
}

/// A hand-checked conflict and a certain mismatch.
#[test]
fn hand_checked_conflict_and_mismatch() {
    let timeline = StateTimeline::new(0, 0, 8);
    let on = [StateImpact::Set { at: 0, value: 1 }];
    let off = [StateImpact::Set { at: 0, value: 2 }];
    let flexible = [
        FlexibleState {
            activity: ActivityId(1),
            window: Window::new(0, 4).expect("valid"),
            impacts: &on,
        },
        FlexibleState {
            activity: ActivityId(2),
            window: Window::new(3, 6).expect("valid"),
            impacts: &off,
        },
    ];
    let verdict = StateEnvelope::new(&timeline, &flexible)
        .expect("valid")
        .check()
        .expect("memory");
    assert_eq!(
        verdict,
        StateVerdict::Possible(StateEnvelopeViolation::Conflict {
            at: 3,
            setters: vec![ActivityId(1), ActivityId(2)],
        })
    );

    // A requirement of value 1 during [2, 4) while nothing can set 1 before tick 5.
    let late = [StateImpact::Set { at: 5, value: 1 }];
    let need = [StateImpact::Require {
        start: 0,
        end: 2,
        value: 1,
    }];
    let flexible = [
        FlexibleState {
            activity: ActivityId(1),
            window: Window::at(0),
            impacts: &late,
        },
        FlexibleState {
            activity: ActivityId(3),
            window: Window::new(2, 2).expect("valid"),
            impacts: &need,
        },
    ];
    let verdict = StateEnvelope::new(&timeline, &flexible)
        .expect("valid")
        .check()
        .expect("memory");
    assert_eq!(
        verdict,
        StateVerdict::Certain(StateEnvelopeViolation::Mismatch {
            at: 2,
            required: 1,
            requirer: ActivityId(3),
        })
    );
}

/// A requirement for the rest of time is decided without walking every tick.
#[test]
fn unbounded_requirement_terminates() {
    let mut timeline = StateTimeline::new(0, 0, 8);
    timeline
        .add(
            ActivityId(1),
            StateImpact::Require {
                start: 5,
                end: Tick::MAX,
                value: 0,
            },
        )
        .expect("valid");
    let late = [StateImpact::Set { at: 0, value: 1 }];
    let flexible = [FlexibleState {
        activity: ActivityId(2),
        window: Window::new(0, 1_000_000_000).expect("valid"),
        impacts: &late,
    }];
    let verdict = StateEnvelope::new(&timeline, &flexible)
        .expect("valid")
        .check()
        .expect("memory");
    // Once the set has happened in every schedule (at the latest start), the requirement fails
    // in every schedule; found without walking a billion ticks.
    assert_eq!(
        verdict,
        StateVerdict::Certain(StateEnvelopeViolation::Mismatch {
            at: 1_000_000_000,
            required: 0,
            requirer: ActivityId(1),
        })
    );

    let fixed_only = StateEnvelope::new(&timeline, &[]).expect("valid");
    assert_eq!(fixed_only.check(), Ok(StateVerdict::Safe));
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
        match StateEnvelope::new(&timeline, &flexible)
            .expect("valid")
            .check()
        {
            Ok(StateVerdict::Safe) => safe += 1,
            Ok(StateVerdict::Possible(_)) => possible += 1,
            Ok(StateVerdict::Certain(_)) => certain += 1,
            Err(error) => panic!("unexpected {error}"),
        }
    }

    assert!(
        safe > 10 && possible > 10 && certain > 10,
        "{safe} {possible} {certain}"
    );
}
