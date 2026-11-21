//! serde deserialization: arbitrary JSON into every type with a validating `Deserialize`.
//!
//! [`Bounds`] and [`Window`] deserialize through their checked constructors, timelines and
//! profiles by replaying their content through the normal constructors. Checked: no panic;
//! accepted values obey their invariants and survive a JSON round trip; the fixed-capacity
//! timeline accepts what the heap timeline accepts, unless it runs out of room.

#![no_main]

use libfuzzer_sys::fuzz_target;
use resource_timeline::storage::Fixed;
use resource_timeline::{Bounds, LevelProfile, StateTimeline, Timeline, Window};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Capacity of the fixed-storage timeline.
const N: usize = 16;

fuzz_target!(|data: &[u8]| {
    let Ok(json) = core::str::from_utf8(data) else {
        return;
    };

    if let Some(bounds) = round_trip::<Bounds>(json) {
        assert!(bounds.min() <= bounds.max(), "inverted {bounds:?}");
    }

    if let Some(window) = round_trip::<Window>(json) {
        assert!(window.earliest() <= window.latest(), "inverted {window:?}");
    }

    if let Some(timeline) = round_trip::<Timeline>(json) {
        let _ = timeline.first_violation();
        if let Ok(fixed) = serde_json::from_str::<Timeline<Fixed<N>>>(json) {
            assert_eq!(fixed.impacts(), timeline.impacts(), "heap and fixed differ");
            assert_eq!(fixed.targets(), timeline.targets(), "heap and fixed differ");
        } else {
            assert!(timeline.impacts().len() > N || timeline.targets().len() > N);
        }
    }

    if let Some(states) = round_trip::<StateTimeline>(json) {
        let _ = states.first_violation();
    }

    round_trip::<LevelProfile>(json);
});

/// Deserializes `json` as `T` and, if that succeeds, checks that serializing and
/// deserializing again gives the same value.
fn round_trip<T>(json: &str) -> Option<T>
where
    T: Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    let value: T = serde_json::from_str(json).ok()?;
    let again = serde_json::to_string(&value).expect("serializable");
    assert_eq!(
        serde_json::from_str::<T>(&again).ok(),
        Some(value),
        "round trip"
    );
    serde_json::from_str(json).ok()
}
