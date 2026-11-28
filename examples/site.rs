//! Crews on one floor of a building site, kept out of each other's workspace by zone resources.
//!
//! One tick is one working hour. Two slab bays (A and B) are formed, reinforced, fitted with
//! electrical sleeves and poured. Each task needs a crew of its trade (carpenters, ironworkers,
//! electricians, the concrete gang) and follows its predecessors. Some tasks also need the same
//! floor space: rebar and sleeves on the same bay cannot run at once, the formwork of bay B
//! blocks the corridor that the sleeves crew of bay A walks through, and both pours share the
//! pump position. These spatial overlaps are given as an undirected graph, as in multi-crew
//! construction scheduling models, and [`Overlaps`] turns each edge into a zone resource.
//!
//! The schedule is built by a serial schedule generation scheme, the usual priority-rule
//! baseline for project scheduling: tasks are taken in a precedence-feasible order and each
//! is placed at the earliest hour at which its predecessors are done and its crew and zones
//! are free ([`Plan::earliest_start`]). Without zones the crews alone allow a shorter schedule
//! that sends two crews into the same space; [`Conflict::of`] names the zone and both tasks.
//! With zones the schedule is conflict-free.
//!
//! Run with `cargo run --example site`.

use resource_timeline::zone::{Conflict, Overlaps, Zones, occupy};
use resource_timeline::{
    ActivityId, Demand, Error, Impact, Plan, Tick, Timeline, TimelineId, Window,
};

/// The last hour any task may start.
const HORIZON: Tick = 200;

/// The trades, as indices into the crew timelines.
#[derive(Debug, Clone, Copy)]
enum Trade {
    /// Formwork; two crews.
    Carpenters,
    /// Reinforcement; one crew.
    Ironworkers,
    /// Sleeves and conduits; two crews.
    Electricians,
    /// Pouring; one gang with the pump.
    Concrete,
}

/// The number of crews of each trade, in the order of [`Trade`].
const CREWS: [i64; 4] = [2, 1, 2, 1];

/// A task of the floor.
struct Task {
    /// The task's name for the report.
    name: &'static str,
    /// The trade whose crew does the work.
    trade: Trade,
    /// The working hours.
    hours: Tick,
    /// The tasks that must be done first, by index.
    after: &'static [usize],
}

/// The tasks in a precedence-feasible order.
const TASKS: [Task; 8] = [
    Task {
        name: "formwork A",
        trade: Trade::Carpenters,
        hours: 8,
        after: &[],
    },
    Task {
        name: "formwork B",
        trade: Trade::Carpenters,
        hours: 8,
        after: &[],
    },
    Task {
        name: "rebar A",
        trade: Trade::Ironworkers,
        hours: 6,
        after: &[0],
    },
    Task {
        name: "sleeves A",
        trade: Trade::Electricians,
        hours: 5,
        after: &[0],
    },
    Task {
        name: "rebar B",
        trade: Trade::Ironworkers,
        hours: 6,
        after: &[1],
    },
    Task {
        name: "sleeves B",
        trade: Trade::Electricians,
        hours: 5,
        after: &[1],
    },
    Task {
        name: "pour A",
        trade: Trade::Concrete,
        hours: 4,
        after: &[2, 3],
    },
    Task {
        name: "pour B",
        trade: Trade::Concrete,
        hours: 4,
        after: &[4, 5],
    },
];

/// Pairs of tasks whose workspaces intersect.
const OVERLAPS: [(usize, usize); 4] = [(2, 3), (4, 5), (1, 3), (6, 7)];

/// The activity id of task `index`.
fn task(index: usize) -> ActivityId {
    ActivityId(u32::try_from(index).expect("few tasks"))
}

/// A plan with one crew timeline per trade, and the handles of the crews.
fn crews() -> Result<(Plan, Vec<TimelineId>), Error> {
    let mut plan = Plan::new();
    let crews = CREWS
        .iter()
        .map(|&count| plan.add_timeline(Timeline::claimable(0, count, 32)?))
        .collect::<Result<_, _>>()?;
    Ok((plan, crews))
}

/// The demands of task `index`: one crew of its trade and every zone it shares.
fn demands(index: usize, crews: &[TimelineId], zones: Option<&Zones<'_>>) -> Vec<Demand> {
    let work = &TASKS[index];
    let crew = Demand::Level {
        timeline: crews[work.trade as usize],
        impact: Impact::Claim {
            start: 0,
            end: work.hours,
            amount: 1,
        },
    };
    let spaces = zones
        .into_iter()
        .flat_map(|zones| zones.of(task(index)))
        .map(|zone| occupy(zone, 0, work.hours));
    core::iter::once(crew).chain(spaces).collect()
}

/// Places every task by the serial schedule generation scheme and returns the starts.
fn serial(
    plan: &mut Plan,
    crews: &[TimelineId],
    zones: Option<&Zones<'_>>,
) -> Result<Vec<Tick>, Error> {
    let mut starts: Vec<Tick> = Vec::new();
    for (index, work) in TASKS.iter().enumerate() {
        let ready = work
            .after
            .iter()
            .map(|&before| starts[before] + TASKS[before].hours)
            .max()
            .unwrap_or(0);
        let demands = demands(index, crews, zones);
        let start = plan
            .earliest_start(&demands, Window::new(ready, HORIZON)?, 1_000)?
            .expect("the horizon leaves room");
        plan.place(task(index), start, &demands)?;
        starts.push(start);
    }

    Ok(starts)
}

/// The hour at which the last task ends.
fn makespan(starts: &[Tick]) -> Tick {
    starts
        .iter()
        .zip(&TASKS)
        .map(|(start, work)| start + work.hours)
        .max()
        .unwrap_or(0)
}

/// Prints the schedule as one line per task.
fn report(starts: &[Tick]) {
    for (work, start) in TASKS.iter().zip(starts) {
        let end = start + work.hours;
        let trade = format!("{:?}", work.trade);
        println!(
            "  {:<11} {trade:<12} hours {start:>2} to {end:>2}",
            work.name
        );
    }
}

fn main() -> Result<(), Error> {
    let edges: Vec<(ActivityId, ActivityId)> =
        OVERLAPS.iter().map(|&(a, b)| (task(a), task(b))).collect();
    let overlaps = Overlaps::new(&edges)?;

    // Crews only: the shortest schedule the crews allow.
    let (mut plan, crew_lines) = crews()?;
    let crews_only = serial(&mut plan, &crew_lines, None)?;
    println!("Crews only, makespan {} h:", makespan(&crews_only));
    report(&crews_only);

    // The same starts with the zones: two crews in one space.
    let (mut check, crew_lines) = crews()?;
    let zones = overlaps.add_zones(&mut check, 0, 4)?;
    for (index, start) in crews_only.iter().enumerate() {
        check.place(
            task(index),
            *start,
            &demands(index, &crew_lines, Some(&zones)),
        )?;
    }

    let violation = check.first_violation()?.expect("crews share a space");
    let conflict = Conflict::of(&check, &violation).expect("a zone conflict");
    let (first, second) = (conflict.first.0 as usize, conflict.second.0 as usize);
    println!(
        "  zone conflict at hour {}: {} and {} share a workspace",
        conflict.at, TASKS[first].name, TASKS[second].name
    );
    assert_eq!(zones.edge(conflict.zone), Some((task(2), task(3))));
    assert_eq!(conflict.at, 8);

    // Crews and zones: conflict-free.
    let (mut plan, crew_lines) = crews()?;
    let zones = overlaps.add_zones(&mut plan, 0, 4)?;
    let with_zones = serial(&mut plan, &crew_lines, Some(&zones))?;
    println!("Crews and zones, makespan {} h:", makespan(&with_zones));
    report(&with_zones);
    assert_eq!(plan.first_violation()?, None);
    assert!(makespan(&with_zones) > makespan(&crews_only));
    Ok(())
}
