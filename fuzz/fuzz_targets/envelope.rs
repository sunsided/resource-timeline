//! Level envelopes over arbitrary fixed impacts and flexible activities, extreme values
//! included: no panic, and the envelopes are sound for a concrete schedule.
//!
//! The harness builds a timeline with fixed impacts and up to three flexible activities with
//! short start windows, then picks one start per activity inside its window and builds that
//! concrete schedule as a plain timeline. Checked:
//!
//! - the concrete level lies within `[lower_at, upper_at]` at every probe tick;
//! - a `Safe` verdict means the concrete schedule has no violation;
//! - a `Certain` violation is broken by the concrete schedule at the reported tick.

#![no_main]

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use resource_timeline::envelope::{Envelope, EnvelopeViolation, Flexible, Verdict};
use resource_timeline::{ActivityId, Bound, Bounds, Impact, Tick, Timeline, Window};

/// Largest number of fixed impacts per input.
const MAX_FIXED: usize = 6;

/// Largest number of flexible activities per input.
const MAX_FLEXIBLE: usize = 3;

/// Largest number of impacts per flexible activity.
const MAX_IMPACTS: usize = 2;

/// Widest start window, in ticks.
const MAX_WIDTH: u8 = 8;

/// Impact budget of the timelines; larger than any input needs.
const BUDGET: usize = 32;

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

/// A flexible activity and the start the concrete schedule picks for it.
#[derive(Debug, Arbitrary)]
struct RawFlexible {
    /// The earliest start.
    earliest: Value,
    /// The window width, reduced modulo `MAX_WIDTH + 1`.
    width: u8,
    /// The concrete start, as an offset into the window (reduced modulo its width plus one).
    pick: u8,
    /// The impacts relative to the start, cut to [`MAX_IMPACTS`].
    impacts: Vec<RawImpact>,
}

/// The timeline and activities to check.
#[derive(Debug, Arbitrary)]
struct Input {
    /// The initial level.
    initial: Value,
    /// The lower bound.
    min: Value,
    /// The upper bound.
    max: Value,
    /// The fixed impacts, cut to [`MAX_FIXED`].
    fixed: Vec<RawImpact>,
    /// The flexible activities, cut to [`MAX_FLEXIBLE`].
    flexible: Vec<RawFlexible>,
}

/// A flexible activity in owned form: its window, impacts and concrete start.
struct Activity {
    /// The allowed starts.
    window: Window,
    /// The impacts relative to the start.
    impacts: Vec<Impact>,
    /// The start the concrete schedule uses, inside the window.
    start: Tick,
}

fuzz_target!(|input: Input| {
    let Some(timeline) = base(&input) else {
        return;
    };
    let activities: Vec<Activity> = input
        .flexible
        .iter()
        .take(MAX_FLEXIBLE)
        .map(activity)
        .collect();
    let flexible: Vec<Flexible<'_>> = (1..)
        .zip(&activities)
        .map(|(id, activity)| Flexible {
            activity: ActivityId(id),
            window: activity.window,
            impacts: &activity.impacts,
        })
        .collect();
    let Ok(envelope) = Envelope::new(&timeline, &flexible) else {
        return;
    };
    let Some(concrete) = concrete(&timeline, &activities) else {
        return;
    };

    check_bounds(&envelope, &concrete);
    check_verdict(&envelope, &concrete);
});

/// The timeline with the fixed impacts that `input` describes, or `None` if its bounds are
/// invalid. Invalid fixed impacts are skipped.
fn base(input: &Input) -> Option<Timeline> {
    let bounds = Bounds::new(input.min.get(), input.max.get()).ok()?;
    let mut timeline = Timeline::depletable(0, input.initial.get(), bounds, BUDGET).ok()?;
    for raw in input.fixed.iter().take(MAX_FIXED) {
        let _ = timeline.add(ActivityId(0), raw.impact());
    }

    Some(timeline)
}

/// The owned activity `raw` describes.
fn activity(raw: &RawFlexible) -> Activity {
    let earliest = raw.earliest.get();
    let width = raw.width % (MAX_WIDTH + 1);
    let latest = earliest.saturating_add(i64::from(width));
    Activity {
        window: Window::new(earliest, latest).expect("earliest <= latest"),
        impacts: raw
            .impacts
            .iter()
            .take(MAX_IMPACTS)
            .map(|raw| raw.impact())
            .collect(),
        start: earliest
            .saturating_add(i64::from(raw.pick % (width + 1)))
            .min(latest),
    }
}

/// `impact` with every tick moved by `by`, or `None` on overflow.
fn shifted(impact: Impact, by: Tick) -> Option<Impact> {
    Some(match impact {
        Impact::Step { at, delta } => Impact::Step {
            at: at.checked_add(by)?,
            delta,
        },
        Impact::Claim { start, end, amount } => Impact::Claim {
            start: start.checked_add(by)?,
            end: end.checked_add(by)?,
            amount,
        },
        Impact::Rate {
            start,
            end,
            per_tick,
        } => Impact::Rate {
            start: start.checked_add(by)?,
            end: end.checked_add(by)?,
            per_tick,
        },
    })
}

/// The timeline with every activity placed at its concrete start, or `None` if that start
/// shifts an impact out of range or the timeline rejects it.
fn concrete(timeline: &Timeline, activities: &[Activity]) -> Option<Timeline> {
    let mut concrete = timeline.clone();
    for (id, activity) in (1..).zip(activities) {
        for &impact in &activity.impacts {
            concrete
                .add(ActivityId(id), shifted(impact, activity.start)?)
                .ok()?;
        }
    }

    Some(concrete)
}

/// The ticks worth probing: the start and every tick an impact of `timeline` changes
/// something, with their neighbours.
fn probes(timeline: &Timeline) -> Vec<Tick> {
    let edges = timeline
        .impacts()
        .iter()
        .flat_map(|&(_, impact)| match impact {
            Impact::Step { at, .. } => [at, at],
            Impact::Claim { start, end, .. } | Impact::Rate { start, end, .. } => [start, end],
        });
    core::iter::once(timeline.start())
        .chain(edges)
        .flat_map(|t| [t.saturating_sub(1), t, t.saturating_add(1)])
        .filter(|&t| t >= timeline.start())
        .collect()
}

/// Checks that the concrete level lies between the envelopes wherever all three are defined.
fn check_bounds(envelope: &Envelope<'_>, concrete: &Timeline) {
    for t in probes(concrete) {
        let (Ok(lower), Ok(upper), Ok(level)) = (
            envelope.lower_at(t),
            envelope.upper_at(t),
            concrete.level_at(t),
        ) else {
            continue;
        };
        assert!(
            lower <= level && level <= upper,
            "level {level} at {t} outside [{lower}, {upper}]"
        );
    }
}

/// Checks the verdict against the concrete schedule.
fn check_verdict(envelope: &Envelope<'_>, concrete: &Timeline) {
    match envelope.check() {
        Ok(Verdict::Safe) => {
            if let Ok(found) = concrete.first_violation() {
                assert!(found.is_none(), "safe, but the schedule breaks {found:?}");
            }
        }
        Ok(Verdict::Certain(violation)) => check_certain(&violation, concrete),
        Ok(Verdict::Possible(_)) | Err(_) => {}
    }
}

/// Checks that the concrete schedule breaks a certain violation's bound at its tick.
fn check_certain(violation: &EnvelopeViolation, concrete: &Timeline) {
    let Ok(level) = concrete.level_at(violation.at) else {
        return;
    };
    let broken = match violation.bound {
        Bound::BelowMin(min) | Bound::BelowTarget(min) => level < min,
        Bound::AboveMax(max) => level > max,
    };
    assert!(
        broken,
        "certain {violation:?}, but the schedule has {level}"
    );
}
