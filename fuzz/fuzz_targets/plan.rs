//! Plans under arbitrary placements and searches, extreme values included: no panic, and every
//! answer that comes back is consistent.
//!
//! The harness builds a heap plan and a fixed-capacity copy with a battery, a channel and a
//! mode timeline, places fuzzer-chosen activities (keeping only those that leave the plan
//! violation-free), then probes one more activity. Checked:
//!
//! - both copies accept and reject the same placements and answer `fits` and `earliest_start`
//!   alike, until the fixed one runs out of room;
//! - a rejected placement leaves the plan unchanged;
//! - `fits` at a start agrees with placing the activity on a copy and checking it;
//! - an earliest start lies in the window and fits, and in a short window no start fits when
//!   the search answers `None`.

#![no_main]

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use resource_timeline::storage::{FixedPlan, PlanStorage};
use resource_timeline::{
    ActivityId, Bounds, Demand, Error, Impact, Plan, StateImpact, StateTimeline, Tick, Timeline,
    Window,
};

/// Capacity of each timeline in the fixed-storage copy.
const N: usize = 16;

/// The fixed-storage plan: two numeric timelines and one state timeline.
type Fixed = FixedPlan<2, 1, N>;

/// Largest number of placements per input.
const MAX_PLACEMENTS: usize = 10;

/// Largest number of demands per activity.
const MAX_DEMANDS: usize = 3;

/// Impact budget of every timeline; never reached, because `fits` does not consult it.
const BUDGET: usize = 64;

/// Pieces the earliest-start search may examine.
const MAX_CHECKS: usize = 1_000;

/// Widest window in which a `None` answer is cross-checked start by start.
const SCAN_WIDTH: i64 = 48;

/// The activity whose start is probed.
const PROBE: ActivityId = ActivityId(99);

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

/// A demand with fuzzer-chosen fields, valid or not.
#[derive(Debug, Clone, Copy, Arbitrary)]
enum RawDemand {
    /// A step on the battery (`at`, `delta`).
    Step(Value, Value),
    /// A claim on the battery or the channel (`start`, `end`, `amount`).
    Claim(bool, Value, Value, Value),
    /// A rate on the battery (`start`, `end`, `per_tick`).
    Rate(Value, Value, Value),
    /// Sets the mode (`at`, `value % 3`).
    Set(Value, u8),
    /// Requires a mode (`start`, `end`, `value % 3`).
    Require(Value, Value, u8),
}

impl RawDemand {
    /// The demand this describes, on the timelines of [`build`].
    fn demand(self, handles: &Handles) -> Demand {
        let level = |timeline, impact| Demand::Level { timeline, impact };
        let state = |impact| Demand::State {
            timeline: handles.mode,
            impact,
        };
        match self {
            Self::Step(at, delta) => level(
                handles.battery,
                Impact::Step {
                    at: at.get(),
                    delta: delta.get(),
                },
            ),
            Self::Claim(channel, start, end, amount) => level(
                if channel {
                    handles.channel
                } else {
                    handles.battery
                },
                Impact::Claim {
                    start: start.get(),
                    end: end.get(),
                    amount: amount.get(),
                },
            ),
            Self::Rate(start, end, per_tick) => level(
                handles.battery,
                Impact::Rate {
                    start: start.get(),
                    end: end.get(),
                    per_tick: per_tick.get(),
                },
            ),
            Self::Set(at, value) => state(StateImpact::Set {
                at: at.get(),
                value: u32::from(value % 3),
            }),
            Self::Require(start, end, value) => state(StateImpact::Require {
                start: start.get(),
                end: end.get(),
                value: u32::from(value % 3),
            }),
        }
    }
}

/// An activity: a start and its demands, cut to [`MAX_DEMANDS`].
#[derive(Debug, Arbitrary)]
struct Activity {
    /// The start tick.
    start: Value,
    /// The demands, relative to the start.
    demands: Vec<RawDemand>,
}

/// The plan to build and the activity to probe.
#[derive(Debug, Arbitrary)]
struct Input {
    /// The battery's initial level.
    initial: Value,
    /// The battery's lower bound.
    min: Value,
    /// The battery's upper bound.
    max: Value,
    /// The channel's capacity.
    capacity: Value,
    /// The activities to place, cut to [`MAX_PLACEMENTS`].
    placed: Vec<Activity>,
    /// The probed activity; its start is the start probed with `fits`.
    probe: Activity,
    /// The search window, from the probe start on.
    width: u8,
}

/// The handles of the timelines of a plan built by [`build`], identical for both storages.
struct Handles {
    /// The depletable battery.
    battery: resource_timeline::TimelineId,
    /// The claimable channel.
    channel: resource_timeline::TimelineId,
    /// The mode.
    mode: resource_timeline::StateId,
}

fuzz_target!(|input: Input| {
    let Some((mut heap, handles)) = build::<resource_timeline::storage::Heap>(&input) else {
        return;
    };
    let mut fixed = build::<Fixed>(&input).map(|(plan, _)| plan);

    for (id, activity) in (0..).zip(input.placed.iter().take(MAX_PLACEMENTS)) {
        let demands = demands(activity, &handles);
        let result = place_safely(&mut heap, ActivityId(id), activity.start.get(), &demands);
        if let Some(copy) = &mut fixed {
            match place_safely(copy, ActivityId(id), activity.start.get(), &demands) {
                Err(Error::CapacityExceeded { .. }) => fixed = None,
                other => assert_eq!(other, result, "heap and fixed placements differ"),
            }
        }
    }

    let demands = demands(&input.probe, &handles);
    let start = input.probe.start.get();
    check_fits(&heap, &demands, start);
    let window = Window::new(start, start.saturating_add(i64::from(input.width)))
        .expect("earliest <= latest");
    check_search(&heap, &demands, window);
    if let Some(copy) = &fixed {
        compare_fixed(&heap, copy, &demands, start, window);
    }
});

/// The heap or fixed plan `input` describes, or `None` if its bounds are invalid.
fn build<P: PlanStorage>(input: &Input) -> Option<(Plan<P>, Handles)> {
    let bounds = Bounds::new(input.min.get(), input.max.get()).ok()?;
    let battery = Timeline::depletable_in(0, input.initial.get(), bounds, BUDGET).ok()?;
    let channel = Timeline::claimable_in(0, input.capacity.get(), BUDGET).ok()?;
    let mut plan = Plan::new_in();
    let handles = Handles {
        battery: plan.add_timeline(battery).expect("room for two"),
        channel: plan.add_timeline(channel).expect("room for two"),
        mode: plan
            .add_state(StateTimeline::new_in(0, 0, BUDGET))
            .expect("room for one"),
    };
    if plan.first_violation().ok()?.is_some() {
        return None;
    }

    Some((plan, handles))
}

/// The demands of `activity`, cut to [`MAX_DEMANDS`].
fn demands(activity: &Activity, handles: &Handles) -> Vec<Demand> {
    activity
        .demands
        .iter()
        .take(MAX_DEMANDS)
        .map(|raw| raw.demand(handles))
        .collect()
}

/// Places `demands`, checks that a rejected placement changed nothing, and takes the
/// activity out again if it left a violation (or the check overflowed), so the plan stays
/// violation-free.
fn place_safely<P: PlanStorage>(
    plan: &mut Plan<P>,
    activity: ActivityId,
    start: Tick,
    demands: &[Demand],
) -> Result<bool, Error>
where
    Plan<P>: Clone + PartialEq + core::fmt::Debug,
{
    let before = plan.clone();
    if let Err(error) = plan.place(activity, start, demands) {
        assert_eq!(*plan, before, "rejected placement changed the plan");
        return Err(error);
    }

    let safe = matches!(plan.first_violation(), Ok(None));
    if !safe {
        plan.remove(activity);
    }

    Ok(safe)
}

/// Whether placing `demands` at `start` on a copy of `plan` succeeds and leaves no violation.
fn fits_by_placing(plan: &Plan, demands: &[Demand], start: Tick) -> bool {
    let mut copy = plan.clone();
    copy.place(PROBE, start, demands).is_ok() && copy.first_violation() == Ok(None)
}

/// Checks `fits` against [`fits_by_placing`] at `start`.
fn check_fits(plan: &Plan, demands: &[Demand], start: Tick) {
    if let Ok(fits) = plan.fits(demands, start) {
        assert_eq!(
            fits,
            fits_by_placing(plan, demands, start),
            "fits at {start}"
        );
    }
}

/// Runs the earliest-start search and checks its answer.
fn check_search(plan: &Plan, demands: &[Demand], window: Window) {
    match plan.earliest_start(demands, window, MAX_CHECKS) {
        Ok(Some(start)) => {
            assert!(window.contains(start), "{start} outside {window:?}");
            assert_eq!(
                plan.fits(demands, start),
                Ok(true),
                "found {start} does not fit"
            );
        }
        Ok(None) if window.latest() - window.earliest() <= SCAN_WIDTH => {
            for start in window.earliest()..=window.latest() {
                assert_ne!(plan.fits(demands, start), Ok(true), "missed {start}");
            }
        }
        _ => {}
    }
}

/// Checks that the fixed copy answers `fits` and `earliest_start` like the heap plan, unless
/// it runs out of room.
fn compare_fixed(
    heap: &Plan,
    fixed: &Plan<Fixed>,
    demands: &[Demand],
    start: Tick,
    window: Window,
) {
    let found = fixed.earliest_start(demands, window, MAX_CHECKS);
    if !matches!(found, Err(Error::CapacityExceeded { .. })) {
        assert_eq!(
            found,
            heap.earliest_start(demands, window, MAX_CHECKS),
            "heap and fixed searches differ"
        );
    }

    let fits = fixed.fits(demands, start);
    if !matches!(fits, Err(Error::CapacityExceeded { .. })) {
        assert_eq!(
            fits,
            heap.fits(demands, start),
            "heap and fixed fits differ"
        );
    }
}
