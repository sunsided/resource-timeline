//! Errors of timeline construction, impact insertion and checks.

use core::fmt;

use crate::{Level, Tick};

/// Why a timeline could not be built, an impact was rejected, or a check could not finish.
///
/// None of these is a resource violation. A violation is a successful check result, see
/// [`Violation`](crate::Violation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The lower bound is greater than the upper bound, or a claimable capacity is negative.
    InvalidBounds {
        /// The requested lower bound.
        min: Level,
        /// The requested upper bound.
        max: Level,
    },
    /// The timeline already holds as many impacts as its budget allows.
    BudgetExhausted {
        /// The impact budget the timeline was created with.
        limit: usize,
    },
    /// An impact starts before the timeline start.
    BeforeStart {
        /// The first tick of the rejected impact.
        at: Tick,
        /// The start tick of the timeline.
        start: Tick,
    },
    /// A claim interval `[start, end)` is empty or inverted.
    EmptyInterval {
        /// The requested first tick.
        start: Tick,
        /// The requested end tick.
        end: Tick,
    },
    /// A claim amount is zero or negative.
    NonPositiveAmount {
        /// The rejected amount.
        amount: Level,
    },
    /// A step impact was added to a claimable timeline, which accepts claims only.
    KindMismatch,
    /// An intermediate sum, or the level at the first violation, does not fit the number range.
    Overflow,
    /// Memory for the impact list or the check could not be allocated.
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidBounds { min, max } => write!(f, "invalid bounds [{min}, {max}]"),
            Self::BudgetExhausted { limit } => write!(f, "impact budget of {limit} exhausted"),
            Self::BeforeStart { at, start } => {
                write!(f, "impact at tick {at} precedes timeline start {start}")
            }
            Self::EmptyInterval { start, end } => write!(f, "empty interval [{start}, {end})"),
            Self::NonPositiveAmount { amount } => {
                write!(f, "claim amount {amount} is not positive")
            }
            Self::KindMismatch => f.write_str("claimable timelines accept claims only"),
            Self::Overflow => f.write_str("arithmetic overflow"),
            Self::OutOfMemory => f.write_str("allocation failed"),
        }
    }
}

impl core::error::Error for Error {}
