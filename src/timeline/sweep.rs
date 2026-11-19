//! The sweep that finds the first bound violation of a timeline.
//!
//! ```mermaid
//! graph LR
//!     impacts --> events["change events, sorted by tick"]
//!     events --> scan["net events per tick, compare with bounds"]
//!     scan --> explain["net impacts per activity at the violating tick"]
//! ```

use alloc::vec::Vec;

use super::{Bounds, Timeline};
use crate::{ActivityId, Bound, Error, Impact, Level, Tick, Violation};

/// A change of the level at one tick. A claim yields two: one at its start, one at its end.
#[derive(Debug, Clone, Copy)]
struct Event {
    /// The tick at which the change takes effect.
    at: Tick,
    /// The change, widened so that negating and summing `i64` values cannot overflow.
    delta: i128,
}

/// The first out-of-bounds tick of a timeline, before it is explained.
#[derive(Debug, Clone, Copy)]
struct Breach {
    /// The earliest tick at which the level is out of bounds.
    at: Tick,
    /// The level at that tick.
    level: i128,
    /// The broken bound.
    bound: Bound,
}

/// Finds and explains the first violation of `timeline`'s bounds.
pub(super) fn first_violation(timeline: &Timeline) -> Result<Option<Violation>, Error> {
    let events = sorted_events(&timeline.impacts)?;
    let Some(breach) = first_breach(timeline, &events)? else {
        return Ok(None);
    };

    let level = Level::try_from(breach.level).map_err(|_| Error::Overflow)?;
    let contributors = contributors(&timeline.impacts, breach.at, breach.bound)?;

    Ok(Some(Violation {
        at: breach.at,
        level,
        bound: breach.bound,
        contributors,
    }))
}

/// Turns impacts into level change events, sorted by tick.
fn sorted_events(impacts: &[(ActivityId, Impact)]) -> Result<Vec<Event>, Error> {
    let mut events = Vec::new();
    events
        .try_reserve_exact(impacts.len().saturating_mul(2))
        .map_err(|_| Error::OutOfMemory)?;

    for &(_, impact) in impacts {
        match impact {
            Impact::Step { at, delta } => events.push(Event {
                at,
                delta: i128::from(delta),
            }),
            Impact::Claim { start, end, amount } => {
                events.push(Event {
                    at: start,
                    delta: -i128::from(amount),
                });
                events.push(Event {
                    at: end,
                    delta: i128::from(amount),
                });
            }
        }
    }

    events.sort_unstable_by_key(|event| event.at);
    Ok(events)
}

/// Scans the start tick and every event tick, in order, for a level outside the bounds.
///
/// The level is constant between event ticks, so these ticks cover all ticks from the start.
fn first_breach(timeline: &Timeline, events: &[Event]) -> Result<Option<Breach>, Error> {
    let mut level = i128::from(timeline.initial);
    let mut at = timeline.start;
    let mut rest = events;

    loop {
        let same_tick = rest.iter().take_while(|event| event.at == at).count();
        let (now, later) = rest.split_at(same_tick);
        level = apply(level, now)?;
        rest = later;

        if let Some(bound) = broken_bound(timeline.bounds, level) {
            return Ok(Some(Breach { at, level, bound }));
        }

        match rest.first() {
            Some(next) => at = next.at,
            None => return Ok(None),
        }
    }
}

/// Adds the deltas of `events` to `level` with overflow checks.
fn apply(level: i128, events: &[Event]) -> Result<i128, Error> {
    events.iter().try_fold(level, |sum, event| {
        sum.checked_add(event.delta).ok_or(Error::Overflow)
    })
}

/// The bound that `level` breaks, if any.
fn broken_bound(bounds: Bounds, level: i128) -> Option<Bound> {
    if level < i128::from(bounds.min) {
        Some(Bound::BelowMin(bounds.min))
    } else if level > i128::from(bounds.max) {
        Some(Bound::AboveMax(bounds.max))
    } else {
        None
    }
}

/// The activities whose net effect in force at tick `at` pushes towards `bound`.
///
/// Sorted ascending, without duplicates.
fn contributors(
    impacts: &[(ActivityId, Impact)],
    at: Tick,
    bound: Bound,
) -> Result<Vec<ActivityId>, Error> {
    let net = net_per_activity(impacts, at)?;
    let pushes = |effect: i128| match bound {
        Bound::BelowMin(_) => effect < 0,
        Bound::AboveMax(_) => effect > 0,
    };

    let mut activities = Vec::new();
    activities
        .try_reserve_exact(net.len())
        .map_err(|_| Error::OutOfMemory)?;
    activities.extend(
        net.into_iter()
            .filter(|&(_, effect)| pushes(effect))
            .map(|(activity, _)| activity),
    );

    Ok(activities)
}

/// The summed effect of each activity's impacts in force at tick `at`, sorted by activity.
fn net_per_activity(
    impacts: &[(ActivityId, Impact)],
    at: Tick,
) -> Result<Vec<(ActivityId, i128)>, Error> {
    let mut effects = Vec::new();
    effects
        .try_reserve_exact(impacts.len())
        .map_err(|_| Error::OutOfMemory)?;
    effects.extend(
        impacts
            .iter()
            .map(|&(activity, impact)| (activity, impact.contribution_at(at))),
    );
    effects.sort_unstable_by_key(|&(activity, _)| activity);

    merge_adjacent(&mut effects)?;
    Ok(effects)
}

/// Sums the effects of adjacent entries with the same activity, in place.
fn merge_adjacent(effects: &mut Vec<(ActivityId, i128)>) -> Result<(), Error> {
    let mut merged: usize = 0;
    for read in 0..effects.len() {
        let (activity, effect) = effects[read];
        match merged.checked_sub(1).map(|last| &mut effects[last]) {
            Some((last, sum)) if *last == activity => {
                *sum = sum.checked_add(effect).ok_or(Error::Overflow)?;
            }
            _ => {
                effects[merged] = (activity, effect);
                merged += 1;
            }
        }
    }

    effects.truncate(merged);
    Ok(())
}
