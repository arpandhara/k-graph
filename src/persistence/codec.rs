use bincode::Options;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{CodeEdge, CodeEntityNode, SourceSpan};

/// Current schema version written into every persisted record.
///
/// Bump this whenever the wire layout or validation semantics change in a way
/// that cannot be read by older code. Version `0` is reserved and rejected.
pub const SCHEMA_VERSION: u16 = 1;

/// Maximum number of bytes accepted for a single encoded record.
///
/// This bounds both allocation and decode time for attacker-controlled data.
/// It is intentionally generous enough for a full node or edge record; large
/// blobs must be stored in separate tables instead of being embedded here.
pub const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

/// Errors raised while encoding or decoding a persisted record.
#[derive(Debug, Error)]
pub enum CodecError {
    #[error("record is {size} bytes, exceeding the {max}-byte limit")]
    TooLarge { size: u64, max: u64 },
    #[error("record serialization failed: {0}")]
    Encode(#[source] Box<bincode::ErrorKind>),
    #[error("record deserialization failed: {0}")]
    Decode(#[source] Box<bincode::ErrorKind>),
    #[error("unsupported record schema version {found}; expected {expected}")]
    UnsupportedVersion { found: u16, expected: u16 },
    #[error("record failed validation: {0}")]
    InvalidRecord(String),
}

/// Validation hook applied to a payload before it is written or after it is
/// decoded.
///
/// Implementors must check every invariant that the in-memory graph relies on.
/// A record that cannot be validated is never returned to the caller.
pub trait ValidateRecord {
    fn validate_record(&self) -> Result<(), String>;
}

/// Envelope written around every payload.
///
/// The type is private so callers cannot construct or deserialize it without
/// going through [`encode_record`] or [`decode_record`].
#[derive(Serialize, Deserialize)]
struct RecordEnvelope<T> {
    schema_version: u16,
    payload: T,
}

fn options() -> impl bincode::Options {
    // Fixint keeps compatibility with the legacy `bincode::serialize` helpers
    // while the limit and trailing-byte rejection close the KG-04 gaps.
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RECORD_BYTES)
        .reject_trailing_bytes()
}

/// Encode `payload` with a schema version and a hard byte limit.
///
/// The payload is validated before serialization, so invalid state is never
/// persisted.
pub fn encode_record<T>(payload: &T) -> Result<Vec<u8>, CodecError>
where
    T: Serialize + ValidateRecord,
{
    payload
        .validate_record()
        .map_err(CodecError::InvalidRecord)?;

    let envelope = RecordEnvelope {
        schema_version: SCHEMA_VERSION,
        payload,
    };

    options().serialize(&envelope).map_err(CodecError::Encode)
}

/// Decode a versioned record and validate its payload.
///
/// Rejects oversized inputs before decoding, rejects unknown schema versions,
/// rejects trailing bytes, and returns [`CodecError::InvalidRecord`] when the
/// decoded payload violates an invariant.
pub fn decode_record<T>(bytes: &[u8]) -> Result<T, CodecError>
where
    T: DeserializeOwned + ValidateRecord,
{
    let size = bytes.len() as u64;
    if size > MAX_RECORD_BYTES {
        return Err(CodecError::TooLarge {
            size,
            max: MAX_RECORD_BYTES,
        });
    }

    let envelope: RecordEnvelope<T> = options().deserialize(bytes).map_err(CodecError::Decode)?;

    if envelope.schema_version != SCHEMA_VERSION {
        return Err(CodecError::UnsupportedVersion {
            found: envelope.schema_version,
            expected: SCHEMA_VERSION,
        });
    }

    envelope
        .payload
        .validate_record()
        .map_err(CodecError::InvalidRecord)?;

    Ok(envelope.payload)
}

impl ValidateRecord for CodeEntityNode {
    fn validate_record(&self) -> Result<(), String> {
        self.validate().map_err(|err| err.to_string())?;
        validate_span(&self.span(), "node span")
    }
}

impl ValidateRecord for CodeEdge {
    fn validate_record(&self) -> Result<(), String> {
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(format!(
                "edge confidence {} is not finite or is outside [0.0, 1.0]",
                self.confidence
            ));
        }

        if let Some(span) = &self.call_site {
            validate_span(span, "edge call_site")?;
        }

        Ok(())
    }
}

fn validate_span(span: &SourceSpan, label: &str) -> Result<(), String> {
    if span.start_line > span.end_line {
        return Err(format!(
            "{label} start_line {} is after end_line {}",
            span.start_line, span.end_line
        ));
    }

    if span.start_line == span.end_line && span.start_col > span.end_col {
        return Err(format!(
            "{label} start_col {} is after end_col {} on line {}",
            span.start_col, span.end_col, span.start_line
        ));
    }

    if span.byte_start > span.byte_end {
        return Err(format!(
            "{label} byte_start {} is after byte_end {}",
            span.byte_start, span.byte_end
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeKind, NodeKind, SourceSpan, SymbolUri, Visibility};

    fn sample_node() -> CodeEntityNode {
        let uri = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
        CodeEntityNode::try_new(
            uri,
            "add_file",
            NodeKind::Function,
            "src/add.rs",
            "rust",
            SourceSpan::new(10, 0, 25, 1, 200, 500),
        )
        .unwrap()
        .with_signature("pub fn add_file(path: &Path) -> Result<()>")
        .with_visibility(Visibility::Public)
    }

    fn sample_edge() -> CodeEdge {
        let source = SymbolUri::new("rusty", "src/add.rs", None, "add_file", "fn").unwrap();
        let target = SymbolUri::new("rusty", "src/index.rs", Some("Index"), "load", "fn").unwrap();
        CodeEdge::new(source, target, EdgeKind::Calls)
            .with_confidence(0.75)
            .with_call_site(SourceSpan::new(12, 18, 12, 33, 250, 265))
    }

    #[test]
    fn test_node_record_roundtrip() {
        let node = sample_node();
        let bytes = encode_record(&node).unwrap();
        let decoded: CodeEntityNode = decode_record(&bytes).unwrap();
        assert_eq!(node, decoded);
    }

    #[test]
    fn test_edge_record_roundtrip() {
        let edge = sample_edge();
        let bytes = encode_record(&edge).unwrap();
        let decoded: CodeEdge = decode_record(&bytes).unwrap();
        assert_eq!(edge, decoded);
    }

    #[test]
    fn test_decode_rejects_oversized_input() {
        let bytes = vec![0u8; MAX_RECORD_BYTES as usize + 1];
        assert!(matches!(
            decode_record::<CodeEdge>(&bytes),
            Err(CodecError::TooLarge { .. })
        ));
    }

    #[test]
    fn test_decode_rejects_trailing_bytes() {
        let mut bytes = encode_record(&sample_edge()).unwrap();
        bytes.push(0);
        assert!(matches!(
            decode_record::<CodeEdge>(&bytes),
            Err(CodecError::Decode(_))
        ));
    }

    #[test]
    fn test_decode_rejects_truncated_input() {
        let bytes = encode_record(&sample_edge()).unwrap();
        assert!(matches!(
            decode_record::<CodeEdge>(&bytes[..bytes.len() - 1]),
            Err(CodecError::Decode(_))
        ));
    }

    #[test]
    fn test_decode_rejects_unknown_schema_version() {
        let envelope = RecordEnvelope {
            schema_version: SCHEMA_VERSION + 1,
            payload: sample_edge(),
        };
        let bytes = options().serialize(&envelope).unwrap();

        assert!(matches!(
            decode_record::<CodeEdge>(&bytes),
            Err(CodecError::UnsupportedVersion {
                found,
                expected: SCHEMA_VERSION
            }) if found == SCHEMA_VERSION + 1
        ));
    }

    #[test]
    fn test_decode_rejects_nan_confidence() {
        let mut edge = sample_edge();
        edge.confidence = f32::NAN;
        let envelope = RecordEnvelope {
            schema_version: SCHEMA_VERSION,
            payload: edge,
        };
        let bytes = options().serialize(&envelope).unwrap();

        assert!(matches!(
            decode_record::<CodeEdge>(&bytes),
            Err(CodecError::InvalidRecord(_))
        ));
    }

    #[test]
    fn test_decode_rejects_inverted_span() {
        let mut edge = sample_edge();
        edge.call_site = Some(SourceSpan::new(20, 0, 10, 0, 500, 200));
        let envelope = RecordEnvelope {
            schema_version: SCHEMA_VERSION,
            payload: edge,
        };
        let bytes = options().serialize(&envelope).unwrap();

        assert!(matches!(
            decode_record::<CodeEdge>(&bytes),
            Err(CodecError::InvalidRecord(_))
        ));
    }

    #[test]
    fn test_encode_refuses_invalid_payload() {
        let mut edge = sample_edge();
        edge.confidence = -0.5;
        assert!(matches!(
            encode_record(&edge),
            Err(CodecError::InvalidRecord(_))
        ));
    }

    #[test]
    fn test_enum_wire_codes_are_stable() {
        assert_eq!(NodeKind::Function.stable_code(), 11);
        assert_eq!(NodeKind::from_stable_code(11), Some(NodeKind::Function));
        assert_eq!(NodeKind::from_stable_code(u8::MAX), None);

        assert_eq!(EdgeKind::Calls.stable_code(), 2);
        assert_eq!(EdgeKind::from_stable_code(2), Some(EdgeKind::Calls));
        assert_eq!(EdgeKind::from_stable_code(u8::MAX), None);

        assert_eq!(Visibility::ModulePrivate.stable_code(), 2);
        assert_eq!(
            Visibility::from_stable_code(2),
            Some(Visibility::ModulePrivate)
        );
        assert_eq!(Visibility::from_stable_code(u8::MAX), None);

        // Binary persistence uses the stable code, not the declaration index
        // emitted by Serde's derived enum serialization.
        assert_eq!(bincode::serialize(&NodeKind::Function).unwrap(), vec![11]);
        assert!(bincode::deserialize::<NodeKind>(&[u8::MAX]).is_err());

        // Human-readable persistence uses the stable string code.
        assert_eq!(
            serde_json::to_string(&NodeKind::Function).unwrap(),
            "\"function\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::CoChangedWith).unwrap(),
            "\"CO_CHANGED_WITH\""
        );
        assert_eq!(
            serde_json::to_string(&Visibility::ModulePrivate).unwrap(),
            "\"module_private\""
        );
    }
}
