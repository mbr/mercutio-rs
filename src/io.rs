//! Optional I/O transports.
//!
//! These modules provide ready-made transport implementations for common use cases. Each transport
//! is behind its own feature flag.

use thiserror::Error;

use crate::{ParseError, ProtocolError};

#[cfg(feature = "io-stdlib")]
#[cfg_attr(docsrs, doc(cfg(feature = "io-stdlib")))]
pub mod stdlib;

#[cfg(feature = "io-tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "io-tokio")))]
pub mod tokio;

#[cfg(feature = "io-axum")]
#[cfg_attr(docsrs, doc(cfg(feature = "io-axum")))]
pub mod axum;

/// Errors from I/O transports.
#[derive(Debug, Error)]
pub enum IoError {
    /// I/O operation failed.
    #[error("IO error")]
    Io(#[source] std::io::Error),

    /// Failed to serialize outgoing message.
    #[error("failed to serialize message")]
    Serialize(#[source] serde_json::Error),

    /// Failed to parse incoming message.
    #[error("failed to parse message")]
    Parse(#[source] ParseError),

    /// Protocol-level error requiring connection close.
    #[error("protocol error")]
    Protocol(#[source] ProtocolError),
}
