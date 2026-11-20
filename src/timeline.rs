//! Numeric resource timelines and their bound checks.

mod profile;
pub(crate) mod sweep;

use alloc::vec::Vec;

use crate::{ActivityId, Error, Impact, Level, Tick, Violation};
use profile::{Point, Profile};
use sweep::{Breach, Frame};

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
///
/// | Resource | Kind | Impacts |
/// | --- | --- | --- |
/// | Energy, data volume, propellant (consumable, cumulative) | [`Depletable`](Self::Depletable) | [`Impact::Step`] for lumps, [`Impact::Rate`] for charge, discharge or drift |
/// | Power bus share, comm channel, instrument (reusable) | [`Claimable`](Self::Claimable) | [`Impact::Claim`] |
/// | Mode, visibility window | not numeric | a state timeline |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Consumed and produced by permanent changes, for example energy or data volume.
    ///
    /// Accepts every [`Impact`]. Steps and rates persist after the activity; claims model a
    /// temporary hold such as a reserved margin.
    Depletable,
    /// Borrowed for an interval and returned, for example an instrument or a transmitter.
    ///
    /// Accepts [`Impact::Claim`] only.
    Claimable,
}

/// The level of one resource over time, built from activity impacts.
///
/// The level starts at an initial value at the start tick and changes only through impacts. It
/// is piecewise linear: steps and claims make it jump, rates give it a slope. Checks cover
/// every tick from the start onward; after the last impact ends the level is constant.
///
/// The timeline keeps its impacts and, alongside them, a sorted list of change points that is
/// updated on every [`add`](Self::add) and [`remove`](Self::remove). A check sweeps the change
/// points once without sorting or allocating.
///
/// # Examples
///
/// ```
/// use resource_timeline::{ActivityId, Bound, Bounds, Impact, Timeline};
///
/// // Battery at 50 units, reserve 10. A drive drains 3 units per tick from tick 0 to tick 20.
/// let mut battery = Timeline::depletable(0, 50, Bounds::new(10, 100)?, 8)?;
/// battery.add(ActivityId(1), Impact::Rate { start: 0, end: 20, per_tick: -3 })?;
///
/// let violation = battery.first_violation()?.expect("the reserve is broken");
/// assert_eq!(violation.at, 14); // 50 - 3 * 14 = 8 < 10
/// assert_eq!(violation.level, 8);
/// assert_eq!(violation.bound, Bound::BelowMin(10));
///
/// // Cancelling the drive restores the battery.
/// assert_eq!(battery.remove(ActivityId(1)), 1);
/// assert_eq!(battery.first_violation()?, None);
/// # Ok::<(), resource_timeline::Error>(())
/// ```
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
    /// The change points of all impacts, kept in sync with `impacts`.
    profile: Profile,
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
            profile: Profile { points: Vec::new() },
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
    /// - [`Error::KindMismatch`] if a step or rate is added to a claimable timeline.
    /// - [`Error::BeforeStart`] if the impact takes effect before the timeline start.
    /// - [`Error::EmptyInterval`] if a claim or rate has `end <= start`.
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
        self.profile.add(&impact)?;
        self.impacts.push((activity, impact));
        Ok(())
    }

    /// Removes every impact of `activity` and returns how many there were.
    ///
    /// Never allocates. Costs `O(n)` for the impact list plus `O(n)` per removed impact for
    /// the change points. Removing an activity without impacts is a no-op that returns `0`.
    pub fn remove(&mut self, activity: ActivityId) -> usize {
        let before = self.impacts.len();
        let profile = &mut self.profile;
        self.impacts.retain(|&(owner, impact)| {
            let keep = owner != activity;
            if !keep {
                profile.remove(&impact);
            }

            keep
        });

        before - self.impacts.len()
    }

    /// Checks that `impact` fits this timeline's kind and start, and is well formed.
    ///
    /// # Errors
    ///
    /// The validation errors listed on [`add`](Self::add).
    pub(crate) fn validate(&self, impact: Impact) -> Result<(), Error> {
        if self.kind == Kind::Claimable && !matches!(impact, Impact::Claim { .. }) {
            return Err(Error::KindMismatch);
        }

        match impact {
            Impact::Claim { start, end, amount } => {
                if end <= start {
                    return Err(Error::EmptyInterval { start, end });
                }

                if amount <= 0 {
                    return Err(Error::NonPositiveAmount { amount });
                }
            }
            Impact::Rate { start, end, .. } if end <= start => {
                return Err(Error::EmptyInterval { start, end });
            }
            Impact::Step { .. } | Impact::Rate { .. } => {}
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

    /// The level at tick `t`, with all impacts at `t` applied.
    ///
    /// Costs `O(n)` for `n` impacts.
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if `t` precedes the timeline start, where no level is defined.
    /// - [`Error::Overflow`] if the level does not fit a [`Level`].
    pub fn level_at(&self, t: Tick) -> Result<Level, Error> {
        if t < self.start {
            return Err(Error::BeforeStart {
                at: t,
                start: self.start,
            });
        }

        let level = self
            .impacts
            .iter()
            .try_fold(i128::from(self.initial), |level, (_, impact)| {
                level.checked_add(impact.contribution_at(t)?)
            })
            .ok_or(Error::Overflow)?;
        Level::try_from(level).map_err(|_| Error::Overflow)
    }

    /// Whether any impact on this timeline is a [`Impact::Rate`] with a non-zero rate.
    #[must_use]
    pub fn has_rates(&self) -> bool {
        self.profile.has_slope()
    }

    /// Finds the earliest tick at which the level is outside the bounds.
    ///
    /// All impacts at a tick are netted before the level is compared, so a claim ending at `t`
    /// and another starting at `t` never overlap. Along a rate the exact crossing tick is
    /// found by integer division, not by stepping tick by tick. Runs in `O(n)` time over the
    /// change points; only the explanation of a violation allocates `O(n)` memory. The result
    /// does not depend on insertion order.
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
        match self.breach_with(&[])? {
            Some(breach) => sweep::explain(&self.impacts, breach).map(Some),
            None => Ok(None),
        }
    }

    /// The first breach of the bounds if the change points `extra` were added.
    ///
    /// `extra` must be sorted by tick and must not reach before the start. Never allocates.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a level along a rate segment exceeds `i128`.
    pub(crate) fn breach_with(&self, extra: &[Point]) -> Result<Option<Breach>, Error> {
        let frame = Frame {
            start: self.start,
            initial: self.initial,
            bounds: self.bounds,
        };
        sweep::first_breach(frame, self.profile.points(), extra)
    }
}
