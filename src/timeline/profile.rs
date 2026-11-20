//! The sorted change-point representation of a timeline's level.
//!
//! The level of a timeline is piecewise linear. It is fully described by the ticks at which the
//! level jumps or the slope changes. [`Profile`] keeps one [`Point`] per such tick, sorted and
//! unique, and updates it incrementally when impacts are added or removed.
//!
//! # Overflow
//!
//! Every field of a point is a sum of at most two widened `i64` values per stored impact.
//! An impact list lives in memory and takes more than 16 bytes per entry, so it holds fewer than
//! `2^60` entries, and every sum stays below `2^124` in magnitude. Point arithmetic therefore
//! cannot overflow `i128`, and the profile uses plain arithmetic.

use alloc::vec::Vec;

use crate::{Error, Impact, Tick};

/// The net change of level and slope at one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Point {
    /// The tick of the change.
    pub(crate) at: Tick,
    /// The jump of the level at `at`, from steps and claim starts or ends.
    pub(crate) step: i128,
    /// The change of the slope (level change per tick) from `at` onward.
    pub(crate) slope: i128,
}

impl Point {
    /// Whether the point changes nothing and can be dropped.
    const fn is_empty(&self) -> bool {
        self.step == 0 && self.slope == 0
    }
}

/// Sorted, unique change points of a timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Profile {
    /// The change points, sorted by tick, at most one per tick, none of them empty.
    pub(super) points: Vec<Point>,
}

impl Profile {
    /// The change points, sorted by tick.
    pub(crate) fn points(&self) -> &[Point] {
        &self.points
    }

    /// Adds the changes of `impact`.
    ///
    /// Costs `O(log n)` for the search plus `O(n)` for shifting when a new tick is inserted.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if room for new points cannot be reserved. The profile is then
    /// unchanged.
    pub(crate) fn add(&mut self, impact: &Impact) -> Result<(), Error> {
        self.points.try_reserve(2).map_err(|_| Error::OutOfMemory)?;
        for (at, step, slope) in impact.changes().into_iter().flatten() {
            self.apply(Point { at, step, slope });
        }

        Ok(())
    }

    /// Removes the changes of an `impact` that was added before.
    ///
    /// Never allocates. Points whose net change becomes zero are dropped.
    pub(crate) fn remove(&mut self, impact: &Impact) {
        for (at, step, slope) in impact.changes().into_iter().flatten() {
            self.apply(Point {
                at,
                step: -step,
                slope: -slope,
            });
        }
    }

    /// Adds `change` to the point at its tick, inserting or dropping the point as needed.
    ///
    /// Inserting needs spare capacity to avoid allocation; [`add`](Self::add) reserves it.
    fn apply(&mut self, change: Point) {
        match self
            .points
            .binary_search_by_key(&change.at, |point| point.at)
        {
            Ok(index) => {
                let point = &mut self.points[index];
                point.step += change.step;
                point.slope += change.slope;
                if point.is_empty() {
                    self.points.remove(index);
                }
            }
            Err(index) if !change.is_empty() => self.points.insert(index, change),
            Err(_) => {}
        }
    }

    /// Whether any part of the level changes linearly.
    pub(crate) fn has_slope(&self) -> bool {
        self.points.iter().any(|point| point.slope != 0)
    }
}
