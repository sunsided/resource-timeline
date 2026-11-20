//! Benchmarks of the hot paths: the bound check sweep and the earliest-start search.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use resource_timeline::envelope::{Envelope, Flexible, Precedence};
use resource_timeline::storage::Fixed;
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

/// Measures `first_violation` on the same battery with fixed-capacity storage, the onboard path.
fn bench_first_violation_fixed(c: &mut Criterion) {
    let heap = battery();
    let bounds = heap.bounds();
    let mut fixed =
        Timeline::<Fixed<1_536>>::depletable_in(0, heap.initial(), bounds, 1_536).expect("valid");
    for &(activity, impact) in heap.impacts() {
        fixed.add(activity, impact).expect("room");
    }

    c.bench_function("first_violation/500_activities_fixed", |b| {
        b.iter(|| black_box(&fixed).first_violation());
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

/// Measures `earliest_start` with rates: a long drain that only fits once the battery has
/// recharged, found by the closed-form solve per piece over a long window.
fn bench_earliest_start_rates(c: &mut Criterion) {
    let bounds = Bounds::new(0, 1_000_000).expect("valid");
    let mut plan = Plan::new();
    let battery = plan
        .add_timeline(Timeline::depletable(0, 100, bounds, 64).expect("valid"))
        .expect("memory");
    let charge = [Demand::Level {
        timeline: battery,
        impact: Impact::Rate {
            start: 0,
            end: 100_000,
            per_tick: 1,
        },
    }];
    plan.place(ActivityId(0), 0, &charge).expect("valid");
    let drain = [Demand::Level {
        timeline: battery,
        impact: Impact::Step {
            at: 0,
            delta: -60_000,
        },
    }];
    let window = Window::new(0, 100_000).expect("valid");
    c.bench_function("earliest_start/rates_long_window", |b| {
        b.iter(|| black_box(&plan).earliest_start(black_box(&drain), window, 1_000));
    });
}

/// Measures exporting the profile of the benchmark battery for telemetry.
fn bench_export(c: &mut Criterion) {
    let timeline = battery();
    c.bench_function("export/500_activities", |b| {
        b.iter(|| black_box(&timeline).export());
    });
}

/// Measures a precedence-coupled envelope check: a chain of 20 claims on a transmitter, each
/// after the previous one, with overlapping windows.
fn bench_envelope_precedences(c: &mut Criterion) {
    let transmitter = Timeline::claimable(0, 1, 8).expect("valid");
    let hold = [Impact::Claim {
        start: 0,
        end: 5,
        amount: 1,
    }];
    let flexible: Vec<Flexible<'_>> = (0..20)
        .map(|id| Flexible {
            activity: ActivityId(id),
            window: Window::new(i64::from(id) * 5, i64::from(id) * 5 + 30).expect("valid"),
            impacts: &hold,
        })
        .collect();
    let chain: Vec<Precedence> = (1..20)
        .map(|id| Precedence {
            before: ActivityId(id - 1),
            after: ActivityId(id),
            gap: 5,
        })
        .collect();
    let envelope = Envelope::new(&transmitter, &flexible)
        .expect("valid")
        .with_precedences(&chain)
        .expect("consistent");
    c.bench_function("envelope/precedence_chain_20", |b| {
        b.iter(|| black_box(&envelope).check());
    });
}

criterion_group!(
    benches,
    bench_envelope_precedences,
    bench_first_violation,
    bench_first_violation_fixed,
    bench_earliest_start,
    bench_earliest_start_rates,
    bench_export
);
criterion_main!(benches);
