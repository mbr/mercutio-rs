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
//! - `chrono`: Uses [`chrono::DateTime<FixedOffset>`] as the inner type
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

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(all(feature = "jiff", feature = "chrono"))]
compile_error!("features `jiff` and `chrono` are mutually exclusive");

#[cfg(feature = "jiff")]
mod backend {
    use std::fmt;

    pub type Inner = jiff::Timestamp;

    pub fn parse(s: &str) -> Result<Inner, impl fmt::Display> {
        s.parse::<jiff::Timestamp>()
    }

    pub fn now_formatted() -> impl fmt::Display {
        jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%S%:z")
    }

    pub fn format(ts: &Inner, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(ts, f)
    }
}

#[cfg(all(feature = "chrono", not(feature = "jiff")))]
mod backend {
    use std::fmt;

    use chrono::SecondsFormat;

    pub type Inner = chrono::DateTime<chrono::FixedOffset>;

    pub fn parse(s: &str) -> Result<Inner, impl fmt::Display> {
        chrono::DateTime::parse_from_rfc3339(s)
    }

    pub fn now_formatted() -> impl fmt::Display {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%:z")
    }

    pub fn format(ts: &Inner, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&ts.to_rfc3339_opts(SecondsFormat::AutoSi, true))
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
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Rfc3339".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": "date-time"
        })
    }
}

impl fmt::Display for Rfc3339 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        backend::format(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::{Rfc3339, backend};

    #[test]
    fn deserialize_valid_timestamps() {
        let _utc: Rfc3339 = serde_json::from_str(r#""2024-03-11T10:00:00Z""#).expect("valid UTC");
        let _offset: Rfc3339 =
            serde_json::from_str(r#""2024-03-11T12:00:00+02:00""#).expect("valid offset");
    }

    #[test]
    fn display_outputs_valid_rfc3339() {
        let cases = [
            r#""2024-03-11T10:00:00Z""#,
            r#""2024-03-11T12:00:00+02:00""#,
            r#""2024-12-31T23:59:59-05:00""#,
            r#""2000-01-01T00:00:00+00:00""#,
        ];
        for input in cases {
            let ts: Rfc3339 = serde_json::from_str(input).expect("valid input");
            let displayed = ts.to_string();
            backend::parse(&displayed).unwrap_or_else(|e| {
                panic!(
                    "Display output '{}' is not valid RFC 3339: {}",
                    displayed, e
                )
            });
        }
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
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Rfc3339",
  "type": "string",
  "format": "date-time"
}
"#);
    }
}
