//! A build order in a real-time strategy game, checked against a scouted enemy rush.
//!
//! One tick is one second. The player starts with 50 ore and income of two ore per second, and
//! can train harvesters (more income), build a supply depot (more supply), a barracks, and
//! soldiers once the barracks stands. A scout reports an attack at second [`RUSH`]; by then at
//! least [`DEFENDERS`] soldiers must be out, a [`Target`] on the army timeline.
//!
//! The greedy scheduler places each order at the earliest second the player can afford it, in
//! priority order. An economy-first order misses the target, and the check says when and by how
//! much. Swapping the priorities to barracks and soldiers first meets it. The crate checks the
//! player's own plan; the opponent appears only as the fixed target.
//!
//! Run with `cargo run --example rush`.

use resource_timeline::greedy::{Request, schedule};
use resource_timeline::{
    ActivityId, Bounds, Demand, Error, Impact, Plan, StateId, StateImpact, StateTimeline, Target,
    Timeline, TimelineId, Window,
};

/// The second the scouted attack arrives.
const RUSH: i64 = 170;
/// The soldiers that must be out when the attack arrives.
const DEFENDERS: i64 = 4;
/// How long a harvester keeps bringing in ore after it is trained, in seconds.
const HARVEST: i64 = 600;
/// The barracks is not built yet.
const NO_BARRACKS: u32 = 0;
/// The barracks stands and can train soldiers.
const BARRACKS: u32 = 1;

/// The starting base income, fixed before scheduling.
const WORLD: ActivityId = ActivityId(0);

/// The handles of the player's resource timelines.
struct Resources {
    /// Ore in the bank; starts at 50, never negative.
    ore: TimelineId,
    /// Free supply: depots add to it, every unit takes one; starts at 4.
    supply: TimelineId,
    /// The main base, training one harvester at a time.
    hall: TimelineId,
    /// The barracks' training queue, one soldier at a time.
    queue: TimelineId,
    /// Soldiers out on the field.
    army: TimelineId,
    /// Whether the barracks stands: [`NO_BARRACKS`] or [`BARRACKS`].
    barracks: StateId,
}

/// The demands of each kind of order, relative to the order's start.
struct Orders {
    /// Train a harvester; see [`harvester`].
    harvester: [Demand; 4],
    /// Build the barracks; see [`barracks`].
    barracks: [Demand; 2],
    /// Build a supply depot; see [`depot`].
    depot: [Demand; 2],
    /// Train a soldier; see [`soldier`].
    soldier: [Demand; 5],
}

/// The priority of each kind of order; higher goes first.
#[derive(Clone, Copy)]
struct Priorities {
    /// Priority of the three harvesters.
    harvester: u32,
    /// Priority of the barracks. Must exceed `soldier`, or no soldier finds a barracks.
    barracks: u32,
    /// Priority of the depot.
    depot: u32,
    /// Priority of the soldiers.
    soldier: u32,
}

/// Plans the economy-first and the defense-first build order against the same rush.
fn main() -> Result<(), Error> {
    let economy = Priorities {
        harvester: 4,
        barracks: 3,
        depot: 2,
        soldier: 1,
    };
    let defense = Priorities {
        barracks: 4,
        soldier: 3,
        depot: 2,
        harvester: 1,
    };

    let missed = play("economy first", economy)?;
    let held = play("defense first", defense)?;
    assert!(missed && !held);

    Ok(())
}

/// Schedules one build order, checks it against the rush, prints both and returns whether the
/// rush target is missed.
fn play(name: &str, priorities: Priorities) -> Result<bool, Error> {
    let (mut plan, r) = base()?;
    plan.place(WORLD, 0, &world(&r))?;

    let orders = Orders::new(&r);
    let (names, requests) = build_order(&orders, priorities)?;
    let starts = schedule(&mut plan, &requests, 10_000)?;
    println!("{name}:");
    for (name, start) in names.iter().zip(&starts) {
        match start {
            Some(start) => println!("{name:>12}: second {start}"),
            None => println!("{name:>12}: does not fit"),
        }
    }

    plan.add_target(
        r.army,
        Target {
            at: RUSH,
            min: DEFENDERS,
        },
    )?;
    let violation = plan.first_violation()?;
    match &violation {
        Some(violation) => println!("  rush at second {RUSH}: {violation:?}\n"),
        None => println!("  rush at second {RUSH}: held with {DEFENDERS} soldiers\n"),
    }

    Ok(violation.is_some())
}

/// An empty plan with the player's timelines.
fn base() -> Result<(Plan, Resources), Error> {
    let mut plan = Plan::new();
    let resources = Resources {
        ore: plan.add_timeline(Timeline::depletable(0, 50, Bounds::new(0, 10_000)?, 64)?)?,
        supply: plan.add_timeline(Timeline::depletable(0, 4, Bounds::new(0, 50)?, 64)?)?,
        hall: plan.add_timeline(Timeline::claimable(0, 1, 64)?)?,
        queue: plan.add_timeline(Timeline::claimable(0, 1, 64)?)?,
        army: plan.add_timeline(Timeline::depletable(0, 0, Bounds::new(0, 50)?, 64)?)?,
        barracks: plan.add_state(StateTimeline::new(0, NO_BARRACKS, 64))?,
    };

    Ok((plan, resources))
}

/// The orders of one build: three harvesters, a barracks, a depot and [`DEFENDERS`] soldiers,
/// with their display names in the same order.
fn build_order(
    orders: &Orders,
    p: Priorities,
) -> Result<(Vec<&'static str>, Vec<Request<'_>>), Error> {
    let window = Window::new(0, 300)?;
    let mut names = Vec::new();
    let mut requests = Vec::new();
    let mut add = |name: &'static str, priority: u32, demands| {
        let id = u32::try_from(requests.len() + 1).expect("a build has few orders");
        names.push(name);
        requests.push(request(id, priority, window, demands));
    };
    for _ in 0..3 {
        add("harvester", p.harvester, &orders.harvester[..]);
    }

    add("barracks", p.barracks, &orders.barracks[..]);
    add("depot", p.depot, &orders.depot[..]);
    for _ in 0..DEFENDERS {
        add("soldier", p.soldier, &orders.soldier[..]);
    }

    Ok((names, requests))
}

/// A scheduling request for activity `id`.
const fn request(id: u32, priority: u32, window: Window, demands: &[Demand]) -> Request<'_> {
    Request {
        activity: ActivityId(id),
        priority,
        window,
        demands,
    }
}

impl Orders {
    /// The demands of every kind of order on the timelines in `r`.
    fn new(r: &Resources) -> Self {
        Self {
            harvester: harvester(r),
            barracks: barracks(r),
            depot: depot(r),
            soldier: soldier(r),
        }
    }
}

/// The starting harvesters bring in two ore per second for the whole game.
fn world(r: &Resources) -> [Demand; 1] {
    [level(
        r.ore,
        Impact::Rate {
            start: 0,
            end: HARVEST,
            per_tick: 2,
        },
    )]
}

/// Train a harvester: 50 ore and one supply, 15 seconds at the hall, then one more ore per
/// second for [`HARVEST`] seconds.
fn harvester(r: &Resources) -> [Demand; 4] {
    [
        level(r.ore, Impact::Step { at: 0, delta: -50 }),
        level(r.supply, Impact::Step { at: 0, delta: -1 }),
        level(
            r.hall,
            Impact::Claim {
                start: 0,
                end: 15,
                amount: 1,
            },
        ),
        level(
            r.ore,
            Impact::Rate {
                start: 15,
                end: 15 + HARVEST,
                per_tick: 1,
            },
        ),
    ]
}

/// Build the barracks: 150 ore, standing after 40 seconds.
fn barracks(r: &Resources) -> [Demand; 2] {
    [
        level(r.ore, Impact::Step { at: 0, delta: -150 }),
        state(
            r.barracks,
            StateImpact::Set {
                at: 40,
                value: BARRACKS,
            },
        ),
    ]
}

/// Build a supply depot: 100 ore, eight more supply after 25 seconds.
fn depot(r: &Resources) -> [Demand; 2] {
    [
        level(r.ore, Impact::Step { at: 0, delta: -100 }),
        level(r.supply, Impact::Step { at: 25, delta: 8 }),
    ]
}

/// Train a soldier: 50 ore and one supply, 20 seconds in the barracks queue, then one more
/// soldier in the army.
fn soldier(r: &Resources) -> [Demand; 5] {
    [
        level(r.ore, Impact::Step { at: 0, delta: -50 }),
        level(r.supply, Impact::Step { at: 0, delta: -1 }),
        level(
            r.queue,
            Impact::Claim {
                start: 0,
                end: 20,
                amount: 1,
            },
        ),
        state(
            r.barracks,
            StateImpact::Require {
                start: 0,
                end: 20,
                value: BARRACKS,
            },
        ),
        level(r.army, Impact::Step { at: 20, delta: 1 }),
    ]
}

/// A demand on a numeric timeline.
const fn level(timeline: TimelineId, impact: Impact) -> Demand {
    Demand::Level { timeline, impact }
}

/// A demand on a state timeline.
const fn state(timeline: StateId, impact: StateImpact) -> Demand {
    Demand::State { timeline, impact }
}
