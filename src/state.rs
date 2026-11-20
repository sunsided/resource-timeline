//! State timelines: discrete values such as modes, switch positions or visibility windows.
//!
//! A numeric [`Timeline`](crate::Timeline) checks that a level stays in a range. A
//! [`StateTimeline`] checks that a discrete value *equals* what activities require: the
//! spectrometer must be warm while it analyzes, the antenna must see Earth while it transmits.
//! Activities change the value with [`StateImpact::Set`] and demand it with
//! [`StateImpact::Require`].
//!
//! Visibility windows (comm passes, daylight) are a state timeline whose value is set by a
//! ground-provided activity at each window start and end:
//!
//! ```
//! use resource_timeline::{ActivityId, StateImpact, StateTimeline, StateViolation};
//!
//! const HIDDEN: u32 = 0;
//! const VISIBLE: u32 = 1;
//! let passes = ActivityId(0);
//! let downlink = ActivityId(7);
//!
//! let mut earth = StateTimeline::new(0, HIDDEN, 16);
//! earth.add(passes, StateImpact::Set { at: 100, value: VISIBLE })?;
//! earth.add(passes, StateImpact::Set { at: 160, value: HIDDEN })?;
//! earth.add(downlink, StateImpact::Require { start: 150, end: 170, value: VISIBLE })?;
//!
//! let violation = earth.first_violation()?.expect("the pass ends too early");
//! assert_eq!(
//!     violation,
//!     StateViolation::Mismatch {
//!         at: 160,
//!         required: VISIBLE,
//!         actual: HIDDEN,
//!         requirer: downlink,
//!         setter: Some(passes),
//!     }
//! );
//! # Ok::<(), resource_timeline::Error>(())
//! ```

mod check;

#[cfg(feature = "alloc")]
use crate::storage::Heap;
use crate::storage::{Seq, Slot, Storage};
use crate::{ActivityId, Container, Error, Tick};
pub(crate) use check::SetPoint;
use check::Sets;

/// A discrete state value. The meaning of each value is chosen by the mission.
pub type StateValue = u32;

/// One effect of an activity on a state timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateImpact {
    /// The state becomes `value` at tick `at` and keeps it until the next set.
    Set {
        /// The tick of the change. Not before the timeline start.
        at: Tick,
        /// The new state value.
        value: StateValue,
    },
    /// The state must equal `value` at every tick of the half-open interval `[start, end)`.
    Require {
        /// The first tick of the requirement. Not before the timeline start.
        start: Tick,
        /// The first tick after the requirement. Strictly greater than `start`.
        end: Tick,
        /// The required state value.
        value: StateValue,
    },
}

impl StateImpact {
    /// The earliest tick the impact refers to.
    #[must_use]
    pub const fn first_tick(&self) -> Tick {
        match *self {
            Self::Set { at, .. } => at,
            Self::Require { start, .. } => start,
        }
    }

    /// The same impact moved `by` ticks later (earlier for negative `by`).
    ///
    /// Returns `None` if a moved tick does not fit a [`Tick`].
    #[must_use]
    pub const fn shifted(&self, by: Tick) -> Option<Self> {
        match *self {
            Self::Set { at, value } => match at.checked_add(by) {
                Some(at) => Some(Self::Set { at, value }),
                None => None,
            },
            Self::Require { start, end, value } => {
                match (start.checked_add(by), end.checked_add(by)) {
                    (Some(start), Some(end)) => Some(Self::Require { start, end, value }),
                    _ => None,
                }
            }
        }
    }
}

/// The first tick at which a state timeline is inconsistent, and why.
///
/// The setter list of a conflict uses the storage `S` of the timeline that reported it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StateViolation<
    #[cfg(feature = "alloc")] S: Storage = Heap,
    #[cfg(not(feature = "alloc"))] S: Storage,
> {
    /// A requirement sees a different value.
    Mismatch {
        /// The earliest tick at which the requirement fails.
        at: Tick,
        /// The value the requirement needs.
        required: StateValue,
        /// The value the state has at `at`.
        actual: StateValue,
        /// The activity that holds the requirement. The smallest id if several fail at `at`;
        /// for several failing requirements of that activity, `required` is the smallest value.
        requirer: ActivityId,
        /// The activity whose set established `actual`, or `None` for the initial value.
        /// The smallest id if several set the same value at the same tick.
        setter: Option<ActivityId>,
    },
    /// Activities set different values at the same tick, so the state there is ambiguous.
    ///
    /// Reported whether or not a requirement depends on the value, because two commands that
    /// contradict each other at the same tick are a scheduling conflict in themselves.
    Conflict {
        /// The tick of the contradicting sets.
        at: Tick,
        /// Every activity that sets a value at `at`. Sorted ascending, without duplicates.
        setters: S::List<ActivityId>,
    },
}

impl<S: Storage> StateViolation<S> {
    /// The tick of the violation.
    #[must_use]
    pub const fn at(&self) -> Tick {
        match *self {
            Self::Mismatch { at, .. } | Self::Conflict { at, .. } => at,
        }
    }
}

/// A requirement stored by a state timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Requirement {
    /// The activity that holds the requirement.
    pub(crate) activity: ActivityId,
    /// The first required tick.
    pub(crate) start: Tick,
    /// The first tick after the requirement.
    pub(crate) end: Tick,
    /// The required value.
    pub(crate) value: StateValue,
}

impl crate::storage::Sealed for Requirement {}

impl Slot for Requirement {
    const EMPTY: Self = Self {
        activity: ActivityId(0),
        start: 0,
        end: 0,
        value: 0,
    };
}

/// A discrete state over time, built from activity sets and checked against requirements.
///
/// The state equals the initial value from the start tick until the first set, and the value of
/// the latest set afterwards. Sets are kept sorted by tick, so a check costs
/// `O(n + r log n)` for `n` sets and `r` requirements and does not allocate unless it reports a
/// [`StateViolation::Conflict`].
///
/// The storage `S` is [`Heap`](crate::storage::Heap) by default; with
/// [`Fixed<N>`](crate::storage::Fixed) the timeline holds at most `N` impacts and never
/// allocates. Heap timelines are created with [`new`](Self::new), timelines with any storage
/// with [`new_in`](Self::new_in).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StateTimeline<
    #[cfg(feature = "alloc")] S: Storage = Heap,
    #[cfg(not(feature = "alloc"))] S: Storage,
> {
    /// The first tick of the timeline. No impact may refer to an earlier tick.
    start: Tick,
    /// The value from the start until the first set.
    initial: StateValue,
    /// The maximum number of impacts the timeline accepts.
    budget: usize,
    /// The impacts in insertion order. Never longer than `budget`.
    impacts: S::List<(ActivityId, StateImpact)>,
    /// The sets, sorted by tick and then by activity.
    sets: S::List<SetPoint>,
    /// The requirements in insertion order.
    requires: S::List<Requirement>,
}

#[cfg(feature = "alloc")]
impl StateTimeline {
    /// Creates a heap state timeline that holds `initial` from `start` on, with room for
    /// `budget` impacts.
    #[must_use]
    pub const fn new(start: Tick, initial: StateValue, budget: usize) -> Self {
        Self::new_in(start, initial, budget)
    }
}

impl<S: Storage> StateTimeline<S> {
    /// Creates a state timeline with storage `S` that holds `initial` from `start` on, with
    /// room for `budget` impacts.
    ///
    /// As [`StateTimeline::new`], for any [`Storage`]. With
    /// [`Fixed<N>`](crate::storage::Fixed), adds beyond `N` impacts fail with
    /// [`Error::CapacityExceeded`] even if the budget is larger.
    #[must_use]
    pub const fn new_in(start: Tick, initial: StateValue, budget: usize) -> Self {
        Self {
            start,
            initial,
            budget,
            impacts: <S::List<(ActivityId, StateImpact)> as Seq<_>>::EMPTY,
            sets: <S::List<SetPoint> as Seq<_>>::EMPTY,
            requires: <S::List<Requirement> as Seq<_>>::EMPTY,
        }
    }

    /// The first tick of the timeline.
    #[must_use]
    pub const fn start(&self) -> Tick {
        self.start
    }

    /// The value from the start until the first set.
    #[must_use]
    pub const fn initial(&self) -> StateValue {
        self.initial
    }

    /// The impacts added so far, in insertion order.
    #[must_use]
    pub fn impacts(&self) -> &[(ActivityId, StateImpact)] {
        &self.impacts
    }

    /// Adds an impact of `activity`.
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if the impact refers to a tick before the start.
    /// - [`Error::EmptyInterval`] if a requirement has `end <= start`.
    /// - [`Error::BudgetExhausted`] if the timeline already holds `budget` impacts.
    /// - [`Error::OutOfMemory`] if the impact lists cannot grow (heap storage).
    /// - [`Error::CapacityExceeded`] if a fixed impact list is full.
    ///
    /// The timeline is unchanged on error.
    pub fn add(&mut self, activity: ActivityId, impact: StateImpact) -> Result<(), Error> {
        self.validate(impact)?;
        self.ensure_room(1)?;
        self.impacts.try_reserve(1, Container::Impacts)?;
        match impact {
            StateImpact::Set { at, value } => {
                let point = SetPoint {
                    at,
                    activity,
                    value,
                };
                let index = self.sets.partition_point(|set| set.key() <= point.key());
                self.sets.try_insert(index, point, Container::Sets)?;
            }
            StateImpact::Require { start, end, value } => {
                let requirement = Requirement {
                    activity,
                    start,
                    end,
                    value,
                };
                self.requires
                    .try_push(requirement, Container::Requirements)?;
            }
        }

        self.impacts
            .try_push((activity, impact), Container::Impacts)
    }

    /// Removes every impact of `activity` and returns how many there were. Never allocates.
    pub fn remove(&mut self, activity: ActivityId) -> usize {
        let before = self.impacts.len();
        self.impacts.retain(|&(owner, _)| owner != activity);
        self.sets.retain(|set| set.activity != activity);
        self.requires.retain(|require| require.activity != activity);
        before - self.impacts.len()
    }

    /// Removes the most recently added impact, if any. Never allocates.
    pub(crate) fn undo_last(&mut self) {
        match self.impacts.pop() {
            Some((activity, StateImpact::Set { at, value })) => {
                let added = SetPoint {
                    at,
                    activity,
                    value,
                };
                if let Some(index) = self.sets.iter().rposition(|set| *set == added) {
                    self.sets.remove(index);
                }
            }
            Some((_, StateImpact::Require { .. })) => {
                self.requires.pop();
            }
            None => {}
        }
    }

    /// The first tick `>= x` at which the outcome of a check can change: the start, a set, or
    /// a requirement start or end.
    ///
    /// Costs `O(log n)` for the sorted sets plus `O(r)` for `r` requirements.
    pub(crate) fn first_change_from(&self, x: i128) -> Option<Tick> {
        let set = self
            .sets
            .get(self.sets.partition_point(|set| i128::from(set.at) < x))
            .map(|set| set.at);
        let requires = self
            .requires
            .iter()
            .flat_map(|require| [require.start, require.end])
            .filter(|&tick| i128::from(tick) >= x);
        let start = (i128::from(self.start) >= x).then_some(self.start);
        start.into_iter().chain(set).chain(requires).min()
    }

    /// `Ok` while the timeline has room for `extra` more impacts, by budget and by capacity.
    ///
    /// Sets and requirements are never more than impacts, so the impact capacity covers them.
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

    /// Checks that `impact` is well formed and does not precede the start.
    ///
    /// # Errors
    ///
    /// The validation errors listed on [`add`](Self::add).
    pub(crate) const fn validate(&self, impact: StateImpact) -> Result<(), Error> {
        if let StateImpact::Require { start, end, .. } = impact {
            if end <= start {
                return Err(Error::EmptyInterval { start, end });
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

    /// The state value at tick `t`, or `None` if contradicting sets make it ambiguous there.
    ///
    /// # Errors
    ///
    /// [`Error::BeforeStart`] if `t` precedes the start.
    pub fn value_at(&self, t: Tick) -> Result<Option<StateValue>, Error> {
        if t < self.start {
            return Err(Error::BeforeStart {
                at: t,
                start: self.start,
            });
        }

        Ok(self.view(&[]).value_at(t, self.initial))
    }

    /// The value changes over time for display and telemetry: `(tick, value)` pairs in time
    /// order, starting with the initial value at the start tick.
    ///
    /// A tick appears only if the value there differs from the one before; `None` marks a tick
    /// where contradicting sets make the value ambiguous. Costs `O(n log n)` and never
    /// allocates.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::{ActivityId, StateImpact, StateTimeline};
    ///
    /// let mut mode = StateTimeline::new(0, 0, 8);
    /// mode.add(ActivityId(1), StateImpact::Set { at: 5, value: 1 })?;
    /// mode.add(ActivityId(2), StateImpact::Set { at: 9, value: 1 })?;
    /// mode.add(ActivityId(3), StateImpact::Set { at: 12, value: 0 })?;
    ///
    /// let changes: Vec<_> = mode.values().collect();
    /// assert_eq!(changes, [(0, Some(0)), (5, Some(1)), (12, Some(0))]);
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn values(&self) -> impl Iterator<Item = (Tick, Option<StateValue>)> + '_ {
        let mut previous: Option<Option<StateValue>> = None;
        let view = self.view(&[]);
        let start = core::iter::once((self.start, view.value_at(self.start, self.initial)));
        let set_ticks = self
            .sets
            .iter()
            .enumerate()
            .filter(|&(index, set)| index == 0 || self.sets[index - 1].at != set.at)
            .map(move |(_, set)| (set.at, view.value_at(set.at, self.initial)));
        start.chain(set_ticks).filter(move |&(_, value)| {
            let changed = previous != Some(value);
            previous = Some(value);
            changed
        })
    }

    /// Finds the earliest violation: a failed requirement or contradicting sets.
    ///
    /// A conflict at tick `c` wins over mismatches at `c` or later, since the state is
    /// undefined from there on. The result does not depend on insertion order.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the setter list of a conflict cannot be allocated (heap
    /// storage). Fixed storage has room for it by construction.
    pub fn first_violation(&self) -> Result<Option<StateViolation<S>>, Error> {
        let view = self.view(&[]);
        match view.first_breach(self.initial, &self.requires, &[]) {
            Some(breach) => view.explain::<S>(breach).map(Some),
            None => Ok(None),
        }
    }

    /// Whether the timeline would be violated if the sorted `extra_sets` and the
    /// `extra_requires` were added. Never allocates.
    pub(crate) fn is_violated_with(
        &self,
        extra_sets: &[SetPoint],
        extra_requires: &[Requirement],
    ) -> bool {
        self.view(extra_sets)
            .first_breach(self.initial, &self.requires, extra_requires)
            .is_some()
    }

    /// The stored sets merged with `extra`, for queries.
    fn view<'a>(&'a self, extra: &'a [SetPoint]) -> Sets<'a> {
        Sets {
            base: &self.sets,
            extra,
        }
    }
}

impl<S: Storage> crate::storage::Sealed for StateTimeline<S> {}

impl<S: Storage> Slot for StateTimeline<S> {
    const EMPTY: Self = Self::new_in(0, 0, 0);
}
