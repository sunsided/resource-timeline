//! # Resource timeline
//!
//! Resource timelines for onboard scheduling: energy, memory, thermal and communication budgets.
//!
//! ## Planned scope
//!
//! - Resource levels over time: energy, memory, thermal, communication windows
//! - Checks of proposed activities against resource limits
//! - Companion to temporal-control
//!
//! ## Status
//!
//! Pre-release. Version 0.0.0 reserves the name; the API is not started yet and will change.
//!
//! Repository: <https://github.com/sunsided/resource-timeline>

#![no_std]

/// A resource whose level is tracked over time.
pub trait Resource {
    /// The level of the resource, for example stored energy.
    type Level: Ord;

    /// The largest level the resource can hold.
    fn capacity(&self) -> Self::Level;
}
