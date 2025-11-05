//! Behavioral tests for ArrowEventRecorder.
//!
//! Validates:
//! - Records are written with correct column values
//! - Batching: flush only happens when batch_size is reached
//! - Explicit flush writes partial batches
//! - Drop finalizes the file (remaining records + IPC footer)
//! - Sampling: only a fraction of records are written
//! - Optional column groups (timing, bytes, errors, CO) are included/excluded
//! - Response header extraction and constant columns
//! - Error classification (transport errors and status threshold)

use std::fs::File;
use std::io::BufReader;
use std::time::{Duration, Instant};

use arrow_array::{
    Array, BooleanArray, RecordBatch, StringArray, UInt16Array, UInt64Array,
};
use arrow_ipc::reader::StreamReader;

use netanvil_events::{EventSchemaBuilder, TimeAnchor};
use netanvil_types::{EventRecorder, ExecutionError, ExecutionResult, TimingBreakdown};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create an ExecutionResult with controllable fields.
fn make_result(
    request_id: u64,
    status: Option<u16>,
    total_us: u64,
    bytes_sent: u64,
    response_size: u64,
    error: Option<ExecutionError>,
    headers: Option<Vec<(String, String)>>,
) -> ExecutionResult {
    let now = Instant::now();
    ExecutionResult {
        request_id,
        intended_time: now,
        actual_time: now,
        timing: TimingBreakdown {
            dns_lookup: Duration::from_micros(100),
            tcp_connect: Duration::from_micros(200),
            tls_handshake: Duration::from_micros(300),
            time_to_first_byte: Duration::from_micros(400),
            content_transfer: Duration::from_micros(500),
            total: Duration::from_micros(total_us),
        },
        status,
        bytes_sent,
        response_size,
        error,
        response_headers: headers,
        response_body: None,
    }
}

fn make_simple_result(request_id: u64, status: u16) -> ExecutionResult {
    make_result(request_id, Some(status), 1500, 256, 1024, None, None)
}

/// Read all record batches from an Arrow IPC stream file.
fn read_arrow_file(path: &str) -> Vec<RecordBatch> {
    let file = File::open(path).expect("open arrow file");
    let reader = StreamReader::try_new(BufReader::new(file), None).expect("create stream reader");
    reader.into_iter().map(|b| b.expect("read batch")).collect()
}

/// Count total rows across all batches.
fn total_rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(|b| b.num_rows()).sum()
}

/// Get a named column as UInt64Array from the first batch.
fn col_u64<'a>(batch: &'a RecordBatch, name: &str) -> &'a UInt64Array {
    let idx = batch.schema().index_of(name).unwrap();
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap()
}

fn col_u16<'a>(batch: &'a RecordBatch, name: &str) -> &'a UInt16Array {
    let idx = batch.schema().index_of(name).unwrap();
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt16Array>()
        .unwrap()
}

fn col_str<'a>(batch: &'a RecordBatch, name: &str) -> &'a StringArray {
    let idx = batch.schema().index_of(name).unwrap();
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
}

fn col_bool<'a>(batch: &'a RecordBatch, name: &str) -> &'a BooleanArray {
    let idx = batch.schema().index_of(name).unwrap();
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn records_written_with_correct_values() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .with_all()
        .batch_size(100) // large enough that nothing auto-flushes
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    // Record a few requests
    recorder.record(&make_result(
        42,
        Some(200),
        1500,
        256,
        1024,
        None,
        None,
    ));
    recorder.record(&make_result(
        43,
        Some(503),
        5000,
        128,
        0,
        Some(ExecutionError::Http("bad gateway".into())),
        None,
    ));
    recorder.record(&make_result(
        44,
        None, // no status (e.g. TCP protocol)
        800,
        64,
        512,
        None,
        None,
    ));

    // Explicit flush to write partial batch
    recorder.flush();
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    assert_eq!(total_rows(&batches), 3);

    let batch = &batches[0];

    // Verify request_id
    let ids = col_u64(batch, "request_id");
    assert_eq!(ids.value(0), 42);
    assert_eq!(ids.value(1), 43);
    assert_eq!(ids.value(2), 44);

    // Verify status
    let statuses = col_u16(batch, "status");
    assert_eq!(statuses.value(0), 200);
    assert_eq!(statuses.value(1), 503);
    assert_eq!(statuses.value(2), 0); // None → 0

    // Verify protocol
    let protocols = col_str(batch, "protocol");
    assert_eq!(protocols.value(0), "http");
    assert_eq!(protocols.value(1), "http");

    // Verify timing
    let total = col_u64(batch, "total_us");
    assert_eq!(total.value(0), 1500);
    assert_eq!(total.value(1), 5000);
    assert_eq!(total.value(2), 800);

    let dns = col_u64(batch, "dns_us");
    assert_eq!(dns.value(0), 100);

    let tcp = col_u64(batch, "tcp_us");
    assert_eq!(tcp.value(0), 200);

    // Verify bytes
    let sent = col_u64(batch, "bytes_sent");
    assert_eq!(sent.value(0), 256);
    let recv = col_u64(batch, "response_size");
    assert_eq!(recv.value(0), 1024);

    // Verify error classification
    let errors = col_bool(batch, "is_error");
    assert!(!errors.value(0)); // 200 OK, no error
    assert!(errors.value(1)); // 503 >= 400 threshold
    assert!(!errors.value(2)); // no status, no error

    let kinds = col_str(batch, "error_kind");
    assert_eq!(kinds.value(0), "");
    assert_eq!(kinds.value(1), "http");
    assert_eq!(kinds.value(2), "");
}

#[test]
fn batch_flushes_at_batch_size() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();
    let batch_size = 10;

    let schema = EventSchemaBuilder::new("tcp")
        .batch_size(batch_size)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    // Record exactly batch_size records — should auto-flush one batch
    for i in 0..batch_size {
        recorder.record(&make_simple_result(i as u64, 200));
    }

    // Record 3 more — below threshold, stays buffered
    for i in 0..3 {
        recorder.record(&make_simple_result(100 + i as u64, 200));
    }

    // Drop triggers final flush of the remaining 3
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);

    // Should have exactly 2 batches: one of batch_size, one of 3
    assert_eq!(batches.len(), 2, "expected 2 batches, got {}", batches.len());
    assert_eq!(batches[0].num_rows(), batch_size);
    assert_eq!(batches[1].num_rows(), 3);
    assert_eq!(total_rows(&batches), batch_size + 3);
}

#[test]
fn explicit_flush_writes_partial_batch() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .batch_size(1000) // much larger than what we'll record
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    recorder.record(&make_simple_result(1, 200));
    recorder.record(&make_simple_result(2, 200));
    recorder.record(&make_simple_result(3, 200));

    // Explicit flush
    recorder.flush();

    // Record more after flush
    recorder.record(&make_simple_result(4, 200));
    recorder.flush();

    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);

    assert_eq!(batches.len(), 2, "expected 2 batches from 2 flushes");
    assert_eq!(batches[0].num_rows(), 3);
    assert_eq!(batches[1].num_rows(), 1);
}

#[test]
fn flush_on_empty_buffer_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http").batch_size(100).build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    // Flush with nothing recorded — should not panic or write empty batch
    recorder.flush();
    recorder.flush();

    recorder.record(&make_simple_result(1, 200));
    recorder.flush();

    // Another empty flush after
    recorder.flush();

    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);

    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 1);
}

#[test]
fn drop_finalizes_file_with_remaining_records() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .batch_size(1000) // never auto-flushes
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    for i in 0..5 {
        recorder.record(&make_simple_result(i, 200));
    }

    // No explicit flush — Drop should handle it
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);

    assert_eq!(total_rows(&batches), 5);
}

#[test]
fn sampling_reduces_output_rows() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .sample_rate(0.1) // 10%
        .batch_size(10000)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    let n = 10_000;
    for i in 0..n {
        recorder.record(&make_simple_result(i, 200));
    }
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    let rows = total_rows(&batches);

    // With 10% sampling of 10K records, expect ~1000 rows.
    // Allow wide tolerance for randomness: 500-2000.
    assert!(
        rows >= 500 && rows <= 2000,
        "expected ~1000 sampled rows, got {rows}"
    );
}

#[test]
fn sample_rate_zero_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .sample_rate(0.0)
        .batch_size(100)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    for i in 0..100 {
        recorder.record(&make_simple_result(i, 200));
    }
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    assert_eq!(total_rows(&batches), 0);
}

#[test]
fn sample_rate_one_writes_all() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .sample_rate(1.0)
        .batch_size(1000)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    for i in 0..50 {
        recorder.record(&make_simple_result(i, 200));
    }
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    assert_eq!(total_rows(&batches), 50);
}

#[test]
fn minimal_schema_excludes_optional_columns() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    // No with_timing(), with_bytes(), with_errors(), with_coordinated_omission()
    let schema = EventSchemaBuilder::new("udp").batch_size(100).build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    recorder.record(&make_simple_result(1, 0));
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    assert_eq!(total_rows(&batches), 1);

    let schema = batches[0].schema();
    let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();

    // Common columns are present
    assert!(field_names.contains(&"request_id"));
    assert!(field_names.contains(&"timestamp_us"));
    assert!(field_names.contains(&"status"));
    assert!(field_names.contains(&"protocol"));

    // Optional columns are NOT present
    assert!(!field_names.contains(&"total_us"));
    assert!(!field_names.contains(&"dns_us"));
    assert!(!field_names.contains(&"bytes_sent"));
    assert!(!field_names.contains(&"is_error"));
    assert!(!field_names.contains(&"intended_us"));
}

#[test]
fn response_header_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .response_header("X-Cache", "cache_tier")
        .response_header("X-Backend", "backend")
        .batch_size(100)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    // Request with both headers
    recorder.record(&make_result(
        1,
        Some(200),
        1000,
        0,
        0,
        None,
        Some(vec![
            ("X-Cache".into(), "HIT".into()),
            ("X-Backend".into(), "dc1-web-03".into()),
        ]),
    ));

    // Request with only one header
    recorder.record(&make_result(
        2,
        Some(200),
        1000,
        0,
        0,
        None,
        Some(vec![("X-Cache".into(), "MISS".into())]),
    ));

    // Request with no headers captured
    recorder.record(&make_result(3, Some(200), 1000, 0, 0, None, None));

    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    let batch = &batches[0];

    let cache = col_str(batch, "cache_tier");
    assert_eq!(cache.value(0), "HIT");
    assert_eq!(cache.value(1), "MISS");
    assert!(cache.is_null(2));

    let backend = col_str(batch, "backend");
    assert_eq!(backend.value(0), "dc1-web-03");
    assert!(backend.is_null(1));
    assert!(backend.is_null(2));
}

#[test]
fn constant_columns() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .constant_str("test_name", "regression-v2")
        .constant_u64("run_id", 99999)
        .batch_size(100)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    recorder.record(&make_simple_result(1, 200));
    recorder.record(&make_simple_result(2, 200));
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    let batch = &batches[0];

    let test_name = col_str(batch, "test_name");
    assert_eq!(test_name.value(0), "regression-v2");
    assert_eq!(test_name.value(1), "regression-v2");

    let run_id = col_u64(batch, "run_id");
    assert_eq!(run_id.value(0), 99999);
    assert_eq!(run_id.value(1), 99999);
}

#[test]
fn error_classification_respects_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http")
        .with_errors(500) // only 5xx are errors
        .batch_size(100)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    // 200 — not an error
    recorder.record(&make_simple_result(1, 200));
    // 404 — below 500 threshold, not an error
    recorder.record(&make_simple_result(2, 404));
    // 500 — at threshold, IS an error
    recorder.record(&make_simple_result(3, 500));
    // Transport error (timeout) — always an error regardless of status
    recorder.record(&make_result(4, Some(200), 1000, 0, 0, Some(ExecutionError::Timeout), None));

    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    let batch = &batches[0];

    let errors = col_bool(batch, "is_error");
    assert!(!errors.value(0), "200 should not be error");
    assert!(!errors.value(1), "404 should not be error with threshold 500");
    assert!(errors.value(2), "500 should be error");
    assert!(errors.value(3), "timeout should be error regardless of status");

    let kinds = col_str(batch, "error_kind");
    assert_eq!(kinds.value(0), "");
    assert_eq!(kinds.value(1), "");
    assert_eq!(kinds.value(2), "");
    assert_eq!(kinds.value(3), "timeout");
}

#[test]
fn multiple_batches_preserve_schema() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();
    let batch_size = 5;

    let schema = EventSchemaBuilder::new("http")
        .with_all()
        .batch_size(batch_size)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    // Write 3 full batches + 2 leftover
    for i in 0..(batch_size * 3 + 2) {
        recorder.record(&make_simple_result(i as u64, 200));
    }
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);

    assert_eq!(batches.len(), 4, "expected 4 batches (3 full + 1 partial)");
    assert_eq!(batches[0].num_rows(), batch_size);
    assert_eq!(batches[1].num_rows(), batch_size);
    assert_eq!(batches[2].num_rows(), batch_size);
    assert_eq!(batches[3].num_rows(), 2);

    // All batches should share the same schema
    let expected_schema = batches[0].schema();
    for (i, batch) in batches.iter().enumerate() {
        assert_eq!(
            batch.schema(),
            expected_schema,
            "batch {i} has different schema"
        );
    }

    // Verify request_ids are sequential across batches
    let mut all_ids: Vec<u64> = Vec::new();
    for batch in &batches {
        let ids = col_u64(batch, "request_id");
        for i in 0..batch.num_rows() {
            all_ids.push(ids.value(i));
        }
    }
    let expected: Vec<u64> = (0..(batch_size * 3 + 2) as u64).collect();
    assert_eq!(all_ids, expected);
}

#[test]
fn per_core_files_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let anchor = TimeAnchor::now();

    let schema = EventSchemaBuilder::new("http").batch_size(100).build();

    let factory = schema.into_factory(dir.path().to_str().unwrap().to_string(), anchor);

    let recorder_0 = factory(0);
    let recorder_1 = factory(1);

    recorder_0.record(&make_simple_result(0 * 1_000_000_000 + 1, 200));
    recorder_0.record(&make_simple_result(0 * 1_000_000_000 + 2, 200));

    recorder_1.record(&make_simple_result(1 * 1_000_000_000 + 1, 404));

    drop(recorder_0);
    drop(recorder_1);

    let batches_0 = read_arrow_file(&format!("{}/events_core_0.arrow", dir.path().display()));
    let batches_1 = read_arrow_file(&format!("{}/events_core_1.arrow", dir.path().display()));

    assert_eq!(total_rows(&batches_0), 2);
    assert_eq!(total_rows(&batches_1), 1);

    // Core IDs are derived from request_id (request_id / 1_000_000_000)
    let core_ids_0 = col_u16(&batches_0[0], "core_id");
    assert_eq!(core_ids_0.value(0), 0);
    assert_eq!(core_ids_0.value(1), 0);

    let core_ids_1 = col_u16(&batches_1[0], "core_id");
    assert_eq!(core_ids_1.value(0), 1);
}

#[test]
fn timestamps_are_epoch_microseconds() {
    let anchor = TimeAnchor::now();
    let dir = tempfile::tempdir().unwrap();

    let schema = EventSchemaBuilder::new("http")
        .with_coordinated_omission()
        .batch_size(100)
        .build();

    let recorder = schema
        .into_factory(dir.path().to_str().unwrap().to_string(), anchor)(0);

    recorder.record(&make_simple_result(1, 200));
    drop(recorder);

    let path = format!("{}/events_core_0.arrow", dir.path().display());
    let batches = read_arrow_file(&path);
    let batch = &batches[0];

    let ts = col_u64(batch, "timestamp_us");
    let intended = col_u64(batch, "intended_us");

    // Timestamps should be epoch microseconds (roughly current time)
    // Current epoch ~1.7e15 us (2024+), must be > 1.5e15 (year 2017)
    assert!(
        ts.value(0) > 1_500_000_000_000_000,
        "timestamp should be epoch us, got {}",
        ts.value(0)
    );
    assert!(
        intended.value(0) > 1_500_000_000_000_000,
        "intended_time should be epoch us, got {}",
        intended.value(0)
    );

    // scheduling_delay_us should be 0 since intended == actual in our test fixture
    let delay = col_u64(batch, "scheduling_delay_us");
    assert_eq!(delay.value(0), 0, "delay should be 0 when intended == actual");
}
