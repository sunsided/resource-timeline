//! Admissions to a rolling therapy group with a hard limit of eight seats.
//!
//! One tick is one group session that takes place. Weeks with 1:1 calls instead, and sessions the
//! therapist cancels, are not ticks: they delay every patient on the calendar alike but never
//! put a ninth patient in a seat, so the plan needs no cap on them. A patient's programme ends
//! after [`SESSIONS`] sessions, but its length is uncertain:
//!
//! - An excused absence is made up the next session, so it adds one session to the stay. The
//!   [`EXCUSED_OUT`]th excused absence ends the programme.
//! - An unexcused absence counts against the [`SESSIONS`] without a make-up. The
//!   [`UNEXCUSED_OUT`]th ends the programme.
//!
//! So a stay lasts between a few sessions (a dropout) and the remaining sessions plus the
//! excused absences still allowed. Each patient holds one seat for as long as the stay
//! runs, a [`Contingent`] claim on a seat timeline with capacity eight. For each patient on the
//! waitlist the example prints two sessions:
//!
//! - **On time:** the session at which a seat frees if everyone attends every session.
//! - **Guaranteed:** the first session at which admitting is [`Verdict::Safe`]: free in every
//!   outcome, so eight is never exceeded.
//!
//! With eight as a hard limit, a patient is admitted at the guaranteed session. The gap between
//! the two is the most seat time the buffer can cost; with absences it costs less, because the
//! stays run longer. Re-running the check after every session, with the stays narrowed by what
//! happened, moves the guaranteed session forward. Turning the gap into an expected idle time
//! needs absence rates, which this crate does not model.
//!
//! Run with `cargo run --example therapy_group`.

use resource_timeline::envelope::{Anchor, Contingent, Envelope, Uncertain, Verdict};
use resource_timeline::{ActivityId, Error, Tick, Timeline, Window};

/// The sessions a programme takes.
const SESSIONS: i64 = 25;
/// The excused absence that ends the programme.
const EXCUSED_OUT: i64 = 6;
/// The unexcused absence that ends the programme.
const UNEXCUSED_OUT: i64 = 3;
/// The seats in the group, a hard limit.
const SEATS: i64 = 8;
/// The patients waiting for a seat.
const WAITLIST: u32 = 3;
/// The sessions ahead that are searched for an admission.
const HORIZON: Tick = 80;

/// One seat, held from the first session until the stay ends.
const SEAT: [Uncertain; 1] = [Uncertain::Claim {
    start: Anchor::Start(0),
    end: Anchor::End(0),
    amount: 1,
}];

/// A patient in the group today.
struct Patient {
    /// Sessions still to attend, absences not counted.
    remaining: i64,
    /// Excused absences so far.
    excused: i64,
    /// Unexcused absences so far.
    unexcused: i64,
}

/// The group today: eight patients at different points of their programme.
const GROUP: [Patient; 8] = [
    patient(2, 4, 0),
    patient(3, 0, 2),
    patient(6, 1, 0),
    patient(9, 5, 1),
    patient(13, 0, 0),
    patient(17, 2, 1),
    patient(21, 0, 0),
    patient(24, 0, 0),
];

/// A patient with `remaining` sessions to attend and the given absences so far.
const fn patient(remaining: i64, excused: i64, unexcused: i64) -> Patient {
    Patient {
        remaining,
        excused,
        unexcused,
    }
}

/// Finds the admission sessions for the waitlist and prints them.
fn main() -> Result<(), Error> {
    let seats = Timeline::claimable(0, SEATS, 64)?;
    let mut members = group()?;
    let mut admitted = Vec::new();

    for (k, on_time) in (1..=WAITLIST).zip(on_time()) {
        let id = ActivityId(100 + k);
        let Some(guaranteed) = guaranteed(&seats, &members, id)? else {
            println!("waitlist #{k}: no guaranteed seat within {HORIZON} sessions");
            break;
        };

        println!(
            "waitlist #{k}: a seat frees on time at session {on_time}, guaranteed at session \
             {guaranteed} (idle at most: {})",
            guaranteed - on_time,
        );
        members.push(admission(id, guaranteed)?);
        admitted.push(guaranteed);
    }

    assert_eq!(admitted, [3, 8, 9]);
    Ok(())
}

/// The sessions at which seats free if everyone in the group attends every session, earliest
/// first.
fn on_time() -> Vec<Tick> {
    let mut ends: Vec<Tick> = GROUP.iter().map(|p| p.remaining).collect();
    ends.sort_unstable();
    ends
}

/// The patients in the group today, each holding a seat from session 0 for an uncertain stay.
fn group() -> Result<Vec<Contingent<'static>>, Error> {
    GROUP
        .iter()
        .zip(1..)
        .map(|(p, id)| {
            Ok(Contingent {
                activity: ActivityId(id),
                window: Window::new(0, 0)?,
                duration: stay(p)?,
                impacts: &SEAT,
            })
        })
        .collect()
}

/// The shortest and longest remaining stay of `p`, in sessions.
///
/// The shortest ends at the earlier of the last session and the absence that ends the
/// programme; the longest makes up every excused absence still allowed.
fn stay(p: &Patient) -> Result<Window, Error> {
    let dropout = (UNEXCUSED_OUT - p.unexcused).min(EXCUSED_OUT - p.excused);
    let shortest = p.remaining.min(dropout);
    let longest = p.remaining + (EXCUSED_OUT - 1 - p.excused);
    Window::new(shortest, longest)
}

/// A new patient `id` admitted at session `at`, with a full programme ahead.
fn admission(id: ActivityId, at: Tick) -> Result<Contingent<'static>, Error> {
    Ok(Contingent {
        activity: id,
        window: Window::new(at, at)?,
        duration: stay(&patient(SESSIONS, 0, 0))?,
        impacts: &SEAT,
    })
}

/// The first session at which admitting patient `id` keeps every outcome within the seats.
fn guaranteed(
    seats: &Timeline,
    members: &[Contingent<'_>],
    id: ActivityId,
) -> Result<Option<Tick>, Error> {
    let mut candidate = members.to_vec();
    candidate.push(admission(id, 0)?);
    for at in 0..HORIZON {
        *candidate.last_mut().expect("the candidate was just pushed") = admission(id, at)?;
        let envelope = Envelope::new(seats, &[])?.with_contingent(&candidate)?;
        if envelope.check()? == Verdict::Safe {
            return Ok(Some(at));
        }
    }

    Ok(None)
}
