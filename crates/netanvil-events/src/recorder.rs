//! Arrow IPC event recorder.
//!
//! Writes per-request events as Arrow record batches to a streaming IPC file.
//! One instance per I/O worker core. Uses interior mutability (`RefCell`)
//! following the same pattern as `HdrMetricsCollector`.

use std::cell::{Cell, RefCell};
use std::io::BufWriter;
use std::sync::Arc;

use arrow_array::builder::{BooleanBuilder, StringBuilder, UInt16Builder, UInt64Builder};
use arrow_array::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::Schema;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use netanvil_types::{EventRecorder, ExecutionError, ExecutionResult};

use crate::anchor::TimeAnchor;
use crate::schema::{ConstantValue, EventSchema};

/// Column builders for the common (always-present) fields.
struct CommonBuilders {
    request_id: UInt64Builder,
    core_id: UInt16Builder,
    timestamp_us: UInt64Builder,
    status: UInt16Builder,
    protocol: StringBuilder,
}

/// Column builders for optional field groups.
struct OptionalBuilders {
    // Coordinated omission
    intended_us: Option<UInt64Builder>,
    scheduling_delay_us: Option<UInt64Builder>,
    // Timing
    dns_us: Option<UInt64Builder>,
    tcp_us: Option<UInt64Builder>,
    tls_us: Option<UInt64Builder>,
    ttfb_us: Option<UInt64Builder>,
    transfer_us: Option<UInt64Builder>,
    total_us: Option<UInt64Builder>,
    // Bytes
    bytes_sent: Option<UInt64Builder>,
    response_size: Option<UInt64Builder>,
    // Errors
    is_error: Option<BooleanBuilder>,
    error_kind: Option<StringBuilder>,
}

/// Per-core Arrow IPC event recorder.
///
/// Accumulates per-request records in typed Arrow builders and periodically
/// flushes them as record batches to an IPC stream file.
pub struct ArrowEventRecorder {
    writer: RefCell<StreamWriter<BufWriter<std::fs::File>>>,
    common: RefCell<CommonBuilders>,
    optional: RefCell<OptionalBuilders>,
    header_builders: RefCell<Vec<StringBuilder>>,
    constant_builders: RefCell<Vec<ConstantBuilder>>,
    schema: Arc<Schema>,
    anchor: TimeAnchor,
    protocol: String,
    error_status_threshold: u16,
    sample_rate: f64,
    sample_rng: RefCell<SmallRng>,
    batch_size: usize,
    rows_buffered: Cell<usize>,
    // Schema flags (cached from EventSchema)
    include_co: bool,
    include_timing: bool,
    include_bytes: bool,
    include_errors: bool,
    // Header extraction config
    header_names: Vec<String>,
    // Constant values
    constant_values: Vec<ConstantValue>,
}

enum ConstantBuilder {
    Str(StringBuilder),
    U64(UInt64Builder),
}

impl ArrowEventRecorder {
    /// Create a new recorder for the given core, writing to `{output_dir}/events_core_{core_id}.arrow`.
    pub fn new(
        core_id: usize,
        schema: &EventSchema,
        output_dir: &str,
        anchor: TimeAnchor,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(output_dir)?;
        let path = format!("{output_dir}/events_core_{core_id}.arrow");
        let file = std::fs::File::create(&path)?;
        let buf_writer = BufWriter::with_capacity(64 * 1024, file);
        let writer = StreamWriter::try_new(buf_writer, &schema.arrow_schema)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let bs = schema.batch_size;

        let common = CommonBuilders {
            request_id: UInt64Builder::with_capacity(bs),
            core_id: UInt16Builder::with_capacity(bs),
            timestamp_us: UInt64Builder::with_capacity(bs),
            status: UInt16Builder::with_capacity(bs),
            protocol: StringBuilder::with_capacity(bs, schema.protocol.len() * bs),
        };

        let optional = OptionalBuilders {
            intended_us: schema.include_co.then(|| UInt64Builder::with_capacity(bs)),
            scheduling_delay_us: schema.include_co.then(|| UInt64Builder::with_capacity(bs)),
            dns_us: schema
                .include_timing
                .then(|| UInt64Builder::with_capacity(bs)),
            tcp_us: schema
                .include_timing
                .then(|| UInt64Builder::with_capacity(bs)),
            tls_us: schema
                .include_timing
                .then(|| UInt64Builder::with_capacity(bs)),
            ttfb_us: schema
                .include_timing
                .then(|| UInt64Builder::with_capacity(bs)),
            transfer_us: schema
                .include_timing
                .then(|| UInt64Builder::with_capacity(bs)),
            total_us: schema
                .include_timing
                .then(|| UInt64Builder::with_capacity(bs)),
            bytes_sent: schema
                .include_bytes
                .then(|| UInt64Builder::with_capacity(bs)),
            response_size: schema
                .include_bytes
                .then(|| UInt64Builder::with_capacity(bs)),
            is_error: schema
                .include_errors
                .then(|| BooleanBuilder::with_capacity(bs)),
            error_kind: schema
                .include_errors
                .then(|| StringBuilder::with_capacity(bs, 8 * bs)),
        };

        let header_builders = schema
            .header_columns
            .iter()
            .map(|_| StringBuilder::with_capacity(bs, 16 * bs))
            .collect();

        let header_names = schema
            .header_columns
            .iter()
            .map(|h| h.header_name.clone())
            .collect();

        let constant_values: Vec<ConstantValue> = schema
            .constant_columns
            .iter()
            .map(|c| c.value.clone())
            .collect();

        let constant_builders = constant_values
            .iter()
            .map(|v| match v {
                ConstantValue::Str(_) => {
                    ConstantBuilder::Str(StringBuilder::with_capacity(bs, 32 * bs))
                }
                ConstantValue::U64(_) => ConstantBuilder::U64(UInt64Builder::with_capacity(bs)),
            })
            .collect();

        // Seed RNG from core_id for deterministic-per-core sampling
        let sample_rng = SmallRng::seed_from_u64(core_id as u64 ^ 0xDEADBEEF_CAFEBABE);

        Ok(Self {
            writer: RefCell::new(writer),
            common: RefCell::new(common),
            optional: RefCell::new(optional),
            header_builders: RefCell::new(header_builders),
            constant_builders: RefCell::new(constant_builders),
            schema: schema.arrow_schema.clone(),
            anchor,
            protocol: schema.protocol.clone(),
            error_status_threshold: schema.error_status_threshold,
            sample_rate: schema.sample_rate,
            sample_rng: RefCell::new(sample_rng),
            batch_size: schema.batch_size,
            rows_buffered: Cell::new(0),
            include_co: schema.include_co,
            include_timing: schema.include_timing,
            include_bytes: schema.include_bytes,
            include_errors: schema.include_errors,
            header_names,
            constant_values,
        })
    }

    fn flush_batch(&self) {
        if self.rows_buffered.get() == 0 {
            return;
        }

        let mut common = self.common.borrow_mut();
        let mut optional = self.optional.borrow_mut();
        let mut header_builders = self.header_builders.borrow_mut();
        let mut constant_builders = self.constant_builders.borrow_mut();

        let mut columns: Vec<Arc<dyn arrow_array::Array>> = Vec::new();

        // Common columns (must match schema field order)
        columns.push(Arc::new(common.request_id.finish()));
        columns.push(Arc::new(common.core_id.finish()));
        columns.push(Arc::new(common.timestamp_us.finish()));
        columns.push(Arc::new(common.status.finish()));
        columns.push(Arc::new(common.protocol.finish()));

        // Coordinated omission
        if let Some(ref mut b) = optional.intended_us {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.scheduling_delay_us {
            columns.push(Arc::new(b.finish()));
        }

        // Timing
        if let Some(ref mut b) = optional.dns_us {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.tcp_us {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.tls_us {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.ttfb_us {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.transfer_us {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.total_us {
            columns.push(Arc::new(b.finish()));
        }

        // Bytes
        if let Some(ref mut b) = optional.bytes_sent {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.response_size {
            columns.push(Arc::new(b.finish()));
        }

        // Errors
        if let Some(ref mut b) = optional.is_error {
            columns.push(Arc::new(b.finish()));
        }
        if let Some(ref mut b) = optional.error_kind {
            columns.push(Arc::new(b.finish()));
        }

        // Header columns
        for b in header_builders.iter_mut() {
            columns.push(Arc::new(b.finish()));
        }

        // Constant columns
        for b in constant_builders.iter_mut() {
            match b {
                ConstantBuilder::Str(sb) => columns.push(Arc::new(sb.finish())),
                ConstantBuilder::U64(ub) => columns.push(Arc::new(ub.finish())),
            }
        }

        let batch = RecordBatch::try_new(self.schema.clone(), columns)
            .expect("column/schema mismatch in event recorder");

        let mut writer = self.writer.borrow_mut();
        if let Err(e) = writer.write(&batch) {
            tracing::warn!("event recorder: failed to write batch: {e}");
        }

        self.rows_buffered.set(0);
    }
}

impl EventRecorder for ArrowEventRecorder {
    fn record(&self, result: &ExecutionResult) {
        // Sampling check
        if self.sample_rate < 1.0 {
            let r: f64 = self.sample_rng.borrow_mut().gen();
            if r >= self.sample_rate {
                return;
            }
        }

        let timestamp_us = self.anchor.to_epoch_us(result.actual_time);

        // Common columns
        {
            let mut c = self.common.borrow_mut();
            c.request_id.append_value(result.request_id);
            c.core_id
                .append_value((result.request_id / 1_000_000_000) as u16);
            c.timestamp_us.append_value(timestamp_us);
            c.status.append_value(result.status.unwrap_or(0));
            c.protocol.append_value(&self.protocol);
        }

        // Optional column groups
        {
            let mut o = self.optional.borrow_mut();

            if self.include_co {
                let intended_us = self.anchor.to_epoch_us(result.intended_time);
                o.intended_us.as_mut().unwrap().append_value(intended_us);
                o.scheduling_delay_us
                    .as_mut()
                    .unwrap()
                    .append_value(timestamp_us.saturating_sub(intended_us));
            }

            if self.include_timing {
                o.dns_us
                    .as_mut()
                    .unwrap()
                    .append_value(result.timing.dns_lookup.as_micros() as u64);
                o.tcp_us
                    .as_mut()
                    .unwrap()
                    .append_value(result.timing.tcp_connect.as_micros() as u64);
                o.tls_us
                    .as_mut()
                    .unwrap()
                    .append_value(result.timing.tls_handshake.as_micros() as u64);
                o.ttfb_us
                    .as_mut()
                    .unwrap()
                    .append_value(result.timing.time_to_first_byte.as_micros() as u64);
                o.transfer_us
                    .as_mut()
                    .unwrap()
                    .append_value(result.timing.content_transfer.as_micros() as u64);
                o.total_us
                    .as_mut()
                    .unwrap()
                    .append_value(result.timing.total.as_micros() as u64);
            }

            if self.include_bytes {
                o.bytes_sent
                    .as_mut()
                    .unwrap()
                    .append_value(result.bytes_sent);
                o.response_size
                    .as_mut()
                    .unwrap()
                    .append_value(result.response_size);
            }

            if self.include_errors {
                let is_error = result.error.is_some()
                    || (self.error_status_threshold > 0
                        && result.status.unwrap_or(0) >= self.error_status_threshold);
                o.is_error.as_mut().unwrap().append_value(is_error);
                o.error_kind
                    .as_mut()
                    .unwrap()
                    .append_value(error_kind_str(&result.error));
            }
        }

        // Header columns
        if !self.header_names.is_empty() {
            let mut hb = self.header_builders.borrow_mut();
            for (i, header_name) in self.header_names.iter().enumerate() {
                let value = result
                    .response_headers
                    .as_ref()
                    .and_then(|headers| {
                        headers
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case(header_name))
                    })
                    .map(|(_, v)| v.as_str());
                match value {
                    Some(v) => hb[i].append_value(v),
                    None => hb[i].append_null(),
                }
            }
        }

        // Constant columns
        if !self.constant_values.is_empty() {
            let mut cb = self.constant_builders.borrow_mut();
            for (i, val) in self.constant_values.iter().enumerate() {
                match (&mut cb[i], val) {
                    (ConstantBuilder::Str(b), ConstantValue::Str(s)) => b.append_value(s),
                    (ConstantBuilder::U64(b), ConstantValue::U64(v)) => b.append_value(*v),
                    _ => unreachable!("constant builder/value type mismatch"),
                }
            }
        }

        let count = self.rows_buffered.get() + 1;
        self.rows_buffered.set(count);
        if count >= self.batch_size {
            self.flush_batch();
        }
    }

    fn flush(&self) {
        self.flush_batch();
    }
}

impl Drop for ArrowEventRecorder {
    fn drop(&mut self) {
        self.flush_batch();
        if let Ok(mut writer) = self.writer.try_borrow_mut() {
            let _ = writer.finish();
        }
    }
}

fn error_kind_str(err: &Option<ExecutionError>) -> &'static str {
    match err {
        None => "",
        Some(ExecutionError::Connect(_)) => "connect",
        Some(ExecutionError::Timeout) => "timeout",
        Some(ExecutionError::Http(_)) => "http",
        Some(ExecutionError::Tls(_)) => "tls",
        Some(ExecutionError::Protocol(_)) => "protocol",
        Some(ExecutionError::Other(_)) => "other",
    }
}
