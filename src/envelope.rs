//! Level envelopes for activities whose start lies in a window.
//!
//! A flexible schedule fixes each activity's start only to a [`Window`], for example the
//! windows a simple temporal network allows. The envelope bounds the level over *every*
//! schedule the windows admit: at each tick, the lower envelope is the lowest level any
//! schedule can reach there and the upper envelope the highest. If the lower envelope never
//! falls below the minimum and the upper never exceeds the maximum, every schedule is safe and
//! the dispatcher may pick any start in the windows without re-checking resources.
//!
//! This is the resource-envelope idea from Muscettola's work on flexible schedules. In its
//! simplest form the windows are independent: the activities' starts may be chosen
//! separately, so the extreme level at a tick is the sum of each activity's own extreme
//! contribution there, and the envelope computed here is exactly the tightest one for
//! independent windows. Three refinements build on it:
//!
//! - **Uncertain durations** ([`Envelope::with_contingent`]): a [`Contingent`] activity has a
//!   start window and a duration range, and its impacts are [`Uncertain`]: anchored at the
//!   start or at the (uncertain) end. Claims held until the end on a claimable timeline and
//!   rates that run until the end are covered for every start and every duration, exactly.
//! - **Precedences** ([`Envelope::with_precedences`]): [`Precedence`] constraints between the
//!   starts and ends of activities tighten windows and durations by propagation and couple the
//!   linked activities exactly (a maximum-weight closure for stepwise ones, a discretised
//!   minimum cut with rates or uncertain durations); see the `precedence` module.
//! - **States** ([`StateEnvelope`]): the same question for a state timeline, with
//!   requirements and conflicting sets instead of bounds; exact for independent windows.
//!
//! # Bridge to `temporal-control`
//!
//! The crates do not depend on each other. Integration code builds the windows from
//! `temporal-control`: before execution from the minimal network, `MinimalNetwork::lower` and
//! `upper` between the origin and each activity's start time point; during execution from
//! `Dispatcher::window`, which narrows as events are observed. Unbounded sides must be clamped
//! to a mission horizon. Re-running [`Envelope::check`] on the narrowed windows after each
//! observation keeps the resource guarantee in step with the temporal one. Contingent link
//! bounds `[l, u]` of an STNU become the [`Contingent::duration`], and the network's
//! requirement links between starts become [`Precedence`]s.
//!
//! ```mermaid
//! graph LR
//!     stn["temporal-control: start windows"] --> flexible["Flexible activities"]
//!     fixed["Timeline: fixed impacts"] --> envelope["Envelope: lower and upper level per tick"]
//!     contingent["Contingent activities: start window, duration range"] --> envelope
//!     precedences["Precedences between starts"] --> envelope
//!     flexible --> envelope
//!     envelope --> verdict["Verdict: Safe, Possible or Certain"]
//! ```

mod closure;
mod contingent;
mod cut;
mod member;
mod precedence;
mod state;

use alloc::vec::Vec;

pub use contingent::{Anchor, Contingent, Uncertain};
use member::{Extreme, Member, Shape};
use precedence::Network;
pub use precedence::{Endpoint, Precedence};
pub use state::{FlexibleState, StateEnvelope, StateEnvelopeViolation, StateVerdict};

use crate::storage::{Heap, Storage};

/// The default node budget of the exact coupled cut, see [`Envelope::with_cut_budget`].
const DEFAULT_CUT_BUDGET: usize = 4096;
use crate::{ActivityId, Bound, Error, Impact, Level, Tick, Timeline, Window};

/// An activity whose start is only known to lie in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Flexible<'a> {
    /// The activity, reported back in explanations.
    pub activity: ActivityId,
    /// The allowed starts.
    pub window: Window,
    /// The effects of the activity on the timeline, with ticks relative to its start.
    pub impacts: &'a [Impact],
}

/// The outcome of an envelope check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Every schedule the windows admit keeps the level within bounds at every tick.
    Safe,
    /// Some schedule may leave the bounds at the reported tick, but the envelopes do not show
    /// that every schedule leaves them at that same tick. This does not imply that a safe
    /// schedule exists: every schedule may still violate, each at a different tick. The
    /// envelopes are exact, so some schedule does leave the bounds there, unless precedences
    /// link activities with rates or uncertain durations whose domains exceed the cut budget
    /// ([`Envelope::with_cut_budget`]); then it may be a false alarm.
    Possible(EnvelopeViolation),
    /// Every schedule the windows admit leaves the bounds at the reported tick.
    Certain(EnvelopeViolation),
}

/// A tick at which the envelope breaks a bound, and the activities that can push it there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvelopeViolation {
    /// The tick at which the envelope breaks the bound.
    pub at: Tick,
    /// The envelope value that breaks the bound: for [`Bound::BelowMin`] the lower envelope of
    /// a possible violation or the upper envelope of a certain one, mirrored for
    /// [`Bound::AboveMax`].
    pub level: Level,
    /// The broken bound.
    pub bound: Bound,
    /// The activities, fixed or flexible, that can push the level towards the bound at `at`:
    /// a fixed activity whose net effect there points towards the bound, or a flexible
    /// activity for which some start in its window does. Sorted ascending, without duplicates.
    pub contributors: Vec<ActivityId>,
}

/// The lower and upper level envelopes of a timeline with additional flexible activities.
///
/// The timeline's own impacts count as fixed. Create it with [`Envelope::new`].
///
/// # Examples
///
/// ```
/// use resource_timeline::envelope::{Envelope, Flexible, Verdict};
/// use resource_timeline::{ActivityId, Bounds, Impact, Timeline, Window};
///
/// // A battery at 10 units, reserve 0, charged by 6 units at tick 10.
/// let mut battery = Timeline::depletable(0, 10, Bounds::new(0, 100)?, 8)?;
/// battery.add(ActivityId(0), Impact::Step { at: 10, delta: 6 })?;
///
/// // Two heaters, each drawing 6 units, may start anywhere in [0, 20].
/// let heat = [Impact::Step { at: 0, delta: -6 }];
/// let window = Window::new(0, 20)?;
/// let flexible = [
///     Flexible { activity: ActivityId(1), window, impacts: &heat },
///     Flexible { activity: ActivityId(2), window, impacts: &heat },
/// ];
///
/// let envelope = Envelope::new(&battery, &flexible)?;
/// assert_eq!(envelope.lower_at(0)?, -2); // both heaters start before the charge
/// assert_eq!(envelope.upper_at(0)?, 10); // neither has started yet
/// match envelope.check()? {
///     Verdict::Possible(violation) => assert_eq!(violation.at, 0),
///     verdict => panic!("unexpected {verdict:?}"),
/// }
///
/// // With the second heater held back until after the charge, every schedule is safe.
/// let late = Window::new(10, 20)?;
/// let flexible = [flexible[0], Flexible { window: late, ..flexible[1] }];
/// assert_eq!(Envelope::new(&battery, &flexible)?.check()?, Verdict::Safe);
/// # Ok::<(), resource_timeline::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Envelope<'a, S: Storage = Heap> {
    /// The timeline with the fixed impacts, start, initial level, bounds and targets.
    timeline: &'a Timeline<S>,
    /// The flexible and contingent activities, validated against the timeline.
    members: Vec<Member<'a>>,
    /// The start window of each member, tightened by the precedences.
    windows: Vec<Window>,
    /// The duration range of each member, tightened by the precedences (`[0, 0]` for fixed
    /// impacts).
    durations: Vec<Window>,
    /// The largest graph the exact coupled cut may build, in nodes.
    cut_budget: usize,
    /// The precedences between members, as given.
    precedences: Vec<Precedence>,
    /// The temporal network of the precedences, `None` without precedences.
    network: Option<Network>,
}

impl<'a, S: Storage> Envelope<'a, S> {
    /// Combines the fixed impacts of `timeline` with the `flexible` activities.
    ///
    /// The windows are independent until [`with_precedences`](Self::with_precedences) couples
    /// them; [`with_contingent`](Self::with_contingent) adds activities with uncertain
    /// durations.
    ///
    /// # Errors
    ///
    /// - The validation errors of [`Timeline::add`] for any flexible impact at the earliest or
    ///   latest start of its window, including [`Error::BeforeStart`] when the earliest start
    ///   would place an impact before the timeline start.
    /// - [`Error::Overflow`] if a shifted tick does not fit a [`Tick`].
    /// - [`Error::OutOfMemory`] if the activity list cannot be allocated.
    pub fn new(timeline: &'a Timeline<S>, flexible: &'a [Flexible<'a>]) -> Result<Self, Error> {
        for activity in flexible {
            for impact in activity.impacts {
                for start in [activity.window.earliest(), activity.window.latest()] {
                    timeline.validate(impact.shifted(start).ok_or(Error::Overflow)?)?;
                }
            }
        }

        let mut envelope = Self {
            timeline,
            members: Vec::new(),
            windows: Vec::new(),
            durations: Vec::new(),
            cut_budget: DEFAULT_CUT_BUDGET,
            precedences: Vec::new(),
            network: None,
        };
        envelope.extend(flexible.iter().map(|activity| Member {
            activity: activity.activity,
            window: activity.window,
            shape: Shape::Fixed(activity.impacts),
        }))?;
        Ok(envelope)
    }

    /// Adds activities whose durations are uncertain.
    ///
    /// The envelope then covers every start in each window combined with every duration in
    /// each duration range. Claims and rates that last as long as the activity are anchored at
    /// its end, see [`Uncertain`]. Such a member's extreme at a tick is found by evaluating its
    /// profile at the vertices of the (start, duration) box, see the `member` module.
    ///
    /// # Errors
    ///
    /// - [`Error::NegativeDuration`] if a duration range starts below zero.
    /// - [`Error::EmptyInterval`] if a claim or rate interval is inverted for some duration
    ///   (an empty interval at the shortest duration is allowed).
    /// - The other validation errors of [`Timeline::add`] for any impact at the corners of the
    ///   start window and the duration range.
    /// - The errors of [`with_precedences`](Self::with_precedences) if precedences were
    ///   given before.
    /// - [`Error::Overflow`] if a tick does not fit a [`Tick`].
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::envelope::{Anchor, Contingent, Envelope, Uncertain, Verdict};
    /// use resource_timeline::{ActivityId, Bounds, Impact, Timeline, Window};
    ///
    /// // A battery at 30 units; a heater draws 2 units per tick while it runs, 5 to 15 ticks.
    /// let battery = Timeline::depletable(0, 30, Bounds::new(0, 100)?, 8)?;
    /// let heat = [Uncertain::Rate { start: Anchor::Start(0), end: Anchor::End(0), per_tick: -2 }];
    /// let heater = [Contingent {
    ///     activity: ActivityId(1),
    ///     window: Window::new(0, 10)?,
    ///     duration: Window::new(5, 15)?,
    ///     impacts: &heat,
    /// }];
    ///
    /// let envelope = Envelope::new(&battery, &[])?.with_contingent(&heater)?;
    /// assert_eq!(envelope.lower_at(25)?, 0); // the longest run has drawn 30
    /// assert_eq!(envelope.upper_at(25)?, 20); // the shortest run has drawn 10
    /// assert_eq!(envelope.check()?, Verdict::Safe);
    ///
    /// // A 16-tick run would drain the battery: possible, not certain.
    /// let longer = [Contingent { duration: Window::new(5, 16)?, ..heater[0] }];
    /// let envelope = Envelope::new(&battery, &[])?.with_contingent(&longer)?;
    /// assert!(matches!(envelope.check()?, Verdict::Possible(_)));
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn with_contingent(mut self, contingent: &'a [Contingent<'a>]) -> Result<Self, Error> {
        for activity in contingent {
            self.validate_contingent(activity)?;
        }

        self.extend(contingent.iter().map(|activity| Member {
            activity: activity.activity,
            window: activity.window,
            shape: Shape::Uncertain {
                duration: activity.duration,
                impacts: activity.impacts,
            },
        }))?;
        self.couple()?;
        Ok(self)
    }

    /// Restricts the schedules to those that respect `precedences` between the activities.
    ///
    /// Start windows and contingent duration ranges are tightened by propagation, and the
    /// activities that take part in a precedence get a joint, exact extreme: by
    /// maximum-weight closure if all are stepwise, by a discretised minimum cut if some have
    /// rates or uncertain durations and their domains fit the cut budget; see the `precedence`
    /// module. Above the budget the latter keep separate extremes over their tightened ranges,
    /// which may add up to a level no single schedule reaches, so [`Verdict::Possible`] may
    /// then be a false alarm (never [`Verdict::Safe`] a false all-clear). The envelopes are
    /// never looser than without precedences. With the cut, every tick a coupled time point
    /// can take becomes an evaluation tick, so [`check`](Self::check) costs grow with those
    /// windows.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownActivity`] if a precedence names an activity that is not part of the
    ///   envelope. Add contingent activities with [`with_contingent`](Self::with_contingent)
    ///   first; precedences given earlier are re-applied when activities are added later.
    /// - [`Error::InconsistentPrecedences`] if no schedule satisfies the windows and the
    ///   precedences together.
    /// - [`Error::OutOfMemory`] if the network cannot be allocated.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::envelope::{Envelope, Flexible, Precedence, Verdict};
    /// use resource_timeline::{ActivityId, Bounds, Impact, Timeline, Window};
    ///
    /// // One transmitter; two downlinks of 10 ticks, each anywhere in [0, 20].
    /// let transmitter = Timeline::claimable(0, 1, 8)?;
    /// let downlink = [Impact::Claim { start: 0, end: 10, amount: 1 }];
    /// let window = Window::new(0, 20)?;
    /// let flexible = [
    ///     Flexible { activity: ActivityId(1), window, impacts: &downlink },
    ///     Flexible { activity: ActivityId(2), window, impacts: &downlink },
    /// ];
    ///
    /// // Independent windows let the downlinks overlap.
    /// let independent = Envelope::new(&transmitter, &flexible)?;
    /// assert!(matches!(independent.check()?, Verdict::Possible(_)));
    ///
    /// // "2 starts after 1 has ended" rules every overlap out.
    /// let ordered = independent.with_precedences(&[Precedence::after_end(
    ///     ActivityId(1),
    ///     ActivityId(2),
    ///     0,
    /// )])?;
    /// assert_eq!(ordered.check()?, Verdict::Safe);
    /// assert_eq!(ordered.window(ActivityId(1)), Some(Window::new(0, 10)?));
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn with_precedences(mut self, precedences: &[Precedence]) -> Result<Self, Error> {
        self.precedences
            .try_reserve(precedences.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.precedences.extend_from_slice(precedences);
        self.couple()?;
        Ok(self)
    }

    /// Sets the largest graph, in nodes, that the exact coupled extreme of precedence-linked
    /// activities with rates or uncertain durations may build per tick (one node per tick of
    /// each coupled start and end domain). Above it those activities get separate, sound
    /// extremes. The default is 4096.
    ///
    /// # Errors
    ///
    /// The errors of [`with_precedences`](Self::with_precedences), re-run with the new budget.
    pub fn with_cut_budget(mut self, nodes: usize) -> Result<Self, Error> {
        self.cut_budget = nodes;
        self.couple()?;
        Ok(self)
    }

    /// The duration range of the first activity with id `activity`, tightened by the
    /// precedences (`[0, 0]` for a flexible activity), or `None` if there is no such activity.
    #[must_use]
    pub fn duration(&self, activity: ActivityId) -> Option<Window> {
        self.members
            .iter()
            .position(|member| member.activity == activity)
            .map(|index| self.durations[index])
    }

    /// The start window of the first activity with id `activity`, tightened by the
    /// precedences, or `None` if there is no such activity.
    #[must_use]
    pub fn window(&self, activity: ActivityId) -> Option<Window> {
        self.members
            .iter()
            .position(|member| member.activity == activity)
            .map(|index| self.windows[index])
    }

    /// The lowest level any admitted schedule has at tick `t`.
    ///
    /// Costs `O(n + f k^2)` for `n` fixed impacts and `f` independent activities with `k`
    /// impacts each, plus a minimum cut over the pending events of coupled activities.
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if `t` precedes the timeline start.
    /// - [`Error::Overflow`] if the value does not fit a [`Level`].
    /// - [`Error::OutOfMemory`] if the event graph of coupled activities cannot be allocated.
    pub fn lower_at(&self, t: Tick) -> Result<Level, Error> {
        self.level_at(t, Extreme::Lower)
    }

    /// The highest level any admitted schedule has at tick `t`.
    ///
    /// # Errors
    ///
    /// As for [`lower_at`](Self::lower_at).
    pub fn upper_at(&self, t: Tick) -> Result<Level, Error> {
        self.level_at(t, Extreme::Upper)
    }

    /// The lowest level any admitted schedule reaches at any tick from `from` on.
    ///
    /// On a claimable timeline, `capacity - lowest_from(t)` is the most units the fixed and
    /// flexible activities can hold at once from `t` on, under every start and duration the
    /// windows admit: the worst-case need of an agent when a shared resource is split, see
    /// [`share`](crate::share). The lower envelope is concave between evaluation ticks (see
    /// [`check`](Self::check)), so its minimum lies at `from` or at an evaluation tick. The value
    /// is exact where the envelope is; above the cut budget of
    /// [`with_cut_budget`](Self::with_cut_budget) it is a sound lower bound, so a need computed
    /// from it may be over-estimated, never under-estimated.
    ///
    /// # Errors
    ///
    /// The errors of [`lower_at`](Self::lower_at), and [`Error::OutOfMemory`] if the
    /// evaluation ticks cannot be allocated.
    ///
    /// # Examples
    ///
    /// ```
    /// use resource_timeline::envelope::{Envelope, Flexible};
    /// use resource_timeline::{ActivityId, Impact, Timeline, Window};
    ///
    /// // Two uplinks of 4 bandwidth units each, anywhere in [0, 10], on a 10-unit link.
    /// let link = Timeline::claimable(0, 10, 8)?;
    /// let uplink = [Impact::Claim { start: 0, end: 5, amount: 4 }];
    /// let window = Window::new(0, 10)?;
    /// let flexible = [
    ///     Flexible { activity: ActivityId(1), window, impacts: &uplink },
    ///     Flexible { activity: ActivityId(2), window, impacts: &uplink },
    /// ];
    ///
    /// // In the worst case both overlap: 8 units held, 2 left.
    /// assert_eq!(Envelope::new(&link, &flexible)?.lowest_from(0)?, 2);
    /// # Ok::<(), resource_timeline::Error>(())
    /// ```
    pub fn lowest_from(&self, from: Tick) -> Result<Level, Error> {
        let mut lowest = self.lower_at(from)?;
        for t in self.evaluation_ticks()? {
            if t >= from {
                lowest = lowest.min(self.lower_at(t)?);
            }
        }

        Ok(lowest)
    }

    /// Checks the envelopes against the timeline's bounds and end-state targets.
    ///
    /// The envelopes are evaluated at every tick where their shape can change and at the tick
    /// before. Without rates they are constant in between, so the reported tick is the
    /// earliest one with a possible (or certain) violation. With rates the upper envelope is
    /// convex and the lower one concave between those ticks, so [`Verdict::Safe`] is still
    /// exact, but a reported tick may lie after the first one, and a certain violation that
    /// exists only strictly between evaluation ticks is reported as possible. A certain
    /// violation anywhere takes precedence over possible ones.
    ///
    /// # Errors
    ///
    /// - [`Error::Overflow`] if an envelope value does not fit a [`Level`].
    /// - [`Error::OutOfMemory`] if the evaluation ticks cannot be allocated.
    pub fn check(&self) -> Result<Verdict, Error> {
        let mut possible = None;
        for t in self.evaluation_ticks()? {
            let (lower, upper) = (self.lower_at(t)?, self.upper_at(t)?);
            for (min, max, target) in self.limits_at(t) {
                if let Some((level, bound)) = certain_breach(lower, upper, min, max, target) {
                    return Ok(Verdict::Certain(self.explain(t, level, bound)?));
                }

                if possible.is_none() {
                    if let Some((level, bound)) = possible_breach(lower, upper, min, max, target) {
                        possible = Some(self.explain(t, level, bound)?);
                    }
                }
            }
        }

        Ok(possible.map_or(Verdict::Safe, Verdict::Possible))
    }

    /// Appends members with their windows as given.
    fn extend(&mut self, members: impl ExactSizeIterator<Item = Member<'a>>) -> Result<(), Error> {
        let extra = members.len();
        self.members
            .try_reserve(extra)
            .map_err(|_| Error::OutOfMemory)?;
        self.windows
            .try_reserve(extra)
            .map_err(|_| Error::OutOfMemory)?;
        self.durations
            .try_reserve(extra)
            .map_err(|_| Error::OutOfMemory)?;
        for member in members {
            self.windows.push(member.window);
            self.durations.push(member.duration());
            self.members.push(member);
        }

        Ok(())
    }

    /// Recomputes the tightened windows and the network from the precedences.
    fn couple(&mut self) -> Result<(), Error> {
        if self.precedences.is_empty() {
            return Ok(());
        }

        let (tightened, network) =
            Network::propagate(&self.members, &self.precedences, self.cut_budget)?;
        self.windows = tightened.windows;
        self.durations = tightened.durations;
        self.network = Some(network);
        Ok(())
    }

    /// Checks a contingent activity's impacts at every corner of its start window and
    /// duration range.
    fn validate_contingent(&self, activity: &Contingent<'_>) -> Result<(), Error> {
        let duration = activity.duration;
        if duration.earliest() < 0 {
            return Err(Error::NegativeDuration {
                shortest: duration.earliest(),
            });
        }

        for impact in activity.impacts {
            for length in [duration.earliest(), duration.latest()] {
                let relative = impact.resolve(length).ok_or(Error::Overflow)?;
                let relative = contingent::widened(relative)?;
                for start in [activity.window.earliest(), activity.window.latest()] {
                    let shifted = relative.shifted(start).ok_or(Error::Overflow)?;
                    self.timeline.validate(shifted)?;
                }
            }
        }

        Ok(())
    }

    /// The limits in force at `t`: the bounds, then each target at `t`, as
    /// `(min, max, is_target)`.
    fn limits_at(&self, t: Tick) -> impl Iterator<Item = (Level, Level, bool)> + '_ {
        let bounds = self.timeline.bounds();
        let targets = self
            .timeline
            .targets()
            .iter()
            .filter(move |target| target.at == t)
            .map(|target| (target.min, Level::MAX, true));
        core::iter::once((bounds.min(), bounds.max(), false)).chain(targets)
    }

    /// The fixed level plus the extreme contributions of the members at `t`: coupled members
    /// jointly, the others one by one.
    fn level_at(&self, t: Tick, extreme: Extreme) -> Result<Level, Error> {
        let mut level = i128::from(self.timeline.level_at(t)?);
        for (index, member) in self.members.iter().enumerate() {
            if self
                .network
                .as_ref()
                .is_some_and(|network| network.is_coupled(index))
            {
                continue;
            }

            let (window, duration) = (self.windows[index], self.durations[index]);
            let contribution = member.extreme_at(window, duration, t, extreme)?;
            level = level.checked_add(contribution).ok_or(Error::Overflow)?;
        }

        if let Some(network) = &self.network {
            let coupled = network.coupled_extreme(&self.members, t, extreme)?;
            level = level.checked_add(coupled).ok_or(Error::Overflow)?;
        }

        Level::try_from(level).map_err(|_| Error::Overflow)
    }

    /// Every tick from the start on at which an envelope can change shape, and the tick
    /// before each, sorted and without duplicates.
    fn evaluation_ticks(&self) -> Result<Vec<Tick>, Error> {
        let start = self.timeline.start();
        let mut ticks = Vec::new();
        let mut push = |tick: i128| -> Result<(), Error> {
            for tick in [tick - 1, tick] {
                if let Ok(tick) = Tick::try_from(tick) {
                    if tick >= start {
                        ticks.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                        ticks.push(tick);
                    }
                }
            }

            Ok(())
        };

        push(i128::from(start))?;
        for tick in self.timeline.change_ticks() {
            push(i128::from(tick))?;
        }

        for ((member, window), duration) in
            self.members.iter().zip(&self.windows).zip(&self.durations)
        {
            for offset in member.offsets(*duration) {
                for start in [window.earliest(), window.latest()] {
                    push(offset + i128::from(start))?;
                }
            }
        }

        // The exact coupled cut over rates or durations may change shape at any tick a coupled
        // time point can take; its graph is within the node budget, so are these ranges.
        if let Some(network) = &self.network {
            for (lo, hi) in network.dense_ranges(&self.members) {
                for tick in lo..=hi {
                    push(tick)?;
                }
            }
        }

        ticks.sort_unstable();
        ticks.dedup();
        Ok(ticks)
    }

    /// The violation at `t` with the activities that can push towards `bound` there.
    fn explain(&self, t: Tick, level: Level, bound: Bound) -> Result<EnvelopeViolation, Error> {
        let towards = if bound.is_lower() {
            Extreme::Lower
        } else {
            Extreme::Upper
        };

        let mut contributors = Vec::new();
        let fixed = self.timeline.contributors_at(t, bound)?;
        contributors
            .try_reserve_exact(fixed.len() + self.members.len())
            .map_err(|_| Error::OutOfMemory)?;
        contributors.extend_from_slice(&fixed);
        let ranges = self.members.iter().zip(&self.windows).zip(&self.durations);
        for ((member, window), duration) in ranges {
            if bound.is_pushed_by(member.extreme_at(*window, *duration, t, towards)?) {
                contributors.push(member.activity);
            }
        }

        contributors.sort_unstable();
        contributors.dedup();
        Ok(EnvelopeViolation {
            at: t,
            level,
            bound,
            contributors,
        })
    }
}

/// The broken lower bound: [`Bound::BelowTarget`] for a target, [`Bound::BelowMin`] otherwise.
const fn lower_bound(min: Level, target: bool) -> Bound {
    if target {
        Bound::BelowTarget(min)
    } else {
        Bound::BelowMin(min)
    }
}

/// The level and bound of a violation every schedule has, if the envelopes `[lower, upper]`
/// lie entirely outside `[min, max]`.
const fn certain_breach(
    lower: Level,
    upper: Level,
    min: Level,
    max: Level,
    target: bool,
) -> Option<(Level, Bound)> {
    if upper < min {
        Some((upper, lower_bound(min, target)))
    } else if lower > max {
        Some((lower, Bound::AboveMax(max)))
    } else {
        None
    }
}

/// The level and bound of a violation some schedule may have, if the envelopes `[lower, upper]`
/// reach outside `[min, max]`.
const fn possible_breach(
    lower: Level,
    upper: Level,
    min: Level,
    max: Level,
    target: bool,
) -> Option<(Level, Bound)> {
    if lower < min {
        Some((lower, lower_bound(min, target)))
    } else if upper > max {
        Some((upper, Bound::AboveMax(max)))
    } else {
        None
    }
}
