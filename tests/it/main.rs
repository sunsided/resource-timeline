//! Integration tests for `resource-timeline`.

mod codec;
mod edge_cases;
mod envelope;
mod envelope_contingent;
mod envelope_precedence;
mod envelope_state;
mod errors;
mod extract;
mod fixed;
mod greedy;
mod oracle;
mod plan;
mod properties;
#[cfg(feature = "serde")]
mod serde_round_trip;
mod share;
mod state;
