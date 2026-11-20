//! Errors of timeline construction, impact insertion and checks.

use core::fmt;

use crate::{Level, Tick};

/// Why a timeline could not be built, an impact was rejected, or a check could not finish.
///
/// None of these is a resource violation. A violation is a successful check result, see
/// [`Violation`](crate::Violation).
///
/// The enum is `#[non_exhaustive]`: new failure modes (for example of future checks) are added
/// without a breaking release, so a `match` needs a wildcard arm. Callers that only need the
/// distinction "retry with more room" versus "malformed input" can use
/// [`is_capacity`](Self::is_capacity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
    /// A claim or rate interval `[start, end)` is empty or inverted.
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
    /// A step or rate impact was added to a claimable timeline, which accepts claims only.
    KindMismatch,
    /// An intermediate sum, or the level at the first violation, does not fit the number range.
    Overflow,
    /// Memory for the impact list or the check could not be allocated.
    OutOfMemory,
    /// A fixed-capacity container is full.
    ///
    /// Only fixed-capacity storage ([`Fixed`](crate::storage::Fixed),
    /// [`FixedPlan`](crate::storage::FixedPlan)) reports this; heap storage reports
    /// [`OutOfMemory`](Self::OutOfMemory) instead. The operation that failed left its target
    /// unchanged.
    CapacityExceeded {
        /// Which container ran out of room.
        container: Container,
        /// The fixed capacity of that container, in elements.
        capacity: usize,
    },
    /// A start window has `earliest > latest`.
    InvalidWindow {
        /// The requested earliest start.
        earliest: Tick,
        /// The requested latest start.
        latest: Tick,
    },
    /// An output buffer is too short for an encoding.
    BufferTooSmall {
        /// The number of bytes the encoding needs.
        needed: usize,
    },
    /// Encoded bytes have the wrong magic, length or content.
    Malformed,
    /// Encoded bytes do not match their checksum.
    ChecksumMismatch,
    /// Encoded bytes use a format version this build does not read.
    UnsupportedVersion {
        /// The version found.
        version: u8,
    },
    /// A sampling step is zero or negative.
    InvalidStep {
        /// The rejected step.
        step: Tick,
    },
    /// A demand names a timeline that is not part of the plan.
    UnknownResource,
    /// A precedence names an activity that is not part of the envelope.
    UnknownActivity {
        /// The unknown activity.
        activity: crate::ActivityId,
    },
    /// The start windows and precedences of an envelope admit no schedule at all.
    InconsistentPrecedences,
    /// A duration range of an activity starts below zero.
    NegativeDuration {
        /// The requested shortest duration.
        shortest: Tick,
    },
    /// A search ran out of its caller-chosen number of candidate checks before it found an
    /// answer.
    SearchExhausted {
        /// The number of candidate checks the search was allowed.
        checks: usize,
    },
}

/// The containers a timeline, plan or check keeps, named in [`Error::CapacityExceeded`].
///
/// With [`Fixed<N>`](crate::storage::Fixed) storage, the per-timeline containers hold `N`
/// elements, except [`ChangePoints`](Self::ChangePoints), which holds `2 N` (every impact
/// changes the level at most at two ticks). [`FixedPlan`](crate::storage::FixedPlan) adds its
/// own capacities for [`Timelines`](Self::Timelines) and [`States`](Self::States).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Container {
    /// The impacts of a numeric or state timeline.
    Impacts,
    /// The sorted change points of a numeric timeline.
    ChangePoints,
    /// The end-state targets of a numeric timeline.
    Targets,
    /// The sorted sets of a state timeline.
    Sets,
    /// The requirements of a state timeline.
    Requirements,
    /// The numeric timelines of a plan.
    Timelines,
    /// The state timelines of a plan.
    States,
    /// The contributor or setter list of a violation explanation.
    Contributors,
    /// Working memory of a check or search, sized like the impacts of one timeline.
    Scratch,
}

impl Error {
    /// Whether the error means "no room": [`OutOfMemory`](Self::OutOfMemory),
    /// [`CapacityExceeded`](Self::CapacityExceeded) or [`BudgetExhausted`](Self::BudgetExhausted).
    ///
    /// These depend on the chosen limits, not on the input being malformed, so a caller may
    /// retry with larger limits or shed load.
    #[must_use]
    pub const fn is_capacity(&self) -> bool {
        matches!(
            self,
            Self::OutOfMemory | Self::CapacityExceeded { .. } | Self::BudgetExhausted { .. }
        )
    }
}

impl fmt::Display for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Impacts => "impact list",
            Self::ChangePoints => "change-point list",
            Self::Targets => "target list",
            Self::Sets => "set list",
            Self::Requirements => "requirement list",
            Self::Timelines => "timeline list",
            Self::States => "state timeline list",
            Self::Contributors => "contributor list",
            Self::Scratch => "scratch buffer",
        })
    }
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
            Self::CapacityExceeded {
                container,
                capacity,
            } => write!(f, "{container} is full at {capacity} elements"),
            Self::InvalidWindow { earliest, latest } => {
                write!(f, "invalid start window [{earliest}, {latest}]")
            }
            Self::BufferTooSmall { needed } => write!(f, "buffer too small, {needed} bytes needed"),
            Self::Malformed => f.write_str("malformed encoding"),
            Self::ChecksumMismatch => f.write_str("checksum mismatch"),
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported encoding version {version}")
            }
            Self::InvalidStep { step } => write!(f, "sampling step {step} is not positive"),
            Self::UnknownResource => f.write_str("demand names an unknown timeline"),
            Self::UnknownActivity { activity } => {
                write!(f, "precedence names unknown activity {}", activity.0)
            }
            Self::InconsistentPrecedences => f.write_str("precedences admit no schedule"),
            Self::NegativeDuration { shortest } => {
                write!(f, "shortest duration {shortest} is negative")
            }
            Self::SearchExhausted { checks } => {
                write!(f, "search exhausted its budget of {checks} checks")
            }
        }
    }
}

impl core::error::Error for Error {}
