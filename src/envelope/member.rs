//! The activities of an envelope and their extreme contributions at a tick.
//!
//! A member's contribution at tick `t` with start `s` and duration `d` is its own profile at
//! the relative time `u = t - s`. Over all starts in a window `[lo, hi]` and durations in
//! `[d_lo, d_hi]`, the extreme is the extreme of the profile `g(u, d)` over the box
//! `[t - hi, t - lo] x [d_lo, d_hi]`. The profile is linear on the cells cut out by the lines
//! `u = o` (start-anchored ticks), `u = d + o` (end-anchored ticks) and their integer
//! neighbours, so the extreme lies at a vertex of that arrangement inside the box. The
//! candidates are therefore: for `u`, the box ends, every start offset and the tick before it,
//! and every `d_lo + o`, `d_hi + o` and the tick before; for `d`, the box ends and, for each
//! candidate `u`, every `u - o` and the tick after.

use super::contingent::{Anchor, Uncertain};
use crate::{ActivityId, Error, Impact, Tick, Window};

/// Which extreme of the level an evaluation asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Extreme {
    /// The lowest level over all admitted schedules.
    Lower,
    /// The highest level over all admitted schedules.
    Upper,
}

impl Extreme {
    /// The better of `a` and `b` for this extreme.
    const fn pick(self, a: i128, b: i128) -> i128 {
        match self {
            Self::Lower if b < a => b,
            Self::Upper if b > a => b,
            Self::Lower | Self::Upper => a,
        }
    }
}

/// The impacts of a member: fixed offsets, or anchored at a start and an uncertain end.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Shape<'a> {
    /// Impacts relative to the start; the duration plays no role.
    Fixed(&'a [Impact]),
    /// Impacts anchored at the start or the end, with the possible durations.
    Uncertain {
        /// The possible durations.
        duration: Window,
        /// The anchored impacts.
        impacts: &'a [Uncertain],
    },
}

/// One activity of an envelope.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Member<'a> {
    /// The activity, reported back in explanations.
    pub(crate) activity: ActivityId,
    /// The start window as given, before propagation.
    pub(crate) window: Window,
    /// The impacts.
    pub(crate) shape: Shape<'a>,
}

impl Member<'_> {
    /// The possible durations: `[0, 0]` for fixed impacts.
    pub(crate) const fn duration(&self) -> Window {
        match self.shape {
            Shape::Fixed(_) => Window::at(0),
            Shape::Uncertain { duration, .. } => duration,
        }
    }

    /// Whether the member has fixed impacts without rates, so that its contribution at a
    /// tick is a sum of steps at fixed offsets.
    pub(crate) fn is_stepwise(&self) -> bool {
        match self.shape {
            Shape::Fixed(impacts) => !impacts.iter().any(Impact::is_rate),
            Shape::Uncertain { .. } => false,
        }
    }

    /// The anchors of every impact.
    pub(crate) fn anchors(&self) -> impl Iterator<Item = Anchor> + '_ {
        let (fixed, uncertain): (&[Impact], &[Uncertain]) = match self.shape {
            Shape::Fixed(impacts) => (impacts, &[]),
            Shape::Uncertain { impacts, .. } => (&[], impacts),
        };
        let fixed = fixed.iter().flat_map(|impact| {
            [
                Anchor::Start(impact.first_tick()),
                Anchor::Start(impact.last_tick()),
            ]
        });
        fixed.chain(uncertain.iter().flat_map(Uncertain::anchors))
    }

    /// The ticks, relative to the start, at which the profile can change for some duration:
    /// start offsets, and end offsets at the shortest and the longest duration.
    pub(crate) fn offsets(&self) -> impl Iterator<Item = i128> + '_ {
        let duration = self.duration();
        self.anchors()
            .flat_map(move |anchor| match anchor {
                Anchor::Start(offset) => [Some(i128::from(offset)), None],
                Anchor::End(offset) => [
                    Some(i128::from(duration.earliest()) + i128::from(offset)),
                    Some(i128::from(duration.latest()) + i128::from(offset)),
                ],
            })
            .flatten()
    }

    /// The extreme contribution at tick `t` over all starts in `window` and all durations.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a contribution exceeds `i128`.
    pub(crate) fn extreme_at(
        &self,
        window: Window,
        t: Tick,
        extreme: Extreme,
    ) -> Result<i128, Error> {
        let first = i128::from(t) - i128::from(window.latest());
        let last = i128::from(t) - i128::from(window.earliest());
        let duration = self.duration();
        let (d_lo, d_hi) = (
            i128::from(duration.earliest()),
            i128::from(duration.latest()),
        );
        let inner = self.offsets().flat_map(|offset| [offset - 1, offset]);

        let mut best: Option<i128> = None;
        for u in [first, last].into_iter().chain(inner) {
            if u < first || u > last {
                continue;
            }

            for d in self.duration_candidates(u, d_lo, d_hi) {
                let value = self.profile_at(u, d)?;
                best = Some(best.map_or(value, |best| extreme.pick(best, value)));
            }
        }

        Ok(best.unwrap_or(0))
    }

    /// The durations worth evaluating at relative time `u`: the range ends and, for each
    /// end-anchored offset `o`, `u - o` and the tick after, inside `[d_lo, d_hi]`.
    fn duration_candidates(
        &self,
        u: i128,
        d_lo: i128,
        d_hi: i128,
    ) -> impl Iterator<Item = i128> + '_ {
        let ends = self.anchors().flat_map(move |anchor| match anchor {
            Anchor::Start(_) => [None, None],
            Anchor::End(offset) => {
                let meet = u - i128::from(offset);
                [Some(meet), Some(meet + 1)]
            }
        });
        [Some(d_lo), Some(d_hi)]
            .into_iter()
            .chain(ends)
            .flatten()
            .filter(move |&d| d_lo <= d && d <= d_hi)
    }

    /// The summed contribution at relative time `u` for duration `d`.
    ///
    /// `u` may lie outside the [`Tick`] range. Below it nothing has happened yet; above it the
    /// value is the one at [`Tick::MAX`], because every impact ends by then.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a contribution exceeds `i128` or an anchored tick does not fit.
    pub(crate) fn profile_at(&self, u: i128, d: i128) -> Result<i128, Error> {
        if u < i128::from(Tick::MIN) {
            return Ok(0);
        }

        let u = Tick::try_from(u.min(i128::from(Tick::MAX))).map_err(|_| Error::Overflow)?;
        let sum = |sum: i128, impact: Impact| -> Result<i128, Error> {
            let contribution = impact.contribution_at(u).ok_or(Error::Overflow)?;
            sum.checked_add(contribution).ok_or(Error::Overflow)
        };
        match self.shape {
            Shape::Fixed(impacts) => impacts.iter().copied().try_fold(0, sum),
            Shape::Uncertain { impacts, .. } => {
                let d = Tick::try_from(d).map_err(|_| Error::Overflow)?;
                impacts.iter().try_fold(0, |total, impact| {
                    sum(total, impact.resolve(d).ok_or(Error::Overflow)?)
                })
            }
        }
    }
}
