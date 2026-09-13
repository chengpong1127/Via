//! Provider-neutral Via concepts.
//!
//! The legacy top-level modules remain as compatibility facades for clients of
//! the crate. New routing and provider code should use this namespace.

pub mod ids;
pub mod journey;
pub mod realtime;
pub mod transit;
