//! Property tests: the sweep agrees with the tick-by-tick oracle.

use proptest::prelude::*;
use resource_timeline::{ActivityId, Bounds, Impact, Timeline};

use crate::oracle;

/// Impact budget for generated timelines; larger than any generated impact list.
const BUDGET: usize = 64;

/// A generated impact owned by one of a few activities, so that activities often share impacts.
fn owned_impact(start: i64, claims_only: bool) -> impl Strategy<Value = (ActivityId, Impact)> {
    let step = (start..start + 20, -6_i64..=6).prop_map(|(at, delta)| Impact::Step { at, delta });
    let claim = (start..start + 20, 1_i64..=6, 1_i64..=4).prop_map(|(from, length, amount)| {
        Impact::Claim {
            start: from,
            end: from + length,
            amount,
        }
    });
    let impact = if claims_only {
        claim.boxed()
    } else {
        prop_oneof![step, claim].boxed()
    };

    (0_u32..4, impact).prop_map(|(id, impact)| (ActivityId(id), impact))
}

/// A depletable timeline with small bounds and up to a dozen impacts.
fn depletable() -> impl Strategy<Value = Timeline> {
    (-3_i64..3, -2_i64..=12, -3_i64..=3, 0_i64..=12).prop_flat_map(|(start, initial, min, span)| {
        prop::collection::vec(owned_impact(start, false), 0..12).prop_map(move |impacts| {
            let bounds = Bounds::new(min, min + span).expect("min <= min + span");
            let timeline = Timeline::depletable(start, initial, bounds, BUDGET).expect("valid");
            with_impacts(timeline, &impacts)
        })
    })
}

/// A claimable timeline with a small capacity and up to a dozen claims.
fn claimable() -> impl Strategy<Value = Timeline> {
    (-3_i64..3, 0_i64..=5).prop_flat_map(|(start, capacity)| {
        prop::collection::vec(owned_impact(start, true), 0..12).prop_map(move |impacts| {
            let timeline = Timeline::claimable(start, capacity, BUDGET).expect("valid");
            with_impacts(timeline, &impacts)
        })
    })
}

/// `timeline` with all `impacts` added in order.
fn with_impacts(mut timeline: Timeline, impacts: &[(ActivityId, Impact)]) -> Timeline {
    for &(activity, impact) in impacts {
        timeline
            .add(activity, impact)
            .expect("generated impacts are valid");
    }

    timeline
}

proptest! {
    /// Depletable timelines report the same first violation as the oracle.
    #[test]
    fn depletable_matches_oracle(timeline in depletable()) {
        prop_assert_eq!(timeline.first_violation().expect("no overflow"), oracle::first_violation(&timeline));
    }

    /// Claimable timelines report the same first violation as the oracle.
    #[test]
    fn claimable_matches_oracle(timeline in claimable()) {
        prop_assert_eq!(timeline.first_violation().expect("no overflow"), oracle::first_violation(&timeline));
    }

    /// The result does not depend on the order in which impacts were added.
    #[test]
    fn insertion_order_is_irrelevant(
        (timeline, shuffled) in depletable().prop_flat_map(|timeline| {
            let impacts = timeline.impacts().to_vec();
            (Just(timeline), Just(impacts).prop_shuffle())
        })
    ) {
        let empty = Timeline::depletable(timeline.start(), timeline.initial(), timeline.bounds(), BUDGET)
            .expect("valid");
        let reordered = with_impacts(empty, &shuffled);
        prop_assert_eq!(timeline.first_violation(), reordered.first_violation());
    }
}

/// Guards against vacuous properties: the generators produce both outcomes often.
#[test]
fn generators_cover_both_outcomes() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let mut violated = 0;
    let total = 500;
    for _ in 0..total {
        let timeline = depletable().new_tree(&mut runner).expect("tree").current();
        if timeline.first_violation().expect("no overflow").is_some() {
            violated += 1;
        }
    }

    assert!(violated > total / 10, "too few violations: {violated}");
    assert!(
        violated < total * 9 / 10,
        "too few safe timelines: {violated}"
    );
}
