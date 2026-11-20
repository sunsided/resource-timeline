//! A greedy scheduler: place activities by priority at their earliest feasible start.
//!
//! This is deliberately simple. It never moves an activity once placed and never backtracks,
//! so it can fail to place an activity that a smarter order would fit. It serves as an example
//! of the [`Plan`] API and as a test vehicle; an onboard scheduler would typically run it to
//! repair a plan after a change, with a ground-planned order as the priorities.

use alloc::vec::Vec;

use crate::{ActivityId, Demand, Error, Plan, Tick, Window};

/// An activity the greedy scheduler should place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Request<'a> {
    /// The id the placed impacts are tagged with.
    pub activity: ActivityId,
    /// Higher values are placed first. Ties go to the smaller activity id, then to the
    /// earlier request.
    pub priority: u32,
    /// The allowed starts.
    pub window: Window,
    /// The effects of the activity, relative to its start.
    pub demands: &'a [Demand],
}

/// Places every request at the earliest start in its window at which it fits, in priority
/// order, and returns the chosen start per request (in the order of `requests`), or `None` for
/// requests that did not fit anywhere.
///
/// Each request's search may check at most `max_checks` candidate starts, see
/// [`Plan::earliest_start`]. Requests already placed stay in `plan`; a request that does not
/// fit leaves it unchanged.
///
/// # Errors
///
/// The errors of [`Plan::earliest_start`] and [`Plan::place`], and [`Error::OutOfMemory`] if
/// the order or the result cannot be allocated. On error, requests placed before the failing
/// one remain in `plan`.
///
/// # Examples
///
/// ```
/// use resource_timeline::greedy::{Request, schedule};
/// use resource_timeline::{ActivityId, Demand, Impact, Plan, Timeline, Window};
///
/// let mut plan = Plan::new();
/// let antenna = plan.add_timeline(Timeline::claimable(0, 1, 16)?)?;
/// let downlink = [Demand::Level {
///     timeline: antenna,
///     impact: Impact::Claim { start: 0, end: 10, amount: 1 },
/// }];
/// let window = Window::new(0, 15)?;
/// let requests = [
///     Request { activity: ActivityId(1), priority: 1, window, demands: &downlink },
///     Request { activity: ActivityId(2), priority: 5, window, demands: &downlink },
///     Request { activity: ActivityId(3), priority: 3, window, demands: &downlink },
/// ];
///
/// // Priority 5 goes first, priority 3 next; priority 1 no longer fits by tick 15.
/// assert_eq!(schedule(&mut plan, &requests, 1_000)?, [None, Some(0), Some(10)]);
/// # Ok::<(), resource_timeline::Error>(())
/// ```
pub fn schedule(
    plan: &mut Plan,
    requests: &[Request<'_>],
    max_checks: usize,
) -> Result<Vec<Option<Tick>>, Error> {
    let order = priority_order(requests)?;
    let mut starts = Vec::new();
    starts
        .try_reserve_exact(requests.len())
        .map_err(|_| Error::OutOfMemory)?;
    starts.resize(requests.len(), None);

    for index in order {
        let request = &requests[index];
        starts[index] = place_earliest(plan, request, max_checks)?;
    }

    Ok(starts)
}

/// The indices of `requests`, highest priority first, ties by activity id and then by index.
fn priority_order(requests: &[Request<'_>]) -> Result<Vec<usize>, Error> {
    let mut order = Vec::new();
    order
        .try_reserve_exact(requests.len())
        .map_err(|_| Error::OutOfMemory)?;
    order.extend(0..requests.len());
    order.sort_by_key(|&index| {
        let request = &requests[index];
        (core::cmp::Reverse(request.priority), request.activity)
    });
    Ok(order)
}

/// Places `request` at its earliest feasible start and returns that start, if any.
fn place_earliest(
    plan: &mut Plan,
    request: &Request<'_>,
    max_checks: usize,
) -> Result<Option<Tick>, Error> {
    let start = plan.earliest_start(request.demands, request.window, max_checks)?;
    if let Some(start) = start {
        plan.place(request.activity, start, request.demands)?;
    }

    Ok(start)
}
