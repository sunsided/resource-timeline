//! Tentative checks and the earliest-start search of a plan.
//!
//! # Why the candidate starts suffice without rates
//!
//! Without rates every level is a step function. Whether an activity starting at `s` fits
//! depends only on how each of its shifted ticks `s + o` compares (less, equal, greater) with
//! each change tick `p` of the touched timeline. These comparisons change only at `s = p - o`
//! (from less to equal) and `s = p - o + 1` (from equal to greater), so the answer is constant
//! between consecutive candidates, and the earliest fitting start is the window start or one of
//! them.
//!
//! # Closed form per piece with rates
//!
//! With rates a level also depends on the distance between ticks, so the answer may change
//! inside a piece `[c_i, c_{i+1})` between consecutive candidates. The order of all ticks is
//! still fixed there, so the merged level line has the same segments for every start `s` in the
//! piece, and the level at each segment start, at the tick before each segment end and at each
//! target is affine in `s` with an integer coefficient: a shifted point moves along a fixed
//! line, a shifted rate changes its contribution to a fixed tick by its rate per tick. The
//! activity fits at `s` if every such affine value lies within its bounds; the extremes of a
//! linear segment are at its ends, so these values cover every tick. The search evaluates the
//! line at `c_i` and `c_i + 1`, reads each value and its coefficient off the pair, and solves
//! the inequalities for the earliest `s` in closed form, without checking `s` tick by tick.

use super::{Demand, Plan};
use crate::state::{Requirement, SetPoint};
use crate::storage::{PlanStorage, Seq, Storage};
use crate::timeline::profile::Point;
use crate::{Container, Error, Impact, StateImpact, Tick, Timeline, Window};

/// The scratch list types of a probe on plan storage `P`.
type Line<P> = <P as PlanStorage>::Line;

/// Working memory of [`Plan::fits`](super::Plan::fits) and
/// [`Plan::earliest_start`](super::Plan::earliest_start), for callers that keep it outside the
/// stack.
///
/// With [`Fixed<N>`](crate::storage::Fixed) storage it holds two change-point lists of `2 N`
/// points and a set and a requirement list of `N` entries, `216 N + 128` bytes on 64-bit
/// targets: 58 KiB for `N = 256`. `fits` and `earliest_start` put it on the stack;
/// [`Plan::fits_with`](super::Plan::fits_with) and
/// [`Plan::earliest_start_with`](super::Plan::earliest_start_with) borrow one instead, for
/// example from a `static` or a task's own memory. Measured on x86-64 (release, frame sizes
/// from `-Z emit-stack-sizes`, confirmed by running in threads of shrinking stack): a search
/// over a `FixedPlan<2, 1, 256>` needs about 60 KiB of stack with the internal scratch and
/// fits in a 16 KiB thread stack with a borrowed one; its largest frame is the 2.4 KiB of the
/// change-point sort. Explaining a violation (`first_violation`) keeps its own per-activity
/// working list of `32 N` bytes on the stack, about 16 KiB for `N = 256`.
///
/// # Examples
///
/// ```
/// use resource_timeline::storage::{Fixed, FixedPlan};
/// use resource_timeline::{ActivityId, Demand, Impact, Plan, SearchScratch, Timeline, Window};
///
/// let mut plan = Plan::<FixedPlan<1, 0, 64>>::new_in();
/// let transmitter = plan.add_timeline(Timeline::claimable_in(0, 1, 64)?)?;
/// let downlink = [Demand::Level {
///     timeline: transmitter,
///     impact: Impact::Claim { start: 0, end: 8, amount: 1 },
/// }];
/// plan.place(ActivityId(1), 5, &downlink)?;
///
/// let mut scratch = SearchScratch::<Fixed<64>>::new();
/// let window = Window::new(0, 100)?;
/// assert_eq!(plan.earliest_start_with(&downlink, window, 1_000, &mut scratch)?, Some(13));
/// # Ok::<(), resource_timeline::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct SearchScratch<S: Storage> {
    /// Change points of the shifted demands on the numeric timeline being checked.
    points: S::DoubleList<Point>,
    /// The same change points for a start one tick later, for the closed-form solve.
    points_next: S::DoubleList<Point>,
    /// Sets of the shifted demands on the state timeline being checked.
    sets: S::List<SetPoint>,
    /// Requirements of the shifted demands on the state timeline being checked.
    requires: S::List<Requirement>,
}

impl<S: Storage> SearchScratch<S> {
    /// Empty working memory.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            points: <S::DoubleList<Point> as Seq<_>>::EMPTY,
            points_next: <S::DoubleList<Point> as Seq<_>>::EMPTY,
            sets: <S::List<SetPoint> as Seq<_>>::EMPTY,
            requires: <S::List<Requirement> as Seq<_>>::EMPTY,
        }
    }
}

impl<S: Storage> Default for SearchScratch<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Checks one activity at many candidate starts, with borrowed working memory.
pub(super) struct Probe<'a, P: PlanStorage> {
    /// The plan the activity is checked against.
    plan: &'a Plan<P>,
    /// The demands of the activity, relative to its start.
    demands: &'a [Demand],
    /// The working memory.
    scratch: &'a mut SearchScratch<Line<P>>,
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
    pub(super) fn new(
        plan: &'a Plan<P>,
        demands: &'a [Demand],
        scratch: &'a mut SearchScratch<Line<P>>,
    ) -> Result<Self, Error> {
        for demand in demands {
            let known = match *demand {
                Demand::Level { timeline, .. } => plan.timeline(timeline).is_some(),
                Demand::State { timeline, .. } => plan.state(timeline).is_some(),
            };
            if !known {
                return Err(Error::UnknownResource);
            }
        }

        let most = most_on_one_timeline(demands);
        for points in [&mut scratch.points, &mut scratch.points_next] {
            points.clear();
            points.try_reserve(most.saturating_mul(2), Container::Scratch)?;
        }

        scratch.sets.clear();
        scratch.requires.clear();
        scratch.sets.try_reserve(most, Container::Scratch)?;
        scratch.requires.try_reserve(most, Container::Scratch)?;
        Ok(Self {
            plan,
            demands,
            scratch,
        })
    }

    /// Whether the activity fits when it starts at `start`. Never allocates.
    ///
    /// # Errors
    ///
    /// See [`Plan::fits`].
    pub(super) fn fits(&mut self, start: Tick) -> Result<bool, Error> {
        for index in self.representatives() {
            if !self.timeline_fits(&self.demands[index], start)? {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// The earliest start in `[lo, hi]` at which the activity fits, solved in closed form.
    ///
    /// Every start in `[lo, hi]` must be admissible and see the same order of demand ticks and
    /// change ticks (a piece between consecutive search candidates); see the module docs. Never
    /// allocates.
    ///
    /// # Errors
    ///
    /// See [`Plan::fits`].
    fn earliest_in(&mut self, lo: Tick, hi: Tick) -> Result<Option<Tick>, Error> {
        if lo == hi {
            return Ok(self.fits(lo)?.then_some(lo));
        }

        let mut offsets = Offsets {
            lo: 0,
            hi: i128::from(hi) - i128::from(lo),
        };
        for index in self.representatives() {
            let representative = self.demands[index];
            let Demand::Level { timeline, .. } = representative else {
                // The order of ticks fixes every state comparison, so one start decides.
                if !self.timeline_fits(&representative, lo)? {
                    return Ok(None);
                }

                continue;
            };

            let timeline = &self.plan.timelines[timeline.0];
            let first = collect_points(
                timeline,
                self.demands,
                &representative,
                lo,
                &mut self.scratch.points,
            )?;
            let second = collect_points(
                timeline,
                self.demands,
                &representative,
                lo + 1,
                &mut self.scratch.points_next,
            )?;
            if !(first && second) {
                return Ok(None);
            }

            timeline.narrow(
                &self.scratch.points,
                &self.scratch.points_next,
                &mut offsets,
            )?;
            if offsets.is_empty() {
                return Ok(None);
            }
        }

        let start = i128::from(lo) + offsets.lo;
        Ok(Some(Tick::try_from(start).map_err(|_| Error::Overflow)?))
    }

    /// The indices of the first demand on each touched timeline.
    fn representatives(&self) -> impl Iterator<Item = usize> + 'a {
        let demands = self.demands;
        (0..demands.len()).filter(move |&index| {
            !demands[..index]
                .iter()
                .any(|earlier| earlier.same_timeline(&demands[index]))
        })
    }

    /// Whether the timeline of `representative` stays valid with all demands on it added.
    fn timeline_fits(&mut self, representative: &Demand, start: Tick) -> Result<bool, Error> {
        match *representative {
            Demand::Level { timeline, .. } => {
                let timeline = &self.plan.timelines[timeline.0];
                if !collect_points(
                    timeline,
                    self.demands,
                    representative,
                    start,
                    &mut self.scratch.points,
                )? {
                    return Ok(false);
                }

                Ok(timeline.breach_with(&self.scratch.points)?.is_none())
            }
            Demand::State { timeline, .. } => {
                let timeline = &self.plan.states[timeline.0];
                if !self.collect_states(representative, start)? {
                    return Ok(false);
                }

                Ok(!timeline.is_violated_with(&self.scratch.sets, &self.scratch.requires))
            }
        }
    }

    /// Fills `sets` and `requires` with the shifted state demands on the timeline of
    /// `representative`. Returns `false` if one of them would precede the timeline start.
    fn collect_states(&mut self, representative: &Demand, start: Tick) -> Result<bool, Error> {
        self.scratch.sets.clear();
        self.scratch.requires.clear();
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

        self.scratch.sets.sort_unstable_by_key(SetPoint::key);
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
                self.scratch.sets.try_push(set, Container::Scratch)
            }
            StateImpact::Require { start, end, value } => {
                let requirement = Requirement {
                    activity,
                    start,
                    end,
                    value,
                };
                self.scratch
                    .requires
                    .try_push(requirement, Container::Scratch)
            }
        }
    }
}

/// Fills `points` with the shifted changes of all numeric demands of `demands` on the timeline
/// of `representative`, which is `timeline`, sorted by tick. Returns `false` if one of them
/// would precede the timeline start.
///
/// # Errors
///
/// [`Error::Overflow`] if a shifted tick does not fit, the validation errors of
/// [`Timeline::add`] other than [`Error::BeforeStart`], and capacity errors if `points` was not
/// reserved.
fn collect_points<S: Storage>(
    timeline: &Timeline<S>,
    demands: &[Demand],
    representative: &Demand,
    start: Tick,
    points: &mut impl Seq<Point>,
) -> Result<bool, Error> {
    points.clear();
    for demand in demands {
        let Demand::Level { impact, .. } = *demand else {
            continue;
        };
        if !demand.same_timeline(representative) {
            continue;
        }

        let Some(impact) = admissible(timeline, impact, start)? else {
            return Ok(false);
        };
        for (at, step, slope) in impact.changes().into_iter().flatten() {
            points.try_push(Point::loose(at, step, slope), Container::Scratch)?;
        }
    }

    points.sort_unstable_by_key(|point| point.at);
    Ok(true)
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
/// pieces between consecutive candidates.
pub(super) fn earliest_start<P: PlanStorage>(
    plan: &Plan<P>,
    demands: &[Demand],
    window: Window,
    max_checks: usize,
    scratch: &mut SearchScratch<Line<P>>,
) -> Result<Option<Tick>, Error> {
    let mut probe = Probe::new(plan, demands, scratch)?;
    let Some(window) = admissible_window(plan, demands, window) else {
        return Ok(None);
    };

    let rates = involves_rates(plan, demands);
    let mut checks = 0_usize;
    let mut piece = Some(window.earliest());
    while let Some(lo) = piece {
        if checks == max_checks {
            return Err(Error::SearchExhausted { checks: max_checks });
        }

        checks += 1;
        let next = next_candidate(plan, demands, lo, window);
        let found = if rates {
            let hi = next.map_or(window.latest(), |next| next - 1);
            probe.earliest_in(lo, hi)?
        } else {
            probe.fits(lo)?.then_some(lo)
        };
        if found.is_some() {
            return Ok(found);
        }

        piece = next;
    }

    Ok(None)
}

/// A range `[lo, hi]` of start offsets from the first start of a piece. Empty if `lo > hi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offsets {
    /// The smallest offset left.
    pub(crate) lo: i128,
    /// The largest offset left.
    pub(crate) hi: i128,
}

impl Offsets {
    /// Whether no offset is left.
    pub(crate) const fn is_empty(&self) -> bool {
        self.lo > self.hi
    }

    /// Keeps the offsets `k` with `min <= value + slope * k <= max` (no upper limit for
    /// `max == None`).
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a difference of levels exceeds `i128`.
    pub(crate) fn keep(
        &mut self,
        value: i128,
        slope: i128,
        min: i128,
        max: Option<i128>,
    ) -> Result<(), Error> {
        let below_min = value < min;
        let above_max = max.is_some_and(|max| value > max);
        if slope == 0 {
            if below_min || above_max {
                self.hi = self.lo - 1;
            }

            return Ok(());
        }

        let room_min = min.checked_sub(value).ok_or(Error::Overflow)?;
        let room_max = max
            .map(|max| max.checked_sub(value).ok_or(Error::Overflow))
            .transpose()?;
        if slope > 0 {
            self.lo = self.lo.max(ceil_div(room_min, slope));
            if let Some(room) = room_max {
                self.hi = self.hi.min(floor_div(room, slope));
            }
        } else {
            self.hi = self.hi.min(floor_div(room_min, slope));
            if let Some(room) = room_max {
                self.lo = self.lo.max(ceil_div(room, slope));
            }
        }

        Ok(())
    }
}

/// `a / b` rounded towards negative infinity. `b` must not be zero.
const fn floor_div(a: i128, b: i128) -> i128 {
    let quotient = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        quotient - 1
    } else {
        quotient
    }
}

/// `a / b` rounded towards positive infinity. `b` must not be zero.
const fn ceil_div(a: i128, b: i128) -> i128 {
    let quotient = a / b;
    if a % b != 0 && ((a < 0) == (b < 0)) {
        quotient + 1
    } else {
        quotient
    }
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
/// Enumerating candidates one at a time this way needs no memory. For a demand offset `o` the
/// next meeting start is `p - o` for the first change tick `p >= after + o + 1`, and the next
/// passing start `p - o + 1` for the first `p >= after + o`; sorted change ticks make each a
/// binary search, so a call costs `O(k log n)` for `k` demands (plus the unsorted requirements
/// of touched state timelines).
fn next_candidate<P: PlanStorage>(
    plan: &Plan<P>,
    demands: &[Demand],
    after: Tick,
    window: Window,
) -> Option<Tick> {
    let mut next: Option<i128> = None;
    for demand in demands {
        for offset in demand.offsets() {
            let threshold = i128::from(after) + i128::from(offset);
            let first_from = |x: i128| match *demand {
                Demand::Level { timeline, .. } => plan.timelines[timeline.0].first_change_from(x),
                Demand::State { timeline, .. } => plan.states[timeline.0].first_change_from(x),
            };
            let meet = first_from(threshold + 1).map(|p| i128::from(p) - i128::from(offset));
            let pass = first_from(threshold).map(|p| i128::from(p) - i128::from(offset) + 1);
            for start in [meet, pass].into_iter().flatten() {
                if next.is_none_or(|next| start < next) {
                    next = Some(start);
                }
            }
        }
    }

    let next = Tick::try_from(next?).ok()?;
    window.contains(next).then_some(next)
}

#[cfg(test)]
mod tests {
    //! Brute-force checks of the closed-form helpers.

    use super::{Offsets, ceil_div, floor_div};

    /// Rounding division agrees with exact rational comparison for all small operands.
    #[test]
    fn rounding_division() {
        for a in -20_i128..=20 {
            for b in (-6_i128..=6).filter(|&b| b != 0) {
                let floor = floor_div(a, b);
                let ceil = ceil_div(a, b);
                // floor * b <= a < (floor + 1) * b for b > 0, mirrored for b < 0.
                if b > 0 {
                    assert!(floor * b <= a && a < (floor + 1) * b, "floor {a}/{b}");
                    assert!((ceil - 1) * b < a && a <= ceil * b, "ceil {a}/{b}");
                } else {
                    assert!(floor * b >= a && a > (floor + 1) * b, "floor {a}/{b}");
                    assert!((ceil - 1) * b > a && a >= ceil * b, "ceil {a}/{b}");
                }
            }
        }
    }

    /// `keep` leaves exactly the offsets whose affine value lies in the range.
    #[test]
    fn keep_matches_enumeration() {
        for value in -8_i128..=8 {
            for slope in -3_i128..=3 {
                for max in [None, Some(2_i128), Some(5)] {
                    let mut offsets = Offsets { lo: 0, hi: 6 };
                    offsets.keep(value, slope, -1, max).expect("small");
                    let expected = (0..=6).filter(|k| {
                        let level = value + slope * k;
                        level >= -1 && max.is_none_or(|max| level <= max)
                    });
                    assert!(
                        (offsets.lo..=offsets.hi).eq(expected),
                        "value {value} slope {slope} max {max:?}"
                    );
                }
            }
        }
    }
}
