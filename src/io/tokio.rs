//! Asynchronous stdin/stdout transport using Tokio.
//!
//! Runs an MCP server using newline-delimited JSON over stdin/stdout. This is the standard
//! transport for local MCP servers spawned as child processes.

use std::future::Future;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub use super::IoError;
use crate::{McpServer, OutgoingMessage, Output, ToolOutput, ToolRegistry};

/// Handles tool invocations for an MCP server.
///
/// A blanket implementation covers closures returning `Result<T, E>` where `T: Into<ToolOutput>`.
/// For async handlers with mutable state, implement this trait on a struct.
pub trait ToolHandler<R: ToolRegistry> {
    /// Error type returned by the handler.
    type Error: std::fmt::Display;

    /// Handles a tool invocation and returns the result.
    fn handle(&mut self, tool: R) -> impl Future<Output = Result<ToolOutput, Self::Error>>;
}

impl<R, F, T, E> ToolHandler<R> for F
where
    R: ToolRegistry,
    F: FnMut(R) -> Result<T, E>,
    T: Into<ToolOutput>,
    E: std::fmt::Display,
{
    type Error = E;

    async fn handle(&mut self, tool: R) -> Result<ToolOutput, E> {
        self(tool).map(Into::into)
    }
}

/// Runs an MCP server over stdin/stdout asynchronously.
///
/// Reads newline-delimited JSON-RPC messages from stdin and writes responses to stdout. The
/// handler is called for each tool invocation and must produce an [`OutgoingMessage`].
///
/// Returns when stdin reaches EOF or a protocol error occurs.
///
/// # Warning
///
/// Do not use [`println!`], [`print!`], or other stdout-writing macros inside the handler. While
/// this won't deadlock (unlike the sync version), it will corrupt the JSON-RPC protocol stream.
/// Use [`eprintln!`] for debug output.
///
/// # Cancellation Safety
///
/// This function is partially cancellation-safe. If cancelled mid-write, the client may receive a
/// truncated response. Each message is serialized to a buffer and written with a single
/// `write_all` call before flushing, which minimizes the window for partial writes.
///
/// # Example
///
/// ```no_run
/// use std::convert::Infallible;
/// use mercutio::{McpServer, ToolOutput, io::tokio::{run_stdio, ToolHandler}};
///
/// mercutio::tool_registry! {
///     enum MyTools {
///         GetWeather("get_weather", "Gets weather") { city: String },
///     }
/// }
///
/// struct Handler {
///     request_count: u32,
/// }
///
/// impl ToolHandler<MyTools> for Handler {
///     type Error = Infallible;
///
///     async fn handle(&mut self, tool: MyTools) -> Result<ToolOutput, Self::Error> {
///         self.request_count += 1;
///         match tool {
///             MyTools::GetWeather(input) => {
///                 Ok(format!("Weather in {}: sunny", input.city).into())
///             }
///         }
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
///     run_stdio(server, Handler { request_count: 0 }).await
/// }
/// ```
pub async fn run_stdio<R, H>(server: McpServer<R>, handler: H) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: ToolHandler<R>,
{
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    run_on(stdin, stdout, server, handler).await
}

/// Runs an MCP server on arbitrary async buffered input/output streams.
///
/// For most use cases, prefer [`run_stdio`] which handles stdin/stdout. Use this function for
/// custom transports or testing.
pub async fn run_on<R, H, I, O>(
    mut input: I,
    mut output: O,
    mut server: McpServer<R>,
    mut handler: H,
) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: ToolHandler<R>,
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
            Output::ToolCall { tool, responder } => {
                let response = responder.respond(handler.handle(tool).await);
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
    let mut json = serde_json::to_vec(msg.as_inner()).map_err(IoError::Serialize)?;
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
            |_: NoTools| -> Result<String, std::convert::Infallible> { unreachable!("no tools") },
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
            |_: NoTools| -> Result<String, std::convert::Infallible> { unreachable!("no tools") },
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
            |_: NoTools| -> Result<String, std::convert::Infallible> { unreachable!("no tools") },
        )
        .await;

        assert!(matches!(result, Err(IoError::Protocol(_))));
    }
}
