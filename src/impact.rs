//! Effects of an activity on a single resource timeline.

use crate::{Level, Tick};

/// One effect of an activity on the level of a resource.
///
/// The level at tick `t` is the timeline's initial level plus the sum of all steps with
/// `at <= t`, minus the amounts of all claims with `start <= t < end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Impact {
    /// A permanent change of the level, as for a depletable resource.
    ///
    /// Consumption (energy drawn, storage filled) is a negative `delta`, production (charging,
    /// downlinked data freed) a positive one.
    Step {
        /// The tick at which the change takes effect. Not before the timeline start.
        at: Tick,
        /// The change of the level from `at` onward. May be zero or negative.
        delta: Level,
    },
    /// A temporary hold on a resource during the half-open interval `[start, end)`.
    ///
    /// The level is the amount *available*, so a claim lowers it by `amount` while it holds and
    /// returns it at `end`. A claim ending at `t` and another starting at `t` do not overlap.
    Claim {
        /// The first tick of the hold. Not before the timeline start.
        start: Tick,
        /// The first tick after the hold. Strictly greater than `start`.
        end: Tick,
        /// The number of units held. Strictly positive.
        amount: Level,
    },
}

impl Impact {
    /// The earliest tick at which this impact changes the level.
    #[must_use]
    pub const fn first_tick(&self) -> Tick {
        match *self {
            Self::Step { at, .. } => at,
            Self::Claim { start, .. } => start,
        }
    }

    /// The contribution of this impact to the level at tick `t`, widened to avoid overflow.
    #[must_use]
    pub(crate) fn contribution_at(&self, t: Tick) -> i128 {
        match *self {
            Self::Step { at, delta } if at <= t => i128::from(delta),
            Self::Claim { start, end, amount } if start <= t && t < end => -i128::from(amount),
            Self::Step { .. } | Self::Claim { .. } => 0,
        }
    }
}
