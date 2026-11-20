//! Effects of an activity on a single resource timeline.

use crate::{Level, Tick};

/// One effect of an activity on the level of a resource.
///
/// The level at tick `t` is the timeline's initial level plus the contribution of every
/// impact at `t`:
///
/// ```text
/// Step  { at, delta }              contributes  delta                          if at <= t
/// Claim { start, end, amount }     contributes  -amount                        if start <= t < end
/// Rate  { start, end, per_tick }   contributes  per_tick * (min(t, end) - start) if start <= t
/// ```
///
/// Steps and rates are permanent (consumption and production persist), claims are returned at
/// their end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Impact {
    /// A permanent change of the level, as for a consumable resource.
    ///
    /// Consumption (energy drawn, storage filled) is a negative `delta`, production (charging,
    /// downlinked data freed) a positive one.
    Step {
        /// The tick at which the change takes effect. Not before the timeline start.
        at: Tick,
        /// The change of the level from `at` onward. May be zero or negative.
        delta: Level,
    },
    /// A temporary hold on a resource during the half-open interval `[start, end)`.
    ///
    /// The level is the amount *available*, so a claim lowers it by `amount` while it holds and
    /// returns it at `end`. A claim ending at `t` and another starting at `t` do not overlap.
    Claim {
        /// The first tick of the hold. Not before the timeline start.
        start: Tick,
        /// The first tick after the hold. Strictly greater than `start`.
        end: Tick,
        /// The number of units held. Strictly positive.
        amount: Level,
    },
    /// A linear change of the level by `per_tick` for every tick elapsed in `[start, end)`.
    ///
    /// Models a battery discharging under a power draw (negative), solar charging or thermal
    /// drift (either sign). The level at `start` is unchanged, at `start + k` it has changed by
    /// `k * per_tick`, and from `end` onward the total `(end - start) * per_tick` persists.
    /// Fractional rates are expressed by choosing a finer level unit. The level is never
    /// clamped (a full battery does not stop charging): exceeding the capacity is reported as a
    /// violation instead.
    Rate {
        /// The first tick of the change. Not before the timeline start.
        start: Tick,
        /// The tick at which the change stops. Strictly greater than `start`.
        end: Tick,
        /// The change of the level per elapsed tick. May be zero or negative.
        per_tick: Level,
    },
}

impl Impact {
    /// The earliest tick at which this impact changes the level.
    #[must_use]
    pub const fn first_tick(&self) -> Tick {
        match *self {
            Self::Step { at, .. } => at,
            Self::Claim { start, .. } | Self::Rate { start, .. } => start,
        }
    }

    /// The last tick at which this impact changes the level or its slope.
    ///
    /// From this tick onward the contribution of the impact is constant.
    #[must_use]
    pub const fn last_tick(&self) -> Tick {
        match *self {
            Self::Step { at, .. } => at,
            Self::Claim { end, .. } | Self::Rate { end, .. } => end,
        }
    }

    /// Whether the impact changes the level linearly rather than in steps.
    #[must_use]
    pub const fn is_rate(&self) -> bool {
        matches!(self, Self::Rate { .. })
    }

    /// The same impact moved `by` ticks later (earlier for negative `by`).
    ///
    /// Returns `None` if a moved tick does not fit a [`Tick`].
    #[must_use]
    pub const fn shifted(&self, by: Tick) -> Option<Self> {
        match *self {
            Self::Step { at, delta } => match at.checked_add(by) {
                Some(at) => Some(Self::Step { at, delta }),
                None => None,
            },
            Self::Claim { start, end, amount } => {
                match (start.checked_add(by), end.checked_add(by)) {
                    (Some(start), Some(end)) => Some(Self::Claim { start, end, amount }),
                    _ => None,
                }
            }
            Self::Rate {
                start,
                end,
                per_tick,
            } => match (start.checked_add(by), end.checked_add(by)) {
                (Some(start), Some(end)) => Some(Self::Rate {
                    start,
                    end,
                    per_tick,
                }),
                _ => None,
            },
        }
    }

    /// The contribution of this impact to the level at tick `t`, widened to avoid overflow.
    ///
    /// Returns `None` only if a rate contribution exceeds `i128`, which needs a rate and a
    /// duration both close to the `i64` limits.
    #[must_use]
    pub(crate) fn contribution_at(&self, t: Tick) -> Option<i128> {
        match *self {
            Self::Step { at, delta } if at <= t => Some(i128::from(delta)),
            Self::Claim { start, end, amount } if start <= t && t < end => {
                Some(-i128::from(amount))
            }
            Self::Rate {
                start,
                end,
                per_tick,
            } if start <= t => {
                let elapsed = i128::from(t.min(end)) - i128::from(start);
                i128::from(per_tick).checked_mul(elapsed)
            }
            Self::Step { .. } | Self::Claim { .. } | Self::Rate { .. } => Some(0),
        }
    }

    /// The changes of level and slope this impact causes, as `(tick, step, slope)` triples.
    ///
    /// A step yields one change, a claim or rate two (at its start and its end). Unused entries
    /// are `None`.
    pub(crate) fn changes(&self) -> [Option<(Tick, i128, i128)>; 2] {
        match *self {
            Self::Step { at, delta } => [Some((at, i128::from(delta), 0)), None],
            Self::Claim { start, end, amount } => [
                Some((start, -i128::from(amount), 0)),
                Some((end, i128::from(amount), 0)),
            ],
            Self::Rate {
                start,
                end,
                per_tick,
            } => [
                Some((start, 0, i128::from(per_tick))),
                Some((end, 0, -i128::from(per_tick))),
            ],
        }
    }
}
