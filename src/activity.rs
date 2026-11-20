//! Identification of the activities that cause impacts.

/// An opaque identifier of an activity, chosen by the caller.
///
/// The crate never interprets the value. It only tags impacts with it and reports it back in a
/// [`Violation`](crate::Violation). Integration code maps it to its own activity records, for
/// example to the start and end time points of a `temporal-control` network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ActivityId(pub u32);
