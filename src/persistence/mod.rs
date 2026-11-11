//! Persistence codecs and versioning.
//!
//! All durable records must be written and read through [`codec`] so that
//! untrusted bytes are bounded, version-checked, and validated before they are
//! inserted into the in-memory graph.

pub mod codec;

pub use codec::{
    CodecError, MAX_RECORD_BYTES, SCHEMA_VERSION, ValidateRecord, decode_record, encode_record,
};
