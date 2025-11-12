//! Response capture executor decorator.
//!
//! Wraps any `RequestExecutor` and writes response headers and bodies to a
//! log file. Equivalent to the legacy netanvil `-capture <logfile>` flag.

use std::cell::RefCell;
use std::io::{BufWriter, Write};

use netanvil_types::request::{ExecutionResult, RequestContext};
use netanvil_types::traits::RequestExecutor;

/// Executor decorator that captures response data to a file.
pub struct CapturingExecutor<E> {
    inner: E,
    file: RefCell<BufWriter<std::fs::File>>,
}

impl<E> CapturingExecutor<E> {
    pub fn new(inner: E, path: &str) -> std::io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            inner,
            file: RefCell::new(BufWriter::new(file)),
        })
    }
}

impl<E: RequestExecutor> RequestExecutor for CapturingExecutor<E> {
    type Spec = E::Spec;
    type PacketSource = E::PacketSource;

    async fn execute(&self, spec: &Self::Spec, ctx: &RequestContext) -> ExecutionResult {
        let result = self.inner.execute(spec, ctx).await;
        if let Some(ref body) = result.response_body {
            let mut f = self.file.borrow_mut();
            // Write status line
            if let Some(status) = result.status {
                let _ = writeln!(
                    f,
                    "--- request_id={} status={} size={} ---",
                    result.request_id, status, result.response_size
                );
            }
            if let Some(ref headers) = result.response_headers {
                for (k, v) in headers {
                    let _ = writeln!(f, "{}: {}", k, v);
                }
                let _ = writeln!(f);
            }
            let _ = f.write_all(body);
            let _ = writeln!(f);
            let _ = f.flush();
        }
        result
    }
}
