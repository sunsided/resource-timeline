//! Three lunar rovers share the return link of a relay orbiter without seeing each other's use.
//!
//! One tick is one minute of a 180-minute relay pass; the link carries 16 Mbit/s. Each rover
//! downlinks over the same link but only learns what the others do when the ground tells it.
//! Before the pass the ground splits the link into shares with [`split`]: every rover gets its
//! peak need plus a margin, and the shares never add up to more than 16, so a rover that stays
//! within its own share can never oversubscribe the link, whatever the others do.
//!
//! - Rovers A and B have fixed downlinks; their need is the peak of their own share timeline.
//! - Rover C may start its two downlinks anywhere in a window. Its need is the worst case over
//!   every start, from [`Envelope::lowest_from`]. With independent windows both downlinks may
//!   overlap and the link is oversubscribed; ordering them with a [`Precedence`] fixes it.
//!
//! Onboard, each rover keeps the link at its full capacity with the part that belongs to the
//! others held by a claim of the reserved activity [`OTHERS`]. At minute 90 rovers A and B are
//! in contact with the ground (C is behind a crater rim). B wants a panorama that its share
//! cannot carry, so A and B re-split *their own two shares* by their remaining needs; C keeps
//! its share and may be using it.
//!
//! Run with `cargo run --example relay`.

use resource_timeline::envelope::{Envelope, Flexible, Precedence, Verdict};
use resource_timeline::share::split;
use resource_timeline::{ActivityId, Bound, Error, Impact, Level, Tick, Timeline, Window};

/// The capacity of the relay's return link in Mbit/s.
const LINK: Level = 16;
/// The end of the relay pass in minutes.
const PASS: Tick = 180;
/// The margin every share keeps above its need, for work added onboard.
const MARGIN: Level = 1;
/// The minute at which rovers A and B are in contact with the ground.
const CONTACT: Tick = 90;
/// The reservation that holds the others' part of the link on a rover's share timeline.
const OTHERS: ActivityId = ActivityId(0);

/// A downlink of `rate` Mbit/s from minute `start` to `end`.
const fn downlink(start: Tick, end: Tick, rate: Level) -> Impact {
    Impact::Claim {
        start,
        end,
        amount: rate,
    }
}

/// A share timeline: the full link, with everything above `share` held for the others.
fn share_line(share: Level) -> Result<Timeline, Error> {
    let mut line = Timeline::claimable(0, LINK, 16)?;
    reserve(&mut line, 0, PASS, share)?;
    Ok(line)
}

/// Holds the part of the link above `share` for the others from `from` to `until`.
fn reserve(line: &mut Timeline, from: Tick, until: Tick, share: Level) -> Result<(), Error> {
    if share < LINK && from < until {
        line.add(OTHERS, downlink(from, until, LINK - share))?;
    }

    Ok(())
}

/// Changes the share of `line` from `old` to `new` at `at`, keeping the past as it was.
fn reshare(line: &mut Timeline, at: Tick, old: Level, new: Level) -> Result<(), Error> {
    line.remove(OTHERS);
    reserve(line, 0, at, old)?;
    reserve(line, at, PASS, new)
}

/// Rover C's downlink: 4 Mbit/s for 20 minutes.
const C_BURST: [Impact; 1] = [downlink(0, 20, 4)];
/// Rover C's second downlink starts after its first one ends.
const C_ORDER: [Precedence; 1] = [Precedence::after_end(ActivityId(21), ActivityId(22), 0)];

/// The peak use of `line` from `from` on, given its current `share`.
fn need(line: &Timeline, share: Level, from: Tick) -> Result<Level, Error> {
    Ok(share - line.lowest_from(from)?)
}

/// Rover C's two downlinks, each starting anywhere in minutes 0 to 60.
fn c_downlinks() -> Result<[Flexible<'static>; 2], Error> {
    let window = Window::new(0, 60)?;
    let burst = |id| Flexible {
        activity: ActivityId(id),
        window,
        impacts: &C_BURST,
    };
    Ok([burst(21), burst(22)])
}

/// The share timelines of rovers A and B at the full link, with their fixed downlinks.
fn fixed_rovers() -> Result<(Timeline, Timeline), Error> {
    let mut a = share_line(LINK)?;
    a.add(ActivityId(1), downlink(10, 40, 4))?;
    a.add(ActivityId(2), downlink(100, 130, 3))?;
    let mut b = share_line(LINK)?;
    b.add(ActivityId(11), downlink(0, PASS, 2))?;
    b.add(ActivityId(12), downlink(50, 80, 2))?;
    Ok((a, b))
}

/// The ground's split before the pass; returns the shares and C's worst-case need.
fn ground_split(a: &Timeline, b: &Timeline) -> Result<(Vec<Level>, Level), Error> {
    let c_open = share_line(LINK)?;
    let c = c_downlinks()?;
    let free = Envelope::new(&c_open, &c)?;
    let needs = [
        need(a, LINK, 0)?,
        need(b, LINK, 0)?,
        LINK - free.lowest_from(0)?,
    ];
    println!("Needs with C's downlinks free to overlap: {needs:?}");
    assert_eq!(needs, [4, 4, 8]);
    let refused = split(LINK, MARGIN, &needs).err();
    println!("  split: {refused:?}");
    assert_eq!(
        refused,
        Some(Error::Oversubscribed {
            needed: 19,
            pool: LINK
        })
    );

    // Ordered, C never holds more than one downlink at once.
    let c_need = LINK - free.with_precedences(&C_ORDER)?.lowest_from(0)?;
    let needs = [needs[0], needs[1], c_need];
    let shares: Vec<Level> = split(LINK, MARGIN, &needs)?.collect();
    println!("Needs with C's downlinks ordered: {needs:?}, shares {shares:?}");
    assert_eq!(shares, [6, 5, 5]);
    Ok((shares, c_need))
}

/// The pass up to the contact: every rover checks its own share only.
fn before_contact(a: &mut Timeline, b: &Timeline, c_share: Level) -> Result<(), Error> {
    assert_eq!(a.first_violation()?, None);
    assert_eq!(b.first_violation()?, None);
    let c = share_line(c_share)?;
    let c_flexible = c_downlinks()?;
    let verdict = Envelope::new(&c, &c_flexible)?
        .with_precedences(&C_ORDER)?
        .check()?;
    println!("  C's envelope on its share of {c_share}: {verdict:?}");
    assert_eq!(verdict, Verdict::Safe);

    // A adds a housekeeping dump onboard; the margin absorbs it.
    a.add(ActivityId(3), downlink(60, 90, 2))?;
    assert_eq!(a.first_violation()?, None);
    println!("A adds a 2 Mbit/s dump at minute 60 within its share: safe.");
    Ok(())
}

/// The contact of A and B: B's panorama fails on its share, they re-split their two shares.
fn contact(a: &mut Timeline, b: &mut Timeline, shares: &[Level]) -> Result<Vec<Level>, Error> {
    let panorama = (ActivityId(13), downlink(120, 150, 6));
    let mut trial = b.clone();
    trial.add(panorama.0, panorama.1)?;
    let violation = trial.first_violation()?.expect("the share is too small");
    println!(
        "B's panorama alone: {} Mbit/s short at minute {}, held by {:?}",
        -violation.level, violation.at, violation.contributors
    );
    assert_eq!(violation.bound, Bound::BelowMin(0));
    assert!(violation.contributors.contains(&OTHERS));

    let needs = [
        need(a, shares[0], CONTACT)?,
        need(&trial, shares[1], CONTACT)?,
    ];
    let pool = shares[0] + shares[1];
    let resplit: Vec<Level> = split(pool, 0, &needs)?.collect();
    println!(
        "Contact at minute {CONTACT}: A and B need {needs:?} of their {pool}, new shares {resplit:?}"
    );
    assert_eq!(resplit, [3, 8]);
    reshare(a, CONTACT, shares[0], resplit[0])?;
    reshare(b, CONTACT, shares[1], resplit[1])?;
    b.add(panorama.0, panorama.1)?;
    assert_eq!(a.first_violation()?, None);
    assert_eq!(b.first_violation()?, None);
    println!(
        "  B's panorama fits its new share; C still holds {}.",
        shares[2]
    );
    Ok(resplit)
}

fn main() -> Result<(), Error> {
    println!("Relay pass: {LINK} Mbit/s for {PASS} minutes, three rovers, margin {MARGIN}.");
    let (mut a, mut b) = fixed_rovers()?;
    let (shares, c_need) = ground_split(&a, &b)?;
    reshare(&mut a, 0, LINK, shares[0])?;
    reshare(&mut b, 0, LINK, shares[1])?;
    before_contact(&mut a, &b, shares[2])?;
    let resplit = contact(&mut a, &mut b, &shares)?;

    // The link itself, with C at its worst case: never above its capacity.
    let used = |line: &Timeline, share: Level, t| share - line.level_at(t).unwrap_or(share);
    let peak = (0..PASS)
        .map(|t| {
            let (a_share, b_share) = if t < CONTACT {
                (shares[0], shares[1])
            } else {
                (resplit[0], resplit[1])
            };
            used(&a, a_share, t) + used(&b, b_share, t) + c_need
        })
        .max()
        .unwrap_or(0);
    println!("Peak link use over the pass, C at its worst: {peak} of {LINK} Mbit/s.");
    assert!(peak <= LINK);
    Ok(())
}
