//! Asynchronous stdin/stdout transport using Tokio.
//!
//! Runs an MCP server using newline-delimited JSON over stdin/stdout. This is the standard
//! transport for local MCP servers spawned as child processes.
//!
//! # Cancellation Safety
//!
//! The [`run_stdio`] function is partially cancellation-safe. If cancelled mid-write, the client
//! may receive a truncated response. However, each message is serialized to a buffer and written
//! with a single `write_all` call before flushing, which minimizes the window for partial writes.

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{McpServer, OutgoingMessage, Output, ParseError, ProtocolError, ToolRegistry};

/// Errors from the async stdio transport.
#[derive(Debug, Error)]
pub enum IoError {
    /// I/O operation failed.
    #[error("IO error")]
    Io(#[source] std::io::Error),

    /// Failed to parse incoming message.
    #[error("failed to parse message")]
    Parse(#[source] ParseError),

    /// Protocol-level error requiring connection close.
    #[error("protocol error")]
    Protocol(#[source] ProtocolError),
}

/// Runs an MCP server over stdin/stdout asynchronously.
///
/// Reads newline-delimited JSON-RPC messages from stdin and writes responses to stdout. The
/// handler is called for each tool invocation and must return an [`OutgoingMessage`] to send
/// back to the client.
///
/// Returns when stdin reaches EOF or a protocol error occurs.
///
/// # Example
///
/// ```ignore
/// use mercutio::{McpServer, io::tokio::run_stdio};
///
/// mercutio::tool_registry! {
///     enum MyTools {
///         GetWeather("get_weather", "Gets weather") { city: String },
///     }
/// }
///
/// #[tokio::main]
/// async fn main() -> Result<(), mercutio::io::tokio::IoError> {
///     let server = McpServer::<MyTools>::builder()
///         .name("my-server")
///         .version("1.0.0")
///         .build();
///
///     run_stdio(server, |tool| match tool {
///         MyTools::GetWeather(input, responder) => {
///             responder.success(format!("Weather in {}: sunny", input.city))
///         }
///     }).await
/// }
/// ```
pub async fn run_stdio<R, H>(server: McpServer<R>, handler: H) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: FnMut(R) -> OutgoingMessage,
{
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    run_on(stdin, stdout, server, handler).await
}

/// Runs an MCP server on arbitrary async buffered input/output streams.
async fn run_on<R, H, I, O>(
    mut input: I,
    mut output: O,
    mut server: McpServer<R>,
    mut handler: H,
) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: FnMut(R) -> OutgoingMessage,
    I: AsyncBufReadExt + Unpin,
    O: AsyncWriteExt + Unpin,
{
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = input.read_line(&mut line).await.map_err(IoError::Io)?;
        if bytes == 0 {
            break;
        }

        let msg = crate::parse_line(line.trim_end()).map_err(IoError::Parse)?;

        match server.handle(msg) {
            Output::Send(response) => {
                write_message(&mut output, response).await?;
            }
            Output::ToolCall(tool) => {
                let response = handler(tool);
                write_message(&mut output, response).await?;
            }
            Output::ProtocolError(e) => {
                return Err(IoError::Protocol(e));
            }
            Output::None => {}
        }
    }

    Ok(())
}

/// Writes a JSON-RPC message followed by a newline.
async fn write_message(
    w: &mut (impl AsyncWriteExt + Unpin),
    msg: OutgoingMessage,
) -> Result<(), IoError> {
    let mut json =
        serde_json::to_vec(msg.as_inner()).map_err(|e| IoError::Io(std::io::Error::other(e)))?;
    json.push(b'\n');
    w.write_all(&json).await.map_err(IoError::Io)?;
    w.flush().await.map_err(IoError::Io)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{IoError, run_on};
    use crate::{McpServer, NoTools};

    fn test_server() -> McpServer<NoTools> {
        McpServer::builder().name("test").version("1.0").build()
    }

    #[tokio::test]
    async fn full_session() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"ping"}
"#;
        let mut output = Vec::new();

        let result = run_on(
            Cursor::new(input),
            &mut output,
            test_server(),
            |_: NoTools| unreachable!("no tools"),
        )
        .await;

        assert!(result.is_ok());

        let output_str = String::from_utf8(output).expect("valid utf8");
        let lines: Vec<&str> = output_str.lines().collect();
        assert_eq!(lines.len(), 2);

        let init_response: serde_json::Value = serde_json::from_str(lines[0]).expect("valid json");
        assert_eq!(init_response["id"], 1);
        assert!(init_response["result"]["protocolVersion"].is_string());

        let ping_response: serde_json::Value = serde_json::from_str(lines[1]).expect("valid json");
        assert_eq!(ping_response["id"], 2);
    }

    #[tokio::test]
    async fn parse_error() {
        let input = "not valid json\n";
        let mut output = Vec::new();

        let result = run_on(
            Cursor::new(input),
            &mut output,
            test_server(),
            |_: NoTools| unreachable!("no tools"),
        )
        .await;

        assert!(matches!(result, Err(IoError::Parse(_))));
    }

    #[tokio::test]
    async fn protocol_error_on_unexpected_message() {
        let input = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}
"#;
        let mut output = Vec::new();

        let result = run_on(
            Cursor::new(input),
            &mut output,
            test_server(),
            |_: NoTools| unreachable!("no tools"),
        )
        .await;

        assert!(matches!(result, Err(IoError::Protocol(_))));
    }
}
