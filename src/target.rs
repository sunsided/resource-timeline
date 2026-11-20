//! End-state targets: a minimum level required at one tick.

use crate::storage::{Sealed, Slot};
use crate::{Level, Tick};

/// A minimum level the timeline must have at one tick, for example the battery reserve
/// required at the end of a sol so the rover survives the night.
///
/// Unlike the [`Bounds`](crate::Bounds), which hold at every tick, a target holds at `at`
/// only: activities after `at` may draw the level lower again. The level at `at` includes
/// every impact at `at`. Targets take part in every check, so
/// [`Timeline::first_violation`](crate::Timeline::first_violation), [`Plan::fits`](crate::Plan::fits)
/// and [`Plan::earliest_start`](crate::Plan::earliest_start) reject schedules that miss one; a
/// miss is reported as [`Bound::BelowTarget`](crate::Bound::BelowTarget).
///
/// # Examples
///
/// ```
/// use resource_timeline::{ActivityId, Bound, Bounds, Impact, Target, Timeline};
///
/// // A sol lasts 100 ticks; 40 units must be left at its end.
/// let mut battery = Timeline::depletable(0, 100, Bounds::new(10, 100)?, 8)?;
/// battery.add_target(Target { at: 100, min: 40 })?;
/// battery.add(ActivityId(1), Impact::Rate { start: 20, end: 90, per_tick: -1 })?;
///
/// let violation = battery.first_violation()?.expect("the end-of-sol reserve is missed");
/// assert_eq!((violation.at, violation.level), (100, 30));
/// assert_eq!(violation.bound, Bound::BelowTarget(40));
/// # Ok::<(), resource_timeline::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Target {
    /// The tick at which the level is checked. Not before the timeline start.
    pub at: Tick,
    /// The lowest allowed level at `at`.
    pub min: Level,
}

impl Target {
    /// The sort key of stored targets: by tick, then strictest (highest minimum) first, so that
    /// the first missed target at a tick is the strictest one.
    pub(crate) const fn key(&self) -> (Tick, core::cmp::Reverse<Level>) {
        (self.at, core::cmp::Reverse(self.min))
    }
}

impl Sealed for Target {}

impl Slot for Target {
    const EMPTY: Self = Self { at: 0, min: 0 };
}
