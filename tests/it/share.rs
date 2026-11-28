//! Decoupled budgets: the split, the lowest level from a tick and the worst-case need.

use proptest::prelude::*;
use resource_timeline::envelope::Envelope;
use resource_timeline::share::split;
use resource_timeline::{ActivityId, Bound, Error, Impact, Level, Timeline};

use crate::envelope::{flexible, horizon, scenario, schedules};
use crate::oracle;
use crate::properties::depletable;

proptest! {
    /// The shares add up to the pool, cover need plus margin, and differ by at most one
    /// beyond that.
    #[test]
    fn shares_cover_needs_within_the_pool(
        needs in prop::collection::vec(0_i64..50, 0..6),
        margin in 0_i64..5,
        slack in 0_i64..40,
    ) {
        let total: Level = needs.iter().map(|need| need + margin).sum();
        let pool = total + slack;
        let shares: Vec<Level> = split(pool, margin, &needs).expect("fits").collect();
        prop_assert_eq!(shares.len(), needs.len());
        if !needs.is_empty() {
            prop_assert_eq!(shares.iter().sum::<Level>(), pool);
        }

        let extras: Vec<Level> = shares.iter().zip(&needs).map(|(s, n)| s - n - margin).collect();
        prop_assert!(extras.iter().all(|extra| *extra >= 0));
        let (lo, hi) = (extras.iter().min(), extras.iter().max());
        if let (Some(lo), Some(hi)) = (lo, hi) {
            prop_assert!(hi - lo <= 1);
        }
    }

    /// The lowest level from a tick agrees with a tick-by-tick scan.
    #[test]
    fn lowest_from_matches_the_oracle(timeline in depletable(), offset in 0_i64..30) {
        let from = timeline.start() + offset;
        let last = timeline.start() + 40;
        let lowest = (from..=last.max(from)).map(|t| oracle::level_at(&timeline, t)).min();
        prop_assert_eq!(
            timeline.lowest_from(from).map(i128::from).ok(),
            lowest,
        );
    }

    /// The worst-case lowest level of an envelope is the lowest level of any schedule.
    #[test]
    fn envelope_lowest_from_matches_the_schedules(
        (timeline, generated) in scenario(),
        offset in 0_i64..30,
    ) {
        let flexible = flexible(&generated);
        let envelope = Envelope::new(&timeline, &flexible).expect("valid");
        let from = timeline.start() + offset;
        let last = horizon(&timeline, &generated).max(from);
        let lowest = schedules(&timeline, &flexible)
            .iter()
            .flat_map(|concrete| (from..=last).map(|t| oracle::level_at(concrete, t)))
            .min();
        prop_assert_eq!(envelope.lowest_from(from).map(i128::from).ok(), lowest);
    }
}

/// Precedences lower the worst-case need: ordered downlinks never overlap.
#[test]
fn envelope_lowest_from_respects_precedences() {
    use resource_timeline::Window;
    use resource_timeline::envelope::{Flexible, Precedence};

    let link = Timeline::claimable(0, 10, 8).expect("valid");
    let uplink = [Impact::Claim {
        start: 0,
        end: 5,
        amount: 4,
    }];
    let window = Window::new(0, 10).expect("valid");
    let flexible = [
        Flexible {
            activity: ActivityId(1),
            window,
            impacts: &uplink,
        },
        Flexible {
            activity: ActivityId(2),
            window,
            impacts: &uplink,
        },
    ];
    let envelope = Envelope::new(&link, &flexible).expect("valid");
    assert_eq!(envelope.lowest_from(0), Ok(2));
    let ordered = envelope
        .with_precedences(&[Precedence::after_end(ActivityId(1), ActivityId(2), 0)])
        .expect("consistent");
    assert_eq!(ordered.lowest_from(0), Ok(6));
    assert_eq!(ordered.lowest_from(16), Ok(10));
}

/// Pieces merged into one breakpoint (same slope, continuing level) and pieces that fall to
/// the tick before an upward jump are covered; each case agrees with a tick-by-tick scan.
#[test]
fn lowest_from_covers_merged_and_falling_pieces() {
    use resource_timeline::Bounds;

    let bounds = Bounds::new(-1_000, 1_000).expect("valid");
    let scan = |line: &Timeline, from| (from..60).map(|t| line.level_at(t).expect("small")).min();
    let claim = |start, end, amount| Impact::Claim { start, end, amount };
    let rate = |start, end, per_tick| Impact::Rate {
        start,
        end,
        per_tick,
    };
    let cases: [&[Impact]; 3] = [
        &[claim(10, 20, 90), claim(30, 40, 90)],
        &[
            rate(0, 10, -2),
            rate(10, 20, -2),
            Impact::Step { at: 25, delta: 50 },
        ],
        &[
            rate(0, 10, -3),
            Impact::Step { at: 10, delta: 40 },
            rate(15, 25, -3),
        ],
    ];
    for impacts in cases {
        let mut line = Timeline::depletable(0, 100, bounds, 8).expect("valid");
        for (id, impact) in (1..).zip(impacts) {
            line.add(ActivityId(id), *impact).expect("room");
        }

        for from in [0, 5, 12, 22] {
            assert_eq!(
                line.lowest_from(from).ok(),
                scan(&line, from),
                "{impacts:?} from {from}"
            );
        }
    }
}

/// An envelope over flexible rates is flat once every start has drained fully, and its
/// lowest level agrees with the lower envelope at every tick.
#[test]
fn envelope_lowest_from_covers_flexible_rates() {
    use resource_timeline::envelope::Flexible;
    use resource_timeline::{Bounds, Window};

    let line =
        Timeline::depletable(0, 100, Bounds::new(-1_000, 1_000).expect("valid"), 8).expect("valid");
    let drain = [Impact::Rate {
        start: 0,
        end: 10,
        per_tick: -1,
    }];
    let window = Window::new(0, 20).expect("valid");
    let flexible = [
        Flexible {
            activity: ActivityId(1),
            window,
            impacts: &drain,
        },
        Flexible {
            activity: ActivityId(2),
            window,
            impacts: &drain,
        },
    ];
    let envelope = Envelope::new(&line, &flexible).expect("valid");
    let lower = |t| envelope.lower_at(t).expect("small");
    assert_eq!((lower(10), lower(19)), (80, 80));
    for from in [0, 5, 15] {
        assert_eq!(envelope.lowest_from(from).ok(), (from..60).map(lower).min());
    }
}

/// Negative inputs and too little pool are typed errors, never clipped shares.
#[test]
fn split_rejects_bad_input() {
    assert_eq!(
        split(-1, 0, &[]).err(),
        Some(Error::NegativeAmount { amount: -1 })
    );
    assert_eq!(
        split(10, -2, &[1]).err(),
        Some(Error::NegativeAmount { amount: -2 })
    );
    assert_eq!(
        split(10, 0, &[4, -3]).err(),
        Some(Error::NegativeAmount { amount: -3 })
    );
    assert_eq!(
        split(10, 1, &[4, 5]).err(),
        Some(Error::Oversubscribed {
            needed: 11,
            pool: 10
        })
    );
    assert_eq!(
        split(Level::MAX, 0, &[Level::MAX, Level::MAX]).err(),
        Some(Error::Oversubscribed {
            needed: Level::MAX,
            pool: Level::MAX
        })
    );
    assert_eq!(split(7, 3, &[]).expect("no agents").count(), 0);
}

/// Three agents share 12 units; two meet and re-split only their own shares, so the sum never
/// exceeds the capacity even though the third keeps using its share unseen.
#[test]
fn partial_contact_keeps_the_capacity() {
    const CAPACITY: Level = 12;
    const OTHERS: ActivityId = ActivityId(0);
    const END: i64 = 1_000;
    let shares: Vec<Level> = split(CAPACITY, 1, &[3, 3, 3]).expect("fits").collect();
    assert_eq!(shares, [4, 4, 4]);

    // Each agent's share timeline: the full link with the others' part reserved.
    let mut agents: Vec<Timeline> = shares
        .iter()
        .map(|share| {
            let mut line = Timeline::claimable(0, CAPACITY, 8).expect("valid");
            let held = Impact::Claim {
                start: 0,
                end: END,
                amount: CAPACITY - share,
            };
            line.add(OTHERS, held).expect("room");
            line
        })
        .collect();
    let claim = |start, end, amount| Impact::Claim { start, end, amount };
    agents[0].add(ActivityId(1), claim(0, 5, 3)).expect("room");
    agents[1].add(ActivityId(2), claim(0, 30, 3)).expect("room");
    agents[2].add(ActivityId(3), claim(0, 30, 4)).expect("room");

    // Agents 0 and 1 meet at tick 10: agent 0 is done, agent 1 needs 3 units from now on.
    let contact = 10;
    let needs: Vec<Level> = (0..2)
        .map(|i| shares[i] - agents[i].lowest_from(contact).expect("small"))
        .collect();
    assert_eq!(needs, [0, 3]);
    let pool = shares[0] + shares[1];
    let resplit: Vec<Level> = split(pool, 0, &needs).expect("fits").collect();
    assert_eq!(resplit, [3, 5]);

    for (agent, (old, new)) in agents.iter_mut().zip(shares.iter().zip(&resplit)) {
        agent.remove(OTHERS);
        let before = claim(0, contact, CAPACITY - old);
        let after = claim(contact, END, CAPACITY - new);
        agent.add(OTHERS, before).expect("room");
        agent.add(OTHERS, after).expect("room");
    }

    // Agent 1 uses its larger share; together with agent 2's unseen use, the link holds.
    agents[1]
        .add(ActivityId(4), claim(12, 20, 2))
        .expect("room");
    assert_eq!(agents[1].first_violation().expect("small"), None);
    let total_from_contact = |t| -> Level {
        agents
            .iter()
            .zip([&resplit[0], &resplit[1], &shares[2]])
            .map(|(agent, share)| share - agent.level_at(t).expect("small"))
            .sum()
    };
    assert!((contact..END).all(|t| total_from_contact(t) <= CAPACITY));

    // One unit more and the agent's own check fails, naming the reservation.
    agents[1]
        .add(ActivityId(5), claim(15, 16, 1))
        .expect("room");
    let violation = agents[1].first_violation().expect("small").expect("over");
    assert_eq!((violation.at, violation.bound), (15, Bound::BelowMin(0)));
    assert!(violation.contributors.contains(&OTHERS));
}
