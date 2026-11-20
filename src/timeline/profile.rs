//! The sorted change-point representation of a timeline's level.
//!
//! The level of a timeline is piecewise linear. It is fully described by the ticks at which the
//! level jumps or the slope changes. [`Profile`] keeps one [`Point`] per such tick, sorted and
//! unique, and updates it incrementally when impacts are added or removed.
//!
//! A point stays in the list while at least one stored impact changes something at its tick,
//! even if the changes net to zero. Removing an impact therefore never inserts a point, so
//! [`Profile::remove`] cannot run out of room, and the list never holds more points than twice
//! the number of impacts.
//!
//! # Overflow
//!
//! Every field of a point is a sum of at most two widened `i64` values per stored impact.
//! An impact list lives in memory and takes more than 16 bytes per entry, so it holds fewer than
//! `2^60` entries, and every sum stays below `2^124` in magnitude. Point arithmetic therefore
//! cannot overflow `i128`, and the profile uses plain arithmetic.

use crate::storage::{Seq, Slot, Storage};
use crate::{Container, Error, Impact, Tick};

/// The net change of level and slope at one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Point {
    /// The tick of the change.
    pub(crate) at: Tick,
    /// The jump of the level at `at`, from steps and claim starts or ends.
    pub(crate) step: i128,
    /// The change of the slope (level change per tick) from `at` onward.
    pub(crate) slope: i128,
    /// The number of stored impact changes at `at`. The point is dropped when it reaches zero.
    ///
    /// Unused (zero) for points that are not part of a stored profile.
    pub(crate) refs: usize,
}

impl Point {
    /// A point with the given changes that no stored impact references.
    pub(crate) const fn loose(at: Tick, step: i128, slope: i128) -> Self {
        Self {
            at,
            step,
            slope,
            refs: 0,
        }
    }

    /// Whether the point changes neither the level nor the slope.
    pub(crate) const fn is_empty(&self) -> bool {
        self.step == 0 && self.slope == 0
    }
}

impl crate::storage::Sealed for Point {}

impl Slot for Point {
    const EMPTY: Self = Self::loose(0, 0, 0);
}

/// Sorted, unique change points of a timeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Profile<S: Storage> {
    /// The change points, sorted by tick, at most one per tick, each referenced by at least one
    /// stored impact. Never more than twice the number of stored impacts.
    pub(super) points: S::DoubleList<Point>,
}

impl<S: Storage> Profile<S> {
    /// A profile without change points.
    pub(crate) const EMPTY: Self = Self {
        points: <S::DoubleList<Point> as Seq<Point>>::EMPTY,
    };

    /// The change points, sorted by tick. Points may net to zero.
    pub(crate) fn points(&self) -> &[Point] {
        &self.points
    }

    /// Adds the changes of `impact`.
    ///
    /// Costs `O(log n)` for the search plus `O(n)` for shifting when a new tick is inserted.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] or [`Error::CapacityExceeded`] if room for new points cannot be
    /// reserved. The profile is then unchanged.
    pub(crate) fn add(&mut self, impact: &Impact) -> Result<(), Error> {
        self.points.try_reserve(2, Container::ChangePoints)?;
        for (at, step, slope) in impact.changes().into_iter().flatten() {
            match self.points.binary_search_by_key(&at, |point| point.at) {
                Ok(index) => {
                    let point = &mut self.points[index];
                    point.step += step;
                    point.slope += slope;
                    point.refs += 1;
                }
                Err(index) => {
                    let point = Point {
                        refs: 1,
                        ..Point::loose(at, step, slope)
                    };
                    self.points
                        .try_insert(index, point, Container::ChangePoints)?;
                }
            }
        }

        Ok(())
    }

    /// Removes the changes of an `impact` that was added before.
    ///
    /// Never allocates and never inserts. A point is dropped when no stored impact references
    /// it any more.
    pub(crate) fn remove(&mut self, impact: &Impact) {
        for (at, step, slope) in impact.changes().into_iter().flatten() {
            let Ok(index) = self.points.binary_search_by_key(&at, |point| point.at) else {
                debug_assert!(false, "removed impact was never added");
                continue;
            };

            let point = &mut self.points[index];
            point.step -= step;
            point.slope -= slope;
            point.refs -= 1;
            if point.refs == 0 {
                debug_assert!(
                    point.is_empty(),
                    "unreferenced point still changes the level"
                );
                self.points.remove(index);
            }
        }
    }

    /// Whether any part of the level changes linearly.
    pub(crate) fn has_slope(&self) -> bool {
        self.points.iter().any(|point| point.slope != 0)
    }
}
