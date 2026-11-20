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
//! `[min, max]` [`Bounds`], with the activities responsible. A [`StateTimeline`] does the same
//! for discrete states such as instrument modes and comm visibility windows.
//!
//! On top of single timelines:
//!
//! - a [`Plan`] holds one timeline per resource, places activities given as [`Demand`]s
//!   atomically and finds the [earliest start](Plan::earliest_start) at which an activity fits;
//! - [`greedy::schedule`] places activities in priority order at their earliest feasible start;
//! - [`envelope::Envelope`] bounds the level over every schedule that start [`Window`]s admit,
//!   so a flexible schedule can be checked once instead of per dispatch decision.
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
//! ## Status
//!
//! Pre-release. Milestones 1 to 5 of `docs/design.md` are implemented, plus incremental updates
//! from milestone 6; the fixed-capacity storage is not. The API will change.
//!
//! Repository: <https://github.com/sunsided/resource-timeline>

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod activity;
pub mod envelope;
mod error;
pub mod greedy;
mod impact;
mod plan;
mod state;
mod timeline;
mod violation;
mod window;

pub use activity::ActivityId;
pub use error::Error;
pub use impact::Impact;
pub use plan::{Demand, Plan, PlanViolation, StateId, TimelineId};
pub use state::{StateImpact, StateTimeline, StateValue, StateViolation};
pub use timeline::{Bounds, Kind, Timeline};
pub use violation::{Bound, Violation};
pub use window::Window;

/// A point in time, as a count of integer ticks.
///
/// The unit (seconds, milliseconds, spacecraft clock subticks) and the epoch are chosen by the
/// mission; the crate never converts. `temporal-control` uses the same convention, so values
/// pass between the crates unchanged.
pub type Tick = i64;

/// A resource level in a mission-chosen integer unit, for example mWh, KiB or instrument units.
pub type Level = i64;
