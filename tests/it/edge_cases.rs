//! Hand-written examples of boundary behaviour.

use resource_timeline::{ActivityId, Bound, Bounds, Impact, Timeline, Violation};

/// Activity used where only one is needed.
const A: ActivityId = ActivityId(1);

/// Second activity for overlap scenarios.
const B: ActivityId = ActivityId(2);

/// A claim ending at `t` and another starting at `t` share a capacity of one.
#[test]
fn back_to_back_claims_fit() {
    let mut instrument = Timeline::claimable(0, 1, 8).expect("valid");
    instrument
        .add(
            A,
            Impact::Claim {
                start: 0,
                end: 10,
                amount: 1,
            },
        )
        .expect("valid");
    instrument
        .add(
            B,
            Impact::Claim {
                start: 10,
                end: 20,
                amount: 1,
            },
        )
        .expect("valid");

    assert_eq!(instrument.first_violation(), Ok(None));
}

/// Overlapping claims on a capacity of one are reported at the first shared tick.
#[test]
fn overlapping_claims_conflict() {
    let mut instrument = Timeline::claimable(0, 1, 8).expect("valid");
    instrument
        .add(
            A,
            Impact::Claim {
                start: 0,
                end: 10,
                amount: 1,
            },
        )
        .expect("valid");
    instrument
        .add(
            B,
            Impact::Claim {
                start: 9,
                end: 20,
                amount: 1,
            },
        )
        .expect("valid");

    let expected = Violation {
        at: 9,
        level: -1,
        bound: Bound::BelowMin(0),
        contributors: vec![A, B],
    };
    assert_eq!(instrument.first_violation(), Ok(Some(expected)));
}

/// An initial level outside the bounds is a violation at the start tick without contributors.
#[test]
fn initial_level_out_of_bounds() {
    let bounds = Bounds::new(10, 20).expect("valid");
    let battery = Timeline::depletable(5, 3, bounds, 8).expect("valid");

    let expected = Violation {
        at: 5,
        level: 3,
        bound: Bound::BelowMin(10),
        contributors: vec![],
    };
    assert_eq!(battery.first_violation(), Ok(Some(expected)));
}

/// Impacts at the start tick are netted with the initial level before the check.
#[test]
fn start_tick_impacts_are_netted() {
    let bounds = Bounds::new(10, 20).expect("valid");
    let mut battery = Timeline::depletable(5, 3, bounds, 8).expect("valid");
    battery
        .add(A, Impact::Step { at: 5, delta: 10 })
        .expect("valid");

    assert_eq!(battery.first_violation(), Ok(None));
}

/// Overcharging names only the producers, and an activity that nets to zero is not named.
#[test]
fn above_max_names_producers_only() {
    let bounds = Bounds::new(0, 10).expect("valid");
    let mut storage = Timeline::depletable(0, 5, bounds, 8).expect("valid");
    storage
        .add(A, Impact::Step { at: 1, delta: 4 })
        .expect("valid");
    storage
        .add(B, Impact::Step { at: 2, delta: 3 })
        .expect("valid");
    storage
        .add(B, Impact::Step { at: 2, delta: -3 })
        .expect("valid");
    storage
        .add(ActivityId(3), Impact::Step { at: 3, delta: 2 })
        .expect("valid");

    let violation = storage
        .first_violation()
        .expect("no overflow")
        .expect("violated");
    assert_eq!(violation.at, 3);
    assert_eq!(violation.level, 11);
    assert_eq!(violation.bound, Bound::AboveMax(10));
    assert_eq!(violation.contributors, [A, ActivityId(3)]);
}

/// Levels far beyond `i64` in intermediate sums are handled when the net level fits.
#[test]
fn large_same_tick_deltas_net_without_overflow() {
    let bounds = Bounds::new(i64::MIN, i64::MAX).expect("valid");
    let mut timeline = Timeline::depletable(0, i64::MAX, bounds, 8).expect("valid");
    timeline
        .add(
            A,
            Impact::Step {
                at: 1,
                delta: i64::MAX,
            },
        )
        .expect("valid");
    timeline
        .add(
            B,
            Impact::Step {
                at: 1,
                delta: i64::MIN,
            },
        )
        .expect("valid");

    assert_eq!(timeline.first_violation(), Ok(None));
}

/// A rate crossing a bound inside a segment is reported at the exact crossing tick.
#[test]
fn rate_crossing_inside_a_segment() {
    let bounds = Bounds::new(0, 100).expect("valid");
    let mut battery = Timeline::depletable(0, 10, bounds, 8).expect("valid");
    battery
        .add(
            A,
            Impact::Rate {
                start: 2,
                end: 50,
                per_tick: -3,
            },
        )
        .expect("valid");

    // 10 - 3 * (t - 2) < 0 first at t = 6, where the level is -2.
    let expected = Violation {
        at: 6,
        level: -2,
        bound: Bound::BelowMin(0),
        contributors: vec![A],
    };
    assert_eq!(battery.first_violation(), Ok(Some(expected)));
    assert_eq!(battery.level_at(5), Ok(1));
}

/// Charging and a parallel drain net to a slow drift that stops at the end of the rates.
#[test]
fn opposing_rates_net_and_stop() {
    let bounds = Bounds::new(0, 20).expect("valid");
    let mut battery = Timeline::depletable(0, 10, bounds, 8).expect("valid");
    let charge = Impact::Rate {
        start: 0,
        end: 100,
        per_tick: 2,
    };
    let drain = Impact::Rate {
        start: 0,
        end: 100,
        per_tick: -2,
    };
    battery.add(A, charge).expect("valid");
    battery.add(B, drain).expect("valid");
    assert_eq!(battery.first_violation(), Ok(None));

    battery.remove(B);
    let violation = battery
        .first_violation()
        .expect("no overflow")
        .expect("overcharged");
    assert_eq!((violation.at, violation.level), (6, 22));
    assert_eq!(violation.bound, Bound::AboveMax(20));
    assert_eq!(battery.level_at(1_000), Ok(210));
}
