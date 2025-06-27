//! HTTP control API for netanvil-rs load testing.
//!
//! Provides a lightweight HTTP server that runs alongside the coordinator,
//! enabling mid-test control (rate changes, target updates, stop) and
//! live metrics queries.
//!
//! The `agent` module extends this into a long-lived remotely controllable
//! node for distributed load testing.

pub mod agent;
pub mod handlers;
pub mod server;
pub mod tls;
pub mod types;

pub use agent::AgentServer;
pub use server::ControlServer;
pub use tls::{build_client_config, mtls_request, MtlsServer};
pub use types::{MetricsView, SharedState};
