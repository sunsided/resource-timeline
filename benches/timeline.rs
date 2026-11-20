//! Benchmarks of the hot paths: the bound check sweep and the earliest-start search.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use resource_timeline::{ActivityId, Bounds, Demand, Impact, Plan, Timeline, Window};

/// Number of activities in the benchmark timelines.
const ACTIVITIES: u32 = 500;

/// A battery with `ACTIVITIES` interleaved claims, drains and charges that never violates.
fn battery() -> Timeline {
    let bounds = Bounds::new(0, 1_000_000).expect("valid");
    let mut timeline = Timeline::depletable(0, 500_000, bounds, 4 * 1_024).expect("valid");
    for id in 0..ACTIVITIES {
        let start = i64::from(id) * 7;
        let activity = ActivityId(id);
        let impacts = [
            Impact::Claim {
                start,
                end: start + 20,
                amount: 50,
            },
            Impact::Rate {
                start,
                end: start + 15,
                per_tick: -3,
            },
            Impact::Step {
                at: start + 18,
                delta: 45,
            },
        ];
        for impact in impacts {
            timeline.add(activity, impact).expect("valid");
        }
    }

    timeline
}

/// A plan with one transmitter that is busy for 8 of every 10 ticks.
fn busy_plan() -> (Plan, [Demand; 1]) {
    let mut plan = Plan::new();
    let transmitter = plan
        .add_timeline(Timeline::claimable(0, 1, 1_024).expect("valid"))
        .expect("memory");
    let pass = |length| {
        [Demand::Level {
            timeline: transmitter,
            impact: Impact::Claim {
                start: 0,
                end: length,
                amount: 1,
            },
        }]
    };
    for id in 0..ACTIVITIES {
        plan.place(ActivityId(id), i64::from(id) * 10, &pass(8))
            .expect("valid");
    }

    (plan, pass(5))
}

/// Measures `first_violation` on a violation-free timeline, which sweeps every change point.
fn bench_first_violation(c: &mut Criterion) {
    let timeline = battery();
    c.bench_function("first_violation/500_activities", |b| {
        b.iter(|| black_box(&timeline).first_violation());
    });
}

/// Measures `earliest_start` for a pass that only fits after the last busy slot.
fn bench_earliest_start(c: &mut Criterion) {
    let (plan, pass) = busy_plan();
    let window = Window::new(0, 10_000).expect("valid");
    c.bench_function("earliest_start/500_claims", |b| {
        b.iter(|| black_box(&plan).earliest_start(black_box(&pass), window, 1_000_000));
    });
}

criterion_group!(benches, bench_first_violation, bench_earliest_start);
criterion_main!(benches);
