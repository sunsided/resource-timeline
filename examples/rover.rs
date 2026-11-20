//! A rover sol, scheduled greedily against energy, memory, transmitter and state timelines.
//!
//! One tick is one minute. The activities follow the example in the spaceflight track: warm up
//! the spectrometer, analyze a rock with the warm instrument, transmit the results inside a
//! comm pass. Solar charging runs all sol. The order between analysis and transmission is not
//! stated anywhere; it follows from the memory timeline, because transmitting frees memory that
//! only the analysis fills.
//!
//! Run with `cargo run --example rover`.

use resource_timeline::greedy::{Request, schedule};
use resource_timeline::{
    ActivityId, Bounds, Demand, Error, Impact, Plan, StateId, StateImpact, StateTimeline, Timeline,
    TimelineId, Window,
};

/// The spectrometer is cold.
const COLD: u32 = 0;
/// The spectrometer is at operating temperature.
const WARM: u32 = 1;
/// Earth is below the horizon.
const HIDDEN: u32 = 0;
/// Earth is in view of the high-gain antenna.
const VISIBLE: u32 = 1;

/// The environment: solar input and comm passes, fixed before scheduling.
const ENVIRONMENT: ActivityId = ActivityId(0);

/// The handles of the rover's resource timelines.
struct Resources {
    /// Battery energy in tenths of a watt-hour; reserve 100, capacity 700.
    battery: TimelineId,
    /// Free mass memory in MiB.
    memory: TimelineId,
    /// The high-gain transmitter, one user at a time.
    transmitter: TimelineId,
    /// Spectrometer temperature: [`COLD`] or [`WARM`].
    spectrometer: StateId,
    /// Earth visibility: [`HIDDEN`] or [`VISIBLE`].
    earth: StateId,
}

/// Builds the plan, schedules the sol and prints the outcome.
fn main() -> Result<(), Error> {
    let (mut plan, r) = rover()?;
    plan.place(ENVIRONMENT, 0, &environment(&r))?;

    let (warm_up, analyze, transmit, drive) = (warm_up(&r), analyze(&r), transmit(&r), drive(&r));
    let names = ["warm up", "analyze", "transmit", "drive"];
    let requests = [
        request(1, 3, Window::new(0, 60)?, &warm_up),
        request(2, 2, Window::new(0, 90)?, &analyze),
        request(3, 1, Window::new(0, 110)?, &transmit),
        request(4, 0, Window::new(0, 40)?, &drive),
    ];

    let starts = schedule(&mut plan, &requests, 10_000)?;
    for (name, start) in names.iter().zip(&starts) {
        match start {
            Some(start) => println!("{name:>8}: starts at minute {start}"),
            None => println!("{name:>8}: does not fit"),
        }
    }

    assert_eq!(plan.first_violation()?, None);
    explain(&mut plan, ActivityId(4), &drive)
}

/// An empty plan with the rover's timelines.
fn rover() -> Result<(Plan, Resources), Error> {
    let mut plan = Plan::new();
    let resources = Resources {
        battery: plan.add_timeline(Timeline::depletable(0, 300, Bounds::new(100, 700)?, 64)?)?,
        memory: plan.add_timeline(Timeline::depletable(0, 1_000, Bounds::new(0, 1_000)?, 64)?)?,
        transmitter: plan.add_timeline(Timeline::claimable(0, 1, 64)?)?,
        spectrometer: plan.add_state(StateTimeline::new(0, COLD, 64))?,
        earth: plan.add_state(StateTimeline::new(0, HIDDEN, 64))?,
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

/// Solar charging all sol and one comm pass from minute 60 to minute 75.
fn environment(r: &Resources) -> [Demand; 3] {
    [
        level(
            r.battery,
            Impact::Rate {
                start: 0,
                end: 120,
                per_tick: 2,
            },
        ),
        state(
            r.earth,
            StateImpact::Set {
                at: 60,
                value: VISIBLE,
            },
        ),
        state(
            r.earth,
            StateImpact::Set {
                at: 75,
                value: HIDDEN,
            },
        ),
    ]
}

/// Warm up the spectrometer: 10 minutes of heater power, then the instrument is warm.
fn warm_up(r: &Resources) -> [Demand; 2] {
    [
        level(
            r.battery,
            Impact::Rate {
                start: 0,
                end: 10,
                per_tick: -8,
            },
        ),
        state(
            r.spectrometer,
            StateImpact::Set {
                at: 10,
                value: WARM,
            },
        ),
    ]
}

/// Analyze a rock: 20 minutes with a warm instrument, producing 600 MiB of data.
fn analyze(r: &Resources) -> [Demand; 3] {
    [
        state(
            r.spectrometer,
            StateImpact::Require {
                start: 0,
                end: 20,
                value: WARM,
            },
        ),
        level(
            r.battery,
            Impact::Rate {
                start: 0,
                end: 20,
                per_tick: -6,
            },
        ),
        level(
            r.memory,
            Impact::Step {
                at: 20,
                delta: -600,
            },
        ),
    ]
}

/// Transmit the results: 8 minutes inside a comm pass, freeing the memory afterwards.
fn transmit(r: &Resources) -> [Demand; 4] {
    [
        state(
            r.earth,
            StateImpact::Require {
                start: 0,
                end: 8,
                value: VISIBLE,
            },
        ),
        level(
            r.transmitter,
            Impact::Claim {
                start: 0,
                end: 8,
                amount: 1,
            },
        ),
        level(
            r.battery,
            Impact::Rate {
                start: 0,
                end: 8,
                per_tick: -10,
            },
        ),
        level(r.memory, Impact::Step { at: 8, delta: 600 }),
    ]
}

/// Drive to the next target: 30 minutes of heavy power draw.
fn drive(r: &Resources) -> [Demand; 1] {
    [level(
        r.battery,
        Impact::Rate {
            start: 0,
            end: 30,
            per_tick: -12,
        },
    )]
}

/// A demand on a numeric timeline.
const fn level(timeline: TimelineId, impact: Impact) -> Demand {
    Demand::Level { timeline, impact }
}

/// A demand on a state timeline.
const fn state(timeline: StateId, impact: StateImpact) -> Demand {
    Demand::State { timeline, impact }
}

/// Forces `activity` in at minute 0 and prints why it does not fit.
fn explain(plan: &mut Plan, activity: ActivityId, demands: &[Demand]) -> Result<(), Error> {
    plan.place(activity, 0, demands)?;
    if let Some(violation) = plan.first_violation()? {
        println!("forced in at minute 0: {violation:?}");
    }

    Ok(())
}
