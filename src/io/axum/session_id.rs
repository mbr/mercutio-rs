//! MCP session identifier type.

use std::{fmt, str::FromStr};

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use rand::distr::{Distribution, StandardUniform};

/// Header name for the MCP session ID per the spec.
pub const SESSION_ID_HEADER: &str = "mcp-session-id";

/// Unique identifier for an MCP session.
///
/// Wraps a 128-bit random value, displayed as lowercase hex. Implements [`Distribution`] for
/// generation via `rng.random()`, and [`FromRequestParts`] for extraction from the
/// `Mcp-Session-Id` header.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct McpSessionId(u128);

impl McpSessionId {
    /// Creates a session ID from a raw u128 value.
    pub fn from_raw(value: u128) -> Self {
        Self(value)
    }

    /// Returns the raw u128 value.
    pub fn as_raw(&self) -> u128 {
        self.0
    }
}

impl Distribution<McpSessionId> for StandardUniform {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> McpSessionId {
        McpSessionId(rng.random())
    }
}

impl fmt::Display for McpSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl fmt::Debug for McpSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "McpSessionId({self})")
    }
}

/// Error returned when parsing an [`McpSessionId`] fails.
#[derive(Debug, Clone)]
pub struct ParseSessionIdError;

impl fmt::Display for ParseSessionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid session ID format")
    }
}

impl std::error::Error for ParseSessionIdError {}

impl FromStr for McpSessionId {
    type Err = ParseSessionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        u128::from_str_radix(s, 16)
            .map(McpSessionId)
            .map_err(|_| ParseSessionIdError)
    }
}

/// Rejection type when session ID extraction fails.
#[derive(Debug)]
pub enum SessionIdRejection {
    /// The `Mcp-Session-Id` header is missing.
    Missing,
    /// The header value is not valid UTF-8 or failed to parse.
    Invalid,
}

impl fmt::Display for SessionIdRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "missing session ID header"),
            Self::Invalid => write!(f, "invalid session ID"),
        }
    }
}

impl axum::response::IntoResponse for SessionIdRejection {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            Self::Missing => StatusCode::BAD_REQUEST,
            Self::Invalid => StatusCode::BAD_REQUEST,
        };
        (status, self.to_string()).into_response()
    }
}

impl<S> FromRequestParts<S> for McpSessionId
where
    S: Send + Sync,
{
    type Rejection = SessionIdRejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let value = parts
            .headers
            .get(SESSION_ID_HEADER)
            .ok_or(SessionIdRejection::Missing)?;

        let s = value.to_str().map_err(|_| SessionIdRejection::Invalid)?;
        s.parse().map_err(|_| SessionIdRejection::Invalid)
    }
}

/// Extractor for an optional session ID.
///
/// Returns `None` if the header is missing, `Some(id)` if valid, or rejects with
/// [`SessionIdRejection::Invalid`] if the header is present but malformed.
#[derive(Clone, Copy, Debug)]
pub struct OptionalSessionId(pub Option<McpSessionId>);

impl<S> FromRequestParts<S> for OptionalSessionId
where
    S: Send + Sync,
{
    type Rejection = SessionIdRejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(value) = parts.headers.get(SESSION_ID_HEADER) else {
            return Ok(Self(None));
        };

        let s = value.to_str().map_err(|_| SessionIdRejection::Invalid)?;
        let id = s.parse().map_err(|_| SessionIdRejection::Invalid)?;
        Ok(Self(Some(id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_32_hex_chars() {
        let id = McpSessionId(0x0123456789abcdef0123456789abcdef);
        assert_eq!(id.to_string(), "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn display_pads_with_zeros() {
        let id = McpSessionId(1);
        assert_eq!(id.to_string(), "00000000000000000000000000000001");
    }

    #[test]
    fn debug_uses_display() {
        let id = McpSessionId(0xff);
        assert_eq!(
            format!("{id:?}"),
            "McpSessionId(000000000000000000000000000000ff)"
        );
    }

    #[test]
    fn parse_valid_hex() {
        let id: McpSessionId = "0123456789abcdef0123456789abcdef".parse().unwrap();
        assert_eq!(id.0, 0x0123456789abcdef0123456789abcdef);
    }

    #[test]
    fn parse_short_hex() {
        let id: McpSessionId = "ff".parse().unwrap();
        assert_eq!(id.0, 0xff);
    }

    #[test]
    fn parse_invalid_fails() {
        assert!("not-hex".parse::<McpSessionId>().is_err());
    }

    #[test]
    fn roundtrip() {
        let original = McpSessionId(0xdeadbeef12345678deadbeef12345678);
        let s = original.to_string();
        let parsed: McpSessionId = s.parse().unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn random_generation() {
        use rand::Rng;
        let mut rng = rand::rng();
        let id: McpSessionId = rng.random();
        assert_eq!(id.to_string().len(), 32);
    }
}
