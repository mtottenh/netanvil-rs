//! Distributed load testing coordination for netanvil-rs.
//!
//! Provides the `DistributedCoordinator` which orchestrates tests across
//! multiple agent nodes, plus HTTP-based implementations of the discovery,
//! metrics, and command traits.

pub mod coordinator;
pub mod http_cluster;
pub mod leader_api;
pub mod leader_server;
pub mod result_store;
pub mod signal;
pub mod test_queue;
pub mod test_spec;

pub use coordinator::{DistributedCoordinator, DistributedProgressUpdate, DistributedTestResult};
pub use http_cluster::{
    HttpMetricsFetcher, HttpNodeCommander, MtlsMetricsFetcher, MtlsNodeCommander,
    MtlsStaticDiscovery, StaticDiscovery,
};
pub use leader_api::LeaderApiState;
pub use leader_server::{LeaderMetricsState, LeaderServer};
pub use result_store::ResultStore;
pub use signal::HttpSignalPoller;
pub use test_queue::{QueueConfig, TestQueue};
pub use test_spec::{TestId, TestInfo, TestSpec, TestStatus};
