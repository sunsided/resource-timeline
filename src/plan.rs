//! Plans: several resource timelines checked together, with placement and search.
//!
//! An activity usually touches several resources: a downlink draws power, claims the
//! transmitter, frees mass memory and needs Earth in view. A [`Plan`] owns one timeline per
//! resource and treats an activity as a list of [`Demand`]s whose ticks are relative to the
//! activity start. It places activities atomically, removes them again, reports the earliest
//! violation across all timelines and finds the earliest start at which an activity fits.
//!
//! ```mermaid
//! graph LR
//!     demands["Demands, relative to the start"] --> search["earliest_start: candidate starts"]
//!     search --> fits["fits: tentative check per touched timeline"]
//!     fits --> place["place: add to every timeline, atomically"]
//!     place --> check["first_violation across timelines"]
//! ```

mod search;

pub(crate) use search::Offsets;

#[cfg(feature = "alloc")]
use crate::storage::Heap;
use crate::storage::{PlanStorage, Seq, Storage};
use crate::{
    ActivityId, Container, Error, Impact, StateImpact, StateTimeline, StateViolation, Tick,
    Timeline, Violation, Window,
};

/// A handle of a numeric [`Timeline`] in a [`Plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimelineId(usize);

impl TimelineId {
    /// The position of the timeline in [`Plan::timelines`].
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// A handle of a [`StateTimeline`] in a [`Plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateId(usize);

impl StateId {
    /// The position of the timeline in [`Plan::states`].
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// One effect of an activity on one timeline of a plan, with ticks relative to the activity
/// start.
///
/// An activity that draws 4 power units for its first 10 ticks is
/// `Demand::Level { timeline: power, impact: Impact::Claim { start: 0, end: 10, amount: 4 } }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Demand {
    /// An effect on a numeric timeline.
    Level {
        /// The timeline affected.
        timeline: TimelineId,
        /// The effect, with ticks relative to the activity start.
        impact: Impact,
    },
    /// An effect on a state timeline.
    State {
        /// The timeline affected.
        timeline: StateId,
        /// The effect, with ticks relative to the activity start.
        impact: StateImpact,
    },
}

impl Demand {
    /// The ticks of the demand, relative to the activity start, at which it changes something.
    const fn offsets(&self) -> [Tick; 2] {
        match *self {
            Self::Level { impact, .. } => [impact.first_tick(), impact.last_tick()],
            Self::State {
                impact: StateImpact::Set { at, .. },
                ..
            } => [at, at],
            Self::State {
                impact: StateImpact::Require { start, end, .. },
                ..
            } => [start, end],
        }
    }

    /// Whether the demand addresses the same timeline as `other`.
    fn same_timeline(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Level { timeline: a, .. }, Self::Level { timeline: b, .. }) => a == b,
            (Self::State { timeline: a, .. }, Self::State { timeline: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// The earliest violation of a plan and the timeline it occurs on.
///
/// `S` is the storage of the plan's timelines, [`PlanStorage::Line`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlanViolation<
    #[cfg(feature = "alloc")] S: Storage = Heap,
    #[cfg(not(feature = "alloc"))] S: Storage,
> {
    /// A numeric timeline leaves its bounds.
    Level {
        /// The violated timeline.
        timeline: TimelineId,
        /// The explanation.
        violation: Violation<S>,
    },
    /// A state timeline fails a requirement or has contradicting sets.
    State {
        /// The violated timeline.
        timeline: StateId,
        /// The explanation.
        violation: StateViolation<S>,
    },
}

impl<S: Storage> PlanViolation<S> {
    /// The tick of the violation.
    #[must_use]
    pub const fn at(&self) -> Tick {
        match self {
            Self::Level { violation, .. } => violation.at,
            Self::State { violation, .. } => violation.at(),
        }
    }
}

/// A set of numeric and state timelines, one per resource, checked together.
///
/// The storage `P` is [`Heap`](crate::storage::Heap) by default. With
/// [`FixedPlan<T, S, N>`](crate::storage::FixedPlan) the plan holds at most `T` numeric and `S`
/// state timelines with [`Fixed<N>`](crate::storage::Fixed) storage each, and placement,
/// checks and the earliest-start search never allocate.
///
/// # Examples
///
/// ```
/// use resource_timeline::{ActivityId, Demand, Impact, Plan, Timeline, Window};
///
/// let mut plan = Plan::new();
/// let transmitter = plan.add_timeline(Timeline::claimable(0, 1, 16)?)?;
/// let downlink = [Demand::Level {
///     timeline: transmitter,
///     impact: Impact::Claim { start: 0, end: 8, amount: 1 },
/// }];
///
/// plan.place(ActivityId(1), 5, &downlink)?;
/// let window = Window::new(0, 100)?;
/// // Before tick 5 there is no room for 8 ticks; the transmitter is free again at 13.
/// assert_eq!(plan.earliest_start(&downlink, window, 1_000)?, Some(13));
/// # Ok::<(), resource_timeline::Error>(())
/// ```
///
/// The same plan without allocation, for at most two numeric and one state timeline with room
/// for 16 impacts each:
///
/// ```
/// use resource_timeline::storage::{Fixed, FixedPlan};
/// use resource_timeline::{ActivityId, Demand, Impact, Plan, Timeline, Window};
///
/// let mut plan = Plan::<FixedPlan<2, 1, 16>>::new_in();
/// let transmitter = plan.add_timeline(Timeline::<Fixed<16>>::claimable_in(0, 1, 16)?)?;
/// let downlink = [Demand::Level {
///     timeline: transmitter,
///     impact: Impact::Claim { start: 0, end: 8, amount: 1 },
/// }];
///
/// plan.place(ActivityId(1), 5, &downlink)?;
/// assert_eq!(plan.earliest_start(&downlink, Window::new(0, 100)?, 1_000)?, Some(13));
/// # Ok::<(), resource_timeline::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Plan<
    #[cfg(feature = "alloc")] P: PlanStorage = Heap,
    #[cfg(not(feature = "alloc"))] P: PlanStorage,
> {
    /// The numeric timelines, addressed by [`TimelineId`].
    timelines: P::Timelines,
    /// The state timelines, addressed by [`StateId`].
    states: P::States,
}

#[cfg(feature = "alloc")]
impl Plan {
    /// Creates an empty heap plan.
    #[must_use]
    pub const fn new() -> Self {
        Self::new_in()
    }
}

impl<P: PlanStorage> Plan<P> {
    /// Creates an empty plan with storage `P`.
    #[must_use]
    pub const fn new_in() -> Self {
        Self {
            timelines: <P::Timelines as Seq<_>>::EMPTY,
            states: <P::States as Seq<_>>::EMPTY,
        }
    }

    /// Adds a numeric timeline and returns its handle.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the timeline list cannot grow (heap storage),
    /// [`Error::CapacityExceeded`] naming [`Container::Timelines`] if it is full (fixed
    /// storage).
    pub fn add_timeline(&mut self, timeline: Timeline<P::Line>) -> Result<TimelineId, Error> {
        self.timelines.try_push(timeline, Container::Timelines)?;
        Ok(TimelineId(self.timelines.len() - 1))
    }

    /// Adds a state timeline and returns its handle.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the timeline list cannot grow (heap storage),
    /// [`Error::CapacityExceeded`] naming [`Container::States`] if it is full (fixed storage).
    pub fn add_state(&mut self, timeline: StateTimeline<P::Line>) -> Result<StateId, Error> {
        self.states.try_push(timeline, Container::States)?;
        Ok(StateId(self.states.len() - 1))
    }

    /// The numeric timelines, in the order they were added.
    #[must_use]
    pub fn timelines(&self) -> &[Timeline<P::Line>] {
        &self.timelines
    }

    /// The state timelines, in the order they were added.
    #[must_use]
    pub fn states(&self) -> &[StateTimeline<P::Line>] {
        &self.states
    }

    /// The numeric timeline behind `id`, if it belongs to this plan.
    #[must_use]
    pub fn timeline(&self, id: TimelineId) -> Option<&Timeline<P::Line>> {
        self.timelines.get(id.0)
    }

    /// The state timeline behind `id`, if it belongs to this plan.
    #[must_use]
    pub fn state(&self, id: StateId) -> Option<&StateTimeline<P::Line>> {
        self.states.get(id.0)
    }

    /// Requires the level of numeric timeline `id` at `target.at` to be at least `target.min`.
    ///
    /// See [`Timeline::add_target`]. [`fits`](Self::fits) and
    /// [`earliest_start`](Self::earliest_start) then reject starts that would miss it.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownResource`] if `id` is not part of this plan, and the errors of
    /// [`Timeline::add_target`].
    pub fn add_target(&mut self, id: TimelineId, target: crate::Target) -> Result<(), Error> {
        self.timelines
            .get_mut(id.0)
            .ok_or(Error::UnknownResource)?
            .add_target(target)
    }

    /// Adds every demand of `activity`, shifted to `start`, to its timeline.
    ///
    /// The placement is all or nothing. It does not check the bounds; use
    /// [`fits`](Self::fits) before or [`first_violation`](Self::first_violation) after.
    /// Placing the same activity id twice is allowed; [`remove`](Self::remove) then removes both.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownResource`] if a demand names a timeline outside this plan.
    /// - [`Error::Overflow`] if a shifted tick does not fit a [`Tick`].
    /// - The validation errors of [`Timeline::add`] and [`StateTimeline::add`], including
    ///   [`Error::BudgetExhausted`] and, with fixed storage, [`Error::CapacityExceeded`]. Both
    ///   are detected before anything is added.
    /// - [`Error::OutOfMemory`] if an impact list cannot grow.
    ///
    /// The plan is unchanged on error.
    pub fn place(
        &mut self,
        activity: ActivityId,
        start: Tick,
        demands: &[Demand],
    ) -> Result<(), Error> {
        for (index, demand) in demands.iter().enumerate() {
            let pending = demands[..=index]
                .iter()
                .filter(|other| other.same_timeline(demand))
                .count();
            self.validate(&self.shift(demand, start)?, pending)?;
        }

        for (added, demand) in demands.iter().enumerate() {
            let shifted = self.shift(demand, start)?;
            if let Err(error) = self.add_unchecked(activity, shifted) {
                self.undo(&demands[..added]);
                return Err(error);
            }
        }

        Ok(())
    }

    /// Removes every impact of `activity` from every timeline and returns how many there were.
    pub fn remove(&mut self, activity: ActivityId) -> usize {
        let levels: usize = self.timelines.iter_mut().map(|t| t.remove(activity)).sum();
        let states: usize = self.states.iter_mut().map(|t| t.remove(activity)).sum();
        levels + states
    }

    /// The earliest violation over all timelines.
    ///
    /// Ties at the same tick go to numeric timelines before state timelines, then to the
    /// timeline added first.
    ///
    /// # Errors
    ///
    /// The errors of [`Timeline::first_violation`] and [`StateTimeline::first_violation`].
    pub fn first_violation(&self) -> Result<Option<PlanViolation<P::Line>>, Error> {
        let mut earliest: Option<PlanViolation<P::Line>> = None;
        for (index, timeline) in self.timelines.iter().enumerate() {
            if let Some(violation) = timeline.first_violation()? {
                let found = PlanViolation::Level {
                    timeline: TimelineId(index),
                    violation,
                };
                keep_earlier(&mut earliest, found);
            }
        }

        for (index, timeline) in self.states.iter().enumerate() {
            if let Some(violation) = timeline.first_violation()? {
                let found = PlanViolation::State {
                    timeline: StateId(index),
                    violation,
                };
                keep_earlier(&mut earliest, found);
            }
        }

        Ok(earliest)
    }

    /// Whether the activity described by `demands` could start at `start` without violating
    /// any timeline it touches.
    ///
    /// "Without violating" means the touched timelines have no violation at all once the
    /// demands are added, so a timeline that is already violated admits nothing. Timelines the
    /// demands do not touch are not checked. A demand that would precede its timeline's start
    /// does not fit. The impact budgets are not consulted; [`place`](Self::place) does that.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownResource`] if a demand names a timeline outside this plan.
    /// - [`Error::Overflow`] if a shifted tick or a level does not fit.
    /// - The validation errors of [`Timeline::add`] and [`StateTimeline::add`] other than
    ///   [`Error::BeforeStart`], which mean the demands are malformed at every start.
    /// - [`Error::OutOfMemory`] if the scratch buffers cannot be allocated (heap storage).
    /// - [`Error::CapacityExceeded`] naming [`Container::Scratch`] if more demands address one
    ///   timeline than its fixed capacity holds (fixed storage); such an activity could not be
    ///   placed either.
    pub fn fits(&self, demands: &[Demand], start: Tick) -> Result<bool, Error> {
        search::Probe::new(self, demands)?.fits(start)
    }

    /// The earliest start in `window` at which [`fits`](Self::fits) holds, or `None`.
    ///
    /// The answer can only change where a demand tick meets a change tick or target of its
    /// timeline. The search walks the pieces between consecutive such candidates, `O(n k)` of
    /// them for `n` change ticks and `k` demands, without collecting them. Without rates the
    /// levels are piecewise constant and one check at the piece start decides the whole piece.
    /// With rates (in the demands or a touched timeline) the levels are affine in the start
    /// within a piece, and the earliest fitting start of the piece is solved in closed form
    /// from two sweeps, so the cost does not grow with the window length. Each piece costs
    /// `O(n k)`; nothing is allocated beyond the scratch reserved up front.
    ///
    /// `max_checks` caps the number of pieces examined.
    ///
    /// # Errors
    ///
    /// - [`Error::SearchExhausted`] if `max_checks` pieces were examined without an answer.
    /// - The errors of [`fits`](Self::fits).
    pub fn earliest_start(
        &self,
        demands: &[Demand],
        window: Window,
        max_checks: usize,
    ) -> Result<Option<Tick>, Error> {
        search::earliest_start(self, demands, window, max_checks)
    }

    /// `demand` moved to absolute ticks for an activity starting at `start`.
    fn shift(&self, demand: &Demand, start: Tick) -> Result<Demand, Error> {
        let shifted = match *demand {
            Demand::Level { timeline, impact } => {
                self.timeline(timeline).ok_or(Error::UnknownResource)?;
                impact
                    .shifted(start)
                    .map(|impact| Demand::Level { timeline, impact })
            }
            Demand::State { timeline, impact } => {
                self.state(timeline).ok_or(Error::UnknownResource)?;
                impact
                    .shifted(start)
                    .map(|impact| Demand::State { timeline, impact })
            }
        };

        shifted.ok_or(Error::Overflow)
    }

    /// Checks a shifted demand and that its timeline has room for `pending` more impacts.
    fn validate(&self, demand: &Demand, pending: usize) -> Result<(), Error> {
        match *demand {
            Demand::Level { timeline, impact } => {
                let timeline = &self.timelines[timeline.0];
                timeline.validate(impact)?;
                timeline.ensure_room(pending)
            }
            Demand::State { timeline, impact } => {
                let timeline = &self.states[timeline.0];
                timeline.validate(impact)?;
                timeline.ensure_room(pending)
            }
        }
    }

    /// Adds an already validated, shifted demand.
    fn add_unchecked(&mut self, activity: ActivityId, demand: Demand) -> Result<(), Error> {
        match demand {
            Demand::Level { timeline, impact } => self.timelines[timeline.0].add(activity, impact),
            Demand::State { timeline, impact } => self.states[timeline.0].add(activity, impact),
        }
    }

    /// Takes back the impacts that the given, already added demands created, newest first.
    fn undo(&mut self, added: &[Demand]) {
        for demand in added.iter().rev() {
            match *demand {
                Demand::Level { timeline, .. } => self.timelines[timeline.0].undo_last(),
                Demand::State { timeline, .. } => self.states[timeline.0].undo_last(),
            }
        }
    }
}

/// Replaces `earliest` with `found` if `found` is strictly earlier or `earliest` is empty.
fn keep_earlier<S: Storage>(earliest: &mut Option<PlanViolation<S>>, found: PlanViolation<S>) {
    if earliest
        .as_ref()
        .is_none_or(|current| found.at() < current.at())
    {
        *earliest = Some(found);
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    //! Unit tests of the rollback path, which `place` reaches only on allocation failure.

    use alloc::vec::Vec;

    use super::{Demand, Plan};
    use crate::{ActivityId, Bounds, Impact, StateImpact, StateTimeline, Timeline};

    /// Undoing freshly added demands restores the plan exactly, including earlier impacts of
    /// the same activity and duplicate sets.
    #[test]
    fn undo_restores_the_plan() {
        let mut plan = Plan::new();
        let bounds = Bounds::new(0, 10).expect("valid");
        let battery = plan
            .add_timeline(Timeline::depletable(0, 5, bounds, 8).expect("valid"))
            .expect("memory");
        let mode = plan.add_state(StateTimeline::new(0, 0, 8)).expect("memory");
        let demands = [
            Demand::Level {
                timeline: battery,
                impact: Impact::Rate {
                    start: 0,
                    end: 4,
                    per_tick: -1,
                },
            },
            Demand::State {
                timeline: mode,
                impact: StateImpact::Set { at: 2, value: 1 },
            },
            Demand::State {
                timeline: mode,
                impact: StateImpact::Require {
                    start: 2,
                    end: 5,
                    value: 1,
                },
            },
            Demand::Level {
                timeline: battery,
                impact: Impact::Step { at: 3, delta: 2 },
            },
        ];
        plan.place(ActivityId(1), 1, &demands).expect("valid");
        let before = plan.clone();

        plan.place(ActivityId(1), 1, &demands).expect("valid");
        let shifted: Vec<Demand> = demands
            .iter()
            .map(|d| plan.shift(d, 1).expect("small"))
            .collect();
        plan.undo(&shifted);
        assert_eq!(plan, before);
    }
}
