//! Explanations of resource bound violations.

use alloc::vec::Vec;

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Violation {
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
    pub contributors: Vec<ActivityId>,
}
