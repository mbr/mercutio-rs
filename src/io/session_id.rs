//! MCP session identifier type.

use std::{fmt, num::ParseIntError, str::FromStr};

use thiserror::Error;

/// HTTP header name for the MCP session ID per the spec.
pub const HTTP_SESSION_ID_HEADER: &str = "mcp-session-id";

/// Unique identifier for an MCP session.
///
/// Wraps a 128-bit random value, displayed as lowercase hex.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

#[cfg(feature = "rand")]
impl rand::distr::Distribution<McpSessionId> for rand::distr::StandardUniform {
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
#[derive(Clone, Debug, Error)]
#[error("invalid session ID")]
pub struct ParseSessionIdError(#[source] ParseIntError);

impl FromStr for McpSessionId {
    type Err = ParseSessionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        u128::from_str_radix(s, 16)
            .map(McpSessionId)
            .map_err(ParseSessionIdError)
    }
}

#[cfg(test)]
mod tests {
    //! Checks session identifier wire spelling and input boundaries.

    use super::McpSessionId;

    /// Accepts hexadecimal input and emits a canonical, zero-padded identifier.
    #[test]
    fn canonical_hex_roundtrip() {
        for (input, raw, canonical) in [
            ("0", 0, "00000000000000000000000000000000"),
            ("FF", 255, "000000000000000000000000000000ff"),
            (
                "0123456789abcdef0123456789abcdef",
                0x0123456789abcdef0123456789abcdef,
                "0123456789abcdef0123456789abcdef",
            ),
            (
                "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
                u128::MAX,
                "ffffffffffffffffffffffffffffffff",
            ),
        ] {
            let id: McpSessionId = input.parse().expect("valid hexadecimal identifier");
            assert_eq!(id.as_raw(), raw);
            assert_eq!(id.to_string(), canonical);
            assert_eq!(id, McpSessionId::from_raw(raw));
            assert_eq!(
                canonical
                    .parse::<McpSessionId>()
                    .expect("canonical identifier"),
                id
            );
        }
    }

    /// Rejects malformed and overflowing identifiers.
    #[test]
    fn rejects_invalid_hex() {
        for input in ["", "not-hex", "-1", "100000000000000000000000000000000"] {
            assert!(input.parse::<McpSessionId>().is_err(), "{input}");
        }
    }
}
