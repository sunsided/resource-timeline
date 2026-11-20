//! Start windows of activities whose start time is not fixed yet.

use crate::{Error, Tick};

/// The inclusive range `[earliest, latest]` of allowed start ticks of an activity.
///
/// This is the shape of a time point's window in a simple temporal network: in
/// `temporal-control`, `MinimalNetwork::lower(origin, tp)` and `upper(origin, tp)`, or
/// `Dispatcher::window(tp)` during execution. Unbounded sides there must be clamped to a
/// mission horizon before they become a `Window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Window {
    /// The earliest allowed start. Never greater than `latest`.
    earliest: Tick,
    /// The latest allowed start.
    latest: Tick,
}

impl Window {
    /// Creates the window `[earliest, latest]`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidWindow`] if `earliest > latest`.
    pub const fn new(earliest: Tick, latest: Tick) -> Result<Self, Error> {
        if earliest > latest {
            return Err(Error::InvalidWindow { earliest, latest });
        }

        Ok(Self { earliest, latest })
    }

    /// The window that allows only `start`, for an activity with a fixed start.
    #[must_use]
    pub const fn at(start: Tick) -> Self {
        Self {
            earliest: start,
            latest: start,
        }
    }

    /// The earliest allowed start.
    #[must_use]
    pub const fn earliest(&self) -> Tick {
        self.earliest
    }

    /// The latest allowed start.
    #[must_use]
    pub const fn latest(&self) -> Tick {
        self.latest
    }

    /// Whether `t` is an allowed start.
    #[must_use]
    pub const fn contains(&self, t: Tick) -> bool {
        self.earliest <= t && t <= self.latest
    }
}
