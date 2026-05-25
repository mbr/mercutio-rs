//! RFC 3339 timestamp type for MCP tool inputs.
//!
//! [`Rfc3339`] is a newtype over [`jiff::Timestamp`]. Its [`JsonSchema`] implementation emits
//! `format: "date-time"`, which is defined by JSON Schema and OpenAPI as RFC 3339. Models trained
//! on API specs recognize this format natively.
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

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// RFC 3339 timestamp for MCP tool inputs.
///
/// A transparent wrapper over [`jiff::Timestamp`]. Emits `format: "date-time"` in JSON Schema,
/// which is defined by JSON Schema and OpenAPI as RFC 3339. Deserialization errors include the
/// current time as an example, helping models self-correct.
///
/// # Example
///
/// ```
/// use mercutio::Rfc3339;
///
/// let ts: Rfc3339 = serde_json::from_str(r#""2024-03-11T10:00:00Z""#).expect("valid");
/// assert_eq!(ts.0.as_second(), 1710151200);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Rfc3339(pub Timestamp);

impl<'de> Deserialize<'de> for Rfc3339 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<Timestamp>().map(Rfc3339).map_err(|e| {
            let now = Timestamp::now();
            serde::de::Error::custom(format!(
                "invalid RFC 3339 timestamp '{}': {}\nExample: current time is {}",
                s,
                e,
                now.strftime("%Y-%m-%dT%H:%M:%S%:z")
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
        assert_eq!(utc.0.as_second(), 1710151200);
        assert_eq!(offset.0.as_second(), 1710151200);
    }

    #[test]
    fn error_message_format() {
        let mut settings = insta::Settings::clone_current();
        settings.add_filter(
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}",
            "[timestamp]",
        );
        settings.bind(|| {
            let err = serde_json::from_str::<Rfc3339>(r#""2025-05-25 14:30:00""#).unwrap_err();
            insta::assert_snapshot!(err.to_string(), @r#"
invalid RFC 3339 timestamp '2025-05-25 14:30:00': failed to find offset component, which is required for parsing a timestamp
Example: current time is [timestamp]
"#);
        });
    }

    #[test]
    fn roundtrip() {
        let ts: Rfc3339 = serde_json::from_str(r#""2024-03-11T10:00:00Z""#).expect("valid");
        let serialized = serde_json::to_string(&ts).expect("serializes");
        assert_eq!(serialized, r#""2024-03-11T10:00:00Z""#);

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
