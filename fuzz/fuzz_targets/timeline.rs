//! Timelines under arbitrary edits, extreme values included: no panic, and every answer that
//! comes back is right.
//!
//! The harness replays one sequence of adds, removes and targets into a heap timeline and a
//! fixed-capacity copy, then checks:
//!
//! - both copies accept and reject the same edits, until the fixed one runs out of room;
//! - `level_at` equals an `i128` evaluation of the documented `Impact` formula wherever that
//!   fits a `Level`, and fails only with `Error::Overflow` otherwise;
//! - a reported first violation names a level that `level_at` confirms and that really breaks
//!   the named bound, and both copies report the same one;
//! - the incrementally edited timeline reports the same first violation as one rebuilt from
//!   its remaining impacts and targets;
//! - an exported profile answers `level_at` exactly like the timeline.

#![no_main]

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use resource_timeline::storage::{Fixed, Storage};
use resource_timeline::{
    ActivityId, Bound, Bounds, Error, Impact, Level, Target, Tick, Timeline, Violation,
};

/// Capacity of the fixed-storage copy.
const N: usize = 16;

/// Largest number of edits replayed per input.
const MAX_OPS: usize = 24;

/// A tick or level: mostly small, sometimes extreme, to reach the overflow paths.
#[derive(Debug, Clone, Copy, Arbitrary)]
enum Value {
    /// A small value.
    Small(i8),
    /// `i64::MIN`.
    Min,
    /// `i64::MAX`.
    Max,
    /// Any value.
    Any(i64),
}

impl Value {
    /// The value as an `i64`.
    fn get(self) -> i64 {
        match self {
            Self::Small(v) => i64::from(v),
            Self::Min => i64::MIN,
            Self::Max => i64::MAX,
            Self::Any(v) => v,
        }
    }
}

/// An impact with fuzzer-chosen fields, valid or not.
#[derive(Debug, Clone, Copy, Arbitrary)]
enum RawImpact {
    /// [`Impact::Step`] with `at` and `delta`.
    Step(Value, Value),
    /// [`Impact::Claim`] with `start`, `end` and `amount`.
    Claim(Value, Value, Value),
    /// [`Impact::Rate`] with `start`, `end` and `per_tick`.
    Rate(Value, Value, Value),
}

impl RawImpact {
    /// The impact this describes.
    fn impact(self) -> Impact {
        match self {
            Self::Step(at, delta) => Impact::Step {
                at: at.get(),
                delta: delta.get(),
            },
            Self::Claim(start, end, amount) => Impact::Claim {
                start: start.get(),
                end: end.get(),
                amount: amount.get(),
            },
            Self::Rate(start, end, per_tick) => Impact::Rate {
                start: start.get(),
                end: end.get(),
                per_tick: per_tick.get(),
            },
        }
    }
}

/// One edit of the timeline; activities are small so removes hit something.
#[derive(Debug, Clone, Copy, Arbitrary)]
enum Op {
    /// `add(ActivityId(activity % 4), impact)`.
    Add(u8, RawImpact),
    /// `remove(ActivityId(activity % 4))`.
    Remove(u8),
    /// `add_target(Target { at, min })`.
    Target(Value, Value),
    /// `remove_targets_at(at)`.
    RemoveTargets(Value),
}

/// The timeline to create and the edits to replay.
#[derive(Debug, Arbitrary)]
struct Input {
    /// Claimable instead of depletable.
    claimable: bool,
    /// The start tick.
    start: Value,
    /// The initial level (the capacity of a claimable timeline).
    initial: Value,
    /// The lower bound of a depletable timeline.
    min: Value,
    /// The upper bound of a depletable timeline.
    max: Value,
    /// The edits, cut to [`MAX_OPS`].
    ops: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let Some(mut heap) = create::<resource_timeline::storage::Heap>(&input) else {
        return;
    };
    let mut fixed = create::<Fixed<N>>(&input);

    for &op in input.ops.iter().take(MAX_OPS) {
        let result = apply(&mut heap, op);
        if let Some(copy) = &mut fixed {
            match apply(copy, op) {
                Err(Error::CapacityExceeded { .. }) => fixed = None,
                other => assert_eq!(other, result, "heap and fixed disagree on {op:?}"),
            }
        }
    }

    check_levels(&heap);
    check_export(&heap);
    let violation = checked_violation(&heap);
    if let Some(copy) = &fixed {
        assert_eq!(plain(&checked_violation(copy)), plain(&violation));
    }

    if let Some(rebuilt) = rebuild(&input, &heap) {
        assert_eq!(rebuilt.first_violation(), violation, "rebuild differs");
    }
});

/// Creates the timeline `input` describes in storage `S`, or `None` if its bounds are invalid.
fn create<S: Storage>(input: &Input) -> Option<Timeline<S>> {
    let (start, initial) = (input.start.get(), input.initial.get());
    if input.claimable {
        return Timeline::claimable_in(start, initial, MAX_OPS).ok();
    }

    let bounds = Bounds::new(input.min.get(), input.max.get()).ok()?;
    Timeline::depletable_in(start, initial, bounds, MAX_OPS).ok()
}

/// Applies `op` to `timeline`; removals report their count as `Ok`.
fn apply<S: Storage>(timeline: &mut Timeline<S>, op: Op) -> Result<usize, Error> {
    match op {
        Op::Add(activity, impact) => timeline
            .add(ActivityId(u32::from(activity % 4)), impact.impact())
            .map(|()| 1),
        Op::Remove(activity) => Ok(timeline.remove(ActivityId(u32::from(activity % 4)))),
        Op::Target(at, min) => timeline
            .add_target(Target {
                at: at.get(),
                min: min.get(),
            })
            .map(|()| 1),
        Op::RemoveTargets(at) => Ok(timeline.remove_targets_at(at.get())),
    }
}

/// A fresh timeline with the remaining impacts and targets of `edited`, added in order.
fn rebuild(input: &Input, edited: &Timeline) -> Option<Timeline> {
    let mut timeline = create(input)?;
    for &(activity, impact) in edited.impacts() {
        timeline.add(activity, impact).expect("accepted before");
    }

    for &target in edited.targets() {
        timeline.add_target(target).expect("accepted before");
    }

    Some(timeline)
}

/// The ticks worth probing: the start and every tick an impact or target changes something,
/// with their neighbours.
fn probes<S: Storage>(timeline: &Timeline<S>) -> Vec<Tick> {
    let edges = timeline
        .impacts()
        .iter()
        .flat_map(|&(_, impact)| match impact {
            Impact::Step { at, .. } => [at, at],
            Impact::Claim { start, end, .. } | Impact::Rate { start, end, .. } => [start, end],
        });
    let targets = timeline.targets().iter().map(|target| target.at);
    core::iter::once(timeline.start())
        .chain(edges)
        .chain(targets)
        .flat_map(|t| [t.saturating_sub(1), t, t.saturating_add(1)])
        .filter(|&t| t >= timeline.start())
        .collect()
}

/// The level at `t` by the documented formula in `i128`, or `None` if a term overflows.
fn oracle<S: Storage>(timeline: &Timeline<S>, t: Tick) -> Option<i128> {
    timeline
        .impacts()
        .iter()
        .try_fold(i128::from(timeline.initial()), |level, &(_, impact)| {
            let effect = match impact {
                Impact::Step { at, delta } if at <= t => i128::from(delta),
                Impact::Claim { start, end, amount } if start <= t && t < end => {
                    -i128::from(amount)
                }
                Impact::Rate {
                    start,
                    end,
                    per_tick,
                } if start <= t => {
                    i128::from(per_tick).checked_mul(i128::from(t.min(end)) - i128::from(start))?
                }
                Impact::Step { .. } | Impact::Claim { .. } | Impact::Rate { .. } => 0,
            };
            level.checked_add(effect)
        })
}

/// Checks `level_at` against [`oracle`] at every probe tick.
fn check_levels<S: Storage>(timeline: &Timeline<S>) {
    for t in probes(timeline) {
        let expected = oracle(timeline, t).and_then(|level| Level::try_from(level).ok());
        match (timeline.level_at(t), expected) {
            (Ok(level), Some(expected)) => assert_eq!(level, expected, "level at {t}"),
            (Err(Error::Overflow), None) => {}
            (got, expected) => panic!("level at {t}: got {got:?}, oracle {expected:?}"),
        }
    }
}

/// Checks that the exported profile reports the timeline's level at every probe tick.
fn check_export<S: Storage>(timeline: &Timeline<S>) {
    let Ok(profile) = timeline.export() else {
        return;
    };

    for t in probes(timeline) {
        assert_eq!(
            profile.level_at(t),
            timeline.level_at(t),
            "profile level at {t}"
        );
    }
}

/// The first violation of `timeline`, after checking that `level_at` confirms its level and
/// that the level breaks the named bound.
fn checked_violation<S: Storage>(timeline: &Timeline<S>) -> Result<Option<Violation<S>>, Error> {
    let violation = timeline.first_violation();
    if let Ok(Some(found)) = &violation {
        assert_eq!(timeline.level_at(found.at), Ok(found.level));
        let broken = match found.bound {
            Bound::BelowMin(min) | Bound::BelowTarget(min) => found.level < min,
            Bound::AboveMax(max) => found.level > max,
        };
        assert!(broken, "{found:?} breaks nothing");
    } else if let Err(error) = &violation {
        assert_eq!(*error, Error::Overflow, "unexpected error");
    }

    violation
}

/// A violation's tick, level, bound and contributors, independent of its storage.
type Plain = (Tick, Level, Bound, Vec<ActivityId>);

/// A violation without its storage, to compare heap and fixed results.
fn plain<S: Storage>(
    violation: &Result<Option<Violation<S>>, Error>,
) -> Result<Option<Plain>, Error> {
    violation.as_ref().map_err(Clone::clone).map(|found| {
        found
            .as_ref()
            .map(|v| (v.at, v.level, v.bound, v.contributors.to_vec()))
    })
}
