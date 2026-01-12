//! HTTP executor for load testing using compio and cyper.

mod connector;
mod executor;
pub mod observed;
pub mod throttle;
mod tls;

pub use connector::{Identity, Observe, WrapTcp};
pub use executor::HttpExecutor;
pub use observed::{ObserveConfig, SendObserveConfig};
pub use tls::build_tls_connector;

/// Check whether the kernel supports linked `GetSockOpt` via io_uring
/// (`IORING_OP_URING_CMD`, requires kernel >= 6.0).
///
/// Call this at startup before enabling `--health-sample-rate`.
/// Returns `false` on unsupported kernels or non-Linux platforms.
pub fn is_health_sampling_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        compio_driver::is_uring_cmd_supported()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}
