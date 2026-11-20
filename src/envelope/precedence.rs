//! Precedence constraints between envelope activities: window propagation and coupled extremes.
//!
//! A [`Precedence`] requires `start(after) - start(before) >= gap`. Together with the start
//! windows this is a simple temporal network over the activity starts. The envelope uses it
//! twice:
//!
//! 1. **Propagation.** All-pairs shortest paths (Floyd-Warshall) over the distance graph give
//!    the tightest window of each start and the tightest separation of each pair. Every
//!    admitted schedule stays inside the tightened windows, so an envelope over them is still
//!    sound, and tighter.
//! 2. **Coupled extremes.** For activities with stepwise impacts (steps and claims at fixed
//!    offsets) that take part in a precedence, the extreme level at tick `t` is computed jointly,
//!    following Muscettola's envelope construction: every impact tick ("event") is either
//!    certainly at or before `t`, certainly after it, or pending. The extreme over all
//!    schedules is the forced part plus a maximum-weight closure of the pending events under
//!    the "must not be later than" relation that the separations imply, found with a minimum
//!    cut. This is exact for those activities; activities with rates or uncertain durations
//!    are added with their own extremes over the tightened windows, which keeps the sum sound.
//!
//! ```mermaid
//! graph LR
//!     windows["windows and precedences"] --> fw["Floyd-Warshall: tightened windows, separations"]
//!     fw --> events["events at t: forced, pending"]
//!     events --> cut["max-weight closure by min cut"]
//!     cut --> level["coupled extreme level"]
//! ```

use alloc::vec;
use alloc::vec::Vec;

use super::closure::max_weight_closure;
use super::member::{Extreme, Member, Shape};
use crate::{ActivityId, Error, Tick, Window};

/// A required order between the starts of two envelope activities:
/// `start(after) - start(before) >= gap`.
///
/// `gap` may be negative (`after` may start up to `-gap` ticks before `before`) and is
/// typically the duration of `before` for "`after` starts once `before` has ended". For an
/// activity with an uncertain duration, the shortest duration is a sound gap: the true
/// constraint "after its actual end" only admits fewer schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Precedence {
    /// The activity that starts first. Every envelope activity with this id is constrained.
    pub before: ActivityId,
    /// The activity that starts later. Every envelope activity with this id is constrained.
    pub after: ActivityId,
    /// The smallest allowed distance between the two starts, in ticks.
    pub gap: Tick,
}

/// The temporal network over the starts of the members: tightened windows and separations.
#[derive(Debug, Clone)]
pub(crate) struct Network {
    /// The number of members.
    members: usize,
    /// `distance[i * members + j]`: the largest possible `start(j) - start(i)`, `None` if
    /// unbounded.
    distance: Vec<Option<i128>>,
    /// Whether member `i` takes part in the coupled (closure) extreme.
    coupled: Vec<bool>,
}

impl Network {
    /// Propagates `precedences` over the windows of `members` and returns the tightened
    /// windows with the network.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownActivity`] if a precedence names an activity that is not a member.
    /// - [`Error::InconsistentPrecedences`] if no schedule satisfies the windows and the
    ///   precedences together.
    /// - [`Error::OutOfMemory`] if the distance matrix cannot be allocated.
    pub(crate) fn propagate(
        members: &[Member<'_>],
        precedences: &[Precedence],
    ) -> Result<(Vec<Window>, Self), Error> {
        let count = members.len();
        let nodes = count + 1;
        let mut matrix = Matrix::new(nodes)?;
        for (index, member) in members.iter().enumerate() {
            let node = index + 1;
            matrix.tighten(0, node, i128::from(member.window.latest()));
            matrix.tighten(node, 0, -i128::from(member.window.earliest()));
        }

        let mut coupled = vec![false; count];
        for precedence in precedences {
            let before = indices_of(members, precedence.before)?;
            let after = indices_of(members, precedence.after)?;
            for b in before.clone() {
                for a in after.clone() {
                    // start(a) - start(b) >= gap  <=>  start(b) - start(a) <= -gap.
                    matrix.tighten(a + 1, b + 1, -i128::from(precedence.gap));
                    coupled[a] = true;
                    coupled[b] = true;
                }
            }
        }

        matrix.close()?;
        let windows = (0..count)
            .map(|index| matrix.window(index + 1))
            .collect::<Result<Vec<_>, _>>()?;
        for (flag, member) in coupled.iter_mut().zip(members) {
            *flag &= member.is_stepwise();
        }

        let mut distance = Vec::new();
        distance
            .try_reserve_exact(count * count)
            .map_err(|_| Error::OutOfMemory)?;
        for i in 0..count {
            distance.extend((0..count).map(|j| matrix.get(i + 1, j + 1)));
        }

        let network = Self {
            members: count,
            distance,
            coupled,
        };
        Ok((windows, network))
    }

    /// Whether member `index` is part of the coupled extreme.
    pub(crate) fn is_coupled(&self, index: usize) -> bool {
        self.coupled[index]
    }

    /// The extreme summed contribution of the coupled members at tick `t`.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a sum exceeds `i128`; [`Error::OutOfMemory`] if the event graph
    /// cannot be allocated.
    pub(crate) fn coupled_extreme(
        &self,
        members: &[Member<'_>],
        windows: &[Window],
        t: Tick,
        extreme: Extreme,
    ) -> Result<i128, Error> {
        let sign = match extreme {
            Extreme::Upper => 1,
            Extreme::Lower => -1,
        };
        let mut forced: i128 = 0;
        let mut pending: Vec<Event> = Vec::new();
        for (index, member) in members.iter().enumerate() {
            if !self.coupled[index] {
                continue;
            }

            let Shape::Fixed(impacts) = member.shape else {
                continue;
            };
            for (offset, jump) in jumps(impacts) {
                let earliest = i128::from(windows[index].earliest()) + i128::from(offset);
                let latest = i128::from(windows[index].latest()) + i128::from(offset);
                let t = i128::from(t);
                if latest <= t {
                    forced = forced.checked_add(jump).ok_or(Error::Overflow)?;
                } else if earliest <= t {
                    pending.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    pending.push(Event {
                        member: index,
                        offset,
                        weight: sign * jump,
                    });
                }
            }
        }

        let weights: Vec<i128> = pending.iter().map(|event| event.weight).collect();
        let best = max_weight_closure(&weights, |x, y| self.not_later(&pending[y], &pending[x]))?;
        forced.checked_add(sign * best).ok_or(Error::Overflow)
    }

    /// Whether event `x` is never later than event `y` in any admitted schedule, so that
    /// `y` at or before a tick forces `x` there too.
    fn not_later(&self, x: &Event, y: &Event) -> bool {
        // time(y) - time(x) = start(y) - start(x) + offset(y) - offset(x), and the smallest
        // start(y) - start(x) is the negated largest start(x) - start(y).
        self.distance[y.member * self.members + x.member]
            .is_some_and(|largest| -largest + i128::from(y.offset) - i128::from(x.offset) >= 0)
    }
}

/// A pending impact tick of a coupled member.
#[derive(Debug, Clone, Copy)]
struct Event {
    /// The member index.
    member: usize,
    /// The tick relative to the member's start.
    offset: Tick,
    /// The level jump at the tick, negated for the lower extreme.
    weight: i128,
}

/// The net level jumps of stepwise `impacts` per relative tick, sorted by tick, without zero
/// jumps.
fn jumps(impacts: &[crate::Impact]) -> Vec<(Tick, i128)> {
    let mut jumps: Vec<(Tick, i128)> = impacts
        .iter()
        .flat_map(crate::Impact::changes)
        .flatten()
        .map(|(at, step, _)| (at, step))
        .collect();
    jumps.sort_unstable_by_key(|&(at, _)| at);
    jumps.dedup_by(|later, earlier| {
        let same = later.0 == earlier.0;
        if same {
            earlier.1 += later.1;
        }

        same
    });
    jumps.retain(|&(_, jump)| jump != 0);
    jumps
}

/// The indices of the members with id `activity`.
///
/// # Errors
///
/// [`Error::UnknownActivity`] if there is none.
fn indices_of<'m>(
    members: &'m [Member<'_>],
    activity: ActivityId,
) -> Result<impl Iterator<Item = usize> + Clone + 'm, Error> {
    if !members.iter().any(|member| member.activity == activity) {
        return Err(Error::UnknownActivity { activity });
    }

    Ok(members
        .iter()
        .enumerate()
        .filter(move |(_, member)| member.activity == activity)
        .map(|(index, _)| index))
}

/// A dense distance matrix of a simple temporal network, node 0 being the origin.
struct Matrix {
    /// The number of nodes.
    nodes: usize,
    /// `cells[x * nodes + y]`: the largest allowed `time(y) - time(x)`, `None` if unbounded.
    cells: Vec<Option<i128>>,
}

impl Matrix {
    /// A matrix with zero diagonal and no other constraint.
    fn new(nodes: usize) -> Result<Self, Error> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(nodes * nodes)
            .map_err(|_| Error::OutOfMemory)?;
        cells.resize(nodes * nodes, None);
        for node in 0..nodes {
            cells[node * nodes + node] = Some(0);
        }

        Ok(Self { nodes, cells })
    }

    /// The largest allowed `time(y) - time(x)`.
    fn get(&self, x: usize, y: usize) -> Option<i128> {
        self.cells[x * self.nodes + y]
    }

    /// Requires `time(y) - time(x) <= bound`.
    fn tighten(&mut self, x: usize, y: usize, bound: i128) {
        let cell = &mut self.cells[x * self.nodes + y];
        *cell = Some(cell.map_or(bound, |current| current.min(bound)));
    }

    /// Floyd-Warshall: replaces every cell by the shortest path.
    ///
    /// # Errors
    ///
    /// [`Error::InconsistentPrecedences`] on a negative cycle.
    fn close(&mut self) -> Result<(), Error> {
        let n = self.nodes;
        for k in 0..n {
            for x in 0..n {
                let Some(via) = self.get(x, k) else {
                    continue;
                };

                for y in 0..n {
                    if let Some(rest) = self.get(k, y) {
                        self.tighten(x, y, via + rest);
                    }
                }
            }
        }

        if (0..n).any(|node| self.get(node, node).is_some_and(|loop_| loop_ < 0)) {
            return Err(Error::InconsistentPrecedences);
        }

        Ok(())
    }

    /// The tightened window of `node` relative to the origin.
    fn window(&self, node: usize) -> Result<Window, Error> {
        let latest = self.get(0, node).ok_or(Error::Overflow)?;
        let earliest = -self.get(node, 0).ok_or(Error::Overflow)?;
        let latest = Tick::try_from(latest).map_err(|_| Error::Overflow)?;
        let earliest = Tick::try_from(earliest).map_err(|_| Error::Overflow)?;
        Window::new(earliest, latest)
    }
}
