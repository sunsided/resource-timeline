//! Level envelopes for activities whose start lies in a window.
//!
//! A flexible schedule fixes each activity's start only to a [`Window`], for example the
//! windows a simple temporal network allows. The envelope bounds the level over *every*
//! schedule the windows admit: at each tick, the lower envelope is the lowest level any
//! schedule can reach there and the upper envelope the highest. If the lower envelope never
//! falls below the minimum and the upper never exceeds the maximum, every schedule is safe and
//! the dispatcher may pick any start in the windows without re-checking resources.
//!
//! This is the resource-envelope idea from Muscettola's work on flexible schedules, in its
//! simplest form: the windows are treated as independent. The activities' starts may then be
//! chosen separately, so the extreme level at a tick is the sum of each activity's own extreme
//! contribution there, and the envelope computed here is exactly the tightest one for
//! independent windows. Precedence constraints between activities (`b` starts after `a` ends)
//! rule out some combinations; ignoring them only adds schedules, so the envelope stays sound
//! but may be loose. A precedence-aware envelope is not implemented. Durations are fixed. On a
//! depletable timeline, a hold of uncertain length can be modelled as a step pair: a negative
//! step owned by a start part and a positive step owned by an end part, each with its own
//! window. This does not work for claims on a claimable timeline (steps are rejected there) or
//! for rates, whose total depends on the duration.
//!
//! # Bridge to `temporal-control`
//!
//! The crates do not depend on each other. Integration code builds the windows from
//! `temporal-control`: before execution from the minimal network, `MinimalNetwork::lower` and
//! `upper` between the origin and each activity's start time point; during execution from
//! `Dispatcher::window`, which narrows as events are observed. Unbounded sides must be clamped
//! to a mission horizon. Re-running [`Envelope::check`] on the narrowed windows after each
//! observation keeps the resource guarantee in step with the temporal one.
//!
//! ```mermaid
//! graph LR
//!     stn["temporal-control: start windows"] --> flexible["Flexible activities"]
//!     fixed["Timeline: fixed impacts"] --> envelope["Envelope: lower and upper level per tick"]
//!     flexible --> envelope
//!     envelope --> verdict["Verdict: Safe, Possible or Certain"]
//! ```

use alloc::vec::Vec;

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
    /// Some schedule may leave the bounds at the reported tick, but not every schedule leaves
    /// them at that same tick. This does not imply that a safe schedule exists: every schedule
    /// may still violate, each at a different tick.
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
#[derive(Debug, Clone, Copy)]
pub struct Envelope<'a> {
    /// The timeline with the fixed impacts, start, initial level and bounds.
    timeline: &'a Timeline,
    /// The flexible activities, validated against the timeline.
    flexible: &'a [Flexible<'a>],
}

/// Which extreme of the level an evaluation asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extreme {
    /// The lowest level over all admitted schedules.
    Lower,
    /// The highest level over all admitted schedules.
    Upper,
}

impl<'a> Envelope<'a> {
    /// Combines the fixed impacts of `timeline` with the `flexible` activities.
    ///
    /// # Errors
    ///
    /// - The validation errors of [`Timeline::add`] for any flexible impact at the earliest or
    ///   latest start of its window, including [`Error::BeforeStart`] when the earliest start
    ///   would place an impact before the timeline start.
    /// - [`Error::Overflow`] if a shifted tick does not fit a [`Tick`].
    pub fn new(timeline: &'a Timeline, flexible: &'a [Flexible<'a>]) -> Result<Self, Error> {
        for activity in flexible {
            for impact in activity.impacts {
                for start in [activity.window.earliest(), activity.window.latest()] {
                    timeline.validate(impact.shifted(start).ok_or(Error::Overflow)?)?;
                }
            }
        }

        Ok(Self { timeline, flexible })
    }

    /// The lowest level any admitted schedule has at tick `t`.
    ///
    /// Costs `O(n + f k^2)` for `n` fixed impacts and `f` flexible activities with `k` impacts
    /// each.
    ///
    /// # Errors
    ///
    /// - [`Error::BeforeStart`] if `t` precedes the timeline start.
    /// - [`Error::Overflow`] if the value does not fit a [`Level`].
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

    /// Checks the envelopes against the timeline's bounds.
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
        let bounds = self.timeline.bounds();
        let mut possible = None;
        for t in self.evaluation_ticks()? {
            let (lower, upper) = (self.lower_at(t)?, self.upper_at(t)?);
            if upper < bounds.min() || lower > bounds.max() {
                let (level, bound) = if upper < bounds.min() {
                    (upper, Bound::BelowMin(bounds.min()))
                } else {
                    (lower, Bound::AboveMax(bounds.max()))
                };
                return Ok(Verdict::Certain(self.explain(t, level, bound)?));
            }

            if possible.is_none() && (lower < bounds.min() || upper > bounds.max()) {
                let (level, bound) = if lower < bounds.min() {
                    (lower, Bound::BelowMin(bounds.min()))
                } else {
                    (upper, Bound::AboveMax(bounds.max()))
                };
                possible = Some(self.explain(t, level, bound)?);
            }
        }

        Ok(possible.map_or(Verdict::Safe, Verdict::Possible))
    }

    /// The fixed level plus each flexible activity's extreme contribution at `t`.
    fn level_at(&self, t: Tick, extreme: Extreme) -> Result<Level, Error> {
        let mut level = i128::from(self.timeline.level_at(t)?);
        for activity in self.flexible {
            let contribution = extreme_contribution(activity, t, extreme)?;
            level = level.checked_add(contribution).ok_or(Error::Overflow)?;
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

        for activity in self.flexible {
            for offset in offsets(activity.impacts) {
                for start in [activity.window.earliest(), activity.window.latest()] {
                    push(i128::from(offset) + i128::from(start))?;
                }
            }
        }

        ticks.sort_unstable();
        ticks.dedup();
        Ok(ticks)
    }

    /// The violation at `t` with the activities that can push towards `bound` there.
    fn explain(&self, t: Tick, level: Level, bound: Bound) -> Result<EnvelopeViolation, Error> {
        let pushes = |effect: i128| match bound {
            Bound::BelowMin(_) => effect < 0,
            Bound::AboveMax(_) => effect > 0,
        };
        let towards = match bound {
            Bound::BelowMin(_) => Extreme::Lower,
            Bound::AboveMax(_) => Extreme::Upper,
        };

        let mut contributors = Vec::new();
        let fixed = self.timeline.contributors_at(t, bound)?;
        contributors
            .try_reserve_exact(fixed.len() + self.flexible.len())
            .map_err(|_| Error::OutOfMemory)?;
        contributors.extend(fixed);
        for activity in self.flexible {
            if pushes(extreme_contribution(activity, t, towards)?) {
                contributors.push(activity.activity);
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

/// The extreme contribution of `activity` at tick `t` over all starts in its window.
///
/// With start `s`, the contribution at `t` is the activity's own profile at the relative time
/// `u = t - s`, so the extreme over `s` in `[earliest, latest]` is the extreme of the profile
/// over `u` in `[t - latest, t - earliest]`. The profile is linear between its offsets, so the
/// extreme lies at a window end or at an offset or the tick before one.
fn extreme_contribution(activity: &Flexible<'_>, t: Tick, extreme: Extreme) -> Result<i128, Error> {
    let first = i128::from(t) - i128::from(activity.window.latest());
    let last = i128::from(t) - i128::from(activity.window.earliest());
    let inner = offsets(activity.impacts).flat_map(|offset| {
        let offset = i128::from(offset);
        [offset - 1, offset]
    });

    let mut best: Option<i128> = None;
    for u in [first, last].into_iter().chain(inner) {
        if u < first || u > last {
            continue;
        }

        let value = profile_at(activity.impacts, u)?;
        best = Some(match (best, extreme) {
            (None, _) => value,
            (Some(best), Extreme::Lower) => best.min(value),
            (Some(best), Extreme::Upper) => best.max(value),
        });
    }

    Ok(best.unwrap_or(0))
}

/// The summed contribution of `impacts` at relative time `u`.
///
/// `u` may lie outside the [`Tick`] range. Below it nothing has happened yet; above it the
/// value is the one at [`Tick::MAX`], because every impact ends by then.
fn profile_at(impacts: &[Impact], u: i128) -> Result<i128, Error> {
    if u < i128::from(Tick::MIN) {
        return Ok(0);
    }

    let u = Tick::try_from(u.min(i128::from(Tick::MAX))).map_err(|_| Error::Overflow)?;
    impacts.iter().try_fold(0_i128, |sum, impact| {
        let contribution = impact.contribution_at(u).ok_or(Error::Overflow)?;
        sum.checked_add(contribution).ok_or(Error::Overflow)
    })
}

/// The relative ticks at which `impacts` change the level or its slope.
fn offsets(impacts: &[Impact]) -> impl Iterator<Item = Tick> + '_ {
    impacts
        .iter()
        .flat_map(|impact| [impact.first_tick(), impact.last_tick()])
}
