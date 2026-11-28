//! Benchmark of a multi-crew construction schedule with spatial-overlap zones.
//!
//! The instance has the size of the multi-crew case in *A scalable RL framework for task
//! allocation in flexible construction scenarios* (ISARC 2026, part 2, pp. 774-781): 86 tasks,
//! 7 crews in 5 trades, 36 components and 34 spatial-overlap edges, with durations from
//! quantities and productivity rates. The instance here is **synthetic**: generated
//! deterministically with the same dimensions, because the paper's data was not available when
//! it was written. It does not reproduce the paper's tasks, durations or edges. Its numbers are therefore not comparable with the paper's results.
//!
//! The baseline is a serial schedule generation scheme: tasks in a precedence-feasible order,
//! each placed at its earliest feasible start over its crew and zone timelines. Run with
//! `cargo bench --bench site`; the makespans are printed once before timing.

use std::hint::black_box;

use criterion::Criterion;
use resource_timeline::zone::{Overlaps, Zones, occupy};
use resource_timeline::{ActivityId, Demand, Impact, Plan, Tick, Timeline, TimelineId, Window};

/// The number of tasks.
const TASKS: usize = 86;
/// The number of building components.
const COMPONENTS: usize = 36;
/// The number of spatial-overlap edges.
const EDGES: usize = 34;
/// Crews per trade; 7 crews in 5 trades.
const CREWS: [i64; 5] = [2, 2, 1, 1, 1];
/// Quantity units each trade's crew completes per hour.
const PRODUCTIVITY: [i64; 5] = [6, 4, 5, 3, 8];
/// The last hour any task may start.
const HORIZON: Tick = 100_000;

/// A synthetic task.
#[derive(Debug, Clone)]
struct Task {
    /// The trade, an index into [`CREWS`].
    trade: usize,
    /// The working hours: the quantity divided by the trade's productivity, rounded up.
    hours: Tick,
    /// The previous task on the same component, if any.
    after: Option<usize>,
}

/// A small linear congruential generator, so the instance is the same on every run.
struct Lcg(u64);

impl Lcg {
    /// The next value in `0..bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// The synthetic instance: tasks in a precedence-feasible order and the overlap edges.
fn instance() -> (Vec<Task>, Vec<(ActivityId, ActivityId)>) {
    let mut random = Lcg(0x5eed);
    // Every component gets two tasks, 14 of them a third; trades follow the build order.
    let mut component_of = Vec::with_capacity(TASKS);
    let mut tasks: Vec<Task> = Vec::with_capacity(TASKS);
    for round in 0..3 {
        let components = if round < 2 {
            COMPONENTS
        } else {
            TASKS - 2 * COMPONENTS
        };
        for component in 0..components {
            let trade = (round * 2 + component % 2) % CREWS.len();
            let quantity = 10 + i64::try_from(random.below(50)).expect("small");
            let hours = (quantity + PRODUCTIVITY[trade] - 1) / PRODUCTIVITY[trade];
            tasks.push(Task {
                trade,
                hours,
                after: None,
            });
            component_of.push(component);
        }
    }

    // Precedences: each task after the previous task on its component.
    let mut previous = [None; COMPONENTS];
    for (index, task) in tasks.iter_mut().enumerate() {
        task.after = previous[component_of[index]];
        previous[component_of[index]] = Some(index);
    }

    // Overlaps between tasks of neighbouring components, no repeats.
    let mut edges: Vec<(ActivityId, ActivityId)> = Vec::with_capacity(EDGES);
    while edges.len() < EDGES {
        let a = usize::try_from(random.below(TASKS as u64)).expect("small");
        let b = usize::try_from(random.below(TASKS as u64)).expect("small");
        let neighbours = component_of[a].abs_diff(component_of[b]) == 1;
        let (a, b) = (id(a), id(b));
        if neighbours && !edges.iter().any(|&e| e == (a, b) || e == (b, a)) {
            edges.push((a, b));
        }
    }

    (tasks, edges)
}

/// The activity id of task `index`.
fn id(index: usize) -> ActivityId {
    ActivityId(u32::try_from(index).expect("few tasks"))
}

/// Builds the crew timelines and, if `edges` is given, the zones, then schedules every task
/// serially and returns the makespan.
fn schedule(tasks: &[Task], edges: Option<&[(ActivityId, ActivityId)]>) -> Tick {
    let mut plan = Plan::new();
    let crews: Vec<TimelineId> = CREWS
        .iter()
        .map(|&count| {
            let line = Timeline::claimable(0, count, 2 * TASKS).expect("valid");
            plan.add_timeline(line).expect("memory")
        })
        .collect();
    let zones: Option<Zones<'_>> = edges.map(|edges| {
        Overlaps::new(edges)
            .expect("valid")
            .add_zones(&mut plan, 0, 4)
            .expect("memory")
    });

    let mut ends: Vec<Tick> = Vec::with_capacity(tasks.len());
    let mut demands: Vec<Demand> = Vec::new();
    for (index, task) in tasks.iter().enumerate() {
        let ready = task.after.map_or(0, |before| ends[before]);
        demands.clear();
        demands.push(Demand::Level {
            timeline: crews[task.trade],
            impact: Impact::Claim {
                start: 0,
                end: task.hours,
                amount: 1,
            },
        });
        if let Some(zones) = &zones {
            demands.extend(zones.of(id(index)).map(|zone| occupy(zone, 0, task.hours)));
        }

        let window = Window::new(ready, HORIZON).expect("valid");
        let start = plan
            .earliest_start(&demands, window, 1_000_000)
            .expect("small")
            .expect("the horizon leaves room");
        plan.place(id(index), start, &demands).expect("room");
        ends.push(start + task.hours);
    }

    debug_assert_eq!(plan.first_violation().expect("small"), None);
    ends.into_iter().max().unwrap_or(0)
}

/// Runs the benchmarks with criterion's command-line flags.
fn main() {
    let (tasks, edges) = instance();
    assert_eq!((tasks.len(), edges.len()), (TASKS, EDGES));
    println!(
        "synthetic site: makespan {} h with crews only, {} h with zones",
        schedule(&tasks, None),
        schedule(&tasks, Some(&edges))
    );

    let mut criterion = Criterion::default().configure_from_args();
    criterion.bench_function("site/serial_86_tasks_crews", |b| {
        b.iter(|| schedule(black_box(&tasks), None));
    });
    criterion.bench_function("site/serial_86_tasks_zones", |b| {
        b.iter(|| schedule(black_box(&tasks), Some(black_box(&edges))));
    });
    criterion.final_summary();
}
