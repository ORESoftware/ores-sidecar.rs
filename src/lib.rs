#![forbid(unsafe_code)]

//! Shared, fail-closed configuration and runtime reconciliation primitives for ORESoftware sidecars.
//!
//! The snapshot state machine is retained byte-for-byte in [`core`]. Ordered provider-event
//! reconciliation is additive and re-exported from [`runtime_events`].

#[path = "core.rs"]
mod core;
pub use core::*;

pub mod runtime_events;
pub use runtime_events::*;
