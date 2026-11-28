//! Zone resources: spatially exclusive workspaces that two tasks must not occupy at once.
//!
//! Construction crews, robots or mobile workers that share floor space must not work in the
//! same zone at the same time. A zone is a unary, mutually exclusive resource: a claimable
//! timeline with capacity one, which a task occupies with an [`occupy`] claim (amount one) for
//! as long as it works there. A task may occupy several zones at once, as several demands of
//! the same activity. Two overlapping occupations break the zone's lower bound, and
//! [`Conflict::of`] turns that violation into an explanation naming the zone and both tasks.
//! Every check, search and envelope of the crate applies to zones unchanged, and on
//! [`FixedPlan`](crate::storage::FixedPlan) storage they never allocate.
//!
//! Scheduling models often state spatial exclusion as an undirected overlap graph instead: an
//! edge between two tasks whose workspaces intersect. [`Overlaps`] turns such a graph into
//! zones, one per edge, occupied by exactly the two tasks of the edge. That is exact: two
//! tasks exclude each other if and only if they share an edge.
//!
//! ```mermaid
//! graph LR
//!     edges["Overlap edges between tasks"] --> overlaps["Overlaps::new: validated"]
//!     overlaps --> zones["add_zones: one unary timeline per edge"]
//!     zones --> demands["Zones::of + occupy: demands per task"]
//!     demands --> plan["Plan: fits, earliest_start, first_violation"]
//!     plan --> conflict["Conflict::of: zone and both tasks"]
//! ```
//!
//! # Examples
//!
//! ```
//! use resource_timeline::zone::{Conflict, Overlaps, occupy};
//! use resource_timeline::{ActivityId, Demand, Plan, Window};
//!
//! // Formwork (1) and rebar (2) overlap on the slab; rebar and the crane lift (3) on the bay.
//! let (formwork, rebar, lift) = (ActivityId(1), ActivityId(2), ActivityId(3));
//! let edges = [(formwork, rebar), (rebar, lift)];
//! let mut plan = Plan::new();
//! let zones = Overlaps::new(&edges)?.add_zones(&mut plan, 0, 4)?;
//!
//! // Each task works 8 hours in every zone it shares with another task.
//! let demands = |task| -> Vec<Demand> { zones.of(task).map(|zone| occupy(zone, 0, 8)).collect() };
//! plan.place(formwork, 0, &demands(formwork))?;
//! plan.place(lift, 0, &demands(lift))?;
//!
//! // Rebar waits for both; it may start once formwork and lift are done.
//! assert_eq!(plan.earliest_start(&demands(rebar), Window::new(0, 40)?, 100)?, Some(8));
//!
//! // Placing it at 4 anyway is a conflict on the slab zone.
//! plan.place(rebar, 4, &demands(rebar))?;
//! let violation = plan.first_violation()?.expect("the slab is double-booked");
//! let conflict = Conflict::of(&plan, &violation).expect("a zone conflict");
//! assert_eq!(conflict.zone, zones.zone(0).expect("edge 0"));
//! assert_eq!((conflict.at, conflict.first, conflict.second), (4, formwork, rebar));
//! # Ok::<(), resource_timeline::Error>(())
//! ```

use crate::storage::{PlanStorage, Storage};
use crate::{
    ActivityId, Bound, Demand, Error, Impact, Kind, Plan, PlanViolation, Tick, Timeline, TimelineId,
};

/// The demand of a task that occupies `zone` from `start` to `end`, relative to its own start.
///
/// A claim of one unit on the zone's unary timeline.
#[must_use]
pub const fn occupy(zone: TimelineId, start: Tick, end: Tick) -> Demand {
    Demand::Level {
        timeline: zone,
        impact: Impact::Claim {
            start,
            end,
            amount: 1,
        },
    }
}

/// Two tasks in the same zone at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Conflict {
    /// The zone both tasks occupy.
    pub zone: TimelineId,
    /// The first tick at which both are in the zone.
    pub at: Tick,
    /// The task with the smaller id.
    pub first: ActivityId,
    /// The task with the larger id.
    pub second: ActivityId,
}

impl Conflict {
    /// The zone conflict behind `violation` of `plan`, if it is one.
    ///
    /// A zone conflict is a broken lower bound on a claimable timeline with capacity one,
    /// held by at least two activities. With more than two tasks entering the zone at the
    /// same tick, the two smallest ids are named. An activity overlapping with itself (placed
    /// twice) is no conflict between two tasks and yields `None`.
    #[must_use]
    pub fn of<P: PlanStorage>(plan: &Plan<P>, violation: &PlanViolation<P::Line>) -> Option<Self> {
        let PlanViolation::Level {
            timeline,
            violation,
        } = violation
        else {
            return None;
        };

        if !plan.timeline(*timeline).is_some_and(is_zone) || violation.bound != Bound::BelowMin(0) {
            return None;
        }

        match *violation.contributors {
            [first, second, ..] => Some(Self {
                zone: *timeline,
                at: violation.at,
                first,
                second,
            }),
            _ => None,
        }
    }
}

/// Whether `timeline` is a zone: claimable, with capacity one.
fn is_zone<S: Storage>(timeline: &Timeline<S>) -> bool {
    timeline.kind() == Kind::Claimable && timeline.bounds().max() == 1
}

/// A validated undirected overlap graph between tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Overlaps<'a> {
    /// The edges, each between two different tasks, none repeated in either orientation.
    edges: &'a [(ActivityId, ActivityId)],
}

impl<'a> Overlaps<'a> {
    /// Validates `edges`, each a pair of tasks whose workspaces intersect.
    ///
    /// Costs `O(e^2)` for `e` edges and never allocates.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidOverlap`] for the first edge that links a task with itself or repeats
    /// an earlier edge, in either orientation.
    pub fn new(edges: &'a [(ActivityId, ActivityId)]) -> Result<Self, Error> {
        for (index, &(first, second)) in edges.iter().enumerate() {
            let repeated = edges[..index]
                .iter()
                .any(|&(a, b)| (a, b) == (first, second) || (a, b) == (second, first));
            if first == second || repeated {
                return Err(Error::InvalidOverlap { first, second });
            }
        }

        Ok(Self { edges })
    }

    /// The edges, as given.
    #[must_use]
    pub const fn edges(&self) -> &'a [(ActivityId, ActivityId)] {
        self.edges
    }

    /// Adds one zone per edge to `plan`, starting at `start` with room for `budget` impacts
    /// each, and returns their handles.
    ///
    /// Two tasks share each zone, so a budget of two suffices unless tasks are placed more
    /// than once. The zones are added all or nothing.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the timeline list cannot grow (heap storage),
    /// [`Error::CapacityExceeded`] naming [`Container::Timelines`](crate::Container::Timelines)
    /// if it has no room for every zone (fixed storage).
    pub fn add_zones<P: PlanStorage>(
        self,
        plan: &mut Plan<P>,
        start: Tick,
        budget: usize,
    ) -> Result<Zones<'a>, Error> {
        plan.reserve_timelines(self.edges.len())?;
        let first = plan.timelines().len();
        for _ in self.edges {
            plan.add_timeline(Timeline::claimable_in(start, 1, budget)?)?;
        }

        Ok(Zones {
            first,
            edges: self.edges,
        })
    }
}

/// The zones of an overlap graph in a plan, one per edge; see [`Overlaps::add_zones`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Zones<'a> {
    /// The index of the first zone's timeline; the zones follow in edge order.
    first: usize,
    /// The edges, in the order of the zones.
    edges: &'a [(ActivityId, ActivityId)],
}

impl<'a> Zones<'a> {
    /// The zone of edge `index`, if there is such an edge.
    #[must_use]
    pub const fn zone(&self, index: usize) -> Option<TimelineId> {
        if index < self.edges.len() {
            Some(TimelineId(self.first + index))
        } else {
            None
        }
    }

    /// The two tasks of the edge behind `zone`, if it is one of these zones.
    #[must_use]
    pub fn edge(&self, zone: TimelineId) -> Option<(ActivityId, ActivityId)> {
        zone.0
            .checked_sub(self.first)
            .and_then(|index| self.edges.get(index).copied())
    }

    /// The zones `task` occupies: one per edge it is part of, in edge order.
    pub fn of(&self, task: ActivityId) -> impl Iterator<Item = TimelineId> + 'a {
        let first = self.first;
        self.edges
            .iter()
            .enumerate()
            .filter(move |(_, (a, b))| *a == task || *b == task)
            .map(move |(index, _)| TimelineId(first + index))
    }
}
