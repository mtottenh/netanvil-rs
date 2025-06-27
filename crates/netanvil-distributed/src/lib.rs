//! Distributed load testing coordination for netanvil-rs.
//!
//! Provides the `DistributedCoordinator` which orchestrates tests across
//! multiple agent nodes, plus HTTP-based implementations of the discovery,
//! metrics, and command traits.

pub mod coordinator;
pub mod http_cluster;
pub mod signal;

pub use coordinator::{DistributedCoordinator, DistributedProgressUpdate, DistributedTestResult};
pub use http_cluster::{
    HttpMetricsFetcher, HttpNodeCommander, MtlsMetricsFetcher, MtlsNodeCommander,
    MtlsStaticDiscovery, StaticDiscovery,
};
pub use signal::HttpSignalPoller;
