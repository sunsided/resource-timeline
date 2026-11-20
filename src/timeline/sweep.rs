//! The sweep that finds the first bound violation of a timeline.
//!
//! The sweep walks the sorted change points once, as a sequence of linear [`Segment`]s.
//! Within a segment the first out-of-bounds tick is either its first tick or the tick at which
//! the line crosses a bound, which integer division finds directly; end-state targets inside
//! the segment are evaluated on the line.
//!
//! ```mermaid
//! graph LR
//!     points["change points: profile merged with candidates"] --> groups["net changes per tick"]
//!     groups --> scan["segment by segment: level at the point, crossing inside"]
//!     scan --> explain["net impacts per activity at the violating tick"]
//! ```

use super::Bounds;
use super::profile::Point;
use crate::storage::{Seq, Storage};
use crate::{ActivityId, Bound, Container, Error, Impact, Level, Target, Tick, Violation};

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
pub(crate) struct Frame<'a> {
    /// The first tick of the timeline.
    pub(crate) start: Tick,
    /// The level at `start` before changes at `start` apply.
    pub(crate) initial: Level,
    /// The range the level must stay in.
    pub(crate) bounds: Bounds,
    /// The end-state targets, sorted by [`Target::key`], none before `start`.
    pub(crate) targets: &'a [Target],
}

/// One linear piece of the level: from a change point up to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Segment {
    /// The first tick of the piece. All changes at this tick are applied.
    pub(crate) at: Tick,
    /// The level at `at`.
    pub(crate) level: i128,
    /// The level change per tick after `at`, until `next`.
    pub(crate) slope: i128,
    /// The tick of the next change point, or `None` for the last piece, which lasts forever
    /// with slope zero (every rate ends).
    pub(crate) next: Option<Tick>,
}

impl Segment {
    /// The level at `t`, which must lie in the piece.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if the level exceeds `i128`.
    pub(crate) fn level_at(&self, t: Tick) -> Result<i128, Error> {
        let elapsed = i128::from(t) - i128::from(self.at);
        self.slope
            .checked_mul(elapsed)
            .and_then(|change| self.level.checked_add(change))
            .ok_or(Error::Overflow)
    }

    /// The first tick strictly inside the piece at which the line crosses a bound.
    ///
    /// The level at `at` is in bounds. The line is checked up to the tick before `next`; the
    /// level at `next` includes the changes there and belongs to the next piece.
    fn crossing(&self, bounds: Bounds) -> Result<Option<Breach>, Error> {
        let Some(next) = self.next else {
            debug_assert_eq!(self.slope, 0, "the last piece has a slope");
            return Ok(None);
        };

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

    /// The first missed target in the piece, consuming the targets before `next` from the
    /// front of `targets`.
    fn missed_target(&self, targets: &mut &[Target]) -> Result<Option<Breach>, Error> {
        let inside = targets
            .iter()
            .take_while(|target| self.next.is_none_or(|next| target.at < next))
            .count();
        let (now, rest) = targets.split_at(inside);
        *targets = rest;

        for target in now {
            let level = self.level_at(target.at)?;
            if level < i128::from(target.min) {
                return Ok(Some(Breach {
                    at: target.at,
                    level,
                    bound: Bound::BelowTarget(target.min),
                }));
            }
        }

        Ok(None)
    }
}

/// The pieces of the level built from a frame and two sorted point slices, in time order.
///
/// The first piece starts at the frame start with the changes there applied.
pub(crate) struct Segments<'a> {
    /// The merged change points not yet reached.
    groups: core::iter::Peekable<Grouped<'a>>,
    /// The piece to yield next, `None` once the last piece was yielded.
    current: Option<Segment>,
}

impl<'a> Segments<'a> {
    /// The pieces of the level that starts at `frame` and changes at `base` and `extra`.
    ///
    /// Both slices must be sorted by tick and must not contain ticks before `frame.start`.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if the level at the start exceeds `i128`.
    pub(crate) fn new(
        frame: &Frame<'_>,
        base: &'a [Point],
        extra: &'a [Point],
    ) -> Result<Self, Error> {
        let mut groups = Grouped { base, extra }.peekable();
        let mut first = Segment {
            at: frame.start,
            level: i128::from(frame.initial),
            slope: 0,
            next: None,
        };
        if let Some(point) = groups.next_if(|point| point.at == frame.start) {
            first.level = first.level.checked_add(point.step).ok_or(Error::Overflow)?;
            first.slope = point.slope;
        }

        first.next = groups.peek().map(|point| point.at);
        Ok(Self {
            groups,
            current: Some(first),
        })
    }

    /// The piece after `segment`, which starts at the next change point.
    fn successor(&mut self, segment: &Segment) -> Result<Option<Segment>, Error> {
        let Some(point) = self.groups.next() else {
            return Ok(None);
        };

        let level = segment
            .level_at(point.at)?
            .checked_add(point.step)
            .ok_or(Error::Overflow)?;
        let slope = segment
            .slope
            .checked_add(point.slope)
            .ok_or(Error::Overflow)?;
        Ok(Some(Segment {
            at: point.at,
            level,
            slope,
            next: self.groups.peek().map(|point| point.at),
        }))
    }
}

impl Iterator for Segments<'_> {
    type Item = Result<Segment, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let segment = self.current.take()?;
        match self.successor(&segment) {
            Ok(next) => self.current = next,
            Err(error) => return Some(Err(error)),
        }

        Some(Ok(segment))
    }
}

/// Finds the first tick at which the level built from `base` and `extra` leaves
/// `frame.bounds` or misses a target.
///
/// Both slices must be sorted by tick and must not contain ticks before `frame.start`. `base`
/// is the timeline profile, `extra` holds the points of tentatively added impacts; points of
/// both at the same tick are netted. At the same tick a broken bound is reported before a
/// missed target.
///
/// # Errors
///
/// [`Error::Overflow`] if a level along a rate segment exceeds `i128`.
pub(crate) fn first_breach(
    frame: &Frame<'_>,
    base: &[Point],
    extra: &[Point],
) -> Result<Option<Breach>, Error> {
    let mut targets = frame.targets;
    for segment in Segments::new(frame, base, extra)? {
        let segment = segment?;
        if let Some(bound) = broken_bound(frame.bounds, segment.level) {
            return Ok(Some(Breach {
                at: segment.at,
                level: segment.level,
                bound,
            }));
        }

        let crossing = segment.crossing(frame.bounds)?;
        let target = segment.missed_target(&mut targets)?;
        let earliest = match (crossing, target) {
            (Some(crossing), Some(target)) if target.at < crossing.at => Some(target),
            (Some(crossing), _) => Some(crossing),
            (None, target) => target,
        };
        if earliest.is_some() {
            return Ok(earliest);
        }
    }

    Ok(None)
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

        let mut net = Point::loose(at, 0, 0);
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
/// - [`Error::OutOfMemory`] or [`Error::CapacityExceeded`] if the working memory cannot be
///   allocated.
pub(crate) fn explain<S: Storage>(
    impacts: &[(ActivityId, Impact)],
    breach: Breach,
) -> Result<Violation<S>, Error> {
    let level = Level::try_from(breach.level).map_err(|_| Error::Overflow)?;
    let contributors = contributors::<S>(impacts, breach.at, breach.bound)?;

    Ok(Violation {
        at: breach.at,
        level,
        bound: breach.bound,
        contributors,
    })
}

/// The activities whose net effect in force at tick `at` pushes towards `bound`.
///
/// Sorted ascending, without duplicates. `impacts` must fit an `S` impact list, which the
/// impacts of a timeline with storage `S` do.
///
/// # Errors
///
/// - [`Error::Overflow`] if a contribution overflows.
/// - [`Error::OutOfMemory`] or [`Error::CapacityExceeded`] if the working memory cannot be
///   allocated.
pub(crate) fn contributors<S: Storage>(
    impacts: &[(ActivityId, Impact)],
    at: Tick,
    bound: Bound,
) -> Result<S::List<ActivityId>, Error> {
    let net = net_per_activity::<S>(impacts, at)?;

    let mut activities = <S::List<ActivityId> as Seq<_>>::EMPTY;
    activities.try_reserve(net.len(), Container::Contributors)?;
    for &(activity, effect) in net.iter() {
        if bound.is_pushed_by(effect) {
            activities.try_push(activity, Container::Contributors)?;
        }
    }

    Ok(activities)
}

/// The summed effect of each activity's impacts in force at tick `at`, sorted by activity.
fn net_per_activity<S: Storage>(
    impacts: &[(ActivityId, Impact)],
    at: Tick,
) -> Result<S::List<(ActivityId, i128)>, Error> {
    let mut effects = <S::List<(ActivityId, i128)> as Seq<_>>::EMPTY;
    effects.try_reserve(impacts.len(), Container::Scratch)?;
    for &(activity, impact) in impacts {
        let effect = impact.contribution_at(at).ok_or(Error::Overflow)?;
        effects.try_push((activity, effect), Container::Scratch)?;
    }

    effects.sort_unstable_by_key(|&(activity, _)| activity);
    merge_adjacent(&mut effects)?;
    Ok(effects)
}

/// Sums the effects of adjacent entries with the same activity, in place.
fn merge_adjacent(effects: &mut impl Seq<(ActivityId, i128)>) -> Result<(), Error> {
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
