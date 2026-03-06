//! I/O transports for MCP servers.
//!
//! This module provides handler traits and ready-made transport implementations. The transports
//! are feature-gated:
//!
//! - **`io-stdlib`**: Synchronous stdin/stdout via [`stdlib::run_stdio`]. Blocking, no async
//!   runtime required.
//! - **`io-tokio`**: Async stdin/stdout via [`tokio::run_stdio`]. Requires Tokio runtime.
//! - **`io-axum`**: HTTP transport via [`axum::mcp_router`]. Implements MCP Streamable HTTP with
//!   session management.
//!
//! The handler traits [`ToolHandler`] and [`MutToolHandler`] are always available for custom
//! transport implementations.

use std::future::Future;

use thiserror::Error;

use crate::{ParseError, ProtocolError, ToolOutput, ToolRegistry};

/// Handles tool invocations in concurrent contexts.
///
/// Blanket impl for `Fn(R) -> impl Future<Output = Result<T, E>>`.
pub trait ToolHandler<R: ToolRegistry>: Send + Sync {
    /// Error type returned by the handler.
    type Error: std::fmt::Display;

    /// Handles a tool invocation and returns the result.
    fn handle(&self, tool: R) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send;
}

impl<R, F, Fut, T, E> ToolHandler<R> for F
where
    R: ToolRegistry + Send,
    F: Fn(R) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, E>> + Send,
    T: Into<ToolOutput>,
    E: std::fmt::Display,
{
    type Error = E;

    async fn handle(&self, tool: R) -> Result<ToolOutput, E> {
        self(tool).await.map(Into::into)
    }
}

/// Handles tool invocations in exclusive-access contexts.
///
/// Every [`ToolHandler`] is automatically a `MutToolHandler` via blanket impl.
pub trait MutToolHandler<R: ToolRegistry> {
    /// Error type returned by the handler.
    type Error: std::fmt::Display;

    /// Handles a tool invocation and returns the result.
    fn handle(&mut self, tool: R) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send;
}

/// Every [`ToolHandler`] is automatically a [`MutToolHandler`].
impl<R: ToolRegistry, T: ToolHandler<R>> MutToolHandler<R> for T {
    type Error = <T as ToolHandler<R>>::Error;

    fn handle(&mut self, tool: R) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send {
        ToolHandler::handle(self, tool)
    }
}

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
