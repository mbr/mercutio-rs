//! RFC 3339 timestamp type for MCP tool inputs.
//!
//! [`Rfc3339`] is a newtype over the backend's timestamp type. Its [`JsonSchema`] implementation
//! emits `format: "date-time"`, which is defined by JSON Schema and OpenAPI as RFC 3339. Models
//! trained on API specs recognize this format natively.
//!
//! When parsing fails, the error includes the current time as an example, helping models
//! self-correct:
//!
//! ```text
//! invalid RFC 3339 timestamp '2025-05-25 14:30:00': failed to find ...
//! Example: current time is 2025-05-25T14:30:00+02:00
//! ```
//!
//! RFC 3339 is stricter than ISO 8601, requiring the `T` separator and timezone offset, which
//! reduces ambiguity in LLM-generated timestamps.
//!
//! # Backend Selection
//!
//! Enable either the `jiff` or `chrono` feature to use this type:
//!
//! - `jiff`: Uses [`jiff::Timestamp`] as the inner type
//! - `chrono`: Uses [`chrono::DateTime<Utc>`] as the inner type
//!
//! # Example
//!
//! ```
//! use mercutio::Rfc3339;
//!
//! mercutio::tool_registry! {
//!     enum Tools {
//!         Schedule("schedule", "Schedule a meeting") {
//!             /// Meeting start time.
//!             start: Rfc3339,
//!         },
//!     }
//! }
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(all(feature = "jiff", feature = "chrono"))]
compile_error!("features `jiff` and `chrono` are mutually exclusive");

#[cfg(feature = "jiff")]
mod backend {
    pub type Inner = jiff::Timestamp;

    pub fn parse(s: &str) -> Result<Inner, impl std::fmt::Display> {
        s.parse::<jiff::Timestamp>()
    }

    pub fn now_formatted() -> impl std::fmt::Display {
        jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%S%:z")
    }
}

#[cfg(feature = "chrono")]
mod backend {
    pub type Inner = chrono::DateTime<chrono::Utc>;

    pub fn parse(s: &str) -> Result<Inner, impl std::fmt::Display> {
        chrono::DateTime::parse_from_rfc3339(s).map(|dt| dt.to_utc())
    }

    pub fn now_formatted() -> impl std::fmt::Display {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%:z")
    }
}

use backend::Inner;

/// RFC 3339 timestamp for MCP tool inputs.
///
/// A transparent wrapper over the backend's timestamp type. Emits `format: "date-time"` in JSON
/// Schema, which is defined by JSON Schema and OpenAPI as RFC 3339. Deserialization errors include
/// the current time as an example, helping models self-correct.
///
/// # Example
///
/// ```
/// use mercutio::Rfc3339;
///
/// let ts: Rfc3339 = serde_json::from_str(r#""2024-03-11T10:00:00Z""#).expect("valid");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Rfc3339(pub Inner);

impl<'de> Deserialize<'de> for Rfc3339 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        backend::parse(&s).map(Rfc3339).map_err(|e| {
            serde::de::Error::custom(format!(
                "invalid RFC 3339 timestamp '{}': {}\nExample: current time is {}",
                s,
                e,
                backend::now_formatted()
            ))
        })
    }
}

impl JsonSchema for Rfc3339 {
    fn schema_name() -> String {
        "Rfc3339".to_string()
    }

    fn is_referenceable() -> bool {
        false
    }

    fn json_schema(_gen: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            format: Some("date-time".to_string()),
            ..Default::default()
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::Rfc3339;

    #[test]
    fn deserialize_valid_timestamps() {
        let utc: Rfc3339 = serde_json::from_str(r#""2024-03-11T10:00:00Z""#).expect("valid UTC");
        let offset: Rfc3339 =
            serde_json::from_str(r#""2024-03-11T12:00:00+02:00""#).expect("valid offset");
        // Both represent the same instant
        assert_eq!(utc.0, offset.0);
    }

    #[test]
    fn error_message_format() {
        let err = serde_json::from_str::<Rfc3339>(r#""2025-05-25 14:30:00""#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid RFC 3339 timestamp '2025-05-25 14:30:00'"));
        assert!(msg.contains("Example: current time is"));
    }

    #[test]
    fn roundtrip() {
        let ts: Rfc3339 = serde_json::from_str(r#""2024-03-11T10:00:00Z""#).expect("valid");
        let serialized = serde_json::to_string(&ts).expect("serializes");
        let reparsed: Rfc3339 = serde_json::from_str(&serialized).expect("valid");
        assert_eq!(reparsed, ts);
    }

    #[test]
    fn json_schema() {
        let schema = schemars::schema_for!(Rfc3339);
        let json = serde_json::to_string_pretty(&schema).expect("schema serializes");
        insta::assert_snapshot!(json, @r#"
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Rfc3339",
  "type": "string",
  "format": "date-time"
}
"#);
    }
}
