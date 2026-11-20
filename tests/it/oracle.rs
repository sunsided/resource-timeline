//! A brute-force reference that evaluates the level tick by tick.
//!
//! It follows the closed-form definition from the design document directly, without sorting or
//! sweeping, so it shares no logic with the implementation under test.

use resource_timeline::{ActivityId, Bound, Impact, Level, Tick, Timeline, Violation};

/// The level of `timeline` at tick `t`, from the closed-form definition.
pub fn level_at(timeline: &Timeline, t: Tick) -> i128 {
    timeline
        .impacts()
        .iter()
        .map(|&(_, impact)| effect(impact, t))
        .sum::<i128>()
        + i128::from(timeline.initial())
}

/// The effect of one impact at tick `t`, from the closed-form definition.
///
/// A rate is summed tick by tick rather than multiplied, to stay independent of the
/// implementation.
pub fn effect(impact: Impact, t: Tick) -> i128 {
    match impact {
        Impact::Step { at, delta } if at <= t => i128::from(delta),
        Impact::Claim { start, end, amount } if start <= t && t < end => -i128::from(amount),
        Impact::Rate {
            start,
            end,
            per_tick,
        } => (start..end)
            .filter(|&tick| tick < t)
            .map(|_| i128::from(per_tick))
            .sum(),
        Impact::Step { .. } | Impact::Claim { .. } => 0,
    }
}

/// The last tick at which any impact changes the level, or the start tick if there is none.
fn last_change(timeline: &Timeline) -> Tick {
    timeline
        .impacts()
        .iter()
        .map(|&(_, impact)| match impact {
            Impact::Step { at, .. } => at,
            Impact::Claim { end, .. } | Impact::Rate { end, .. } => end,
        })
        .fold(timeline.start(), Tick::max)
}

/// The first violation of `timeline`, found by checking every tick up to the last change.
///
/// After the last change the level is constant, so later ticks add nothing.
pub fn first_violation(timeline: &Timeline) -> Option<Violation> {
    (timeline.start()..=last_change(timeline)).find_map(|t| violation_at(timeline, t))
}

/// The violation at tick `t`, if the level is out of bounds there.
fn violation_at(timeline: &Timeline, t: Tick) -> Option<Violation> {
    let level = level_at(timeline, t);
    let bounds = timeline.bounds();
    let bound = if level < i128::from(bounds.min()) {
        Bound::BelowMin(bounds.min())
    } else if level > i128::from(bounds.max()) {
        Bound::AboveMax(bounds.max())
    } else {
        return None;
    };

    Some(Violation {
        at: t,
        level: Level::try_from(level).expect("test levels are small"),
        bound,
        contributors: contributors(timeline, t, bound),
    })
}

/// The activities whose summed effect at tick `t` pushes towards `bound`, sorted by id.
fn contributors(timeline: &Timeline, t: Tick, bound: Bound) -> Vec<ActivityId> {
    let mut activities: Vec<ActivityId> = timeline.impacts().iter().map(|&(a, _)| a).collect();
    activities.sort_unstable();
    activities.dedup();

    activities
        .into_iter()
        .filter(|&activity| {
            let effect = net_effect(timeline, activity, t);
            match bound {
                Bound::BelowMin(_) => effect < 0,
                Bound::AboveMax(_) => effect > 0,
            }
        })
        .collect()
}

/// The summed effect of all impacts of `activity` that are in force at tick `t`.
fn net_effect(timeline: &Timeline, activity: ActivityId, t: Tick) -> i128 {
    let mut effect = 0;
    for &(owner, impact) in timeline.impacts() {
        if owner != activity {
            continue;
        }

        effect += self::effect(impact, t);
    }

    effect
}
