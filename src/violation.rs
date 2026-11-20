//! Explanations of resource bound violations.

#[cfg(feature = "alloc")]
use crate::storage::Heap;
use crate::storage::Storage;
use crate::{ActivityId, Level, Tick};

/// The bound a level broke, with the bound's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bound {
    /// The level fell below the lower bound (reserve, or no units left to claim).
    BelowMin(Level),
    /// The level rose above the upper bound (capacity).
    AboveMax(Level),
    /// The level at a [`Target`](crate::Target) tick is below the target's minimum, for
    /// example the battery reserve required at the end of a sol.
    BelowTarget(Level),
}

impl Bound {
    /// Whether the level is too low ([`BelowMin`](Self::BelowMin),
    /// [`BelowTarget`](Self::BelowTarget)) rather than too high.
    #[must_use]
    pub const fn is_lower(&self) -> bool {
        matches!(self, Self::BelowMin(_) | Self::BelowTarget(_))
    }

    /// The value of the broken bound.
    #[must_use]
    pub const fn value(&self) -> Level {
        match *self {
            Self::BelowMin(value) | Self::AboveMax(value) | Self::BelowTarget(value) => value,
        }
    }

    /// Whether a net effect of `effect` pushes the level towards this bound.
    pub(crate) const fn is_pushed_by(&self, effect: i128) -> bool {
        if self.is_lower() {
            effect < 0
        } else {
            effect > 0
        }
    }
}

/// The first tick at which a timeline's level is out of bounds, and why.
///
/// The contributor list uses the storage `S` of the timeline that reported it, so explaining a
/// violation of a [`Fixed`](crate::storage::Fixed) timeline does not allocate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Violation<
    #[cfg(feature = "alloc")] S: Storage = Heap,
    #[cfg(not(feature = "alloc"))] S: Storage,
> {
    /// The earliest tick at which the level is out of bounds.
    ///
    /// All impacts at this tick are already applied.
    pub at: Tick,
    /// The level at [`at`](Self::at).
    pub level: Level,
    /// The bound that is broken.
    pub bound: Bound,
    /// The activities whose net effect in force at [`at`](Self::at) pushes the level towards
    /// the broken bound.
    ///
    /// Net negative for [`Bound::BelowMin`] and [`Bound::BelowTarget`], net positive for
    /// [`Bound::AboveMax`]. Effects are
    /// netted per activity first. Sorted ascending, without duplicates. Empty when the initial
    /// level alone breaks the bound.
    pub contributors: S::List<ActivityId>,
}
