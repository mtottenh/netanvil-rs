//! Event schema definition and builder.
//!
//! The schema defines which columns appear in the per-request Arrow IPC files.
//! Common columns (timing, bytes, errors) are opt-in via builder methods.
//! Protocol-specific columns (response headers, constants) are added explicitly.

use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema};

use crate::anchor::TimeAnchor;
use crate::recorder::ArrowEventRecorder;

/// Factory that creates per-core event recorders.
pub type EventRecorderFactory = Box<dyn Fn(usize) -> Box<dyn netanvil_types::EventRecorder> + Send>;

/// Describes a response header to extract as a column.
#[derive(Debug, Clone)]
pub struct HeaderColumn {
    /// HTTP header name to look up (case-sensitive match against captured headers).
    pub header_name: String,
    /// Column name in the Arrow schema.
    pub column_name: String,
}

/// A constant string column (same value every row).
#[derive(Debug, Clone)]
pub enum ConstantValue {
    Str(String),
    U64(u64),
}

/// A constant column definition.
#[derive(Debug, Clone)]
pub struct ConstantColumn {
    pub name: String,
    pub value: ConstantValue,
}

/// Immutable event schema. `Send + Sync` — shared across worker threads.
///
/// Created via [`EventSchemaBuilder`], then converted to a factory that
/// produces per-core [`ArrowEventRecorder`] instances.
#[derive(Debug, Clone)]
pub struct EventSchema {
    pub(crate) arrow_schema: Arc<Schema>,
    pub(crate) protocol: String,
    pub(crate) error_status_threshold: u16,
    pub(crate) sample_rate: f64,
    pub(crate) batch_size: usize,
    pub(crate) include_timing: bool,
    pub(crate) include_bytes: bool,
    pub(crate) include_errors: bool,
    pub(crate) include_co: bool,
    pub(crate) header_columns: Vec<HeaderColumn>,
    pub(crate) constant_columns: Vec<ConstantColumn>,
}

impl EventSchema {
    /// Create a factory that produces per-core `ArrowEventRecorder` instances.
    pub fn into_factory(self, output_dir: String, anchor: TimeAnchor) -> EventRecorderFactory {
        let schema = Arc::new(self);
        Box::new(move |core_id| {
            Box::new(
                ArrowEventRecorder::new(core_id, &schema, &output_dir, anchor)
                    .unwrap_or_else(|e| panic!("create event recorder for core {core_id}: {e}")),
            ) as Box<dyn netanvil_types::EventRecorder>
        })
    }
}

/// Builder for configuring which columns appear in per-request event logs.
///
/// # Example
///
/// ```ignore
/// let schema = EventSchemaBuilder::new("http")
///     .with_timing()
///     .with_bytes()
///     .with_errors(400)
///     .with_coordinated_omission()
///     .response_header("X-Cache", "cache_tier")
///     .sample_rate(0.01)
///     .build();
/// ```
pub struct EventSchemaBuilder {
    protocol: String,
    sample_rate: f64,
    batch_size: usize,
    include_timing: bool,
    include_bytes: bool,
    include_errors: bool,
    include_co: bool,
    error_status_threshold: u16,
    header_columns: Vec<HeaderColumn>,
    constant_columns: Vec<ConstantColumn>,
}

impl EventSchemaBuilder {
    /// Create a new builder for the given protocol name.
    pub fn new(protocol: &str) -> Self {
        Self {
            protocol: protocol.to_string(),
            sample_rate: 1.0,
            batch_size: 8192,
            include_timing: false,
            include_bytes: false,
            include_errors: false,
            include_co: false,
            error_status_threshold: 400,
            header_columns: Vec::new(),
            constant_columns: Vec::new(),
        }
    }

    /// Include all common column groups (timing, bytes, errors, coordinated omission).
    pub fn with_all(self) -> Self {
        let threshold = self.error_status_threshold;
        self.with_timing()
            .with_bytes()
            .with_errors(threshold)
            .with_coordinated_omission()
    }

    /// Include timing breakdown columns: dns_us, tcp_us, tls_us, ttfb_us, transfer_us, total_us.
    pub fn with_timing(mut self) -> Self {
        self.include_timing = true;
        self
    }

    /// Include byte counter columns: bytes_sent, response_size.
    pub fn with_bytes(mut self) -> Self {
        self.include_bytes = true;
        self
    }

    /// Include error columns: is_error, error_kind.
    /// `threshold` is the HTTP status code threshold for status-based errors.
    pub fn with_errors(mut self, threshold: u16) -> Self {
        self.include_errors = true;
        self.error_status_threshold = threshold;
        self
    }

    /// Include coordinated omission columns: intended_us, scheduling_delay_us.
    pub fn with_coordinated_omission(mut self) -> Self {
        self.include_co = true;
        self
    }

    /// Add a column extracted from a response header value.
    pub fn response_header(mut self, header: &str, column_name: &str) -> Self {
        self.header_columns.push(HeaderColumn {
            header_name: header.to_string(),
            column_name: column_name.to_string(),
        });
        self
    }

    /// Add a constant string column (same value every row).
    pub fn constant_str(mut self, name: &str, value: &str) -> Self {
        self.constant_columns.push(ConstantColumn {
            name: name.to_string(),
            value: ConstantValue::Str(value.to_string()),
        });
        self
    }

    /// Add a constant u64 column (same value every row).
    pub fn constant_u64(mut self, name: &str, value: u64) -> Self {
        self.constant_columns.push(ConstantColumn {
            name: name.to_string(),
            value: ConstantValue::U64(value),
        });
        self
    }

    /// Set the sampling rate (0.0-1.0). Default: 1.0 (log all requests).
    pub fn sample_rate(mut self, rate: f64) -> Self {
        self.sample_rate = rate.clamp(0.0, 1.0);
        self
    }

    /// Set the Arrow record batch size (rows per batch). Default: 8192.
    pub fn batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.max(1);
        self
    }

    /// Build the immutable event schema.
    pub fn build(self) -> EventSchema {
        let arrow_schema = Arc::new(self.build_arrow_schema());
        EventSchema {
            arrow_schema,
            protocol: self.protocol,
            error_status_threshold: self.error_status_threshold,
            sample_rate: self.sample_rate,
            batch_size: self.batch_size,
            include_timing: self.include_timing,
            include_bytes: self.include_bytes,
            include_errors: self.include_errors,
            include_co: self.include_co,
            header_columns: self.header_columns,
            constant_columns: self.constant_columns,
        }
    }

    fn build_arrow_schema(&self) -> Schema {
        let mut fields = Vec::new();

        // Always present: identity + timestamp + status + protocol
        fields.push(Field::new("request_id", DataType::UInt64, false));
        fields.push(Field::new("core_id", DataType::UInt16, false));
        fields.push(Field::new("timestamp_us", DataType::UInt64, false));
        fields.push(Field::new("status", DataType::UInt16, false));
        fields.push(Field::new("protocol", DataType::Utf8, false));

        // Coordinated omission
        if self.include_co {
            fields.push(Field::new("intended_us", DataType::UInt64, false));
            fields.push(Field::new("scheduling_delay_us", DataType::UInt64, false));
        }

        // Timing breakdown
        if self.include_timing {
            fields.push(Field::new("dns_us", DataType::UInt64, false));
            fields.push(Field::new("tcp_us", DataType::UInt64, false));
            fields.push(Field::new("tls_us", DataType::UInt64, false));
            fields.push(Field::new("ttfb_us", DataType::UInt64, false));
            fields.push(Field::new("transfer_us", DataType::UInt64, false));
            fields.push(Field::new("total_us", DataType::UInt64, false));
        }

        // Bytes
        if self.include_bytes {
            fields.push(Field::new("bytes_sent", DataType::UInt64, false));
            fields.push(Field::new("response_size", DataType::UInt64, false));
        }

        // Errors
        if self.include_errors {
            fields.push(Field::new("is_error", DataType::Boolean, false));
            fields.push(Field::new("error_kind", DataType::Utf8, false));
        }

        // Response header columns
        for hc in &self.header_columns {
            fields.push(Field::new(&hc.column_name, DataType::Utf8, true));
        }

        // Constant columns
        for cc in &self.constant_columns {
            match &cc.value {
                ConstantValue::Str(_) => {
                    fields.push(Field::new(&cc.name, DataType::Utf8, false));
                }
                ConstantValue::U64(_) => {
                    fields.push(Field::new(&cc.name, DataType::UInt64, false));
                }
            }
        }

        Schema::new(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_minimal_schema() {
        let schema = EventSchemaBuilder::new("tcp").build();
        let fields: Vec<&str> = schema
            .arrow_schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect();
        assert!(fields.contains(&"request_id"));
        assert!(fields.contains(&"core_id"));
        assert!(fields.contains(&"timestamp_us"));
        assert!(fields.contains(&"status"));
        assert!(fields.contains(&"protocol"));
        // Timing/bytes/errors not included by default
        assert!(!fields.contains(&"total_us"));
        assert!(!fields.contains(&"bytes_sent"));
        assert!(!fields.contains(&"is_error"));
    }

    #[test]
    fn builder_with_all_includes_everything() {
        let schema = EventSchemaBuilder::new("http")
            .with_all()
            .response_header("X-Cache", "cache_tier")
            .constant_str("test", "val")
            .build();
        let fields: Vec<&str> = schema
            .arrow_schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect();
        assert!(fields.contains(&"total_us"));
        assert!(fields.contains(&"dns_us"));
        assert!(fields.contains(&"bytes_sent"));
        assert!(fields.contains(&"is_error"));
        assert!(fields.contains(&"intended_us"));
        assert!(fields.contains(&"scheduling_delay_us"));
        assert!(fields.contains(&"cache_tier"));
        assert!(fields.contains(&"test"));
    }
}
