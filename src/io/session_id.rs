//! MCP session identifier type.

use std::{fmt, num::ParseIntError, str::FromStr};

use thiserror::Error;

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

    #[cfg(feature = "rand")]
    #[test]
    fn random_generation() {
        use rand::Rng;
        let mut rng = rand::rng();
        let id: McpSessionId = rng.random();
        assert_eq!(id.to_string().len(), 32);
    }
}
