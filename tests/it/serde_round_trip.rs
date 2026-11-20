//! `serde` round trips through JSON (feature `serde`).

use proptest::prelude::*;
use resource_timeline::storage::Fixed;
use resource_timeline::{Bounds, LevelProfile, StateTimeline, Timeline, Window};

use crate::properties::depletable;
use crate::state;

proptest! {
    /// Timelines, state timelines and profiles survive JSON, into heap and fixed storage.
    #[test]
    fn json_round_trips(timeline in depletable(), states in state::timeline()) {
        let json = serde_json::to_string(&timeline).expect("serializable");
        prop_assert_eq!(&serde_json::from_str::<Timeline>(&json).expect("valid"), &timeline);
        let fixed: Timeline<Fixed<64>> = serde_json::from_str(&json).expect("room");
        prop_assert_eq!(fixed.impacts(), timeline.impacts());

        let profile = timeline.export().expect("small");
        let json = serde_json::to_string(&profile).expect("serializable");
        prop_assert_eq!(serde_json::from_str::<LevelProfile>(&json).expect("valid"), profile);

        let json = serde_json::to_string(&states).expect("serializable");
        prop_assert_eq!(serde_json::from_str::<StateTimeline>(&json).expect("valid"), states);
    }
}

/// Deserializing goes through the validating constructors.
#[test]
fn invalid_content_is_rejected() {
    assert!(serde_json::from_str::<Bounds>(r#"{"min":3,"max":1}"#).is_err());
    assert!(serde_json::from_str::<Window>(r#"{"min":3,"max":1}"#).is_err());
    assert_eq!(
        serde_json::from_str::<Window>(r#"{"min":1,"max":3}"#).ok(),
        Window::new(1, 3).ok()
    );
    let backwards = r#"[{"at":5,"level":1,"slope":0},{"at":2,"level":1,"slope":0}]"#;
    assert!(serde_json::from_str::<LevelProfile>(backwards).is_err());
    let full = r#"{"kind":"Claimable","start":0,"initial":1,"bounds":{"min":0,"max":1},"budget":1,
        "impacts":[[1,{"Claim":{"start":0,"end":2,"amount":1}}],[2,{"Claim":{"start":3,"end":4,"amount":1}}]],
        "targets":[]}"#;
    assert!(serde_json::from_str::<Timeline>(full).is_err());
}
