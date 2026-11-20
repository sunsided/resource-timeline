//! Activities with uncertain durations: impacts anchored at the start or at the end.

use crate::{ActivityId, Error, Impact, Level, Tick, Window};

/// A tick of an activity, relative to its start or to its (uncertain) end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Anchor {
    /// The given number of ticks after the start (before it for a negative offset).
    Start(Tick),
    /// The given number of ticks after the end, which is the start plus the duration.
    End(Tick),
}

impl Anchor {
    /// The tick relative to the start for an activity that lasts `duration` ticks.
    ///
    /// Returns `None` if the tick does not fit a [`Tick`].
    #[must_use]
    pub const fn resolve(self, duration: Tick) -> Option<Tick> {
        match self {
            Self::Start(offset) => Some(offset),
            Self::End(offset) => duration.checked_add(offset),
        }
    }
}

/// One effect of an activity whose duration is uncertain.
///
/// The variants mirror [`Impact`], with ticks given as [`Anchor`]s. A claim from
/// `Anchor::Start(0)` to `Anchor::End(0)` holds a resource for exactly as long as the activity
/// runs; a rate over the same interval drains energy for as long as it runs. The interval may
/// be empty for the shortest duration (a zero-length run), but never inverted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Uncertain {
    /// A permanent change of the level at `at`.
    Step {
        /// The tick of the change.
        at: Anchor,
        /// The change of the level from `at` onward.
        delta: Level,
    },
    /// A hold of `amount` units during `[start, end)`.
    Claim {
        /// The first tick of the hold.
        start: Anchor,
        /// The first tick after the hold. Not before `start` for any duration.
        end: Anchor,
        /// The number of units held. Strictly positive.
        amount: Level,
    },
    /// A change of `per_tick` for every tick elapsed in `[start, end)`.
    Rate {
        /// The first tick of the change.
        start: Anchor,
        /// The tick at which the change stops. Not before `start` for any duration.
        end: Anchor,
        /// The change of the level per elapsed tick.
        per_tick: Level,
    },
}

impl Uncertain {
    /// The impact, relative to the start, for an activity that lasts `duration` ticks.
    ///
    /// The interval of a claim or rate may be empty. Returns `None` if a tick does not fit a
    /// [`Tick`].
    #[must_use]
    pub const fn resolve(&self, duration: Tick) -> Option<Impact> {
        match *self {
            Self::Step { at, delta } => match at.resolve(duration) {
                Some(at) => Some(Impact::Step { at, delta }),
                None => None,
            },
            Self::Claim { start, end, amount } => {
                match (start.resolve(duration), end.resolve(duration)) {
                    (Some(start), Some(end)) => Some(Impact::Claim { start, end, amount }),
                    _ => None,
                }
            }
            Self::Rate {
                start,
                end,
                per_tick,
            } => match (start.resolve(duration), end.resolve(duration)) {
                (Some(start), Some(end)) => Some(Impact::Rate {
                    start,
                    end,
                    per_tick,
                }),
                _ => None,
            },
        }
    }

    /// The anchors of the impact.
    pub(super) const fn anchors(&self) -> [Anchor; 2] {
        match *self {
            Self::Step { at, .. } => [at, at],
            Self::Claim { start, end, .. } | Self::Rate { start, end, .. } => [start, end],
        }
    }
}

/// An activity whose start lies in a window and whose duration lies in a range, as for a
/// contingent link of a simple temporal network with uncertainty.
///
/// The envelope covers every combination of a start in `window` and a duration in `duration`,
/// chosen independently per activity: the start by the dispatcher, the duration by nature. See
/// [`Envelope::with_contingent`](super::Envelope::with_contingent) for an example.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Contingent<'a> {
    /// The activity, reported back in explanations.
    pub activity: ActivityId,
    /// The allowed starts.
    pub window: Window,
    /// The possible durations `[shortest, longest]`. Never negative.
    pub duration: Window,
    /// The effects of the activity, anchored at its start or end.
    pub impacts: &'a [Uncertain],
}

/// The impact with an empty interval widened to one tick, so that the checks of
/// [`Timeline::add`](crate::Timeline::add) that do not concern the length still apply.
///
/// # Errors
///
/// - [`Error::EmptyInterval`] if the interval is inverted.
/// - [`Error::Overflow`] if the widened end does not fit a [`Tick`].
pub(super) fn widened(impact: Impact) -> Result<Impact, Error> {
    let widen = |start: Tick, end: Tick| -> Result<Tick, Error> {
        match end.cmp(&start) {
            core::cmp::Ordering::Less => Err(Error::EmptyInterval { start, end }),
            core::cmp::Ordering::Equal => start.checked_add(1).ok_or(Error::Overflow),
            core::cmp::Ordering::Greater => Ok(end),
        }
    };

    Ok(match impact {
        Impact::Step { .. } => impact,
        Impact::Claim { start, end, amount } => Impact::Claim {
            start,
            end: widen(start, end)?,
            amount,
        },
        Impact::Rate {
            start,
            end,
            per_tick,
        } => Impact::Rate {
            start,
            end: widen(start, end)?,
            per_tick,
        },
    })
}
