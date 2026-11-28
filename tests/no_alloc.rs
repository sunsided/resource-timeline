//! Allocation-free timelines and plans, built without the `alloc` feature in CI.
//!
//! Run with `cargo test --no-default-features --test no_alloc`.

#![no_std]

use resource_timeline::storage::{Fixed, FixedPlan};
use resource_timeline::{
    ActivityId, Bound, Bounds, Container, Demand, Error, Impact, Plan, PlanViolation, StateImpact,
    StateTimeline, StateViolation, Timeline, Window,
};

/// A fixed timeline explains a violation with its contributors, without an allocator.
#[test]
fn fixed_timeline_explains_a_violation() {
    let bounds = Bounds::new(20, 120).expect("valid");
    let mut battery = Timeline::<Fixed<4>>::depletable_in(0, 100, bounds, 4).expect("valid");
    battery
        .add(ActivityId(1), Impact::Step { at: 5, delta: -40 })
        .expect("room");
    battery
        .add(ActivityId(2), Impact::Step { at: 20, delta: -50 })
        .expect("room");

    let violation = battery
        .first_violation()
        .expect("no overflow")
        .expect("broken");
    assert_eq!((violation.at, violation.level), (20, 10));
    assert_eq!(violation.bound, Bound::BelowMin(20));
    assert_eq!(violation.contributors, [ActivityId(1), ActivityId(2)]);
}

/// A fixed state timeline reports a conflict with its setters.
#[test]
fn fixed_state_timeline_reports_a_conflict() {
    let mut mode = StateTimeline::<Fixed<4>>::new_in(0, 0, 4);
    mode.add(ActivityId(2), StateImpact::Set { at: 3, value: 1 })
        .expect("room");
    mode.add(ActivityId(1), StateImpact::Set { at: 3, value: 2 })
        .expect("room");

    match mode.first_violation().expect("room") {
        Some(StateViolation::Conflict { at, setters }) => {
            assert_eq!(at, 3);
            assert_eq!(setters, [ActivityId(1), ActivityId(2)]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// A fixed plan places, searches and checks; full containers are typed errors.
#[test]
fn fixed_plan_places_and_searches() {
    let mut plan = Plan::<FixedPlan<1, 0, 2>>::new_in();
    let transmitter = plan
        .add_timeline(Timeline::claimable_in(0, 1, 8).expect("valid"))
        .expect("room");
    assert_eq!(
        plan.add_timeline(Timeline::claimable_in(0, 1, 8).expect("valid")),
        Err(Error::CapacityExceeded {
            container: Container::Timelines,
            capacity: 1
        })
    );

    let downlink = [Demand::Level {
        timeline: transmitter,
        impact: Impact::Claim {
            start: 0,
            end: 8,
            amount: 1,
        },
    }];
    plan.place(ActivityId(1), 5, &downlink).expect("room");
    let window = Window::new(0, 100).expect("valid");
    assert_eq!(plan.earliest_start(&downlink, window, 100), Ok(Some(13)));

    plan.place(ActivityId(2), 6, &downlink).expect("room");
    let before = plan.clone();
    assert_eq!(
        plan.place(ActivityId(3), 30, &downlink),
        Err(Error::CapacityExceeded {
            container: Container::Impacts,
            capacity: 2
        })
    );
    assert_eq!(plan, before);

    match plan.first_violation().expect("room") {
        Some(PlanViolation::Level { violation, .. }) => {
            assert_eq!(violation.at, 6);
            assert_eq!(violation.contributors, [ActivityId(1), ActivityId(2)]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// A fixed timeline and its profile round-trip through the byte encoding without allocation.
#[test]
fn fixed_encoding_round_trips() {
    use resource_timeline::LevelProfile;

    let bounds = Bounds::new(0, 50).expect("valid");
    let mut battery = Timeline::<Fixed<4>>::depletable_in(0, 30, bounds, 4).expect("valid");
    battery
        .add(
            ActivityId(1),
            Impact::Rate {
                start: 2,
                end: 9,
                per_tick: -3,
            },
        )
        .expect("room");
    let mut bytes = [0_u8; 256];
    let written = battery.encode(&mut bytes).expect("room");
    assert_eq!(
        Timeline::<Fixed<4>>::decode_in(&bytes[..written]),
        Ok(battery.clone())
    );

    let profile = battery.export().expect("small");
    let written = profile.encode(&mut bytes).expect("room");
    assert_eq!(
        LevelProfile::<Fixed<4>>::decode_in(&bytes[..written]),
        Ok(profile)
    );
}

/// Shares are split and checked onboard without an allocator.
#[test]
fn fixed_share_split_and_check() {
    use resource_timeline::share::split;

    let mut shares = [0_i64; 3];
    for (slot, share) in shares
        .iter_mut()
        .zip(split(10, 1, &[2, 0, 4]).expect("fits"))
    {
        *slot = share;
    }
    assert_eq!(shares, [4, 1, 5]);

    let mut link = Timeline::<Fixed<4>>::claimable_in(0, 10, 4).expect("valid");
    let others = Impact::Claim {
        start: 0,
        end: 100,
        amount: 10 - shares[0],
    };
    link.add(ActivityId(0), others).expect("room");
    let uplink = Impact::Claim {
        start: 5,
        end: 9,
        amount: 4,
    };
    link.add(ActivityId(1), uplink).expect("room");
    assert_eq!(link.first_violation().expect("small"), None);
    assert_eq!(link.lowest_from(0).expect("small"), 0);
}

/// Zones from an overlap graph report conflicts in a fixed plan without an allocator.
#[test]
fn fixed_zone_conflict() {
    use resource_timeline::zone::{Conflict, Overlaps, occupy};

    let edges = [(ActivityId(1), ActivityId(2))];
    let mut plan = Plan::<FixedPlan<1, 0, 2>>::new_in();
    let zones = Overlaps::new(&edges)
        .expect("valid")
        .add_zones(&mut plan, 0, 2)
        .expect("room");
    let zone = zones.zone(0).expect("edge 0");
    let demands = [occupy(zone, 0, 6)];
    plan.place(ActivityId(1), 0, &demands).expect("room");
    let window = Window::new(0, 20).expect("valid");
    assert_eq!(plan.earliest_start(&demands, window, 16), Ok(Some(6)));
    plan.place(ActivityId(2), 4, &demands).expect("room");
    let violation = plan.first_violation().expect("small").expect("overlap");
    let conflict = Conflict::of(&plan, &violation).expect("zone");
    assert_eq!(conflict.zone, zone);
    assert_eq!(
        (conflict.at, conflict.first, conflict.second),
        (4, ActivityId(1), ActivityId(2))
    );
}
