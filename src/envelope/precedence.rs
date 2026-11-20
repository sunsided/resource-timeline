//! Precedence constraints between envelope activities: propagation and coupled extremes.
//!
//! A [`Precedence`] requires `time(after, to) - time(before, from) >= gap` between the starts
//! or ends of two activities. With the start windows and the duration ranges of contingent
//! activities (`end - start` in `[shortest, longest]`) this is a simple temporal network over
//! the starts and ends. The envelope uses it twice:
//!
//! 1. **Propagation.** All-pairs shortest paths (Floyd-Warshall) give the tightest window of
//!    each start, the tightest duration range of each contingent activity, and the tightest
//!    separation of each pair of time points. Every admitted schedule stays inside them, so
//!    separate extremes over them are still sound, and tighter.
//! 2. **Coupled extremes** of the activities that take part in a precedence:
//!    - all stepwise (steps and claims at fixed offsets): Muscettola's construction. Every
//!      impact tick is forced at or before `t`, forced after it, or pending; the extreme is the
//!      forced part plus a maximum-weight closure of the pending events under "never later
//!      than", found by a minimum cut. Exact, and independent of the window lengths.
//!    - with rates or uncertain durations: every impact is a separable function of its time
//!      points (a step an indicator, a claim a difference of two indicators, a rate a difference
//!      of two clamped ramps), so the extreme is the maximum of a separable function under
//!      difference constraints, which the discretised minimum cut of the `cut` module solves
//!      exactly. Its graph has a node per domain value, so it is used only while the domains
//!      fit a node budget ([`Envelope::with_cut_budget`](super::Envelope::with_cut_budget)).
//!    - above the budget: closure for the stepwise activities, separate extremes over the
//!      tightened windows and durations for the others. Sound, but a possible violation may be
//!      a false alarm.
//!
//! ```mermaid
//! graph LR
//!     windows["windows and precedences"] --> fw["Floyd-Warshall: tightened windows, separations"]
//!     fw --> events["stepwise: events forced or pending"]
//!     events --> closure["max-weight closure by min cut"]
//!     fw --> layered["rates, durations: layered graph per time point"]
//!     layered --> cut["min cut, within the node budget"]
//!     closure --> level["coupled extreme level"]
//!     cut --> level
//! ```

use alloc::vec;
use alloc::vec::Vec;

use super::closure::max_weight_closure;
use super::contingent::{Anchor, Uncertain};
use super::cut::{Variable, max_separable, nodes_for};
use super::member::{Extreme, Member, Shape};
use crate::{ActivityId, Error, Tick, Window};

/// Which time point of an activity a [`Precedence`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    /// The start.
    Start,
    /// The end: for a [`Contingent`](super::Contingent) activity the start plus its uncertain
    /// duration, for a [`Flexible`](super::Flexible) activity the start plus the last tick of
    /// its impacts (the start itself without impacts).
    End,
}

/// A required order between time points of two envelope activities:
/// `time(after, to) - time(before, from) >= gap`.
///
/// The four combinations of [`Endpoint`]s give start-start, start-end, end-start and end-end
/// constraints. `gap` may be negative. "`after` starts once `before` has ended" is
/// [`Precedence::after_end`]; for a contingent `before` that waits for its actual end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Precedence {
    /// The earlier activity. Every envelope activity with this id is constrained.
    pub before: ActivityId,
    /// The time point of `before` the gap is measured from.
    pub from: Endpoint,
    /// The later activity. Every envelope activity with this id is constrained.
    pub after: ActivityId,
    /// The time point of `after` the gap is measured to.
    pub to: Endpoint,
    /// The smallest allowed distance between the two time points, in ticks.
    pub gap: Tick,
}

impl Precedence {
    /// `start(after) - start(before) >= gap`.
    #[must_use]
    pub const fn starts(before: ActivityId, after: ActivityId, gap: Tick) -> Self {
        Self {
            before,
            from: Endpoint::Start,
            after,
            to: Endpoint::Start,
            gap,
        }
    }

    /// `start(after) - end(before) >= gap`: `after` starts `gap` ticks after `before` ended.
    #[must_use]
    pub const fn after_end(before: ActivityId, after: ActivityId, gap: Tick) -> Self {
        Self {
            before,
            from: Endpoint::End,
            after,
            to: Endpoint::Start,
            gap,
        }
    }
}

/// How the coupled members are evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Every coupled member is stepwise: maximum-weight closure over pending events, exact.
    Closure,
    /// Some coupled member has rates or an uncertain duration, and the domains fit the cut
    /// budget: the discretised minimum cut over all coupled time points, exact.
    Cut,
    /// The domains exceed the cut budget: closure over the stepwise members, separate
    /// extremes over tightened windows and durations for the others, sound.
    Partial,
}

/// The temporal network over the time points of the members.
#[derive(Debug, Clone)]
pub(crate) struct Network {
    /// The number of nodes, the origin being node 0.
    nodes: usize,
    /// `distance[x * nodes + y]`: the largest possible `time(y) - time(x)`, `None` if
    /// unbounded.
    distance: Vec<Option<i128>>,
    /// The start node of each member.
    start: Vec<usize>,
    /// The end node of each contingent member.
    end: Vec<Option<usize>>,
    /// Whether member `i` is evaluated jointly.
    coupled: Vec<bool>,
    /// How the jointly evaluated members are evaluated.
    mode: Mode,
}

/// The windows and durations a propagation tightened.
pub(crate) struct Tightened {
    /// The start window of each member.
    pub(crate) windows: Vec<Window>,
    /// The duration range of each member (`[0, 0]` for fixed impacts).
    pub(crate) durations: Vec<Window>,
}

impl Network {
    /// Propagates `precedences` over the windows and durations of `members`.
    ///
    /// Coupled members are evaluated by closure if all are stepwise, by the discretised cut
    /// if their time-point domains need at most `budget` graph nodes, and partly separately
    /// otherwise.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownActivity`] if a precedence names an activity that is not a member.
    /// - [`Error::InconsistentPrecedences`] if no schedule satisfies the windows, durations
    ///   and precedences together.
    /// - [`Error::OutOfMemory`] if the distance matrix cannot be allocated.
    pub(crate) fn propagate(
        members: &[Member<'_>],
        precedences: &[Precedence],
        budget: usize,
    ) -> Result<(Tightened, Self), Error> {
        let (start, end, nodes) = number_nodes(members);
        let mut matrix = Matrix::new(nodes)?;
        for (index, member) in members.iter().enumerate() {
            matrix.tighten(0, start[index], i128::from(member.window.latest()));
            matrix.tighten(start[index], 0, -i128::from(member.window.earliest()));
            if let Some(end) = end[index] {
                let duration = member.duration();
                matrix.tighten(start[index], end, i128::from(duration.latest()));
                matrix.tighten(end, start[index], -i128::from(duration.earliest()));
            }
        }

        let point = |index: usize, endpoint: Endpoint| match (endpoint, end[index]) {
            (Endpoint::Start, _) => (start[index], 0),
            (Endpoint::End, Some(end)) => (end, 0),
            (Endpoint::End, None) => (start[index], i128::from(members[index].span())),
        };
        let mut coupled = vec![false; members.len()];
        for precedence in precedences {
            let before = indices_of(members, precedence.before)?;
            let after = indices_of(members, precedence.after)?;
            for b in before.clone() {
                for a in after.clone() {
                    let (from, from_offset) = point(b, precedence.from);
                    let (to, to_offset) = point(a, precedence.to);
                    // to + to_offset - (from + from_offset) >= gap
                    // <=> from - to <= to_offset - from_offset - gap.
                    let bound = to_offset - from_offset - i128::from(precedence.gap);
                    matrix.tighten(to, from, bound);
                    coupled[a] = true;
                    coupled[b] = true;
                }
            }
        }

        matrix.close()?;
        let tightened = tighten(members, &matrix, &start, &end)?;
        let mut network = Self {
            nodes,
            distance: matrix.cells,
            start,
            end,
            coupled,
            mode: Mode::Closure,
        };
        network.choose_mode(members, budget);
        Ok((tightened, network))
    }

    /// Picks the evaluation mode of the coupled members and drops members that the partial
    /// mode evaluates separately.
    fn choose_mode(&mut self, members: &[Member<'_>], budget: usize) {
        let coupled = || (0..members.len()).filter(|&index| self.coupled[index]);
        if coupled().all(|index| members[index].is_stepwise()) {
            self.mode = Mode::Closure;
            return;
        }

        let domains = coupled()
            .flat_map(|index| [Some(self.start[index]), self.end[index]])
            .flatten()
            .map(|node| {
                let (lo, hi) = self.domain(node);
                usize::try_from(hi - lo + 1).unwrap_or(usize::MAX)
            });
        if nodes_for(domains) <= budget {
            self.mode = Mode::Cut;
            return;
        }

        self.mode = Mode::Partial;
        for (flag, member) in self.coupled.iter_mut().zip(members) {
            *flag &= member.is_stepwise();
        }
    }

    /// The largest possible `time(y) - time(x)`.
    fn distance(&self, x: usize, y: usize) -> Option<i128> {
        self.distance[x * self.nodes + y]
    }

    /// The tightened domain `[lo, hi]` of `node`.
    fn domain(&self, node: usize) -> (i128, i128) {
        let hi = self.distance(0, node).unwrap_or(i128::MAX);
        let lo = self.distance(node, 0).map_or(i128::MIN, |bound| -bound);
        (lo, hi)
    }

    /// Whether member `index` is part of the coupled extreme.
    pub(crate) fn is_coupled(&self, index: usize) -> bool {
        self.coupled[index]
    }

    /// For the cut mode with rates or uncertain durations: the tick ranges, relative to which
    /// every tick may be an evaluation tick, as `(lo, hi)` of every coupled time point's domain
    /// plus each offset of the member's anchors. Empty in the other modes.
    pub(crate) fn dense_ranges<'m>(
        &'m self,
        members: &'m [Member<'m>],
    ) -> impl Iterator<Item = (i128, i128)> + 'm {
        let active = self.mode == Mode::Cut;
        (0..members.len())
            .filter(move |&index| active && self.coupled[index] && !members[index].is_stepwise())
            .flat_map(move |index| {
                members[index].anchored().flat_map(move |impact| {
                    impact.anchors().into_iter().map(move |anchor| {
                        let (node, offset) = self.anchor_point(index, anchor);
                        let (lo, hi) = self.domain(node);
                        (lo + offset, hi + offset)
                    })
                })
            })
    }

    /// The node and offset of `anchor` of member `index`.
    fn anchor_point(&self, index: usize, anchor: Anchor) -> (usize, i128) {
        match (anchor, self.end[index]) {
            (Anchor::End(offset), Some(end)) => (end, i128::from(offset)),
            // Fixed impacts only carry start anchors; an end anchor without an end node is
            // measured from the start like them.
            (Anchor::Start(offset) | Anchor::End(offset), _) => {
                (self.start[index], i128::from(offset))
            }
        }
    }

    /// The extreme summed contribution of the coupled members at tick `t`.
    ///
    /// # Errors
    ///
    /// [`Error::Overflow`] if a sum exceeds `i128`; [`Error::OutOfMemory`] if the graph
    /// cannot be allocated.
    pub(crate) fn coupled_extreme(
        &self,
        members: &[Member<'_>],
        t: Tick,
        extreme: Extreme,
    ) -> Result<i128, Error> {
        match self.mode {
            Mode::Cut => self.cut_extreme(members, t, extreme),
            Mode::Closure | Mode::Partial => self.closure_extreme(members, t, extreme),
        }
    }

    /// The coupled extreme by the discretised minimum cut over every coupled time point.
    fn cut_extreme(
        &self,
        members: &[Member<'_>],
        t: Tick,
        extreme: Extreme,
    ) -> Result<i128, Error> {
        let sign: i128 = match extreme {
            Extreme::Upper => 1,
            Extreme::Lower => -1,
        };
        let mut nodes: Vec<usize> = Vec::new();
        let mut variables: Vec<Variable> = Vec::new();
        for index in (0..members.len()).filter(|&index| self.coupled[index]) {
            for node in [Some(self.start[index]), self.end[index]]
                .into_iter()
                .flatten()
            {
                let (lo, hi) = self.domain(node);
                let size = usize::try_from(hi - lo + 1).map_err(|_| Error::Overflow)?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(size)
                    .map_err(|_| Error::OutOfMemory)?;
                values.resize(size, 0);
                nodes.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                variables.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                nodes.push(node);
                variables.push(Variable { lo, values });
            }

            for impact in members[index].anchored() {
                self.add_terms(index, impact, i128::from(t), sign, &nodes, &mut variables)?;
            }
        }

        max_separable(&variables, |p, q| self.distance(nodes[p], nodes[q])).map(|best| sign * best)
    }

    /// Adds the contribution of `impact` of member `index` at tick `t`, times `sign`, to the
    /// values of the variables of its time points.
    fn add_terms(
        &self,
        index: usize,
        impact: Uncertain,
        t: i128,
        sign: i128,
        nodes: &[usize],
        variables: &mut [Variable],
    ) -> Result<(), Error> {
        // Every impact is separable: a step is an indicator of its tick, a claim the difference
        // of two indicators, a rate the difference of two clamped ramps.
        let mut add = |anchor: Anchor, term: &dyn Fn(i128) -> i128| -> Result<(), Error> {
            let (node, offset) = self.anchor_point(index, anchor);
            let position = nodes
                .iter()
                .position(|&other| other == node)
                .ok_or(Error::Overflow)?;
            let variable = &mut variables[position];
            for (k, value) in variable.values.iter_mut().enumerate() {
                let x = variable.lo + i128::try_from(k).map_err(|_| Error::Overflow)?;
                *value = value
                    .checked_add(sign * term(x + offset))
                    .ok_or(Error::Overflow)?;
            }

            Ok(())
        };
        let reached = |tick: i128| i128::from(tick <= t);
        let ramp = |tick: i128| (t - tick).max(0);
        match impact {
            Uncertain::Step { at, delta } => add(at, &|tick| i128::from(delta) * reached(tick)),
            Uncertain::Claim { start, end, amount } => {
                let amount = i128::from(amount);
                add(start, &|tick| -amount * reached(tick))?;
                add(end, &|tick| amount * reached(tick))
            }
            Uncertain::Rate {
                start,
                end,
                per_tick,
            } => {
                let rate = i128::from(per_tick);
                add(start, &|tick| rate * ramp(tick))?;
                add(end, &|tick| -rate * ramp(tick))
            }
        }
    }

    /// The coupled extreme of the stepwise coupled members by maximum-weight closure.
    fn closure_extreme(
        &self,
        members: &[Member<'_>],
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
            let (lo, hi) = self.domain(self.start[index]);
            for (offset, jump) in jumps(impacts) {
                let (earliest, latest) = (lo + i128::from(offset), hi + i128::from(offset));
                let t = i128::from(t);
                if latest <= t {
                    forced = forced.checked_add(jump).ok_or(Error::Overflow)?;
                } else if earliest <= t {
                    pending.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    pending.push(Event {
                        node: self.start[index],
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
        // time(y) - time(x) = node(y) - node(x) + offset(y) - offset(x), and the smallest
        // node(y) - node(x) is the negated largest node(x) - node(y).
        self.distance(y.node, x.node)
            .is_some_and(|largest| -largest + i128::from(y.offset) - i128::from(x.offset) >= 0)
    }
}

/// The start node of each member, the end node of each contingent member, and the node count.
fn number_nodes(members: &[Member<'_>]) -> (Vec<usize>, Vec<Option<usize>>, usize) {
    let mut next = 1;
    let mut start = Vec::with_capacity(members.len());
    let mut end = Vec::with_capacity(members.len());
    for member in members {
        start.push(next);
        next += 1;
        if matches!(member.shape, Shape::Uncertain { .. }) {
            end.push(Some(next));
            next += 1;
        } else {
            end.push(None);
        }
    }

    (start, end, next)
}

/// The tightened start windows and durations from a closed matrix.
fn tighten(
    members: &[Member<'_>],
    matrix: &Matrix,
    start: &[usize],
    end: &[Option<usize>],
) -> Result<Tightened, Error> {
    let mut windows = Vec::new();
    let mut durations = Vec::new();
    windows
        .try_reserve_exact(members.len())
        .map_err(|_| Error::OutOfMemory)?;
    durations
        .try_reserve_exact(members.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (index, member) in members.iter().enumerate() {
        windows.push(matrix.window(start[index])?);
        let duration = match end[index] {
            Some(end) => {
                let longest = matrix.get(start[index], end).ok_or(Error::Overflow)?;
                let shortest = -matrix.get(end, start[index]).ok_or(Error::Overflow)?;
                let longest = Tick::try_from(longest).map_err(|_| Error::Overflow)?;
                let shortest = Tick::try_from(shortest).map_err(|_| Error::Overflow)?;
                Window::new(shortest, longest)?
            }
            None => member.duration(),
        };
        durations.push(duration);
    }

    Ok(Tightened { windows, durations })
}

/// A pending impact tick of a coupled member.
#[derive(Debug, Clone, Copy)]
struct Event {
    /// The start node of the member.
    node: usize,
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
