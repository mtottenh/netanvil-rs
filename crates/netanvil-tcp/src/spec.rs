//! TCP protocol specification types.
//!
//! Re-exported from `netanvil_types::tcp_spec` so that plugin crates can
//! implement conversion traits without depending on compio.

pub use netanvil_types::tcp_spec::{TcpFraming, TcpRequestSpec, TcpTestMode};
