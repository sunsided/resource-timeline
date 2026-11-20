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
    /// Net negative for [`Bound::BelowMin`], net positive for [`Bound::AboveMax`]. Effects are
    /// netted per activity first. Sorted ascending, without duplicates. Empty when the initial
    /// level alone breaks the bound.
    pub contributors: S::List<ActivityId>,
}
