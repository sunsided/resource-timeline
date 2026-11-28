//! A small satellite replans an imaging request onboard without a heap and downlinks its battery
//! profile in a checksummed frame.
//!
//! One tick is one minute of two 95-minute orbits: 60 minutes of sunlight (the panels charge
//! the battery) and 35 of eclipse (housekeeping drains it). A ground station is in view twice.
//! The ground commands one image and its downlink; the flight computer finds the earliest start
//! for each with fixed-capacity timelines ([`FixedPlan`]) and borrowed [`SearchScratch`], so
//! nothing on the onboard path allocates, and every container that runs full is a typed error.
//!
//! After placing them, the satellite samples its battery for housekeeping telemetry, exports
//! the level line as a [`LevelProfile`] (breakpoints, without the impacts behind it) and
//! encodes it into a byte frame with a CRC-32. The ground decodes the frame and answers level
//! queries from the profile alone; a corrupted byte is caught by the checksum. For replanning on
//! the ground, the whole timeline with its impacts is encoded the same way.
//!
//! Run with `cargo run --example cubesat`.

use resource_timeline::storage::{Fixed, FixedPlan};
use resource_timeline::{
    ActivityId, Bounds, Demand, Error, Impact, LevelProfile, Plan, SearchScratch, StateId,
    StateImpact, StateTimeline, Tick, Timeline, TimelineId, Window,
};

/// Impacts per timeline the flight software reserves room for.
const ROOM: usize = 32;
/// The onboard plan: two numeric timelines, one state timeline, [`ROOM`] impacts each.
type Onboard = Plan<FixedPlan<2, 1, ROOM>>;
/// The orbital period in minutes.
const ORBIT: Tick = 95;
/// The minutes of sunlight at the start of each orbit.
const SUNLIT: Tick = 60;
/// The ground station is below the horizon.
const HIDDEN: u32 = 0;
/// The ground station is in view.
const IN_VIEW: u32 = 1;
/// The orbit environment: charging, housekeeping and passes.
const ORBIT_ENV: ActivityId = ActivityId(0);
/// The commanded image.
const IMAGE: ActivityId = ActivityId(1);
/// The downlink of the image.
const DOWNLINK: ActivityId = ActivityId(2);

/// The handles of the onboard timelines.
struct Lines {
    /// Battery in tenths of a watt-hour: reserve 120, capacity 400.
    battery: TimelineId,
    /// The radio, one user at a time.
    radio: TimelineId,
    /// Ground station visibility: [`HIDDEN`] or [`IN_VIEW`].
    station: StateId,
}

/// The onboard plan with the two orbits of charging, housekeeping and passes.
fn onboard() -> Result<(Onboard, Lines), Error> {
    let mut plan = Onboard::new_in();
    let bounds = Bounds::new(120, 400)?;
    let lines = Lines {
        battery: plan.add_timeline(Timeline::depletable_in(0, 140, bounds, ROOM)?)?,
        radio: plan.add_timeline(Timeline::claimable_in(0, 1, ROOM)?)?,
        station: plan.add_state(StateTimeline::new_in(0, HIDDEN, ROOM))?,
    };

    let rate = |start, end, per_tick| Demand::Level {
        timeline: lines.battery,
        impact: Impact::Rate {
            start,
            end,
            per_tick,
        },
    };
    let set = |at, value| Demand::State {
        timeline: lines.station,
        impact: StateImpact::Set { at, value },
    };
    for t0 in [0, ORBIT] {
        let orbit = [
            rate(t0, t0 + SUNLIT, 3),
            rate(t0 + SUNLIT, t0 + ORBIT, -3),
            set(t0 + 40, IN_VIEW),
            set(t0 + 52, HIDDEN),
        ];
        plan.place(ORBIT_ENV, 0, &orbit)?;
    }

    Ok((plan, lines))
}

/// Places the image and its downlink at their earliest starts; returns both starts.
fn replan(plan: &mut Onboard, lines: &Lines) -> Result<(Tick, Tick), Error> {
    // The image draws 15 per minute for 10 minutes; the downlink draws 2 per minute for 8
    // minutes and needs the radio and the station in view.
    let image = [Demand::Level {
        timeline: lines.battery,
        impact: Impact::Rate {
            start: 0,
            end: 10,
            per_tick: -15,
        },
    }];
    let downlink = [
        Demand::Level {
            timeline: lines.battery,
            impact: Impact::Rate {
                start: 0,
                end: 8,
                per_tick: -2,
            },
        },
        Demand::Level {
            timeline: lines.radio,
            impact: Impact::Claim {
                start: 0,
                end: 8,
                amount: 1,
            },
        },
        Demand::State {
            timeline: lines.station,
            impact: StateImpact::Require {
                start: 0,
                end: 8,
                value: IN_VIEW,
            },
        },
    ];

    // Working memory for the search, borrowed instead of taken from the stack per call.
    let mut scratch = SearchScratch::<Fixed<ROOM>>::new();
    // The slot the ground asked for first, at sunset, would leave too little for the eclipse.
    assert!(!plan.fits_with(&image, SUNLIT, &mut scratch)?);
    let image_at = plan
        .earliest_start_with(&image, Window::new(0, 150)?, 256, &mut scratch)?
        .expect("the second orbit has room");
    plan.place(IMAGE, image_at, &image)?;
    let window = Window::new(image_at + 10, 180)?;
    let downlink_at = plan
        .earliest_start_with(&downlink, window, 256, &mut scratch)?
        .expect("the second pass carries the downlink");
    plan.place(DOWNLINK, downlink_at, &downlink)?;
    Ok((image_at, downlink_at))
}

/// Encodes the battery profile into `frame` and returns the encoded length.
fn telemetry(line: &Timeline<Fixed<ROOM>>, frame: &mut [u8]) -> Result<usize, Error> {
    // Housekeeping telemetry: the battery every 20 minutes, straight from the timeline.
    print!("Battery every 20 min:");
    for sample in line.samples(0, 20, 10)? {
        let (t, level) = sample?;
        print!(" {t}:{level}");
    }
    println!();

    // Downlink frame: the exported profile, encoded with a checksum.
    let profile = line.export()?;
    let length = profile.encode(frame)?;
    println!(
        "Profile: {} breakpoints for {} impacts, {length} bytes in the frame.",
        profile.len(),
        line.impacts().len()
    );
    Ok(length)
}

fn main() -> Result<(), Error> {
    let (mut plan, lines) = onboard()?;
    let (image_at, downlink_at) = replan(&mut plan, &lines)?;
    println!("Image at minute {image_at}, downlink at minute {downlink_at}.");
    assert_eq!(plan.first_violation()?, None);
    // The first eclipse would drain the battery below its reserve after any image in the
    // first orbit; in the second, charging must first make up for it (found in closed form
    // along the charging rate, not minute by minute). The downlink waits for the second pass.
    assert_eq!((image_at, downlink_at), (104, 135));

    let line = plan.timeline(lines.battery).expect("added above");
    let mut frame = [0_u8; 512];
    let length = telemetry(line, &mut frame)?;

    // On the ground: decode and query without the impacts.
    let received = LevelProfile::decode(&frame[..length])?;
    for t in [0, image_at + 10, downlink_at + 8, 2 * ORBIT] {
        assert_eq!(received.level_at(t)?, line.level_at(t)?);
    }
    let lowest = received
        .breakpoints()
        .min_by_key(|point| point.level)
        .expect("at least one breakpoint");
    println!(
        "Lowest breakpoint on the ground: {} at minute {}.",
        lowest.level, lowest.at
    );
    assert_eq!((lowest.at, lowest.level), (image_at + 10, 122));

    // For ground replanning, the whole battery timeline with its impacts and bounds.
    let mut full_frame = [0_u8; 1024];
    let full_length = line.encode(&mut full_frame)?;
    let copy = Timeline::decode(&full_frame[..full_length])?;
    println!(
        "Full timeline: {} impacts in {full_length} bytes; the ground copy is unviolated: {}.",
        copy.impacts().len(),
        copy.first_violation()?.is_none()
    );
    assert_eq!(copy.impacts(), line.impacts());

    // A bit flip on the way down is caught by the CRC.
    frame[length / 2] ^= 0x10;
    let corrupted = LevelProfile::decode(&frame[..length]);
    println!("Corrupted frame: {corrupted:?}");
    assert_eq!(corrupted, Err(Error::ChecksumMismatch));

    // The onboard containers are fixed: one more timeline than reserved is a typed error.
    let full = plan.add_timeline(Timeline::claimable_in(0, 1, ROOM)?);
    println!("Adding a third numeric timeline: {full:?}");
    assert!(full.is_err_and(|error| error.is_capacity()));
    Ok(())
}
