//! Asynchronous stdin/stdout transport using Tokio.
//!
//! Runs an MCP server using newline-delimited JSON over stdin/stdout. This is the standard
//! transport for local MCP servers spawned as child processes.
//!
//! # Example
//!
//! ```no_run
//! use std::convert::Infallible;
//! use mercutio::{McpServer, ToolOutput, io::tokio::{run_stdio, MutToolHandler}};
//!
//! mercutio::tool_registry! {
//!     enum MyTools {
//!         GetWeather("get_weather", "Gets weather") { city: String },
//!     }
//! }
//!
//! struct Handler {
//!     request_count: u32,
//! }
//!
//! impl MutToolHandler<MyTools> for Handler {
//!     type Error = Infallible;
//!
//!     async fn handle(
//!         &mut self,
//!         _session_id: Option<mercutio::io::McpSessionId>,
//!         tool: MyTools,
//!     ) -> Result<ToolOutput, Self::Error> {
//!         self.request_count += 1;
//!         match tool {
//!             MyTools::GetWeather(input) => {
//!                 Ok(format!("Weather in {}: sunny", input.city).into())
//!             }
//!         }
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), mercutio::io::tokio::IoError> {
//!     let server = McpServer::<MyTools>::builder()
//!         .name("my-server")
//!         .version("1.0.0")
//!         .build();
//!
//!     run_stdio(server, Handler { request_count: 0 }).await
//! }
//! ```

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub use super::{IoError, MutToolHandler, ToolHandler};
use crate::{McpServer, OutgoingMessage, Output, ToolRegistry};

/// Runs an MCP server over stdin/stdout asynchronously.
///
/// Reads newline-delimited JSON-RPC messages from stdin and writes responses to stdout. Returns
/// when stdin reaches EOF or a protocol error occurs. See the [module documentation](self) for a
/// complete example.
///
/// # Warning
///
/// Do not use [`println!`] or other stdout-writing macros inside the handler - this will corrupt
/// the JSON-RPC protocol stream.
///
/// # Cancellation Safety
///
/// Partially cancellation-safe. If cancelled mid-write, the client may receive a truncated
/// response.
pub async fn run_stdio<R, H>(server: McpServer<R>, handler: H) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: MutToolHandler<R>,
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
    H: MutToolHandler<R>,
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

        let response = match server.handle(msg) {
            Output::Send(response) => response,
            Output::ToolCall { tool, responder } => {
                responder.respond(handler.handle(None, tool).await)
            }
            Output::ProtocolError(error) => return Err(IoError::Protocol(error)),
            Output::None => continue,
        };
        write_message(&mut output, response).await?;
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
