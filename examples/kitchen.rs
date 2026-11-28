//! A restaurant kitchen checks its prep plan once for every start the cooks may still choose.
//!
//! One tick is one minute of the two hours before service. The kitchen runs on one 11 kW
//! circuit; the dishwasher draws 4 kW for the first hour. Three jobs have start windows instead
//! of fixed times, because the cooks pick the moment: bread in the combi oven (6 kW, 25
//! minutes, steam mode), a roast in the same oven (6 kW, 40 minutes, dry heat) and a fryer
//! batch (5 kW, 15 minutes).
//!
//! Two envelopes answer whether the cooks can start each job anywhere in its window:
//!
//! - [`Envelope`] bounds the circuit load over every combination of starts. If its lower
//!   envelope never drops below zero free kilowatts, no choice trips the breaker.
//! - [`StateEnvelope`] does the same for the oven's mode: each job sets the mode it needs at
//!   its start and requires it while it runs, so overlapping jobs may find the oven in the
//!   wrong mode.
//!
//! The first windows allow bad combinations, reported as possible violations with the jobs
//! that can cause them. Narrowed windows make both checks [`Verdict::Safe`] and
//! [`StateVerdict::Safe`]: the cooks are free within them and need no further checks.
//!
//! Run with `cargo run --example kitchen`.

use resource_timeline::envelope::{
    Envelope, Flexible, FlexibleState, StateEnvelope, StateVerdict, Verdict,
};
use resource_timeline::{ActivityId, Error, Impact, StateImpact, StateTimeline, Timeline, Window};

/// The oven is off.
const OFF: u32 = 0;
/// The oven steams, for bread.
const STEAM: u32 = 1;
/// The oven bakes with dry heat, for the roast.
const DRY: u32 = 2;

/// The dishwasher, fixed before the plan.
const DISHWASHER: ActivityId = ActivityId(0);
/// The bread bake.
const BREAD: ActivityId = ActivityId(1);
/// The roast.
const ROAST: ActivityId = ActivityId(2);
/// The fryer batch.
const FRYER: ActivityId = ActivityId(3);

/// A draw of `kw` kilowatts for the first `minutes` of a job.
const fn draw(minutes: i64, kw: i64) -> Impact {
    Impact::Claim {
        start: 0,
        end: minutes,
        amount: kw,
    }
}

/// The oven set to `mode` at the job start and required in it for `minutes`.
const fn mode(minutes: i64, mode: u32) -> [StateImpact; 2] {
    [
        StateImpact::Set { at: 0, value: mode },
        StateImpact::Require {
            start: 0,
            end: minutes,
            value: mode,
        },
    ]
}

/// The circuit and oven checks for windows of bread, roast and fryer.
fn check(windows: [Window; 3]) -> Result<(Verdict, StateVerdict), Error> {
    let mut circuit = Timeline::claimable(0, 11, 8)?;
    circuit.add(
        DISHWASHER,
        Impact::Claim {
            start: 0,
            end: 60,
            amount: 4,
        },
    )?;
    let (bread, roast, fryer) = ([draw(25, 6)], [draw(40, 6)], [draw(15, 5)]);
    let jobs = [
        Flexible {
            activity: BREAD,
            window: windows[0],
            impacts: &bread,
        },
        Flexible {
            activity: ROAST,
            window: windows[1],
            impacts: &roast,
        },
        Flexible {
            activity: FRYER,
            window: windows[2],
            impacts: &fryer,
        },
    ];
    let power = Envelope::new(&circuit, &jobs)?;
    println!(
        "  free kW at minute 30: {} to {}",
        power.lower_at(30)?,
        power.upper_at(30)?
    );

    let oven = StateTimeline::new(0, OFF, 8);
    let (steam, dry) = (mode(25, STEAM), mode(40, DRY));
    let modes = [
        FlexibleState {
            activity: BREAD,
            window: windows[0],
            impacts: &steam,
        },
        FlexibleState {
            activity: ROAST,
            window: windows[1],
            impacts: &dry,
        },
    ];
    let oven_check = StateEnvelope::new(&oven, &modes)?.check()?;
    Ok((power.check()?, oven_check))
}

fn main() -> Result<(), Error> {
    println!("First windows: bread 10-50, roast 20-80, fryer 0-60.");
    let first = [
        Window::new(10, 50)?,
        Window::new(20, 80)?,
        Window::new(0, 60)?,
    ];
    let (power, oven) = check(first)?;
    println!("  circuit: {power:?}");
    println!("  oven:    {oven:?}");
    match &power {
        Verdict::Possible(violation) => {
            // At minute 10 the dishwasher, the bread and the fryer may all draw at once.
            assert_eq!((violation.at, violation.level), (10, -4));
            assert_eq!(violation.contributors, [DISHWASHER, BREAD, FRYER]);
        }
        other => panic!("expected a possible overload, got {other:?}"),
    }
    assert!(matches!(oven, StateVerdict::Possible(_)));

    // Bread first, the roast once the bread is out, the fryer after the dishwasher.
    println!("Narrowed windows: bread 10-30, roast 55-80, fryer 60-90.");
    let narrowed = [
        Window::new(10, 30)?,
        Window::new(55, 80)?,
        Window::new(60, 90)?,
    ];
    let (power, oven) = check(narrowed)?;
    println!("  circuit: {power:?}");
    println!("  oven:    {oven:?}");
    assert_eq!(power, Verdict::Safe);
    assert_eq!(oven, StateVerdict::Safe);
    Ok(())
}
