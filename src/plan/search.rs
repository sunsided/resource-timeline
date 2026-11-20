//! Tentative checks and the earliest-start search of a plan.
//!
//! # Why the candidate starts suffice without rates
//!
//! Without rates every level is a step function. Whether an activity starting at `s` fits
//! depends only on how each of its shifted ticks `s + o` compares (less, equal, greater) with
//! each change tick `p` of the touched timeline. These comparisons change only at `s = p - o`
//! (from less to equal) and `s = p - o + 1` (from equal to greater), so the answer is constant
//! between consecutive candidates, and the earliest fitting start is the window start or one of
//! them. Rates break this, because a level then also depends on the distance between ticks.

use super::{Demand, Plan};
use crate::state::{Requirement, SetPoint};
use crate::storage::{PlanStorage, Seq, Storage};
use crate::timeline::profile::Point;
use crate::{Container, Error, Impact, StateImpact, Tick, Timeline, Window};

/// The scratch list types of a probe on plan storage `P`.
type Line<P> = <P as PlanStorage>::Line;

/// Reusable scratch space for checking one activity at many candidate starts.
pub(super) struct Probe<'a, P: PlanStorage> {
    /// The plan the activity is checked against.
    plan: &'a Plan<P>,
    /// The demands of the activity, relative to its start.
    demands: &'a [Demand],
    /// Change points of the shifted demands on the numeric timeline being checked.
    points: <Line<P> as Storage>::DoubleList<Point>,
    /// Sets of the shifted demands on the state timeline being checked.
    sets: <Line<P> as Storage>::List<SetPoint>,
    /// Requirements of the shifted demands on the state timeline being checked.
    requires: <Line<P> as Storage>::List<Requirement>,
}

impl<'a, P: PlanStorage> Probe<'a, P> {
    /// Prepares scratch space for `demands`.
    ///
    /// Reserves room for the largest number of demands on any one timeline, so that
    /// [`fits`](Self::fits) never allocates.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownResource`] if a demand names a timeline outside `plan`.
    /// - [`Error::OutOfMemory`] or [`Error::CapacityExceeded`] if the scratch buffers cannot
    ///   be allocated.
    pub(super) fn new(plan: &'a Plan<P>, demands: &'a [Demand]) -> Result<Self, Error> {
        for demand in demands {
            let known = match *demand {
                Demand::Level { timeline, .. } => plan.timeline(timeline).is_some(),
                Demand::State { timeline, .. } => plan.state(timeline).is_some(),
            };
            if !known {
                return Err(Error::UnknownResource);
            }
        }

        let mut probe = Self {
            plan,
            demands,
            points: Seq::EMPTY,
            sets: Seq::EMPTY,
            requires: Seq::EMPTY,
        };
        let most = most_on_one_timeline(demands);
        probe
            .points
            .try_reserve(most.saturating_mul(2), Container::Scratch)?;
        probe.sets.try_reserve(most, Container::Scratch)?;
        probe.requires.try_reserve(most, Container::Scratch)?;
        Ok(probe)
    }

    /// Whether the activity fits when it starts at `start`. Never allocates.
    ///
    /// # Errors
    ///
    /// See [`Plan::fits`].
    pub(super) fn fits(&mut self, start: Tick) -> Result<bool, Error> {
        for (index, demand) in self.demands.iter().enumerate() {
            let first_on_timeline = !self.demands[..index]
                .iter()
                .any(|earlier| earlier.same_timeline(demand));
            if first_on_timeline && !self.timeline_fits(demand, start)? {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Whether the timeline of `representative` stays valid with all demands on it added.
    fn timeline_fits(&mut self, representative: &Demand, start: Tick) -> Result<bool, Error> {
        match *representative {
            Demand::Level { timeline, .. } => {
                let timeline = &self.plan.timelines[timeline.0];
                if !self.collect_points(representative, start)? {
                    return Ok(false);
                }

                Ok(timeline.breach_with(&self.points)?.is_none())
            }
            Demand::State { timeline, .. } => {
                let timeline = &self.plan.states[timeline.0];
                if !self.collect_states(representative, start)? {
                    return Ok(false);
                }

                Ok(!timeline.is_violated_with(&self.sets, &self.requires))
            }
        }
    }

    /// Fills `points` with the shifted changes of all numeric demands on the timeline of
    /// `representative`. Returns `false` if one of them would precede the timeline start.
    fn collect_points(&mut self, representative: &Demand, start: Tick) -> Result<bool, Error> {
        self.points.clear();
        for demand in self.demands {
            let Demand::Level { timeline, impact } = *demand else {
                continue;
            };
            if !demand.same_timeline(representative) {
                continue;
            }

            let Some(impact) = admissible(&self.plan.timelines[timeline.0], impact, start)? else {
                return Ok(false);
            };
            for (at, step, slope) in impact.changes().into_iter().flatten() {
                self.points
                    .try_push(Point::loose(at, step, slope), Container::Scratch)?;
            }
        }

        self.points.sort_unstable_by_key(|point| point.at);
        Ok(true)
    }

    /// Fills `sets` and `requires` with the shifted state demands on the timeline of
    /// `representative`. Returns `false` if one of them would precede the timeline start.
    fn collect_states(&mut self, representative: &Demand, start: Tick) -> Result<bool, Error> {
        self.sets.clear();
        self.requires.clear();
        for demand in self.demands {
            let Demand::State { timeline, impact } = *demand else {
                continue;
            };
            if !demand.same_timeline(representative) {
                continue;
            }

            let shifted = impact.shifted(start).ok_or(Error::Overflow)?;
            match self.plan.states[timeline.0].validate(shifted) {
                Ok(()) => {}
                Err(Error::BeforeStart { .. }) => return Ok(false),
                Err(error) => return Err(error),
            }

            self.push_state(shifted)?;
        }

        self.sets.sort_unstable_by_key(SetPoint::key);
        Ok(true)
    }

    /// Stores one shifted, validated state impact in the scratch lists.
    ///
    /// # Errors
    ///
    /// None in practice: [`new`](Self::new) reserved the room.
    fn push_state(&mut self, impact: StateImpact) -> Result<(), Error> {
        // The probe's own activity id is irrelevant for the yes/no answer; any fixed id works.
        let activity = crate::ActivityId(u32::MAX);
        match impact {
            StateImpact::Set { at, value } => {
                let set = SetPoint {
                    at,
                    activity,
                    value,
                };
                self.sets.try_push(set, Container::Scratch)
            }
            StateImpact::Require { start, end, value } => {
                let requirement = Requirement {
                    activity,
                    start,
                    end,
                    value,
                };
                self.requires.try_push(requirement, Container::Scratch)
            }
        }
    }
}

/// The largest number of demands that address the same timeline.
fn most_on_one_timeline(demands: &[Demand]) -> usize {
    demands
        .iter()
        .map(|demand| {
            demands
                .iter()
                .filter(|other| other.same_timeline(demand))
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// `impact` shifted to `start` if the timeline accepts it there, `None` if it would precede
/// the timeline start.
fn admissible<S: Storage>(
    timeline: &Timeline<S>,
    impact: Impact,
    start: Tick,
) -> Result<Option<Impact>, Error> {
    let shifted = impact.shifted(start).ok_or(Error::Overflow)?;
    match timeline.validate(shifted) {
        Ok(()) => Ok(Some(shifted)),
        Err(Error::BeforeStart { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Finds the earliest start in `window` at which `demands` fit, checking at most `max_checks`
/// candidates.
pub(super) fn earliest_start<P: PlanStorage>(
    plan: &Plan<P>,
    demands: &[Demand],
    window: Window,
    max_checks: usize,
) -> Result<Option<Tick>, Error> {
    let mut probe = Probe::new(plan, demands)?;
    let mut checks = 0_usize;
    let mut check = |start: Tick| -> Result<bool, Error> {
        if checks == max_checks {
            return Err(Error::SearchExhausted { checks: max_checks });
        }

        checks += 1;
        probe.fits(start)
    };

    let Some(window) = admissible_window(plan, demands, window) else {
        return Ok(None);
    };

    if involves_rates(plan, demands) {
        // shortcut: with rates every tick of the window is a candidate, so the cost grows with
        // the window length; restrict candidates per linear piece if long windows matter.
        for start in window.earliest()..=window.latest() {
            if check(start)? {
                return Ok(Some(start));
            }
        }

        return Ok(None);
    }

    let mut start = Some(window.earliest());
    while let Some(candidate) = start {
        if check(candidate)? {
            return Ok(Some(candidate));
        }

        start = next_candidate(plan, demands, candidate, window);
    }

    Ok(None)
}

/// `window` without the starts at which a demand would precede its timeline's start, or
/// `None` if no start is left.
fn admissible_window<P: PlanStorage>(
    plan: &Plan<P>,
    demands: &[Demand],
    window: Window,
) -> Option<Window> {
    let mut earliest = i128::from(window.earliest());
    for demand in demands {
        let timeline_start = match *demand {
            Demand::Level { timeline, .. } => plan.timelines[timeline.0].start(),
            Demand::State { timeline, .. } => plan.states[timeline.0].start(),
        };
        let first_offset = demand.offsets()[0];
        earliest = earliest.max(i128::from(timeline_start) - i128::from(first_offset));
    }

    Window::new(Tick::try_from(earliest).ok()?, window.latest()).ok()
}

/// Whether a demand or a touched numeric timeline has a rate.
fn involves_rates<P: PlanStorage>(plan: &Plan<P>, demands: &[Demand]) -> bool {
    demands.iter().any(|demand| match *demand {
        Demand::Level { timeline, impact } => {
            impact.is_rate() || plan.timelines[timeline.0].has_rates()
        }
        Demand::State { .. } => false,
    })
}

/// The smallest start in `window` after `after` where a demand tick meets a change tick of
/// its timeline, or the tick after one, so that the comparison of the two changes.
///
/// Enumerating candidates one at a time this way needs no memory. Each call costs `O(n k)` for
/// `n` change ticks and `k` demands, the same order as checking one candidate.
fn next_candidate<P: PlanStorage>(
    plan: &Plan<P>,
    demands: &[Demand],
    after: Tick,
    window: Window,
) -> Option<Tick> {
    let mut next: Option<Tick> = None;
    let mut consider = |tick: Tick, offset: Tick| {
        let meet = i128::from(tick) - i128::from(offset);
        for start in [meet, meet + 1] {
            let Ok(start) = Tick::try_from(start) else {
                continue;
            };

            if start > after && window.contains(start) && next.is_none_or(|next| start < next) {
                next = Some(start);
            }
        }
    };

    for demand in demands {
        for offset in demand.offsets() {
            match *demand {
                Demand::Level { timeline, .. } => {
                    let timeline = &plan.timelines[timeline.0];
                    let ticks = core::iter::once(timeline.start()).chain(timeline.change_ticks());
                    ticks.for_each(|tick| consider(tick, offset));
                }
                Demand::State { timeline, .. } => plan.states[timeline.0]
                    .change_ticks()
                    .for_each(|tick| consider(tick, offset)),
            }
        }
    }

    next
}
