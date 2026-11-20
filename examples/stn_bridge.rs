//! Checks a flexible rover schedule against its battery and transmitter with the windows that
//! `temporal-control`'s dispatcher maintains, before and during execution.
//!
//! The plan is an STNU: the spectrometer warms up (5 to 15 minutes, contingent), then the rock
//! is analyzed (10 to 25 minutes, contingent), then the results are transmitted for 8 minutes
//! inside a comm pass from minute 100 to 160. `temporal-control` checks dynamic
//! controllability and dispatches it; this crate checks that every schedule the dispatcher may
//! still pick, under every duration nature may still choose, keeps the battery above its
//! reserve and the transmitter free of double bookings:
//!
//! - start windows come from `Dispatcher::window` (clamped to the horizon where unbounded);
//! - contingent durations come from the STNU's contingent links;
//! - "starts after the previous activity ended" becomes [`Precedence::after_end`].
//!
//! After each observation the windows narrow and the envelope check is re-run. At minute 0 a
//! long warm-up followed by a long analysis may drain the battery below its reserve during
//! the transmission; once the warm-up is observed to end at minute 12, every remaining
//! schedule is safe.
//!
//! Run with `cargo run --example stn_bridge`.

use resource_timeline::envelope::{Anchor, Contingent, Envelope, Precedence, Uncertain, Verdict};
use resource_timeline::{ActivityId, Bounds, Impact, Tick, Timeline, Window};
use temporal_control::{Budget, Controllability, Dispatcher, Stnu, Ticks, TimePoint};

/// The planning horizon in minutes; unbounded windows are clamped to it.
const HORIZON: Tick = 200;

/// The time points of the plan.
struct Points {
    /// The plan origin, executed at minute 0.
    origin: TimePoint,
    /// Start of the warm-up.
    warm: TimePoint,
    /// End of the warm-up (contingent).
    warm_end: TimePoint,
    /// Start of the analysis.
    analyze: TimePoint,
    /// End of the analysis (contingent).
    analyze_end: TimePoint,
    /// Start of the transmission.
    transmit: TimePoint,
}

/// The activities and their contingent duration ranges, from the STNU.
const WARM_UP: (ActivityId, Tick, Tick) = (ActivityId(1), 5, 15);
/// The analysis.
const ANALYZE: (ActivityId, Tick, Tick) = (ActivityId(2), 10, 25);
/// The transmission (fixed 8 minutes, modelled as a contingent of exactly 8).
const TRANSMIT: (ActivityId, Tick, Tick) = (ActivityId(3), 8, 8);

/// Builds the STNU, dispatches the origin and checks resources as observations arrive.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (stnu, points) = plan()?;
    let Controllability::Controllable(network) = stnu.dispatchable(&Budget::UNLIMITED)? else {
        return Err("the plan is not dynamically controllable".into());
    };
    let mut dispatcher: Dispatcher<8, 64> = network.dispatcher()?;
    let mut executed = [TimePoint(0); 8];

    // Minute 0: the origin and (as soon as possible) the warm-up start.
    let count = dispatcher.step(Ticks(0), &[], &mut executed)?;
    println!("minute 0: executed {:?}", &executed[..count]);
    report("after minute 0", &dispatcher, &points)?;

    // Minute 12: the warm-up ends, earlier than its worst case.
    let count = dispatcher.step(Ticks(12), &[points.warm_end], &mut executed)?;
    println!(
        "minute 12: warm-up ended, executed {:?}",
        &executed[..count]
    );
    report("after minute 12", &dispatcher, &points)?;
    Ok(())
}

/// The rover STNU and its time points.
fn plan() -> Result<(Stnu, Points), temporal_control::Error> {
    let mut stnu = Stnu::new();
    let origin = stnu.add_time_point()?;
    let warm = stnu.add_time_point()?;
    stnu.add_constraint(origin, warm, Some(Ticks(0)), Some(Ticks(60)))?;
    let warm_end = stnu.add_contingent_link(warm, Ticks(WARM_UP.1), Ticks(WARM_UP.2))?;
    let analyze = stnu.add_time_point()?;
    stnu.add_constraint(warm_end, analyze, Some(Ticks(0)), Some(Ticks(HORIZON)))?;
    let analyze_end = stnu.add_contingent_link(analyze, Ticks(ANALYZE.1), Ticks(ANALYZE.2))?;
    let transmit = stnu.add_time_point()?;
    stnu.add_constraint(analyze_end, transmit, Some(Ticks(0)), Some(Ticks(HORIZON)))?;
    stnu.add_constraint(
        origin,
        transmit,
        Some(Ticks(100)),
        Some(Ticks(160 - TRANSMIT.1)),
    )?;

    let points = Points {
        origin,
        warm,
        warm_end,
        analyze,
        analyze_end,
        transmit,
    };
    Ok((stnu, points))
}

/// The dispatcher's current window of `point`, clamped to `[0, HORIZON]`, or its time once it
/// has happened.
fn window(
    dispatcher: &Dispatcher<8, 64>,
    point: TimePoint,
) -> Result<Window, resource_timeline::Error> {
    if let Some(time) = dispatcher.time(point) {
        return Ok(Window::at(time.0));
    }

    let (lo, hi) = dispatcher.window(point).unwrap_or((None, None));
    let lo = lo.map_or(0, |ticks| ticks.0).max(0);
    let hi = hi.map_or(HORIZON, |ticks| ticks.0).min(HORIZON);
    Window::new(lo, hi)
}

/// Checks the battery and the transmitter over every schedule the dispatcher still admits.
fn report(
    when: &str,
    dispatcher: &Dispatcher<8, 64>,
    points: &Points,
) -> Result<(), Box<dyn std::error::Error>> {
    debug_assert!(dispatcher.time(points.origin).is_some());
    let warm_window = window(dispatcher, points.warm)?;
    // Once the warm-up has ended, its duration is known.
    let warm_duration = match (
        dispatcher.time(points.warm),
        dispatcher.time(points.warm_end),
    ) {
        (Some(start), Some(end)) => Window::at(end.0 - start.0),
        _ => Window::new(WARM_UP.1, WARM_UP.2)?,
    };
    let analyze_duration = match dispatcher.time(points.analyze_end) {
        Some(_) => return Ok(()),
        None => Window::new(ANALYZE.1, ANALYZE.2)?,
    };

    // Battery in tenths of a watt-hour: 450 at minute 0, reserve 100, solar input 1 per minute.
    let mut battery = Timeline::depletable(0, 450, Bounds::new(100, 1_000)?, 16)?;
    battery.add(
        ActivityId(0),
        Impact::Rate {
            start: 0,
            end: HORIZON,
            per_tick: 1,
        },
    )?;
    let warm_draw = [Uncertain::Rate {
        start: Anchor::Start(0),
        end: Anchor::End(0),
        per_tick: -12,
    }];
    let analyze_draw = [Uncertain::Rate {
        start: Anchor::Start(0),
        end: Anchor::End(0),
        per_tick: -6,
    }];
    let transmit_draw = [Uncertain::Rate {
        start: Anchor::Start(0),
        end: Anchor::End(0),
        per_tick: -20,
    }];
    let contingent = [
        Contingent {
            activity: WARM_UP.0,
            window: warm_window,
            duration: warm_duration,
            impacts: &warm_draw,
        },
        Contingent {
            activity: ANALYZE.0,
            window: window(dispatcher, points.analyze)?,
            duration: analyze_duration,
            impacts: &analyze_draw,
        },
        Contingent {
            activity: TRANSMIT.0,
            window: window(dispatcher, points.transmit)?,
            duration: Window::at(TRANSMIT.1),
            impacts: &transmit_draw,
        },
    ];
    let order = [
        Precedence::after_end(WARM_UP.0, ANALYZE.0, 0),
        Precedence::after_end(ANALYZE.0, TRANSMIT.0, 0),
    ];

    let envelope = Envelope::new(&battery, &[])?
        .with_contingent(&contingent)?
        .with_precedences(&order)?;
    println!("{when}:");
    for (name, activity) in [
        ("warm-up", WARM_UP.0),
        ("analysis", ANALYZE.0),
        ("transmit", TRANSMIT.0),
    ] {
        let tightened = envelope.window(activity).ok_or("known activity")?;
        println!(
            "  {name:>9} may start in [{}, {}]",
            tightened.earliest(),
            tightened.latest()
        );
    }

    match envelope.check()? {
        Verdict::Safe => println!("  battery: safe for every remaining schedule and duration"),
        Verdict::Possible(violation) => println!(
            "  battery: may drop to {} at minute {} (activities {:?})",
            violation.level, violation.at, violation.contributors
        ),
        Verdict::Certain(violation) => println!(
            "  battery: drops to {} at minute {} in every schedule",
            violation.level, violation.at
        ),
    }

    let lowest = (0..=HORIZON)
        .map(|t| envelope.lower_at(t))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .unwrap_or_default();
    println!("  lowest possible battery level: {lowest}");
    Ok(())
}
