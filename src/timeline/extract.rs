//! Profile extraction: the level as breakpoints, as samples, or as a compact exported struct.
//!
//! Display and telemetry need the level over time without the impact list behind it. Three
//! views cover that, all allocation-free on fixed storage:
//!
//! - [`Timeline::breakpoints`] iterates the ticks where the line changes, each with the level
//!   there and the slope after it;
//! - [`Timeline::samples`] evaluates the level on a regular tick grid in one pass;
//! - [`Timeline::export`] copies the breakpoints into a [`LevelProfile`], a self-contained value
//!   that answers [`level_at`](LevelProfile::level_at) queries without the timeline.
//!
//! ```mermaid
//! graph LR
//!     timeline["Timeline: change points"] --> breakpoints["breakpoints: tick, level, slope"]
//!     breakpoints --> export["export: LevelProfile"]
//!     timeline --> samples["samples: level every step ticks"]
//!     export --> query["LevelProfile::level_at"]
//! ```

use super::Timeline;
use super::sweep::{Frame, Segment, Segments};
#[cfg(feature = "alloc")]
use crate::storage::Heap;
use crate::storage::{Sealed, Seq, Slot, Storage};
use crate::{Container, Error, Level, Tick};

/// A tick at which the level line of a timeline changes: it jumps, or its slope changes.
///
/// Between two consecutive breakpoints the level is `level + slope * (t - at)`; after the last
/// one it is constant (`slope` is zero there). Three `i64` per breakpoint, so a profile is as
/// compact as the line it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Breakpoint {
    /// The tick of the change.
    pub at: Tick,
    /// The level at `at`, with every impact at `at` applied.
    pub level: Level,
    /// The level change per tick after `at`, until the next breakpoint.
    pub slope: Level,
}

impl Breakpoint {
    /// The level at `t` on the line through this breakpoint.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if the level does not fit a [`Level`].
    fn level_at(&self, t: Tick) -> Result<Level, Error> {
        let elapsed = i128::from(t) - i128::from(self.at);
        let level = i128::from(self.slope)
            .checked_mul(elapsed)
            .and_then(|change| change.checked_add(i128::from(self.level)))
            .ok_or(Error::Overflow)?;
        Level::try_from(level).map_err(|_| Error::Overflow)
    }

    /// The breakpoint of `segment`, if its values fit a [`Level`].
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if the level or the slope does not fit a [`Level`].
    fn of(segment: &Segment) -> Result<Self, Error> {
        Ok(Self {
            at: segment.at,
            level: Level::try_from(segment.level).map_err(|_| Error::Overflow)?,
            slope: Level::try_from(segment.slope).map_err(|_| Error::Overflow)?,
        })
    }
}

impl Sealed for Breakpoint {}

impl Slot for Breakpoint {
    const EMPTY: Self = Self {
        at: 0,
        level: 0,
        slope: 0,
    };
}

/// The breakpoints of a timeline in time order; see [`Timeline::breakpoints`].
pub struct Breakpoints<'a> {
    /// The pieces of the level line.
    segments: Segments<'a>,
    /// The last piece yielded or skipped, to recognise pieces that continue it unchanged.
    previous: Option<Segment>,
}

impl Iterator for Breakpoints<'_> {
    type Item = Result<Breakpoint, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let segment = match self.segments.next()? {
                Ok(segment) => segment,
                Err(error) => return Some(Err(error)),
            };

            let previous = self.previous.replace(segment);
            match previous.map(|previous| continues(&previous, &segment)) {
                Some(Ok(true)) => {}
                Some(Err(error)) => return Some(Err(error)),
                Some(Ok(false)) | None => return Some(Breakpoint::of(&segment)),
            }
        }
    }
}

/// Whether `segment` lies on the line of `previous`, so it is no breakpoint.
///
/// # Errors
///
/// [`Error::Overflow`] if the level on the line exceeds `i128`.
fn continues(previous: &Segment, segment: &Segment) -> Result<bool, Error> {
    Ok(previous.slope == segment.slope && previous.level_at(segment.at)? == segment.level)
}

/// The level of a timeline at regular ticks; see [`Timeline::samples`].
pub struct Samples<'a> {
    /// The pieces of the level line not yet passed.
    segments: Segments<'a>,
    /// The piece containing the next sample tick, once reached.
    current: Option<Segment>,
    /// The next sample tick.
    next: Tick,
    /// The distance between samples. Positive.
    step: Tick,
    /// The number of samples still to yield.
    remaining: usize,
}

impl Samples<'_> {
    /// The level at the next sample tick, moving along the pieces as needed.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if the level does not fit a [`Level`].
    fn sample(&mut self) -> Result<Level, Error> {
        loop {
            match self.current {
                Some(segment) if segment.next.is_none_or(|next| self.next < next) => {
                    let level = segment.level_at(self.next)?;
                    return Level::try_from(level).map_err(|_| Error::Overflow);
                }
                _ => {
                    let segment = self.segments.next().ok_or(Error::Overflow)??;
                    self.current = Some(segment);
                }
            }
        }
    }
}

impl Iterator for Samples<'_> {
    type Item = Result<(Tick, Level), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining = self.remaining.checked_sub(1)?;
        let at = self.next;
        let level = match self.sample() {
            Ok(level) => level,
            Err(error) => {
                self.remaining = 0;
                return Some(Err(error));
            }
        };

        match at.checked_add(self.step) {
            Some(next) => self.next = next,
            None => self.remaining = 0,
        }

        Some(Ok((at, level)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining))
    }
}

/// A self-contained copy of a timeline's level line, for display and telemetry.
///
/// Holds the breakpoints only (three `i64` each), not the impacts, so it is cheap to store,
/// send or compare. With [`Fixed<N>`](crate::storage::Fixed) storage it never allocates and
/// always has room for the profile of an `N`-impact timeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LevelProfile<
    #[cfg(feature = "alloc")] S: Storage = Heap,
    #[cfg(not(feature = "alloc"))] S: Storage,
> {
    /// The breakpoint at the timeline start.
    origin: Breakpoint,
    /// The later breakpoints, sorted by tick, each a real change of the line.
    changes: S::DoubleList<Breakpoint>,
}

impl<S: Storage> LevelProfile<S> {
    /// The first tick of the profile, the start of the timeline it was exported from.
    #[must_use]
    pub const fn start(&self) -> Tick {
        self.origin.at
    }

    /// The breakpoints in time order, the first at [`start`](Self::start).
    pub fn breakpoints(&self) -> impl Iterator<Item = Breakpoint> + '_ {
        core::iter::once(self.origin).chain(self.changes.iter().copied())
    }

    /// The number of breakpoints, at least one.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len() + 1
    }

    /// Always `false`: a profile has at least the breakpoint at its start.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The level at tick `t`, as the exported timeline would report it.
    ///
    /// Costs `O(log n)` for `n` breakpoints.
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if `t` precedes the start.
    /// - [`Error::Overflow`] if the level does not fit a [`Level`].
    pub fn level_at(&self, t: Tick) -> Result<Level, Error> {
        if t < self.origin.at {
            return Err(Error::BeforeStart {
                at: t,
                start: self.origin.at,
            });
        }

        let index = self.changes.partition_point(|point| point.at <= t);
        let line = index
            .checked_sub(1)
            .map_or(self.origin, |last| self.changes[last]);
        line.level_at(t)
    }
}

impl<S: Storage> Timeline<S> {
    /// The ticks at which the level line changes, in time order.
    ///
    /// The first breakpoint is at the start tick. A tick where impacts net to zero is no
    /// breakpoint. Costs `O(n)` in total and never allocates. Targets are not part of the line.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::{ActivityId, Bounds, Breakpoint, Impact, Timeline};
    ///
    /// let mut battery = Timeline::depletable(0, 50, Bounds::new(0, 100)?, 8)?;
    /// battery.add(ActivityId(1), Impact::Rate { start: 10, end: 20, per_tick: -2 })?;
    /// battery.add(ActivityId(2), Impact::Step { at: 30, delta: 5 })?;
    ///
    /// let points: Vec<Breakpoint> = battery.breakpoints().collect::<Result<_, _>>()?;
    /// assert_eq!(points, [
    ///     Breakpoint { at: 0, level: 50, slope: 0 },
    ///     Breakpoint { at: 10, level: 50, slope: -2 },
    ///     Breakpoint { at: 20, level: 30, slope: 0 },
    ///     Breakpoint { at: 30, level: 35, slope: 0 },
    /// ]);
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn breakpoints(&self) -> Breakpoints<'_> {
        Breakpoints {
            segments: self.segments(),
            previous: None,
        }
    }

    /// The level at `count` ticks `from`, `from + step`, `from + 2 step`, and so on.
    ///
    /// Costs `O(n + count)` and never allocates. The samples stop early if a tick would
    /// overflow; an item is an error if its level does not fit a [`Level`].
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if `from` precedes the timeline start.
    /// - [`Error::InvalidStep`] if `step` is not positive.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::{ActivityId, Bounds, Impact, Timeline};
    ///
    /// let mut battery = Timeline::depletable(0, 50, Bounds::new(0, 100)?, 8)?;
    /// battery.add(ActivityId(1), Impact::Rate { start: 10, end: 20, per_tick: -2 })?;
    ///
    /// let telemetry: Vec<(i64, i64)> = battery.samples(0, 5, 6)?.collect::<Result<_, _>>()?;
    /// assert_eq!(telemetry, [(0, 50), (5, 50), (10, 50), (15, 40), (20, 30), (25, 30)]);
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn samples(&self, from: Tick, step: Tick, count: usize) -> Result<Samples<'_>, Error> {
        if from < self.start {
            return Err(Error::BeforeStart {
                at: from,
                start: self.start,
            });
        }

        if step <= 0 {
            return Err(Error::InvalidStep { step });
        }

        Ok(Samples {
            segments: self.segments(),
            current: None,
            next: from,
            step,
            remaining: count,
        })
    }

    /// A self-contained copy of the level line, with the timeline's storage.
    ///
    /// # Errors
    ///
    /// - [`Error::Overflow`] if a level or slope does not fit a [`Level`].
    /// - [`Error::OutOfMemory`] if the breakpoint list cannot be allocated (heap storage).
    ///   Fixed storage always has room.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::storage::Fixed;
    /// use resource_timeline::{ActivityId, Bounds, Impact, Timeline};
    ///
    /// let mut battery = Timeline::<Fixed<4>>::depletable_in(0, 50, Bounds::new(0, 100)?, 4)?;
    /// battery.add(ActivityId(1), Impact::Rate { start: 10, end: 20, per_tick: -2 })?;
    ///
    /// let profile = battery.export()?;
    /// assert_eq!(profile.len(), 3);
    /// assert_eq!(profile.level_at(15)?, battery.level_at(15)?);
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn export(&self) -> Result<LevelProfile<S>, Error> {
        let mut breakpoints = self.breakpoints();
        let origin = breakpoints.next().ok_or(Error::Overflow)??;
        let mut changes = <S::DoubleList<Breakpoint> as Seq<_>>::EMPTY;
        for breakpoint in breakpoints {
            changes.try_push(breakpoint?, Container::ChangePoints)?;
        }

        Ok(LevelProfile { origin, changes })
    }

    /// The pieces of the stored level line, without tentative changes.
    fn segments(&self) -> Segments<'_> {
        let frame = Frame {
            start: self.start,
            initial: self.initial,
            bounds: self.bounds,
            targets: &[],
        };
        Segments::new(&frame, self.profile.points(), &[])
    }
}
