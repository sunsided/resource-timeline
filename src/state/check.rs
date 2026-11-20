//! Queries and the violation search over the sorted sets of a state timeline.

use alloc::vec::Vec;

use super::{Requirement, StateValue, StateViolation};
use crate::{ActivityId, Error, Tick};

/// A stored [`StateImpact::Set`](crate::StateImpact::Set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SetPoint {
    /// The tick of the change.
    pub(crate) at: Tick,
    /// The activity that sets the value.
    pub(crate) activity: ActivityId,
    /// The new value.
    pub(crate) value: StateValue,
}

impl SetPoint {
    /// The sort key: by tick, then by activity.
    pub(crate) const fn key(&self) -> (Tick, ActivityId) {
        (self.at, self.activity)
    }
}

/// A violation found by the search, before a conflict's setter list is collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Breach {
    /// A requirement fails; the fields match [`StateViolation::Mismatch`].
    Mismatch {
        /// The earliest failing tick.
        at: Tick,
        /// The required value.
        required: StateValue,
        /// The value at `at`.
        actual: StateValue,
        /// The activity holding the requirement.
        requirer: ActivityId,
        /// The activity that set `actual`, `None` for the initial value.
        setter: Option<ActivityId>,
    },
    /// Contradicting sets at tick `at`.
    Conflict {
        /// The tick of the contradicting sets.
        at: Tick,
    },
}

impl Breach {
    /// The tick of the breach.
    const fn at(&self) -> Tick {
        match *self {
            Self::Mismatch { at, .. } | Self::Conflict { at } => at,
        }
    }
}

/// Two sorted set lists (stored and tentative) queried as one.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sets<'a> {
    /// The stored sets, sorted by [`SetPoint::key`].
    pub(super) base: &'a [SetPoint],
    /// Tentative sets, sorted by [`SetPoint::key`].
    pub(super) extra: &'a [SetPoint],
}

impl Sets<'_> {
    /// The value at `t`, or `None` if contradicting sets at the latest set tick make it
    /// ambiguous.
    pub(super) fn value_at(&self, t: Tick, initial: StateValue) -> Option<StateValue> {
        let (value, setter) = self.value_and_setter(t, initial);
        match setter {
            Some(_) if self.has_conflict_at(self.latest_tick(t)?) => None,
            _ => Some(value),
        }
    }

    /// The value at `t` and the activity that set it, assuming no conflict at its tick.
    ///
    /// Among several sets at the same latest tick the smallest activity id wins.
    fn value_and_setter(&self, t: Tick, initial: StateValue) -> (StateValue, Option<ActivityId>) {
        let latest = [latest_set(self.base, t), latest_set(self.extra, t)]
            .into_iter()
            .flatten()
            .max_by_key(|set| (set.at, core::cmp::Reverse(set.activity)));
        latest.map_or((initial, None), |set| (set.value, Some(set.activity)))
    }

    /// The tick of the latest set at or before `t`, if any.
    fn latest_tick(&self, t: Tick) -> Option<Tick> {
        [latest_set(self.base, t), latest_set(self.extra, t)]
            .into_iter()
            .flatten()
            .map(|set| set.at)
            .max()
    }

    /// Whether the sets at tick `at` disagree on the value.
    fn has_conflict_at(&self, at: Tick) -> bool {
        let mut values = sets_at(self.base, at)
            .iter()
            .chain(sets_at(self.extra, at))
            .map(|set| set.value);
        values
            .next()
            .is_some_and(|first| values.any(|value| value != first))
    }

    /// The earliest tick with contradicting sets.
    ///
    /// Costs `O(n)` for the stored sets plus `O(k log n)` for `k` tentative ones.
    fn first_conflict(&self) -> Option<Tick> {
        let in_base = self
            .base
            .windows(2)
            .find(|pair| pair[0].at == pair[1].at && pair[0].value != pair[1].value)
            .map(|pair| pair[0].at);
        let with_extra = self
            .extra
            .iter()
            .map(|set| set.at)
            .filter(|&at| self.has_conflict_at(at))
            .min();

        [in_base, with_extra].into_iter().flatten().min()
    }

    /// The first set strictly inside `(start, end)` that changes the value away from `value`.
    fn first_change_in(&self, start: Tick, end: Tick, value: StateValue) -> Option<SetPoint> {
        let first_in = |sets: &[SetPoint]| {
            let from = sets.partition_point(|set| set.at <= start);
            sets[from..]
                .iter()
                .take_while(|set| set.at < end)
                .find(|set| set.value != value)
                .copied()
        };

        [first_in(self.base), first_in(self.extra)]
            .into_iter()
            .flatten()
            .min_by_key(SetPoint::key)
    }

    /// The earliest tick at which `requirement` fails, with the offending value and setter.
    fn mismatch(&self, requirement: &Requirement, initial: StateValue) -> Option<Breach> {
        let breach = |at, actual, setter| Breach::Mismatch {
            at,
            required: requirement.value,
            actual,
            requirer: requirement.activity,
            setter,
        };

        let (actual, setter) = self.value_and_setter(requirement.start, initial);
        if actual != requirement.value {
            return Some(breach(requirement.start, actual, setter));
        }

        self.first_change_in(requirement.start, requirement.end, requirement.value)
            .map(|set| breach(set.at, set.value, Some(set.activity)))
    }

    /// The earliest violation over the stored and tentative requirements.
    ///
    /// A conflict wins over a mismatch at the same or a later tick. Among mismatches at the same
    /// tick the smallest requirer id wins, then the smallest required value.
    pub(super) fn first_breach(
        &self,
        initial: StateValue,
        requires: &[Requirement],
        extra_requires: &[Requirement],
    ) -> Option<Breach> {
        let conflict = self.first_conflict().map(|at| Breach::Conflict { at });
        let mismatch = requires
            .iter()
            .chain(extra_requires)
            .filter_map(|requirement| self.mismatch(requirement, initial))
            .min_by_key(|breach| match *breach {
                Breach::Mismatch {
                    at,
                    requirer,
                    required,
                    ..
                } => (at, requirer, required),
                Breach::Conflict { at } => (at, ActivityId(0), 0),
            });

        match (mismatch, conflict) {
            (Some(mismatch), Some(conflict)) if mismatch.at() < conflict.at() => Some(mismatch),
            (_, Some(conflict)) => Some(conflict),
            (mismatch, None) => mismatch,
        }
    }

    /// Turns `breach` into a reported violation, collecting a conflict's setters.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the setter list cannot be allocated.
    pub(super) fn explain(&self, breach: Breach) -> Result<StateViolation, Error> {
        match breach {
            Breach::Mismatch {
                at,
                required,
                actual,
                requirer,
                setter,
            } => Ok(StateViolation::Mismatch {
                at,
                required,
                actual,
                requirer,
                setter,
            }),
            Breach::Conflict { at } => {
                let (base, extra) = (sets_at(self.base, at), sets_at(self.extra, at));
                let mut setters = Vec::new();
                setters
                    .try_reserve_exact(base.len() + extra.len())
                    .map_err(|_| Error::OutOfMemory)?;
                setters.extend(base.iter().chain(extra).map(|set| set.activity));
                setters.sort_unstable();
                setters.dedup();
                Ok(StateViolation::Conflict { at, setters })
            }
        }
    }
}

/// The set with the latest tick at or before `t` and, among those, the smallest activity.
fn latest_set(sets: &[SetPoint], t: Tick) -> Option<SetPoint> {
    let end = sets.partition_point(|set| set.at <= t);
    let last = sets.get(end.checked_sub(1)?)?;
    sets_at(sets, last.at).first().copied()
}

/// The sets at exactly tick `at`, sorted by activity.
fn sets_at(sets: &[SetPoint], at: Tick) -> &[SetPoint] {
    let from = sets.partition_point(|set| set.at < at);
    let to = sets.partition_point(|set| set.at <= at);
    &sets[from..to]
}
