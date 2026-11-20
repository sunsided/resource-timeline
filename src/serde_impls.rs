//! `serde` support (feature `serde`) for types whose invariants the derive would skip.
//!
//! [`Bounds`] and [`Window`] are deserialized through their validating constructors. Timelines
//! and profiles serialize their content (not their internal indexes) and deserialize by
//! replaying it through the normal constructors, so a deserialized value obeys every invariant
//! and capacity of its storage. Deserializing them collects the lists first and therefore also
//! needs the `alloc` feature.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::storage::Storage;
use crate::{Bounds, LevelProfile, StateTimeline, Timeline, Window};

/// The fields of [`Bounds`] and [`Window`] as plain data.
#[derive(Serialize, Deserialize)]
struct Pair {
    /// The lower end.
    min: i64,
    /// The upper end.
    max: i64,
}

impl Serialize for Bounds {
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        Pair {
            min: self.min(),
            max: self.max(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Bounds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pair = Pair::deserialize(deserializer)?;
        Self::new(pair.min, pair.max).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Window {
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        Pair {
            min: self.earliest(),
            max: self.latest(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Window {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pair = Pair::deserialize(deserializer)?;
        Self::new(pair.min, pair.max).map_err(serde::de::Error::custom)
    }
}

impl<S: Storage> Serialize for Timeline<S> {
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        let mut state = serializer.serialize_struct("Timeline", 7)?;
        state.serialize_field("kind", &self.kind())?;
        state.serialize_field("start", &self.start())?;
        state.serialize_field("initial", &self.initial())?;
        state.serialize_field("bounds", &self.bounds())?;
        state.serialize_field("budget", &self.budget())?;
        state.serialize_field("impacts", self.impacts())?;
        state.serialize_field("targets", self.targets())?;
        state.end()
    }
}

impl<S: Storage> Serialize for StateTimeline<S> {
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        let mut state = serializer.serialize_struct("StateTimeline", 4)?;
        state.serialize_field("start", &self.start())?;
        state.serialize_field("initial", &self.initial())?;
        state.serialize_field("budget", &self.budget())?;
        state.serialize_field("impacts", self.impacts())?;
        state.end()
    }
}

impl<S: Storage> Serialize for LevelProfile<S> {
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        serializer.collect_seq(self.breakpoints())
    }
}

#[cfg(feature = "alloc")]
mod de {
    //! Deserializing timelines and profiles by replaying their content.

    use alloc::vec::Vec;

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer};

    use crate::storage::{Seq, Storage};
    use crate::{
        ActivityId, Bounds, Breakpoint, Container, Impact, Kind, LevelProfile, StateImpact,
        StateTimeline, Target, Timeline,
    };

    /// The serialized content of a [`Timeline`].
    #[derive(Deserialize)]
    struct TimelineRepr {
        /// Which impacts it accepts.
        kind: Kind,
        /// The start tick.
        start: i64,
        /// The initial level.
        initial: i64,
        /// The bounds.
        bounds: Bounds,
        /// The impact budget.
        budget: usize,
        /// The impacts in insertion order.
        impacts: Vec<(ActivityId, Impact)>,
        /// The targets.
        targets: Vec<Target>,
    }

    impl<'de, S: Storage> Deserialize<'de> for Timeline<S> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let repr = TimelineRepr::deserialize(deserializer)?;
            let mut timeline = match repr.kind {
                Kind::Depletable => {
                    Self::depletable_in(repr.start, repr.initial, repr.bounds, repr.budget)
                }
                Kind::Claimable if repr.bounds.min() == 0 && repr.initial == repr.bounds.max() => {
                    Self::claimable_in(repr.start, repr.initial, repr.budget)
                }
                Kind::Claimable => return Err(D::Error::custom("inconsistent claimable timeline")),
            }
            .map_err(D::Error::custom)?;
            for (activity, impact) in repr.impacts {
                timeline.add(activity, impact).map_err(D::Error::custom)?;
            }

            for target in repr.targets {
                timeline.add_target(target).map_err(D::Error::custom)?;
            }

            Ok(timeline)
        }
    }

    /// The serialized content of a [`StateTimeline`].
    #[derive(Deserialize)]
    struct StateRepr {
        /// The start tick.
        start: i64,
        /// The initial value.
        initial: u32,
        /// The impact budget.
        budget: usize,
        /// The impacts in insertion order.
        impacts: Vec<(ActivityId, StateImpact)>,
    }

    impl<'de, S: Storage> Deserialize<'de> for StateTimeline<S> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let repr = StateRepr::deserialize(deserializer)?;
            let mut timeline = Self::new_in(repr.start, repr.initial, repr.budget);
            for (activity, impact) in repr.impacts {
                timeline.add(activity, impact).map_err(D::Error::custom)?;
            }

            Ok(timeline)
        }
    }

    impl<'de, S: Storage> Deserialize<'de> for LevelProfile<S> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let points = Vec::<Breakpoint>::deserialize(deserializer)?;
            let (origin, rest) = points
                .split_first()
                .ok_or_else(|| D::Error::custom("a profile has at least one breakpoint"))?;
            let mut changes = <S::DoubleList<Breakpoint> as Seq<_>>::EMPTY;
            let mut previous = origin.at;
            for &point in rest {
                if point.at <= previous {
                    return Err(D::Error::custom("breakpoints must increase in tick"));
                }

                previous = point.at;
                changes
                    .try_push(point, Container::ChangePoints)
                    .map_err(D::Error::custom)?;
            }

            Ok(Self::from_parts(*origin, changes))
        }
    }
}
