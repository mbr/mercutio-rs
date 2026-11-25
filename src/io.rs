//! I/O transports for MCP servers.
//!
//! This module provides handler traits and ready-made transport implementations. The transports
//! are feature-gated:
//!
//! - **`io-stdlib`**: Synchronous stdin/stdout via `stdlib::run_stdio`. Blocking, no async
//!   runtime required.
//! - **`io-tokio`**: Async stdin/stdout via `tokio::run_stdio`. Requires Tokio runtime.
//! - **`io-axum`**: HTTP transport via `axum::mcp_router`. Implements MCP Streamable HTTP with
//!   session management.
//!
//! The handler traits [`ToolHandler`] and [`MutToolHandler`] are always available for custom
//! transport implementations.

mod session_id;

use std::{convert::Infallible, error::Error as StdError, future::Future};

use thiserror::Error;

pub use self::session_id::{HTTP_SESSION_ID_HEADER, McpSessionId, ParseSessionIdError};
use crate::{ParseError, ProtocolError, ToolOutput, ToolRegistry};

/// Extracts application context once before processing a stdio stream.
///
/// May read configuration, but must not consume stdin or write stdout.
/// Use `run_on_with_context` to supply an already constructed context instead.
pub trait FromStdio: Sized {
    /// Error returned when startup context cannot be constructed.
    type Error: StdError + Send + Sync + 'static;

    /// Constructs context for this stdio invocation.
    fn from_stdio() -> Result<Self, Self::Error>;
}

impl FromStdio for () {
    type Error = Infallible;

    fn from_stdio() -> Result<Self, Self::Error> {
        Ok(())
    }
}

/// Handles tool invocations with explicit application context.
///
/// Closures receive `(&C, Option<McpSessionId>, R)` and return a result future.
/// Copy or clone borrowed context data before returning an async block;
/// implementations of this trait can borrow context across awaits directly.
pub trait ToolHandler<R: ToolRegistry, C = ()>: Send + Sync {
    /// Error type returned by the handler.
    type Error: std::fmt::Display;

    /// Handles a tool invocation and returns the result.
    fn handle(
        &self,
        context: &C,
        session_id: Option<McpSessionId>,
        tool: R,
    ) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send;
}

impl<R, C, F, Fut, T, E> ToolHandler<R, C> for F
where
    R: ToolRegistry + Send,
    C: Sync,
    F: Fn(&C, Option<McpSessionId>, R) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, E>> + Send,
    T: Into<ToolOutput>,
    E: std::fmt::Display,
{
    type Error = E;

    async fn handle(
        &self,
        context: &C,
        session_id: Option<McpSessionId>,
        tool: R,
    ) -> Result<ToolOutput, E> {
        self(context, session_id, tool).await.map(Into::into)
    }
}

/// Handles tool invocations in exclusive-access contexts.
///
/// Every [`ToolHandler`] is automatically a `MutToolHandler` via blanket impl.
pub trait MutToolHandler<R: ToolRegistry, C = ()> {
    /// Error type returned by the handler.
    type Error: std::fmt::Display;

    /// Handles a tool invocation and returns the result.
    fn handle(
        &mut self,
        context: &C,
        session_id: Option<McpSessionId>,
        tool: R,
    ) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send;
}

/// Every [`ToolHandler`] is automatically a [`MutToolHandler`].
impl<R: ToolRegistry, C, T: ToolHandler<R, C>> MutToolHandler<R, C> for T {
    type Error = <T as ToolHandler<R, C>>::Error;

    fn handle(
        &mut self,
        context: &C,
        session_id: Option<McpSessionId>,
        tool: R,
    ) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send {
        ToolHandler::handle(self, context, session_id, tool)
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
    /// Application context could not be extracted before processing input.
    #[error("failed to extract stdio context")]
    Context(#[source] Box<dyn StdError + Send + Sync>),

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
