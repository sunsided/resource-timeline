//! Decoupled budgets: splitting a shared resource into per-agent shares.
//!
//! Agents that share a resource (relay bandwidth, power at a shared station, a common work
//! site) cannot see each other's usage while out of contact. Budget decoupling gives each
//! agent a share of the capacity so that every agent checks only its own share onboard, and
//! the shares together never exceed the capacity: as long as each agent stays within its
//! share, the shared resource is never oversubscribed, whatever the others do.
//!
//! [`split`] computes the shares from each agent's need and a per-agent margin. Onboard, an
//! agent keeps a claimable timeline of the shared resource at its full capacity and holds the
//! part that belongs to the others with one claim per allocation period, tagged with an
//! activity id of the caller's choice (the reservation id then appears among a violation's
//! contributors, which reads as "your share is exhausted"):
//!
//! ```text
//! Impact::Claim { start: from, end: until, amount: capacity - share }
//! ```
//!
//! Every check, search and envelope of the crate then respects the share, and changing the
//! share later is a [`Timeline::remove`](crate::Timeline::remove) and two adds instead of a
//! rebuild.
//!
//! **Re-allocation.** When agents meet (directly or through a ground station), they compute
//! their remaining needs from the meeting tick on and split again. Only the agents in contact
//! can change their shares, so they split the *sum of their current shares*, never the full
//! capacity: the others keep theirs and may be using them. The need of an agent from tick `t`
//! is its peak use from `t` on, `share - lowest_from(t)` on its share timeline
//! ([`Timeline::lowest_from`](crate::Timeline::lowest_from)), or under uncertain demand
//! `share - Envelope::lowest_from(t)`, the worst case over every schedule its start windows
//! and durations admit. Both need the reservation to cover every tick from `t` on at which the
//! agent holds units: after it ends the level returns to the full capacity, and later use would
//! be under-counted, so reserve until the end of the allocation period or beyond the last claim.
//!
//! ```mermaid
//! graph LR
//!     needs["Needs: peak use per agent"] --> split["split: need + margin + even slack"]
//!     split --> shares["Shares: sum <= pool"]
//!     shares --> onboard["Onboard: claimable timeline, others' part reserved"]
//!     onboard --> contact["Contact: needs from now on"]
//!     contact --> split
//! ```

use crate::{Error, Level};

/// Splits `pool` into one share per entry of `needs`, in the same order.
///
/// Each agent gets its need plus `margin` first; whatever is left of the pool is spread
/// evenly, the remainder of the division going to the first agents. The shares therefore
/// add up to exactly `pool` (zero agents get nothing), and each share covers its need with
/// `margin` to spare. Never allocates.
///
/// The margin absorbs demand the needs do not show yet, for example work added onboard before
/// the next contact. A pool the agents should not touch at all (held back for later
/// re-allocation) is simply left out of `pool`.
///
/// # Errors
///
/// - [`Error::NegativeAmount`] if `pool`, `margin` or a need is negative.
/// - [`Error::Oversubscribed`] if the needs plus margins exceed `pool`. Nothing is clipped:
///   the caller decides which agent gives way.
///
/// # Examples
///
/// ```
/// use resource_timeline::share::split;
///
/// // 100 units of relay bandwidth; the rovers need 30 and 50, with a margin of 5 each.
/// let shares: Vec<i64> = split(100, 5, &[30, 50])?.collect();
/// assert_eq!(shares, [40, 60]);
///
/// // Later only 20 units are left between them; one rover still needs 15.
/// let shares: Vec<i64> = split(20, 0, &[0, 15])?.collect();
/// assert_eq!(shares, [3, 17]);
/// # Ok::<(), resource_timeline::Error>(())
/// ```
pub fn split(
    pool: Level,
    margin: Level,
    needs: &[Level],
) -> Result<impl ExactSizeIterator<Item = Level> + '_, Error> {
    for amount in [pool, margin].iter().chain(needs) {
        if *amount < 0 {
            return Err(Error::NegativeAmount { amount: *amount });
        }
    }

    let total = needs
        .iter()
        .map(|need| i128::from(*need) + i128::from(margin))
        .sum::<i128>();
    if total > i128::from(pool) {
        let needed = Level::try_from(total).unwrap_or(Level::MAX);
        return Err(Error::Oversubscribed { needed, pool });
    }

    // Both fit a Level: total <= pool, and the slack per agent is at most the pool.
    let agents = i128::try_from(needs.len()).unwrap_or(i128::MAX).max(1);
    let slack = i128::from(pool) - total;
    let (even, remainder) = (slack / agents, slack % agents);
    Ok(needs.iter().enumerate().map(move |(index, need)| {
        let extra = i128::from(i128::try_from(index).unwrap_or(i128::MAX) < remainder);
        let share = i128::from(*need) + i128::from(margin) + even + extra;
        Level::try_from(share).unwrap_or(Level::MAX)
    }))
}
