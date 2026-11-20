//! The sweep that finds the first bound violation of a timeline.
//!
//! The sweep walks the sorted change points once. Between two change points the level is
//! linear, so the first out-of-bounds tick of a segment is either its first tick or the tick at
//! which the line crosses a bound, which integer division finds directly.
//!
//! ```mermaid
//! graph LR
//!     points["change points: profile merged with candidates"] --> groups["net changes per tick"]
//!     groups --> scan["segment by segment: level at the point, crossing inside"]
//!     scan --> explain["net impacts per activity at the violating tick"]
//! ```

use alloc::vec::Vec;

use super::Bounds;
use super::profile::Point;
use crate::{ActivityId, Bound, Error, Impact, Level, Tick, Violation};

/// The first out-of-bounds tick of a timeline, before it is explained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Breach {
    /// The earliest tick at which the level is out of bounds.
    pub(crate) at: Tick,
    /// The level at that tick.
    pub(crate) level: i128,
    /// The broken bound.
    pub(crate) bound: Bound,
}

/// The fixed parameters of a sweep: where it starts, from which level, against which bounds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frame {
    /// The first tick of the timeline.
    pub(crate) start: Tick,
    /// The level at `start` before changes at `start` apply.
    pub(crate) initial: Level,
    /// The range the level must stay in.
    pub(crate) bounds: Bounds,
}

/// The level and slope at the tick the sweep has reached.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    /// The tick the sweep has reached. All changes at this tick are applied.
    at: Tick,
    /// The level at `at`.
    level: i128,
    /// The level change per tick after `at`, until the next change point.
    slope: i128,
}

/// Finds the first tick at which the level built from `base` and `extra` leaves `frame.bounds`.
///
/// Both slices must be sorted by tick and must not contain ticks before `frame.start`. `base`
/// is the timeline profile, `extra` holds the points of tentatively added impacts; points of
/// both at the same tick are netted.
///
/// # Errors
///
/// [`Error::Overflow`] if a level along a rate segment exceeds `i128`.
pub(crate) fn first_breach(
    frame: Frame,
    base: &[Point],
    extra: &[Point],
) -> Result<Option<Breach>, Error> {
    let mut groups = Grouped { base, extra }.peekable();
    let mut cursor = Cursor {
        at: frame.start,
        level: i128::from(frame.initial),
        slope: 0,
    };
    if let Some(first) = groups.next_if(|point| point.at == frame.start) {
        cursor.apply(first)?;
    }

    if let Some(bound) = broken_bound(frame.bounds, cursor.level) {
        return Ok(Some(cursor.breach(bound)));
    }

    for point in groups {
        if let Some(breach) = cursor.crossing_before(point.at, frame.bounds)? {
            return Ok(Some(breach));
        }

        cursor.advance_to(point.at)?;
        cursor.apply(point)?;
        if let Some(bound) = broken_bound(frame.bounds, cursor.level) {
            return Ok(Some(cursor.breach(bound)));
        }
    }

    Ok(None)
}

impl Cursor {
    /// Applies the changes of `point`, which lies at the cursor's tick.
    fn apply(&mut self, point: Point) -> Result<(), Error> {
        self.level = self.level.checked_add(point.step).ok_or(Error::Overflow)?;
        self.slope = self.slope.checked_add(point.slope).ok_or(Error::Overflow)?;
        Ok(())
    }

    /// Moves the cursor to tick `to` along the current slope, before changes at `to` apply.
    fn advance_to(&mut self, to: Tick) -> Result<(), Error> {
        let elapsed = i128::from(to) - i128::from(self.at);
        self.level = self
            .slope
            .checked_mul(elapsed)
            .and_then(|change| self.level.checked_add(change))
            .ok_or(Error::Overflow)?;
        self.at = to;
        Ok(())
    }

    /// The first tick strictly between the cursor and `next` at which the line crosses a bound.
    ///
    /// The level at the cursor is in bounds. The line is checked up to `next - 1`; the level at
    /// `next` includes the changes there and is checked separately.
    fn crossing_before(&self, next: Tick, bounds: Bounds) -> Result<Option<Breach>, Error> {
        let last = i128::from(next) - 1 - i128::from(self.at);
        if self.slope == 0 || last < 1 {
            return Ok(None);
        }

        let (room, bound) = if self.slope < 0 {
            (
                self.level - i128::from(bounds.min),
                Bound::BelowMin(bounds.min),
            )
        } else {
            (
                i128::from(bounds.max) - self.level,
                Bound::AboveMax(bounds.max),
            )
        };

        // The first k with room - |slope| * k < 0, that is k = room / |slope| + 1.
        let steps = room / self.slope.abs() + 1;
        if steps > last {
            return Ok(None);
        }

        let at = Tick::try_from(i128::from(self.at) + steps).map_err(|_| Error::Overflow)?;
        let level = self.level + self.slope * steps;
        Ok(Some(Breach { at, level, bound }))
    }

    /// A breach at the cursor's tick and level.
    const fn breach(&self, bound: Bound) -> Breach {
        Breach {
            at: self.at,
            level: self.level,
            bound,
        }
    }
}

/// Merges two sorted point slices into one sorted sequence with one point per tick.
struct Grouped<'a> {
    /// Remaining points of the first input.
    base: &'a [Point],
    /// Remaining points of the second input.
    extra: &'a [Point],
}

impl Iterator for Grouped<'_> {
    type Item = Point;

    fn next(&mut self) -> Option<Point> {
        let at = match (self.base.first(), self.extra.first()) {
            (Some(a), Some(b)) => a.at.min(b.at),
            (Some(a), None) => a.at,
            (None, Some(b)) => b.at,
            (None, None) => return None,
        };

        let mut net = Point {
            at,
            step: 0,
            slope: 0,
        };
        take_tick(&mut self.base, &mut net);
        take_tick(&mut self.extra, &mut net);
        Some(net)
    }
}

/// Moves all leading points of `points` at `net.at` into `net`.
fn take_tick(points: &mut &[Point], net: &mut Point) {
    let count = points.iter().take_while(|point| point.at == net.at).count();
    let (now, rest) = points.split_at(count);
    for point in now {
        net.step += point.step;
        net.slope += point.slope;
    }

    *points = rest;
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

/// Explains `breach` with the activities in `impacts` that push towards the broken bound.
///
/// # Errors
///
/// - [`Error::Overflow`] if the level does not fit a [`Level`], or a contribution overflows.
/// - [`Error::OutOfMemory`] if the working memory cannot be allocated.
pub(crate) fn explain(
    impacts: &[(ActivityId, Impact)],
    breach: Breach,
) -> Result<Violation, Error> {
    let level = Level::try_from(breach.level).map_err(|_| Error::Overflow)?;
    let contributors = contributors(impacts, breach.at, breach.bound)?;

    Ok(Violation {
        at: breach.at,
        level,
        bound: breach.bound,
        contributors,
    })
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
    for &(activity, impact) in impacts {
        let effect = impact.contribution_at(at).ok_or(Error::Overflow)?;
        effects.push((activity, effect));
    }

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
