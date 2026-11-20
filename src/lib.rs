//! # Resource timeline
//!
//! Resource timelines for onboard scheduling: energy, memory, thermal and communication budgets.
//!
//! A [`Timeline`] tracks the level of one numeric resource over integer time ticks. Activities
//! add [`Impact`]s to it:
//!
//! - a consumable resource (battery energy, data volume) changes by permanent
//!   [`Impact::Step`]s and by linear [`Impact::Rate`]s (charging, discharging, thermal drift);
//! - a reusable resource (an instrument, a transmitter, a power channel) is borrowed by an
//!   [`Impact::Claim`] for a half-open interval and returned afterwards.
//!
//! [`Timeline::first_violation`] reports the first tick at which the level leaves its
//! `[min, max]` [`Bounds`] or misses an end-state [`Target`], with the activities responsible.
//! A [`StateTimeline`] does the same for discrete states such as instrument modes and comm
//! visibility windows. [`Timeline::breakpoints`], [`Timeline::samples`] and
//! [`Timeline::export`] extract the level line for display and telemetry.
//!
//! On top of single timelines:
//!
//! - a [`Plan`] holds one timeline per resource, places activities given as [`Demand`]s
//!   atomically and finds the [earliest start](Plan::earliest_start) at which an activity fits,
//!   in closed form per linear piece when rates are involved;
//! - [`greedy::schedule`] places activities in priority order at their earliest feasible start;
//! - [`envelope::Envelope`] bounds the level over every schedule that start [`Window`]s admit,
//!   including uncertain durations and precedences, so a flexible schedule can be checked once
//!   instead of per dispatch decision; [`envelope::StateEnvelope`] does the same for states.
//!
//! Every timeline and plan is generic over its [`storage`]: heap-allocated by default, or
//! fixed-capacity and allocation-free ([`FixedTimeline`], [`storage::FixedPlan`]) for onboard
//! use, with the same algorithms and typed [`Error::CapacityExceeded`] errors.
//!
//! This crate is the companion of `temporal-control` but does not depend on it: temporal
//! controllability does not imply resource safety, so both checks must run independently. Both
//! crates share the time representation only by convention: a [`Tick`] is a plain `i64`, and
//! start windows come from the STN (see [`envelope`] for the bridge).
//!
//! ```mermaid
//! graph LR
//!     demands["Demands of an activity"] --> plan["Plan: numeric and state timelines"]
//!     plan --> search["earliest_start, greedy::schedule"]
//!     plan --> check["first_violation with contributors"]
//!     windows["Start windows from an STN"] --> envelope["Envelope: Safe, Possible, Certain"]
//!     plan --> envelope
//! ```
//!
//! ## Example
//!
//! ```
//! use resource_timeline::{ActivityId, Bound, Bounds, Impact, Timeline};
//!
//! // Battery with 100 units, a reserve of 20 and a capacity of 120.
//! let mut battery = Timeline::depletable(0, 100, Bounds::new(20, 120)?, 16)?;
//! let warm_up = ActivityId(1);
//! let analyze = ActivityId(2);
//! battery.add(warm_up, Impact::Step { at: 5, delta: -40 })?;
//! battery.add(analyze, Impact::Step { at: 20, delta: -50 })?;
//!
//! let violation = battery.first_violation()?.expect("the reserve is broken");
//! assert_eq!(violation.at, 20);
//! assert_eq!(violation.level, 10);
//! assert_eq!(violation.bound, Bound::BelowMin(20));
//! assert_eq!(violation.contributors, [warm_up, analyze]);
//! # Ok::<(), resource_timeline::Error>(())
//! ```
//!
//! ## Features
//!
//! - `alloc` (default): heap storage ([`storage::Heap`], the default storage of every type),
//!   [`envelope`] and [`greedy`]. Without it the crate needs no allocator: timelines, state
//!   timelines and plans use [`storage::Fixed`] and [`storage::FixedPlan`].
//!
//! ## Status
//!
//! Pre-release. See `docs/design.md` for the milestones. The API will change.
//!
//! Repository: <https://github.com/sunsided/resource-timeline>
//!
#![cfg_attr(
    feature = "alloc",
    doc = "[`greedy::schedule`]: crate::greedy::schedule"
)]
#![cfg_attr(not(feature = "alloc"), doc = "[`greedy::schedule`]: crate#features")]
#![cfg_attr(
    feature = "alloc",
    doc = "[`envelope::Envelope`]: crate::envelope::Envelope"
)]
#![cfg_attr(not(feature = "alloc"), doc = "[`envelope::Envelope`]: crate#features")]
#![cfg_attr(
    feature = "alloc",
    doc = "[`envelope::StateEnvelope`]: crate::envelope::StateEnvelope"
)]
#![cfg_attr(
    not(feature = "alloc"),
    doc = "[`envelope::StateEnvelope`]: crate#features"
)]
#![cfg_attr(feature = "alloc", doc = "[`envelope`]: crate::envelope")]
#![cfg_attr(not(feature = "alloc"), doc = "[`envelope`]: crate#features")]
#![cfg_attr(feature = "alloc", doc = "[`greedy`]: crate::greedy")]
#![cfg_attr(not(feature = "alloc"), doc = "[`greedy`]: crate#features")]
#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod activity;
#[cfg(feature = "alloc")]
pub mod envelope;
mod error;
#[cfg(feature = "alloc")]
pub mod greedy;
mod impact;
mod plan;
mod state;
pub mod storage;
mod target;
mod timeline;
mod violation;
mod window;

pub use activity::ActivityId;
pub use error::{Container, Error};
pub use impact::Impact;
pub use plan::{Demand, Plan, PlanViolation, SearchScratch, StateId, TimelineId};
pub use state::{StateImpact, StateTimeline, StateValue, StateViolation};
pub use target::Target;
pub use timeline::extract::{Breakpoint, Breakpoints, LevelProfile, Samples};
pub use timeline::{Bounds, Kind, Timeline};
pub use violation::{Bound, Violation};
pub use window::Window;

/// A point in time, as a count of integer ticks.
///
/// The unit (seconds, milliseconds, spacecraft clock subticks) and the epoch are chosen by the
/// mission; the crate never converts. `temporal-control` uses the same convention in its
/// `Ticks(pub i64)` newtype, so integration code converts by wrapping or unwrapping `.0`.
pub type Tick = i64;

/// A resource level in a mission-chosen integer unit, for example mWh, KiB or instrument units.
pub type Level = i64;

/// A numeric timeline with room for `N` impacts that never allocates.
pub type FixedTimeline<const N: usize> = Timeline<storage::Fixed<N>>;

/// A state timeline with room for `N` impacts that never allocates.
pub type FixedStateTimeline<const N: usize> = StateTimeline<storage::Fixed<N>>;
