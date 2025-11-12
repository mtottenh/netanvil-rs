//! Connection-dropping executor decorator.
//!
//! Wraps any `RequestExecutor` and probabilistically aborts requests mid-transfer
//! by cancelling the execution future after a random delay. When cancelled,
//! the underlying TCP connection is dropped (closed), simulating clients that
//! disconnect during a transfer.
//!
//! Equivalent to the legacy netanvil `-drop <fraction>` flag.

use std::cell::Cell;
use std::time::Duration;

use netanvil_types::request::{ExecutionError, ExecutionResult, RequestContext, TimingBreakdown};
use netanvil_types::traits::RequestExecutor;

/// Executor decorator that drops a fraction of connections mid-transfer.
pub struct DroppingExecutor<E> {
    inner: E,
    /// Fraction of requests to drop (0.0 to 1.0).
    drop_fraction: f64,
    /// Simple PRNG state (xorshift32), per-core so no synchronization needed.
    rng_state: Cell<u32>,
}

impl<E> DroppingExecutor<E> {
    pub fn new(inner: E, drop_fraction: f64, seed: u32) -> Self {
        Self {
            inner,
            drop_fraction,
            rng_state: Cell::new(seed.max(1)), // xorshift needs non-zero seed
        }
    }

    fn next_random_f64(&self) -> f64 {
        // xorshift32 PRNG — fast, no allocation, deterministic per-core
        let mut s = self.rng_state.get();
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        self.rng_state.set(s);
        (s as f64) / (u32::MAX as f64)
    }

    fn should_drop(&self) -> bool {
        self.next_random_f64() < self.drop_fraction
    }

    /// Random delay before dropping: uniform [10ms, 500ms].
    /// This approximates the legacy behavior of dropping after a random byte
    /// offset, since we don't have access to the byte stream.
    fn random_drop_delay(&self) -> Duration {
        let frac = self.next_random_f64();
        Duration::from_millis(10 + (frac * 490.0) as u64)
    }
}

impl<E: RequestExecutor> RequestExecutor for DroppingExecutor<E> {
    type Spec = E::Spec;
    type PacketSource = E::PacketSource;

    async fn execute(&self, spec: &Self::Spec, context: &RequestContext) -> ExecutionResult {
        if self.should_drop() {
            let delay = self.random_drop_delay();
            // Race the inner execution against the drop timeout.
            // If the timeout fires first, the inner future is dropped,
            // which closes the TCP connection mid-transfer.
            compio::time::timeout(delay, self.inner.execute(spec, context))
                .await
                .unwrap_or_else(|_| {
                    // Timeout → connection dropped
                    ExecutionResult {
                        request_id: context.request_id,
                        intended_time: context.intended_time,
                        actual_time: context.actual_time,
                        timing: TimingBreakdown::default(),
                        status: None,
                        bytes_sent: 0,
                        response_size: 0,
                        error: Some(ExecutionError::Other("connection dropped".to_string())),
                        response_headers: None,
                        response_body: None,
                    }
                })
        } else {
            self.inner.execute(spec, context).await
        }
    }
}
