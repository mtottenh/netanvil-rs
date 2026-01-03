//! HTTP control API for netanvil-rs load testing.
//!
//! Provides axum-based HTTP servers for mid-test control (rate changes,
//! target updates, stop) and live metrics queries.
//!
//! The `agent` module extends this into a long-lived remotely controllable
//! node for distributed load testing.

pub mod agent;
pub mod handlers;
pub mod identity;
pub mod server;
pub mod tls;
pub mod types;

pub use agent::AgentServer;
pub use identity::{CertExtractingAcceptor, ClientIdentity, SanVerifierLayer};
pub use server::ControlServer;
pub use tls::{build_client_config, build_server_config};
pub use types::{MetricsView, SharedState};
