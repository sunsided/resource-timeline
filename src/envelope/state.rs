//! State envelopes: requirements and conflicts over every schedule that start windows admit.
//!
//! A [`StateEnvelope`] answers for a [`StateTimeline`] what [`Envelope`](super::Envelope)
//! answers for a numeric one: given activities whose starts are only known to lie in windows,
//! can some schedule break a requirement or create contradicting sets, and does every schedule
//! do so at the same tick?
//!
//! # Method
//!
//! The windows are independent, so at a tick `t` each activity contributes one of a few
//! *options*: "its latest own set at or before `t` is the one at offset `o`", available for the
//! starts that put that set at or before `t` and the next own set after `t`, at ticks
//! `[lo, hi]`; or "none of its sets yet". The state at `t` is decided by the latest option
//! chosen. For a requirement of value `v` held by activity `A`:
//!
//! - some schedule breaks it at `t` if some option with a value other than `v` can be the
//!   latest: placed as late as possible, every other activity can stay at or before it;
//! - every schedule breaks it at `t` if `t` is covered for every start of `A` and no option
//!   with value `v` can be the unique latest.
//!
//! "Certain" is per requirement (and per pair of sets), as "certain" is per bound and tick
//! for numeric envelopes: two requirements that each fail in half of the schedules make every
//! schedule fail, yet neither is certain.
//!
//! `A`'s own sets move with its start, so its start is split into pieces by its own offsets;
//! within a piece the best start is its first (to stay early) or its last (to be latest). Two
//! sets of different values conflict in some schedule if their tick ranges meet, in every
//! schedule if both ranges are the same single tick. Both answers are exact for independent
//! windows.
//!
//! Every quantity compared is a window end plus an offset, or the tick plus a difference of two
//! offsets of one activity, so the verdicts can only change near ticks where the two meet.
//! Requirements are evaluated at those candidate ticks only, `O(f^2 k^3)` of them for `f`
//! activities with `k` ticks each, at `O(m^2)` per tick for `m` sets; the cost does not grow
//! with the window lengths or with requirements that last forever.

use alloc::vec::Vec;

use crate::storage::{Heap, Storage};
use crate::{ActivityId, Error, StateImpact, StateTimeline, StateValue, Tick, Window};

/// An activity with state impacts whose start is only known to lie in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlexibleState<'a> {
    /// The activity, reported back in explanations.
    pub activity: ActivityId,
    /// The allowed starts.
    pub window: Window,
    /// The sets and requirements of the activity, with ticks relative to its start.
    pub impacts: &'a [StateImpact],
}

/// The outcome of a state envelope check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StateVerdict {
    /// Every schedule the windows admit has no state violation.
    Safe,
    /// Some schedule has a violation at the reported tick, which is the earliest such tick
    /// over all schedules. No single requirement and no single pair of sets is broken at one
    /// tick in every schedule (although every schedule may still be violated, by different
    /// requirements or at different ticks).
    Possible(StateEnvelopeViolation),
    /// One requirement, or one pair of contradicting sets, is broken at the reported tick in
    /// every schedule; the reported tick is the earliest such tick.
    Certain(StateEnvelopeViolation),
}

/// A state violation some or every schedule has at a tick.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StateEnvelopeViolation {
    /// A requirement sees another (or an ambiguous) value.
    Mismatch {
        /// The tick.
        at: Tick,
        /// The required value.
        required: StateValue,
        /// The activity that holds the requirement; the smallest id if several fail at `at`.
        requirer: ActivityId,
    },
    /// Sets of different values may coincide (or always coincide) at the tick.
    Conflict {
        /// The tick.
        at: Tick,
        /// Every activity with a set that can lie at `at`. Sorted, without duplicates.
        setters: Vec<ActivityId>,
    },
}

impl StateEnvelopeViolation {
    /// The tick of the violation.
    #[must_use]
    pub const fn at(&self) -> Tick {
        match *self {
            Self::Mismatch { at, .. } | Self::Conflict { at, .. } => at,
        }
    }
}

/// The state envelope of a state timeline with additional flexible activities.
///
/// The timeline's own impacts count as fixed.
///
/// # Examples
///
/// ```
/// use resource_timeline::envelope::{FlexibleState, StateEnvelope, StateVerdict};
/// use resource_timeline::{ActivityId, StateImpact, StateTimeline, Window};
///
/// const COLD: u32 = 0;
/// const WARM: u32 = 1;
///
/// // The heater warms the instrument at its start; the analysis needs it warm for 5 ticks.
/// let instrument = StateTimeline::new(0, COLD, 8);
/// let heat = [StateImpact::Set { at: 0, value: WARM }];
/// let analyze = [StateImpact::Require { start: 0, end: 5, value: WARM }];
/// let flexible = [
///     FlexibleState { activity: ActivityId(1), window: Window::new(0, 10)?, impacts: &heat },
///     FlexibleState { activity: ActivityId(2), window: Window::new(10, 20)?, impacts: &analyze },
/// ];
/// assert_eq!(StateEnvelope::new(&instrument, &flexible)?.check()?, StateVerdict::Safe);
///
/// // If the analysis may start first, some schedule finds the instrument cold.
/// let early = [flexible[0], FlexibleState { window: Window::new(5, 20)?, ..flexible[1] }];
/// assert!(matches!(
///     StateEnvelope::new(&instrument, &early)?.check()?,
///     StateVerdict::Possible(_)
/// ));
/// # Ok::<(), resource_timeline::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct StateEnvelope<'a, S: Storage = Heap> {
    /// The timeline with the fixed impacts.
    timeline: &'a StateTimeline<S>,
    /// The activities: the fixed impacts first (window `[0, 0]`, absolute ticks), then the
    /// flexible ones.
    entities: Vec<Entity>,
}

/// A stored set: its offset, value and activity.
type SetEntry = (Tick, StateValue, ActivityId);

/// One independent part of the envelope: the fixed impacts, or one flexible activity.
#[derive(Debug, Clone)]
struct Entity {
    /// The allowed starts.
    window: Window,
    /// The sets as `(offset, value, activity)`, sorted by offset.
    sets: Vec<SetEntry>,
    /// The requirements as `(activity, start, end, value)`, ticks relative to the start.
    requires: Vec<(ActivityId, Tick, Tick, StateValue)>,
}

/// "The latest own set at or before `t` is the one at this offset": the ticks it can take and
/// the values set there.
#[derive(Debug, Clone, Copy)]
struct Opt<'a> {
    /// The earliest tick of the set for the starts that make it the latest own set.
    lo: i128,
    /// The latest such tick.
    hi: i128,
    /// The sets at this offset (one entry per set; several values mean a conflict).
    sets: &'a [SetEntry],
}

impl Opt<'_> {
    /// Whether some set here has a value other than `value`.
    fn differs_from(&self, value: StateValue) -> bool {
        self.sets.iter().any(|&(_, other, _)| other != value)
    }

    /// Whether every set here has `value`.
    fn is_only(&self, value: StateValue) -> bool {
        !self.differs_from(value)
    }
}

/// The options of one entity at a tick.
#[derive(Debug, Clone, Default)]
struct Options<'a> {
    /// Whether some start leaves every own set after the tick.
    none: bool,
    /// The available latest-set options.
    options: Vec<Opt<'a>>,
}

impl Options<'_> {
    /// Whether the entity can stay at or before tick `x` (or have no set yet).
    fn can_be_at_most(&self, x: i128) -> bool {
        self.none || self.options.iter().any(|option| option.lo <= x)
    }

    /// Whether the entity can stay strictly before `x`, or tie at `x` with sets of `value` only.
    fn can_yield_to(&self, x: i128, value: StateValue) -> bool {
        self.none
            || self.options.iter().any(|option| {
                option.lo < x || (option.is_only(value) && option.lo <= x && x <= option.hi)
            })
    }
}

/// A requirement under evaluation: its entity, relative interval and value.
#[derive(Debug, Clone, Copy)]
struct Need {
    /// The entity holding the requirement.
    entity: usize,
    /// The activity holding it.
    activity: ActivityId,
    /// The first required tick, relative to the start.
    start: Tick,
    /// The first tick after the requirement, relative to the start.
    end: Tick,
    /// The required value.
    value: StateValue,
}

/// One piece of a requirer's starts with the same latest own set at the tick.
#[derive(Debug, Clone, Copy)]
struct Piece<'a> {
    /// The first start of the piece.
    first: i128,
    /// The last start of the piece.
    last: i128,
    /// The latest own set (offset and sets there), `None` if no own set is due yet.
    own: Option<(Tick, &'a [SetEntry])>,
}

impl<'a, S: Storage> StateEnvelope<'a, S> {
    /// Combines the fixed impacts of `timeline` with the `flexible` activities.
    ///
    /// # Errors
    ///
    /// - The validation errors of [`StateTimeline::add`] for any flexible impact at the
    ///   earliest or latest start of its window.
    /// - [`Error::Overflow`] if a shifted tick does not fit a [`Tick`].
    /// - [`Error::OutOfMemory`] if the activity lists cannot be allocated.
    pub fn new(
        timeline: &'a StateTimeline<S>,
        flexible: &'a [FlexibleState<'a>],
    ) -> Result<Self, Error> {
        for activity in flexible {
            for impact in activity.impacts {
                for start in [activity.window.earliest(), activity.window.latest()] {
                    timeline.validate(impact.shifted(start).ok_or(Error::Overflow)?)?;
                }
            }
        }

        let mut entities = Vec::new();
        entities
            .try_reserve_exact(flexible.len() + 1)
            .map_err(|_| Error::OutOfMemory)?;
        let fixed = timeline.impacts().iter().copied();
        entities.push(entity(Window::at(0), fixed)?);
        for activity in flexible {
            let impacts = activity
                .impacts
                .iter()
                .map(|&impact| (activity.activity, impact));
            entities.push(entity(activity.window, impacts)?);
        }

        Ok(Self { timeline, entities })
    }

    /// Checks every requirement and every pair of sets over all admitted schedules.
    ///
    /// A certain violation anywhere takes precedence over possible ones. At the same tick a
    /// conflict is reported before a mismatch, and among mismatches the smallest requirer.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the working memory cannot be allocated.
    pub fn check(&self) -> Result<StateVerdict, Error> {
        let (possible_conflict, certain_conflict) = self.conflicts()?;
        let (possible_mismatch, certain_mismatch) = self.mismatches()?;

        let certain = earliest(certain_conflict, certain_mismatch);
        if let Some(violation) = certain {
            return Ok(StateVerdict::Certain(violation));
        }

        Ok(earliest(possible_conflict, possible_mismatch)
            .map_or(StateVerdict::Safe, StateVerdict::Possible))
    }

    /// The timeline the envelope was built on.
    #[must_use]
    pub const fn timeline(&self) -> &StateTimeline<S> {
        self.timeline
    }

    /// The earliest possible and the earliest certain conflict between two sets.
    fn conflicts(
        &self,
    ) -> Result<
        (
            Option<StateEnvelopeViolation>,
            Option<StateEnvelopeViolation>,
        ),
        Error,
    > {
        let mut possible: Option<Tick> = None;
        let mut certain: Option<Tick> = None;
        let sets = self.set_ranges();
        for (index, a) in sets.clone().enumerate() {
            for b in sets.clone().skip(index + 1) {
                if a.value == b.value {
                    continue;
                }

                let coupled = a.entity == b.entity;
                if coupled && a.offset != b.offset {
                    continue;
                }

                let meet = if coupled { Some(a.lo) } else { a.meet(&b) };
                if let Some(at) = meet {
                    possible = Some(possible.map_or(at, |p| p.min(at)));
                }

                if a.lo == a.hi && b.lo == b.hi && a.lo == b.lo {
                    certain = Some(certain.map_or(a.lo, |c| c.min(a.lo)));
                }
            }
        }

        let explain = |at: Option<Tick>| -> Result<Option<StateEnvelopeViolation>, Error> {
            at.map(|at| self.conflict_at(at)).transpose()
        };
        Ok((explain(possible)?, explain(certain)?))
    }

    /// Every set with the ticks it can take.
    fn set_ranges(&self) -> impl Iterator<Item = SetRange> + Clone + '_ {
        self.entities.iter().enumerate().flat_map(|(entity, e)| {
            e.sets
                .iter()
                .map(move |&(offset, value, activity)| SetRange {
                    entity,
                    offset,
                    value,
                    activity,
                    lo: e.window.earliest() + offset,
                    hi: e.window.latest() + offset,
                })
        })
    }

    /// A conflict at `at`, with every activity whose set can lie there.
    fn conflict_at(&self, at: Tick) -> Result<StateEnvelopeViolation, Error> {
        let mut setters = Vec::new();
        for set in self.set_ranges().filter(|set| set.lo <= at && at <= set.hi) {
            setters.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            setters.push(set.activity);
        }

        setters.sort_unstable();
        setters.dedup();
        Ok(StateEnvelopeViolation::Conflict { at, setters })
    }

    /// The earliest possible and the earliest certain mismatch of any requirement.
    fn mismatches(
        &self,
    ) -> Result<
        (
            Option<StateEnvelopeViolation>,
            Option<StateEnvelopeViolation>,
        ),
        Error,
    > {
        let needs: Vec<Need> = self.needs().collect();
        let mut possible: Option<(Tick, ActivityId, StateValue)> = None;
        let mut certain: Option<(Tick, ActivityId, StateValue)> = None;
        let Some((from, to)) = self.coverage(&needs) else {
            return Ok((None, None));
        };

        let settled = self.settled();
        for t in self.candidate_ticks(&needs, from, to)? {
            if possible.is_some_and(|p| p.0 < t) && certain.is_some() {
                break;
            }

            let options = self.options_at(t)?;
            let relevant =
                |need: &&Need| self.may_cover(need, t) && t <= self.last_tick(need, settled);
            for need in needs.iter().filter(relevant) {
                let key = (t, need.activity, need.value);
                if possible.is_none_or(|p| key < p) && self.possibly_broken(need, t, &options)? {
                    possible = Some(key);
                }

                if certain.is_none_or(|c| key < c) && self.certainly_broken(need, t, &options)? {
                    certain = Some(key);
                }
            }
        }

        let explain = |found: Option<(Tick, ActivityId, StateValue)>| {
            found.map(
                |(at, requirer, required)| StateEnvelopeViolation::Mismatch {
                    at,
                    required,
                    requirer,
                },
            )
        };
        Ok((explain(possible), explain(certain)))
    }

    /// Every requirement of every entity.
    fn needs(&self) -> impl Iterator<Item = Need> + '_ {
        self.entities.iter().enumerate().flat_map(|(entity, e)| {
            e.requires
                .iter()
                .map(move |&(activity, start, end, value)| Need {
                    entity,
                    activity,
                    start,
                    end,
                    value,
                })
        })
    }

    /// The ticks worth evaluating for any requirement, `None` without requirements.
    fn coverage(&self, needs: &[Need]) -> Option<(Tick, Tick)> {
        let settled = self.settled();
        let from = needs
            .iter()
            .map(|need| self.starts(need.entity).earliest() + need.start);
        let to = needs.iter().map(|need| self.last_tick(need, settled));
        Some((from.min()?, to.max()?))
    }

    /// The ticks in `[from, to]` at which a mismatch verdict can start to hold, sorted.
    ///
    /// Every quantity the predicates compare is a constant (a window end plus an offset) or the
    /// tick plus a constant (a difference of two offsets of one activity), so a comparison can
    /// only flip where the tick equals such a constant minus such a difference. Taking these
    /// points (and two ticks either side, for strict comparisons and the `+ 1` of half-open
    /// ends) plus each requirement's first covered tick finds the start of every interval on
    /// which a predicate holds, without walking long windows tick by tick.
    fn candidate_ticks(&self, needs: &[Need], from: Tick, to: Tick) -> Result<Vec<Tick>, Error> {
        let mut constants: Vec<i128> = Vec::new();
        let mut shifts: Vec<i128> = Vec::new();
        for (index, entity) in self.entities.iter().enumerate() {
            let mut offsets: Vec<i128> = Vec::new();
            offsets.push(0);
            offsets.extend(entity.sets.iter().map(|&(offset, _, _)| i128::from(offset)));
            for need in needs.iter().filter(|need| need.entity == index) {
                offsets.extend([i128::from(need.start), i128::from(need.end)]);
            }

            offsets.sort_unstable();
            offsets.dedup();
            let (lo, hi) = (
                i128::from(entity.window.earliest()),
                i128::from(entity.window.latest()),
            );
            constants.extend(
                offsets
                    .iter()
                    .flat_map(|&offset| [lo + offset, hi + offset]),
            );
            for &p in &offsets {
                shifts.extend(offsets.iter().map(|&q| p - q));
            }
        }

        constants.sort_unstable();
        constants.dedup();
        shifts.sort_unstable();
        shifts.dedup();
        let (from_wide, to_wide) = (i128::from(from), i128::from(to));
        let mut ticks: Vec<Tick> = Vec::new();
        let mut push = |tick: i128| -> Result<(), Error> {
            if from_wide <= tick && tick <= to_wide {
                ticks.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                ticks.push(Tick::try_from(tick).map_err(|_| Error::Overflow)?);
            }

            Ok(())
        };
        for need in needs {
            push(i128::from(self.starts(need.entity).earliest()) + i128::from(need.start))?;
        }

        for &constant in &constants {
            for &shift in &shifts {
                for nudge in -2..=2 {
                    push(constant - shift + nudge)?;
                }
            }
        }

        ticks.sort_unstable();
        ticks.dedup();
        Ok(ticks)
    }

    /// The latest tick any set can take, `None` without sets. After it every activity's
    /// options are constant.
    fn settled(&self) -> Option<Tick> {
        self.set_ranges().map(|set| set.hi).max()
    }

    /// The last tick worth evaluating for `need`.
    ///
    /// Past both `settled` (the latest set tick) and the tick where every start of the
    /// requirer covers the requirement, the other activities' options are constant, the
    /// requirer's latest own set is fixed and its earliest covering start only grows. A
    /// possible mismatch can then only disappear and a certain one stays as it is, so one tick
    /// past both decides the rest of the (possibly unbounded) requirement.
    fn last_tick(&self, need: &Need, settled: Option<Tick>) -> Tick {
        let window = self.starts(need.entity);
        let covered_end = i128::from(window.latest()) + i128::from(need.end) - 1;
        let fully_covered = i128::from(window.latest()) + i128::from(need.start);
        let settled = settled.map_or(i128::MIN, i128::from);
        let last = covered_end.min(settled.max(fully_covered) + 1);
        // The requirement end was validated to fit a tick at the latest start.
        Tick::try_from(last).unwrap_or(Tick::MAX)
    }

    /// The start window of entity `index`.
    fn starts(&self, index: usize) -> Window {
        self.entities[index].window
    }

    /// Whether some start of the requirer makes the requirement cover `t`.
    fn may_cover(&self, need: &Need, t: Tick) -> bool {
        self.covering_starts(need, t).is_some()
    }

    /// The starts of the requirer for which the requirement covers `t`.
    fn covering_starts(&self, need: &Need, t: Tick) -> Option<(i128, i128)> {
        let window = self.starts(need.entity);
        let t = i128::from(t);
        let first = i128::from(window.earliest()).max(t - i128::from(need.end) + 1);
        let last = i128::from(window.latest()).min(t - i128::from(need.start));
        (first <= last).then_some((first, last))
    }

    /// The options of every entity at tick `t`.
    fn options_at(&self, t: Tick) -> Result<Vec<Options<'_>>, Error> {
        let mut all = Vec::new();
        all.try_reserve_exact(self.entities.len())
            .map_err(|_| Error::OutOfMemory)?;
        for entity in &self.entities {
            all.push(entity.options_at(t)?);
        }

        Ok(all)
    }

    /// Whether some schedule breaks `need` at `t`.
    fn possibly_broken(
        &self,
        need: &Need,
        t: Tick,
        options: &[Options<'_>],
    ) -> Result<bool, Error> {
        let Some(starts) = self.covering_starts(need, t) else {
            return Ok(false);
        };

        let others = |x: i128, skip: Option<usize>| {
            options.iter().enumerate().all(|(index, other)| {
                index == need.entity || Some(index) == skip || other.can_be_at_most(x)
            })
        };
        for piece in self.entities[need.entity].pieces(t, starts)? {
            let own_tick = piece
                .own
                .map(|(offset, _)| piece.first + i128::from(offset));
            if let Some((offset, sets)) = piece.own {
                let latest = piece.last + i128::from(offset);
                let own = Opt {
                    lo: latest,
                    hi: latest,
                    sets,
                };
                if own.differs_from(need.value) && others(latest, None) {
                    return Ok(true);
                }
            }

            for (index, other) in options.iter().enumerate() {
                if index == need.entity {
                    continue;
                }

                for option in other.options.iter().filter(|o| o.differs_from(need.value)) {
                    let own_fits = own_tick.is_none_or(|own| own <= option.hi);
                    if own_fits && others(option.hi, Some(index)) {
                        return Ok(true);
                    }
                }
            }

            let all_none = options
                .iter()
                .enumerate()
                .all(|(index, other)| index == need.entity || other.none);
            if piece.own.is_none() && all_none && self.timeline.initial() != need.value {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Whether every schedule breaks `need` at `t`.
    fn certainly_broken(
        &self,
        need: &Need,
        t: Tick,
        options: &[Options<'_>],
    ) -> Result<bool, Error> {
        let window = self.starts(need.entity);
        let starts = (i128::from(window.earliest()), i128::from(window.latest()));
        if self.covering_starts(need, t) != Some(starts) {
            return Ok(false);
        }

        let value = need.value;
        let others = |x: i128, skip: Option<usize>| {
            options.iter().enumerate().all(|(index, other)| {
                index == need.entity || Some(index) == skip || other.can_yield_to(x, value)
            })
        };
        for piece in self.entities[need.entity].pieces(t, starts)? {
            if let Some((offset, sets)) = piece.own {
                let latest = piece.last + i128::from(offset);
                let own = Opt {
                    lo: latest,
                    hi: latest,
                    sets,
                };
                if own.is_only(value) && others(latest, None) {
                    return Ok(false);
                }
            }

            for (index, other) in options.iter().enumerate() {
                if index == need.entity {
                    continue;
                }

                for option in other.options.iter().filter(|o| o.is_only(value)) {
                    if piece_yields(&piece, option.hi, value) && others(option.hi, Some(index)) {
                        return Ok(false);
                    }
                }
            }

            let all_none = options
                .iter()
                .enumerate()
                .all(|(index, other)| index == need.entity || other.none);
            if piece.own.is_none() && all_none && self.timeline.initial() == value {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

/// Whether the requirer's own latest set in `piece` can stay before `x`, or tie at `x` with
/// sets of `value` only.
fn piece_yields(piece: &Piece<'_>, x: i128, value: StateValue) -> bool {
    let Some((offset, sets)) = piece.own else {
        return true;
    };

    let offset = i128::from(offset);
    let before = piece.first + offset < x;
    let tie = (piece.first..=piece.last).contains(&(x - offset))
        && sets.iter().all(|&(_, other, _)| other == value);
    before || tie
}

/// A set with the ticks it can take.
#[derive(Debug, Clone, Copy)]
struct SetRange {
    /// The entity it belongs to.
    entity: usize,
    /// Its tick relative to the entity's start.
    offset: Tick,
    /// The value it sets.
    value: StateValue,
    /// The activity that sets it.
    activity: ActivityId,
    /// Its earliest tick.
    lo: Tick,
    /// Its latest tick.
    hi: Tick,
}

impl SetRange {
    /// The earliest tick both sets can take at once, if their ranges meet.
    fn meet(&self, other: &Self) -> Option<Tick> {
        let lo = self.lo.max(other.lo);
        (lo <= self.hi.min(other.hi)).then_some(lo)
    }
}

impl Entity {
    /// The options at tick `t`.
    fn options_at(&self, t: Tick) -> Result<Options<'_>, Error> {
        let t = i128::from(t);
        let (lo, hi) = (
            i128::from(self.window.earliest()),
            i128::from(self.window.latest()),
        );
        let mut options = Vec::new();
        let groups = self.groups();
        let none = groups
            .first()
            .is_none_or(|&(offset, _)| hi + i128::from(offset) > t);
        for (index, &(offset, sets)) in groups.iter().enumerate() {
            let offset = i128::from(offset);
            let after = groups
                .get(index + 1)
                .map_or(lo, |&(next, _)| t - i128::from(next) + 1);
            let first = lo.max(after);
            let last = hi.min(t - offset);
            if first <= last {
                options.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                options.push(Opt {
                    lo: first + offset,
                    hi: last + offset,
                    sets,
                });
            }
        }

        Ok(Options { none, options })
    }

    /// The sets grouped by offset, ascending.
    fn groups(&self) -> Vec<(Tick, &[SetEntry])> {
        let mut groups = Vec::new();
        let mut rest = self.sets.as_slice();
        while let Some(&(offset, _, _)) = rest.first() {
            let count = rest.iter().take_while(|set| set.0 == offset).count();
            let (now, later) = rest.split_at(count);
            groups.push((offset, now));
            rest = later;
        }

        groups
    }

    /// The pieces of the starts `[first, last]` with the same latest own set at `t`.
    fn pieces(&self, t: Tick, (first, last): (i128, i128)) -> Result<Vec<Piece<'_>>, Error> {
        let t = i128::from(t);
        let groups = self.groups();
        let mut pieces = Vec::new();
        pieces
            .try_reserve(groups.len() + 1)
            .map_err(|_| Error::OutOfMemory)?;
        // No own set yet: t - s < first offset, that is s > t - first offset.
        let none_from = groups
            .first()
            .map_or(first, |&(offset, _)| first.max(t - i128::from(offset) + 1));
        if none_from <= last {
            pieces.push(Piece {
                first: none_from,
                last,
                own: None,
            });
        }

        for (index, &(offset, sets)) in groups.iter().enumerate() {
            let from = groups
                .get(index + 1)
                .map_or(first, |&(next, _)| first.max(t - i128::from(next) + 1));
            let to = last.min(t - i128::from(offset));
            if from <= to {
                pieces.push(Piece {
                    first: from,
                    last: to,
                    own: Some((offset, sets)),
                });
            }
        }

        Ok(pieces)
    }
}

/// The entity of the given impacts with start window `window`.
fn entity(
    window: Window,
    impacts: impl Iterator<Item = (ActivityId, StateImpact)>,
) -> Result<Entity, Error> {
    let mut entity = Entity {
        window,
        sets: Vec::new(),
        requires: Vec::new(),
    };
    for (activity, impact) in impacts {
        match impact {
            StateImpact::Set { at, value } => {
                entity.sets.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                entity.sets.push((at, value, activity));
            }
            StateImpact::Require { start, end, value } => {
                entity
                    .requires
                    .try_reserve(1)
                    .map_err(|_| Error::OutOfMemory)?;
                entity.requires.push((activity, start, end, value));
            }
        }
    }

    entity.sets.sort_unstable();
    Ok(entity)
}

/// The earlier of a conflict and a mismatch; a conflict wins at the same tick.
fn earliest(
    conflict: Option<StateEnvelopeViolation>,
    mismatch: Option<StateEnvelopeViolation>,
) -> Option<StateEnvelopeViolation> {
    match (conflict, mismatch) {
        (Some(conflict), Some(mismatch)) if mismatch.at() < conflict.at() => Some(mismatch),
        (Some(conflict), _) => Some(conflict),
        (None, mismatch) => mismatch,
    }
}
