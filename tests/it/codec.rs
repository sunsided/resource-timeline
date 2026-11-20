//! The byte encoding: round trips, corruption and truncation.

use proptest::prelude::*;
use resource_timeline::storage::Fixed;
use resource_timeline::{Error, LevelProfile, StateTimeline, Timeline};

use crate::properties::depletable;
use crate::state;

proptest! {
    /// Timelines, state timelines and profiles survive a round trip, into heap and fixed
    /// storage alike, and the encoded length is exact.
    #[test]
    fn round_trips(timeline in depletable(), states in state::timeline()) {
        let mut buffer = vec![0_u8; timeline.encoded_len()];
        prop_assert_eq!(timeline.encode(&mut buffer), Ok(buffer.len()));
        let heap = Timeline::decode(&buffer).expect("valid");
        prop_assert_eq!(&heap, &timeline);
        let fixed = Timeline::<Fixed<64>>::decode_in(&buffer).expect("room");
        prop_assert_eq!(fixed.impacts(), timeline.impacts());
        prop_assert_eq!(fixed.first_violation().expect("small").map(|v| v.at), timeline.first_violation().expect("small").map(|v| v.at));

        let profile = timeline.export().expect("small");
        let mut bytes = vec![0_u8; profile.encoded_len()];
        prop_assert_eq!(profile.encode(&mut bytes), Ok(bytes.len()));
        prop_assert_eq!(LevelProfile::decode(&bytes).expect("valid"), profile);

        let mut bytes = vec![0_u8; states.encoded_len()];
        prop_assert_eq!(states.encode(&mut bytes), Ok(bytes.len()));
        prop_assert_eq!(StateTimeline::decode(&bytes).expect("valid"), states);
    }

    /// Changing any one byte or cutting the encoding is always detected.
    #[test]
    fn corruption_is_detected(timeline in depletable(), position in any::<prop::sample::Index>(), flip in 1_u8..=255, cut in any::<prop::sample::Index>()) {
        let mut buffer = vec![0_u8; timeline.encoded_len()];
        timeline.encode(&mut buffer).expect("room");
        let mut corrupted = buffer.clone();
        corrupted[position.index(buffer.len())] ^= flip;
        prop_assert!(Timeline::decode(&corrupted).is_err());
        prop_assert!(Timeline::decode(&buffer[..cut.index(buffer.len())]).is_err());
    }

    /// Arbitrary bytes never decode into anything and never panic.
    #[test]
    fn garbage_is_rejected(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        prop_assert!(Timeline::decode(&bytes).is_err());
        prop_assert!(StateTimeline::decode(&bytes).is_err());
        prop_assert!(LevelProfile::decode(&bytes).is_err());
    }
}

/// Short buffers, foreign magic, versions and capacity are typed errors.
#[test]
fn encoding_errors() {
    let timeline = Timeline::claimable(0, 2, 4).expect("valid");
    let mut short = [0_u8; 8];
    assert_eq!(
        timeline.encode(&mut short),
        Err(Error::BufferTooSmall {
            needed: timeline.encoded_len()
        })
    );

    let mut bytes = vec![0_u8; timeline.encoded_len()];
    timeline.encode(&mut bytes).expect("room");
    assert_eq!(StateTimeline::decode(&bytes), Err(Error::Malformed));
    bytes[4] = 9;
    assert_eq!(
        Timeline::decode(&bytes),
        Err(Error::UnsupportedVersion { version: 9 })
    );

    let mut full = Timeline::depletable(
        0,
        1,
        resource_timeline::Bounds::new(0, 9).expect("valid"),
        4,
    )
    .expect("valid");
    for at in 0..3 {
        full.add(
            resource_timeline::ActivityId(1),
            resource_timeline::Impact::Step { at, delta: 1 },
        )
        .expect("valid");
    }

    let mut bytes = vec![0_u8; full.encoded_len()];
    full.encode(&mut bytes).expect("room");
    assert!(matches!(
        Timeline::<Fixed<2>>::decode_in(&bytes),
        Err(Error::CapacityExceeded { .. })
    ));
}
