//! Piecewise-constant resource timelines and their bound checks.

mod sweep;

use alloc::vec::Vec;

use crate::{ActivityId, Error, Impact, Level, Tick, Violation};

/// The inclusive range `[min, max]` a resource level must stay in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bounds {
    /// The lowest allowed level, for example an energy reserve. Never greater than `max`.
    min: Level,
    /// The highest allowed level, for example a battery or storage capacity.
    max: Level,
}

impl Bounds {
    /// Creates the range `[min, max]`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBounds`] if `min > max`.
    pub const fn new(min: Level, max: Level) -> Result<Self, Error> {
        if min > max {
            return Err(Error::InvalidBounds { min, max });
        }

        Ok(Self { min, max })
    }

    /// The lowest allowed level.
    #[must_use]
    pub const fn min(&self) -> Level {
        self.min
    }

    /// The highest allowed level.
    #[must_use]
    pub const fn max(&self) -> Level {
        self.max
    }
}

/// How impacts change a timeline's level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Consumed and produced by permanent steps, for example energy or data volume.
    ///
    /// Accepts [`Impact::Step`] and [`Impact::Claim`].
    Depletable,
    /// Borrowed for an interval and returned, for example an instrument or a transmitter.
    ///
    /// Accepts [`Impact::Claim`] only.
    Claimable,
}

/// The level of one resource over time, built from activity impacts.
///
/// The level starts at an initial value at the start tick and changes only at impact ticks, so
/// it is piecewise constant. Checks cover every tick from the start onward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    /// Which impacts the timeline accepts.
    kind: Kind,
    /// The first tick of the timeline. No impact may take effect earlier.
    start: Tick,
    /// The level at the start tick, before impacts at the start tick apply.
    initial: Level,
    /// The range the level must stay in.
    bounds: Bounds,
    /// The maximum number of impacts the timeline accepts.
    budget: usize,
    /// The impacts in insertion order. Never longer than `budget`.
    impacts: Vec<(ActivityId, Impact)>,
}

impl Timeline {
    /// Creates a timeline for a depletable resource such as battery energy or storage.
    ///
    /// `initial` is the level at `start`. `budget` caps the number of impacts.
    ///
    /// # Errors
    ///
    /// Never fails today; returns `Result` so later validation does not break callers.
    pub const fn depletable(
        start: Tick,
        initial: Level,
        bounds: Bounds,
        budget: usize,
    ) -> Result<Self, Error> {
        Ok(Self::new(Kind::Depletable, start, initial, bounds, budget))
    }

    /// Creates a timeline for a claimable resource with `capacity` units, all available at
    /// `start`.
    ///
    /// The bounds are `[0, capacity]`, so overlapping claims may never hold more than
    /// `capacity` units. `budget` caps the number of impacts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBounds`] if `capacity` is negative.
    pub const fn claimable(start: Tick, capacity: Level, budget: usize) -> Result<Self, Error> {
        match Bounds::new(0, capacity) {
            Ok(bounds) => Ok(Self::new(Kind::Claimable, start, capacity, bounds, budget)),
            Err(error) => Err(error),
        }
    }

    /// Creates an empty timeline from its validated parts.
    const fn new(kind: Kind, start: Tick, initial: Level, bounds: Bounds, budget: usize) -> Self {
        Self {
            kind,
            start,
            initial,
            bounds,
            budget,
            impacts: Vec::new(),
        }
    }

    /// Which impacts the timeline accepts.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// The first tick of the timeline.
    #[must_use]
    pub const fn start(&self) -> Tick {
        self.start
    }

    /// The level at the start tick, before impacts at the start tick apply.
    #[must_use]
    pub const fn initial(&self) -> Level {
        self.initial
    }

    /// The range the level must stay in.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// The impacts added so far, in insertion order.
    #[must_use]
    pub fn impacts(&self) -> &[(ActivityId, Impact)] {
        &self.impacts
    }

    /// Adds an impact of `activity` to the timeline.
    ///
    /// The impact is validated but not checked against the bounds; call
    /// [`first_violation`](Self::first_violation) for that.
    ///
    /// # Errors
    ///
    /// - [`Error::KindMismatch`] if a step is added to a claimable timeline.
    /// - [`Error::BeforeStart`] if the impact takes effect before the timeline start.
    /// - [`Error::EmptyInterval`] if a claim has `end <= start`.
    /// - [`Error::NonPositiveAmount`] if a claim amount is zero or negative.
    /// - [`Error::BudgetExhausted`] if the timeline already holds `budget` impacts.
    /// - [`Error::OutOfMemory`] if the impact list cannot grow.
    ///
    /// The timeline is unchanged on error.
    pub fn add(&mut self, activity: ActivityId, impact: Impact) -> Result<(), Error> {
        self.validate(impact)?;
        if self.impacts.len() >= self.budget {
            return Err(Error::BudgetExhausted { limit: self.budget });
        }

        self.impacts
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.impacts.push((activity, impact));
        Ok(())
    }

    /// Checks that `impact` fits this timeline's kind and start, and is well formed.
    fn validate(&self, impact: Impact) -> Result<(), Error> {
        if self.kind == Kind::Claimable && matches!(impact, Impact::Step { .. }) {
            return Err(Error::KindMismatch);
        }

        if let Impact::Claim { start, end, amount } = impact {
            if end <= start {
                return Err(Error::EmptyInterval { start, end });
            }

            if amount <= 0 {
                return Err(Error::NonPositiveAmount { amount });
            }
        }

        let at = impact.first_tick();
        if at < self.start {
            return Err(Error::BeforeStart {
                at,
                start: self.start,
            });
        }

        Ok(())
    }

    /// Finds the earliest tick at which the level is outside the bounds.
    ///
    /// All impacts at a tick are netted before the level is compared, so a claim ending at `t`
    /// and another starting at `t` never overlap. Runs in `O(n log n)` time and `O(n)` extra
    /// memory for `n` impacts. The result does not depend on insertion order.
    ///
    /// Returns `Ok(None)` if the level stays within the bounds forever.
    ///
    /// # Errors
    ///
    /// - [`Error::Overflow`] if a running sum exceeds `i128`, or if the level at the first
    ///   violation does not fit a [`Level`]. A violation exists in the second case, but it
    ///   cannot be reported exactly.
    /// - [`Error::OutOfMemory`] if the working memory cannot be allocated.
    pub fn first_violation(&self) -> Result<Option<Violation>, Error> {
        sweep::first_violation(self)
    }
}
