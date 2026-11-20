//! Profile extraction against the tick-by-tick oracles.

use proptest::prelude::*;
use resource_timeline::storage::Fixed;
use resource_timeline::{
    Bounds, Breakpoint, Error, Impact, StateTimeline, StateValue, Tick, Timeline,
};

use crate::properties::{depletable, owned_impact};
use crate::{oracle, state};

/// The value of `timeline` at `t` from the definition: the latest sets, `None` if they disagree.
fn value_by_definition(timeline: &StateTimeline, t: Tick) -> Option<StateValue> {
    let latest = state::latest_sets(timeline, t);
    match latest.first() {
        None => Some(timeline.initial()),
        Some(&(_, value, _)) if latest.iter().all(|&(_, other, _)| other == value) => Some(value),
        Some(_) => None,
    }
}

/// The last tick of interest of `timeline`: after it the level no longer changes.
fn horizon(timeline: &Timeline) -> Tick {
    timeline.start() + 30
}

proptest! {
    /// The exported profile and the breakpoints reproduce the level at every tick, and every
    /// breakpoint after the first is a real change of the line.
    #[test]
    fn export_matches_oracle(timeline in depletable()) {
        let profile = timeline.export().expect("small");
        let points: Vec<Breakpoint> = timeline.breakpoints().collect::<Result<_, _>>().expect("small");
        prop_assert_eq!(profile.breakpoints().collect::<Vec<_>>(), points.clone());
        prop_assert_eq!(points[0].at, timeline.start());
        for pair in points.windows(2) {
            let predicted = pair[0].level + pair[0].slope * (pair[1].at - pair[0].at);
            prop_assert!(pair[0].at < pair[1].at);
            prop_assert!(predicted != pair[1].level || pair[0].slope != pair[1].slope, "redundant {:?}", pair);
        }

        prop_assert_eq!(points.last().map(|point| point.slope), Some(0));
        for t in timeline.start()..=horizon(&timeline) {
            prop_assert_eq!(i128::from(profile.level_at(t).expect("small")), oracle::level_at(&timeline, t));
        }
    }

    /// Samples agree with the oracle on any grid.
    #[test]
    fn samples_match_oracle(timeline in depletable(), offset in 0_i64..10, step in 1_i64..7, count in 0_usize..12) {
        let from = timeline.start() + offset;
        let samples: Vec<(Tick, i64)> = timeline.samples(from, step, count).expect("valid").collect::<Result<_, _>>().expect("small");
        prop_assert_eq!(samples.len(), count);
        for (k, (at, level)) in (0_i64..).zip(samples) {
            prop_assert_eq!(at, from + k * step);
            prop_assert_eq!(i128::from(level), oracle::level_at(&timeline, at));
        }
    }

    /// A fixed timeline exports the same profile as a heap timeline, with room to spare.
    #[test]
    fn fixed_export_matches_heap(impacts in prop::collection::vec(owned_impact(0, false), 0..8)) {
        let bounds = Bounds::new(-100, 100).expect("valid");
        let mut heap = Timeline::depletable(0, 5, bounds, 8).expect("valid");
        let mut fixed = Timeline::<Fixed<8>>::depletable_in(0, 5, bounds, 8).expect("valid");
        for &(activity, impact) in &impacts {
            heap.add(activity, impact).expect("valid");
            fixed.add(activity, impact).expect("valid");
        }

        let heap = heap.export().expect("small");
        let fixed = fixed.export().expect("room");
        prop_assert!(heap.breakpoints().eq(fixed.breakpoints()));
    }

    /// State value changes reproduce the value at every tick and list real changes only.
    #[test]
    fn state_values_match_definition(timeline in state::timeline()) {
        let changes: Vec<_> = timeline.values().collect();
        prop_assert_eq!(changes[0].0, timeline.start());
        for pair in changes.windows(2) {
            prop_assert!(pair[0].0 < pair[1].0 && pair[0].1 != pair[1].1);
        }

        for t in timeline.start()..timeline.start() + 20 {
            let latest = changes.iter().rev().find(|&&(at, _)| at <= t).expect("start");
            prop_assert_eq!(latest.1, value_by_definition(&timeline, t));
        }
    }
}

/// Extraction errors: sampling before the start or with a non-positive step.
#[test]
fn sampling_errors() {
    let bounds = Bounds::new(0, 10).expect("valid");
    let mut timeline = Timeline::depletable(5, 3, bounds, 4).expect("valid");
    timeline
        .add(
            resource_timeline::ActivityId(1),
            Impact::Step { at: 6, delta: 1 },
        )
        .expect("valid");
    assert!(matches!(
        timeline.samples(4, 1, 1),
        Err(Error::BeforeStart { at: 4, start: 5 })
    ));
    assert!(matches!(
        timeline.samples(5, 0, 1),
        Err(Error::InvalidStep { step: 0 })
    ));
    let profile = timeline.export().expect("small");
    assert_eq!(
        profile.level_at(4),
        Err(Error::BeforeStart { at: 4, start: 5 })
    );
    assert_eq!((profile.start(), profile.len()), (5, 2));
}
