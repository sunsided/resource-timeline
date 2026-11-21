//! A settler's day in a colony game, scheduled greedily against stamina, stockpile, workbench
//! and daylight timelines.
//!
//! One tick is one in-game minute; the day lasts until minute 120, the night until minute 240.
//! The settler chops wood, turns it into planks at the shared workbench, and builds a wall from
//! the planks. Nothing states the order of the jobs: crafting waits for the wood, the wall waits
//! for the planks, and stamina and daylight push the wall to the next morning. The search
//! answers the question a game AI asks every frame: when can this settler afford the job?
//!
//! Run with `cargo run --example colony`.

use resource_timeline::greedy::{Request, schedule};
use resource_timeline::{
    ActivityId, Bounds, Demand, Error, Impact, Plan, StateId, StateImpact, StateTimeline, Timeline,
    TimelineId, Window,
};

/// The sun is down; wolves roam and outdoor jobs stop.
const NIGHT: u32 = 0;
/// The sun is up.
const DAY: u32 = 1;

/// The world: the day and night cycle and the settler's sleep, fixed before scheduling.
const WORLD: ActivityId = ActivityId(0);

/// The handles of the colony's resource timelines.
struct Resources {
    /// The settler's stamina; starts at 60, between 0 and 100.
    stamina: TimelineId,
    /// Logs in the stockpile; starts at 5, at most 30 fit.
    wood: TimelineId,
    /// Planks in the stockpile; starts empty, at most 50 fit.
    planks: TimelineId,
    /// The workbench, one settler at a time.
    workbench: TimelineId,
    /// Daylight: [`NIGHT`] or [`DAY`].
    daylight: StateId,
}

/// Builds the plan, schedules the settler's jobs and prints the outcome.
fn main() -> Result<(), Error> {
    let (mut plan, r) = colony()?;
    plan.place(WORLD, 0, &world(&r))?;

    let (chop, craft, wall) = (chop(&r), craft(&r), wall(&r));
    let names = ["chop wood", "craft planks", "build wall"];
    let requests = [
        request(1, 3, Window::new(0, 300)?, &chop),
        request(2, 2, Window::new(0, 300)?, &craft),
        request(3, 1, Window::new(0, 300)?, &wall),
    ];

    let starts = schedule(&mut plan, &requests, 10_000)?;
    for (name, start) in names.iter().zip(&starts) {
        match start {
            Some(start) => println!("{name:>12}: starts at minute {start}"),
            None => println!("{name:>12}: does not fit"),
        }
    }

    assert_eq!(starts, [Some(0), Some(30), Some(240)]);
    assert_eq!(plan.first_violation()?, None);
    explain(&mut plan, &r)
}

/// An empty plan with the colony's timelines.
fn colony() -> Result<(Plan, Resources), Error> {
    let mut plan = Plan::new();
    let resources = Resources {
        stamina: plan.add_timeline(Timeline::depletable(0, 60, Bounds::new(0, 100)?, 64)?)?,
        wood: plan.add_timeline(Timeline::depletable(0, 5, Bounds::new(0, 30)?, 64)?)?,
        planks: plan.add_timeline(Timeline::depletable(0, 0, Bounds::new(0, 50)?, 64)?)?,
        workbench: plan.add_timeline(Timeline::claimable(0, 1, 64)?)?,
        daylight: plan.add_state(StateTimeline::new(0, DAY, 64))?,
    };

    Ok((plan, resources))
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

/// Night falls at minute 120 and the day returns at minute 240; the settler sleeps from minute
/// 150 to 210 and regains one stamina per minute.
fn world(r: &Resources) -> [Demand; 3] {
    [
        state(
            r.daylight,
            StateImpact::Set {
                at: 120,
                value: NIGHT,
            },
        ),
        state(
            r.daylight,
            StateImpact::Set {
                at: 240,
                value: DAY,
            },
        ),
        level(
            r.stamina,
            Impact::Rate {
                start: 150,
                end: 210,
                per_tick: 1,
            },
        ),
    ]
}

/// Chop wood: 30 minutes outdoors in daylight at one stamina per minute, 20 logs at the end.
fn chop(r: &Resources) -> [Demand; 3] {
    [
        state(
            r.daylight,
            StateImpact::Require {
                start: 0,
                end: 30,
                value: DAY,
            },
        ),
        level(
            r.stamina,
            Impact::Rate {
                start: 0,
                end: 30,
                per_tick: -1,
            },
        ),
        level(r.wood, Impact::Step { at: 30, delta: 20 }),
    ]
}

/// Craft planks: take 15 logs, work the bench for 20 minutes, put 10 planks on the stockpile.
fn craft(r: &Resources) -> [Demand; 4] {
    [
        level(r.wood, Impact::Step { at: 0, delta: -15 }),
        level(
            r.workbench,
            Impact::Claim {
                start: 0,
                end: 20,
                amount: 1,
            },
        ),
        level(
            r.stamina,
            Impact::Rate {
                start: 0,
                end: 20,
                per_tick: -1,
            },
        ),
        level(r.planks, Impact::Step { at: 20, delta: 10 }),
    ]
}

/// Build a wall: take 10 planks and work 20 minutes outdoors in daylight at two stamina per
/// minute.
fn wall(r: &Resources) -> [Demand; 3] {
    [
        level(r.planks, Impact::Step { at: 0, delta: -10 }),
        state(
            r.daylight,
            StateImpact::Require {
                start: 0,
                end: 20,
                value: DAY,
            },
        ),
        level(
            r.stamina,
            Impact::Rate {
                start: 0,
                end: 20,
                per_tick: -2,
            },
        ),
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

/// Moves the wall to minute 50, right after the planks are ready, and prints why the settler
/// cannot build it then.
fn explain(plan: &mut Plan, r: &Resources) -> Result<(), Error> {
    let wall_id = ActivityId(3);
    plan.remove(wall_id);
    plan.place(wall_id, 50, &wall(r))?;
    if let Some(violation) = plan.first_violation()? {
        println!("forced to minute 50: {violation:?}");
    }

    Ok(())
}
