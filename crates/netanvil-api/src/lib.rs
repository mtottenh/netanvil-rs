//! HTTP control API for netanvil-rs load testing.
//!
//! Provides a lightweight HTTP server that runs alongside the coordinator,
//! enabling mid-test control (rate changes, target updates, stop) and
//! live metrics queries.

pub mod handlers;
pub mod server;
pub mod types;

pub use server::ControlServer;
pub use types::SharedState;
