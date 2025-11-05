//! Per-request event logging for netanvil-rs.
//!
//! Streams per-request records to Arrow IPC files for post-test statistical
//! analysis. One file per I/O worker core — zero cross-core synchronization.
//!
//! # Usage
//!
//! ```ignore
//! use netanvil_events::{EventSchemaBuilder, TimeAnchor};
//!
//! let schema = EventSchemaBuilder::new("http")
//!     .with_all()
//!     .sample_rate(0.01)
//!     .build();
//!
//! let anchor = TimeAnchor::now();
//! let factory = schema.into_factory("/tmp/events".into(), anchor);
//!
//! // Per core:
//! let recorder = factory(0); // core 0
//! ```

pub mod anchor;
pub mod recorder;
pub mod schema;

pub use anchor::TimeAnchor;
pub use recorder::ArrowEventRecorder;
pub use schema::{EventRecorderFactory, EventSchema, EventSchemaBuilder};
