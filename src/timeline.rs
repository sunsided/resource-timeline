//! Numeric resource timelines and their bound checks.

pub(crate) mod extract;
pub(crate) mod profile;
pub(crate) mod sweep;

#[cfg(feature = "alloc")]
use crate::storage::Heap;
use crate::storage::{Seq, Slot, Storage};
use crate::{ActivityId, Container, Error, Impact, Level, Target, Tick, Violation};
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
/// | Mode, visibility window | not numeric | see [`StateTimeline`](crate::StateTimeline) |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
/// The storage `S` is [`storage::Heap`](crate::storage::Heap) by default; with
/// [`Fixed<N>`](crate::storage::Fixed) the timeline holds at most `N` impacts and never
/// allocates (see [`storage`](crate::storage)). Heap timelines are created with
/// [`depletable`] and [`claimable`], timelines with any
/// storage with [`depletable_in`](Self::depletable_in) and [`claimable_in`](Self::claimable_in).
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
///
#[cfg_attr(feature = "alloc", doc = "[`depletable`]: Self::depletable")]
#[cfg_attr(not(feature = "alloc"), doc = "[`depletable`]: crate#features")]
#[cfg_attr(feature = "alloc", doc = "[`claimable`]: Self::claimable")]
#[cfg_attr(not(feature = "alloc"), doc = "[`claimable`]: crate#features")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Timeline<
    #[cfg(feature = "alloc")] S: Storage = Heap,
    #[cfg(not(feature = "alloc"))] S: Storage,
> {
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
    impacts: S::List<(ActivityId, Impact)>,
    /// The change points of all impacts, kept in sync with `impacts`.
    profile: Profile<S>,
    /// The end-state targets, sorted by [`Target::key`]. None before `start`.
    targets: S::List<Target>,
}

#[cfg(feature = "alloc")]
impl Timeline {
    /// Creates a heap timeline for a depletable resource such as battery energy or storage.
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
        Self::depletable_in(start, initial, bounds, budget)
    }

    /// Creates a heap timeline for a claimable resource with `capacity` units, all available
    /// at `start`.
    ///
    /// The bounds are `[0, capacity]`, so overlapping claims may never hold more than
    /// `capacity` units. `budget` caps the number of impacts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBounds`] if `capacity` is negative.
    pub const fn claimable(start: Tick, capacity: Level, budget: usize) -> Result<Self, Error> {
        Self::claimable_in(start, capacity, budget)
    }
}

impl<S: Storage> Timeline<S> {
    /// Creates a timeline with storage `S` for a depletable resource such as battery energy.
    ///
    /// As [`Timeline::depletable`], for any [`Storage`]. With [`Fixed<N>`](crate::storage::Fixed)
    /// a `budget` above `N` is allowed, but adds beyond `N` fail with
    /// [`Error::CapacityExceeded`].
    ///
    #[cfg_attr(
        feature = "alloc",
        doc = "[`Timeline::depletable`]: Timeline::depletable"
    )]
    #[cfg_attr(
        not(feature = "alloc"),
        doc = "[`Timeline::depletable`]: crate#features"
    )]
    ///
    /// # Errors
    ///
    /// Never fails today; returns `Result` so later validation does not break callers.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::storage::Fixed;
    /// use resource_timeline::{Bounds, Timeline};
    ///
    /// let battery = Timeline::<Fixed<32>>::depletable_in(0, 100, Bounds::new(20, 120)?, 32)?;
    /// assert_eq!(battery.level_at(1_000)?, 100);
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub const fn depletable_in(
        start: Tick,
        initial: Level,
        bounds: Bounds,
        budget: usize,
    ) -> Result<Self, Error> {
        Ok(Self::new(Kind::Depletable, start, initial, bounds, budget))
    }

    /// Creates a timeline with storage `S` for a claimable resource with `capacity` units.
    ///
    /// As [`Timeline::claimable`], for any [`Storage`].
    ///
    #[cfg_attr(
        feature = "alloc",
        doc = "[`Timeline::claimable`]: Timeline::claimable"
    )]
    #[cfg_attr(
        not(feature = "alloc"),
        doc = "[`Timeline::claimable`]: crate#features"
    )]
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBounds`] if `capacity` is negative.
    pub const fn claimable_in(start: Tick, capacity: Level, budget: usize) -> Result<Self, Error> {
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
            impacts: <S::List<(ActivityId, Impact)> as Seq<_>>::EMPTY,
            profile: Profile::EMPTY,
            targets: <S::List<Target> as Seq<_>>::EMPTY,
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

    /// The maximum number of impacts the timeline accepts.
    #[must_use]
    pub const fn budget(&self) -> usize {
        self.budget
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
    /// - [`Error::OutOfMemory`] if the impact list cannot grow (heap storage).
    /// - [`Error::CapacityExceeded`] if the impact list or the change points are full
    ///   (fixed storage).
    ///
    /// The timeline is unchanged on error.
    pub fn add(&mut self, activity: ActivityId, impact: Impact) -> Result<(), Error> {
        self.validate(impact)?;
        self.ensure_room(1)?;
        self.impacts.try_reserve(1, Container::Impacts)?;
        self.profile.add(&impact)?;
        self.impacts
            .try_push((activity, impact), Container::Impacts)
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

    /// Requires the level at `target.at` to be at least `target.min`.
    ///
    /// Targets do not count against the impact budget. Several targets may share a tick; the
    /// strictest one is reported first. See [`Target`] for an example.
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if the target precedes the timeline start.
    /// - [`Error::OutOfMemory`] if the target list cannot grow (heap storage).
    /// - [`Error::CapacityExceeded`] naming [`Container::Targets`] if it is full (fixed
    ///   storage).
    pub fn add_target(&mut self, target: Target) -> Result<(), Error> {
        if target.at < self.start {
            return Err(Error::BeforeStart {
                at: target.at,
                start: self.start,
            });
        }

        let index = self
            .targets
            .partition_point(|other| other.key() <= target.key());
        self.targets.try_insert(index, target, Container::Targets)
    }

    /// The end-state targets, sorted by tick and, within a tick, strictest first.
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// Removes every target at tick `at` and returns how many there were. Never allocates.
    pub fn remove_targets_at(&mut self, at: Tick) -> usize {
        let before = self.targets.len();
        self.targets.retain(|target| target.at != at);
        before - self.targets.len()
    }

    /// Removes the most recently added impact, if any. Never allocates.
    pub(crate) fn undo_last(&mut self) {
        if let Some((_, impact)) = self.impacts.pop() {
            self.profile.remove(&impact);
        }
    }

    /// The ticks at which a check outcome can change: every change point and every target.
    /// Not sorted across the two kinds, possibly with duplicates.
    #[cfg(feature = "alloc")]
    pub(crate) fn change_ticks(&self) -> impl Iterator<Item = Tick> + '_ {
        let points = self.profile.points().iter().map(|point| point.at);
        points.chain(self.targets.iter().map(|target| target.at))
    }

    /// The first tick `>= x` among the start, the change points and the targets, if any.
    ///
    /// Costs `O(log n)`.
    pub(crate) fn first_change_from(&self, x: i128) -> Option<Tick> {
        let points = self.profile.points();
        let point = points.get(points.partition_point(|point| i128::from(point.at) < x));
        let target = self.targets.get(
            self.targets
                .partition_point(|target| i128::from(target.at) < x),
        );
        let start = (i128::from(self.start) >= x).then_some(self.start);
        [
            start,
            point.map(|point| point.at),
            target.map(|target| target.at),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// `Ok` while the timeline has room for `extra` more impacts, by budget and by capacity.
    ///
    /// A fixed change-point list has room for two points per impact, so the impact capacity
    /// covers it.
    ///
    /// # Errors
    ///
    /// - [`Error::BudgetExhausted`] if `extra` more impacts would exceed the budget.
    /// - [`Error::CapacityExceeded`] if they would exceed a fixed impact capacity.
    pub(crate) fn ensure_room(&self, extra: usize) -> Result<(), Error> {
        let needed = self.impacts.len().saturating_add(extra);
        if needed > self.budget {
            return Err(Error::BudgetExhausted { limit: self.budget });
        }

        match self.impacts.capacity() {
            Some(capacity) if needed > capacity => Err(Error::CapacityExceeded {
                container: Container::Impacts,
                capacity,
            }),
            _ => Ok(()),
        }
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

    /// Finds the earliest tick at which the level is outside the bounds or misses a
    /// [`Target`].
    ///
    /// At the same tick, a broken bound is reported before a missed target.
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
    /// - [`Error::OutOfMemory`] if the working memory cannot be allocated (heap storage).
    ///   Fixed storage has room for its explanation by construction.
    pub fn first_violation(&self) -> Result<Option<Violation<S>>, Error> {
        match self.breach_with(&[])? {
            Some(breach) => sweep::explain::<S>(&self.impacts, breach).map(Some),
            None => Ok(None),
        }
    }

    /// The activities whose net effect in force at `t` pushes towards `bound`, sorted.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] or [`Error::OutOfMemory`] as for
    /// [`first_violation`](Self::first_violation).
    #[cfg(feature = "alloc")]
    pub(crate) fn contributors_at(
        &self,
        t: Tick,
        bound: crate::Bound,
    ) -> Result<S::List<ActivityId>, Error> {
        sweep::contributors::<S>(&self.impacts, t, bound)
    }

    /// Narrows `offsets` to the start offsets `k` at which a tentative activity keeps this
    /// timeline valid, given its change points `first` at offset 0 and `second` at offset 1.
    ///
    /// Both point lists must produce the same segment structure, so that every evaluated level
    /// is affine in `k` (see the search module). Never allocates.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a level exceeds `i128`.
    pub(crate) fn narrow(
        &self,
        first: &[Point],
        second: &[Point],
        offsets: &mut crate::plan::Offsets,
    ) -> Result<(), Error> {
        let frame = self.frame();
        sweep::narrow(&frame, self.profile.points(), [first, second], offsets)
    }

    /// The fixed parameters of a sweep over this timeline.
    fn frame(&self) -> Frame<'_> {
        Frame {
            start: self.start,
            initial: self.initial,
            bounds: self.bounds,
            targets: &self.targets,
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
        sweep::first_breach(&self.frame(), self.profile.points(), extra)
    }
}

impl<S: Storage> crate::storage::Sealed for Timeline<S> {}

impl<S: Storage> Slot for Timeline<S> {
    const EMPTY: Self = Self::new(Kind::Depletable, 0, 0, Bounds { min: 0, max: 0 }, 0);
}
