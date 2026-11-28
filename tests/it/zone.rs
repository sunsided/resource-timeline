//! Zone resources: overlap graphs, conflicts, envelopes and fixed storage.

use proptest::prelude::*;
use resource_timeline::envelope::{Envelope, Flexible, Precedence, Verdict};
use resource_timeline::storage::FixedPlan;
use resource_timeline::zone::{Conflict, Overlaps, occupy};
use resource_timeline::{ActivityId, Container, Demand, Error, Impact, Plan, Tick, Timeline};

/// Self-loops and repeated edges, in either orientation, are rejected.
#[test]
fn overlaps_reject_self_loops_and_duplicates() {
    let (a, b, c) = (ActivityId(1), ActivityId(2), ActivityId(3));
    assert_eq!(
        Overlaps::new(&[(a, b), (c, c)]).err(),
        Some(Error::InvalidOverlap {
            first: c,
            second: c
        })
    );
    assert_eq!(
        Overlaps::new(&[(a, b), (b, c), (b, a)]).err(),
        Some(Error::InvalidOverlap {
            first: b,
            second: a
        })
    );
    assert_eq!(
        Overlaps::new(&[(a, b), (b, c)])
            .expect("valid")
            .edges()
            .len(),
        2
    );
}

/// Zones follow existing timelines, map back to their edges and list a task's zones.
#[test]
fn zones_map_to_edges() {
    let (a, b, c) = (ActivityId(1), ActivityId(2), ActivityId(3));
    let edges = [(a, b), (b, c)];
    let mut plan = Plan::new();
    let crane = plan
        .add_timeline(Timeline::claimable(0, 1, 4).expect("valid"))
        .expect("memory");
    let zones = Overlaps::new(&edges)
        .expect("valid")
        .add_zones(&mut plan, 0, 4)
        .expect("memory");
    assert_eq!(plan.timelines().len(), 3);
    assert_eq!(zones.zone(2), None);
    assert_eq!(zones.edge(crane), None);
    let second = zones.zone(1).expect("edge 1");
    assert_eq!(zones.edge(second), Some((b, c)));
    assert_eq!(zones.of(b).count(), 2);
    assert_eq!(
        zones.of(a).collect::<Vec<_>>(),
        [zones.zone(0).expect("edge 0")]
    );
    assert_eq!(zones.of(ActivityId(9)).count(), 0);
}

/// Conflicts name the zone and both tasks; other violations are no zone conflicts.
#[test]
fn conflicts_name_zone_and_tasks() {
    let mut plan = Plan::new();
    let zone = plan
        .add_timeline(Timeline::claimable(0, 1, 8).expect("valid"))
        .expect("memory");
    let pool = plan
        .add_timeline(Timeline::claimable(0, 2, 8).expect("valid"))
        .expect("memory");
    let stay = [occupy(zone, 0, 10)];
    plan.place(ActivityId(7), 0, &stay).expect("valid");
    plan.place(ActivityId(7), 5, &stay).expect("valid");
    let violation = plan.first_violation().expect("small").expect("overlap");
    assert_eq!(Conflict::of(&plan, &violation), None, "a task with itself");

    plan.remove(ActivityId(7));
    let three = [Demand::Level {
        timeline: pool,
        impact: Impact::Claim {
            start: 0,
            end: 4,
            amount: 3,
        },
    }];
    plan.place(ActivityId(1), 0, &three).expect("valid");
    plan.place(ActivityId(2), 0, &stay).expect("valid");
    let violation = plan.first_violation().expect("small").expect("over");
    assert_eq!(
        Conflict::of(&plan, &violation),
        None,
        "capacity two is no zone"
    );

    plan.remove(ActivityId(1));
    plan.place(ActivityId(3), 6, &stay).expect("valid");
    let violation = plan.first_violation().expect("small").expect("overlap");
    assert_eq!(
        Conflict::of(&plan, &violation),
        Some(Conflict {
            zone,
            at: 6,
            first: ActivityId(2),
            second: ActivityId(3),
        })
    );
}

/// Adding zones to a full fixed plan fails without adding any.
#[test]
fn zones_are_added_all_or_nothing() {
    let edges = [
        (ActivityId(1), ActivityId(2)),
        (ActivityId(2), ActivityId(3)),
    ];
    let mut plan = Plan::<FixedPlan<1, 0, 4>>::new_in();
    let result = Overlaps::new(&edges)
        .expect("valid")
        .add_zones(&mut plan, 0, 4);
    assert_eq!(
        result.err(),
        Some(Error::CapacityExceeded {
            container: Container::Timelines,
            capacity: 1
        })
    );
    assert_eq!(plan.timelines().len(), 0);

    let mut plan = Plan::<FixedPlan<2, 0, 4>>::new_in();
    let zones = Overlaps::new(&edges)
        .expect("valid")
        .add_zones(&mut plan, 0, 4)
        .expect("room");
    let demands = [occupy(zones.zone(0).expect("edge 0"), 0, 5)];
    plan.place(ActivityId(1), 0, &demands).expect("room");
    plan.place(ActivityId(2), 3, &demands).expect("room");
    let violation = plan.first_violation().expect("small").expect("overlap");
    let conflict = Conflict::of(&plan, &violation).expect("zone");
    assert_eq!(
        (conflict.at, conflict.first, conflict.second),
        (3, ActivityId(1), ActivityId(2))
    );
}

/// A flexible zone schedule is safe exactly when a precedence orders the tasks.
#[test]
fn envelopes_check_flexible_zones() {
    let zone = Timeline::claimable(0, 1, 8).expect("valid");
    let stay = [Impact::Claim {
        start: 0,
        end: 6,
        amount: 1,
    }];
    let window = resource_timeline::Window::new(0, 20).expect("valid");
    let flexible = [
        Flexible {
            activity: ActivityId(1),
            window,
            impacts: &stay,
        },
        Flexible {
            activity: ActivityId(2),
            window,
            impacts: &stay,
        },
    ];
    let envelope = Envelope::new(&zone, &flexible).expect("valid");
    assert!(matches!(
        envelope.check().expect("small"),
        Verdict::Possible(_)
    ));
    let ordered = envelope
        .with_precedences(&[Precedence::after_end(ActivityId(1), ActivityId(2), 0)])
        .expect("consistent");
    assert_eq!(ordered.check().expect("small"), Verdict::Safe);
}

/// The interval intersection of two occupations, if any.
fn overlap(a: (Tick, Tick), b: (Tick, Tick)) -> Option<Tick> {
    let start = a.0.max(b.0);
    (start < a.1.min(b.1)).then_some(start)
}

proptest! {
    /// A plan of edge zones has a conflict exactly when two tasks sharing an edge overlap in
    /// time, at the earliest such tick, naming that edge.
    #[test]
    fn edge_zones_match_pairwise_overlap(
        tasks in prop::collection::vec((0_i64..20, 1_i64..6), 2..6),
        raw in prop::collection::vec((0_usize..6, 0_usize..6), 0..8),
    ) {
        let n = tasks.len();
        let mut edges: Vec<(ActivityId, ActivityId)> = Vec::new();
        for (a, b) in raw {
            let (a, b) = (a % n, b % n);
            let (a, b) = (ActivityId(u32::try_from(a).expect("small")), ActivityId(u32::try_from(b).expect("small")));
            if a != b && !edges.iter().any(|&e| e == (a, b) || e == (b, a)) {
                edges.push((a, b));
            }
        }

        let mut plan = Plan::new();
        let zones = Overlaps::new(&edges).expect("valid").add_zones(&mut plan, 0, 4).expect("memory");
        for (index, &(start, length)) in tasks.iter().enumerate() {
            let task = ActivityId(u32::try_from(index).expect("small"));
            let demands: Vec<Demand> = zones.of(task).map(|zone| occupy(zone, 0, length)).collect();
            plan.place(task, start, &demands).expect("valid");
        }

        let interval = |task: ActivityId| {
            let (start, length) = tasks[task.0 as usize];
            (start, start + length)
        };
        let expected = edges
            .iter()
            .enumerate()
            .filter_map(|(index, &(a, b))| overlap(interval(a), interval(b)).map(|at| (at, index)))
            .min();
        let found = plan.first_violation().expect("small").map(|violation| {
            let conflict = Conflict::of(&plan, &violation).expect("only zones");
            let edge = zones.edge(conflict.zone).expect("an edge zone");
            let (lo, hi) = if edge.0 < edge.1 { edge } else { (edge.1, edge.0) };
            prop_assert_eq!((conflict.first, conflict.second), (lo, hi));
            Ok((conflict.at, conflict.zone.index()))
        }).transpose()?;
        prop_assert_eq!(found, expected);
    }
}
