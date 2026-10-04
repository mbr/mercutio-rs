//! Synchronous stdin/stdout transport.
//!
//! Runs an MCP server using newline-delimited JSON over stdin/stdout. This is the standard
//! transport for local MCP servers spawned as child processes.
//!
//! # Example
//!
//! ```no_run
//! use std::convert::Infallible;
//! use mercutio::{McpServer, io::stdlib::run_stdio};
//!
//! mercutio::tool_registry! {
//!     enum MyTools {
//!         GetWeather("get_weather", "Gets weather") { city: String },
//!     }
//! }
//!
//! fn main() -> Result<(), mercutio::io::stdlib::IoError> {
//!     let server = McpServer::<MyTools>::builder()
//!         .name("my-server")
//!         .version("1.0.0")
//!         .build();
//!
//!     run_stdio(server, |_session_id, tool| -> Result<String, Infallible> {
//!         match tool {
//!             MyTools::GetWeather(input) => {
//!                 Ok(format!("Weather in {}: sunny", input.city))
//!             }
//!         }
//!     })
//! }
//! ```

use std::io::{BufRead, BufReader, Write};

pub use super::IoError;
use super::McpSessionId;
use crate::{McpServer, OutgoingMessage, Output, ToolOutput, ToolRegistry};

/// Runs an MCP server over stdin/stdout.
///
/// Reads newline-delimited JSON-RPC messages from stdin and writes responses to stdout. Returns
/// when stdin reaches EOF or a protocol error occurs. See the [module documentation](self) for a
/// complete example.
///
/// # Deadlock Warning
///
/// Stdout is locked for the duration of this call. Using [`println!`] or other stdout-locking
/// macros inside the handler will deadlock.
pub fn run_stdio<R, H, T, E>(server: McpServer<R>, handler: H) -> Result<(), IoError>
where
    R: ToolRegistry,
    T: Into<ToolOutput>,
    E: std::fmt::Display,
    H: FnMut(Option<McpSessionId>, R) -> Result<T, E>,
{
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    run_on(BufReader::new(stdin), stdout, server, handler)
}

/// Runs an MCP server on arbitrary buffered input/output streams.
///
/// For most use cases, prefer [`run_stdio`] which handles stdin/stdout. Use this function for
/// custom transports or testing.
pub fn run_on<R, H, T, E, I, O>(
    mut input: I,
    mut output: O,
    mut server: McpServer<R>,
    mut handler: H,
) -> Result<(), IoError>
where
    R: ToolRegistry,
    T: Into<ToolOutput>,
    E: std::fmt::Display,
    H: FnMut(Option<McpSessionId>, R) -> Result<T, E>,
    I: BufRead,
    O: Write,
{
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = input.read_line(&mut line).map_err(IoError::Io)?;
        if bytes == 0 {
            break;
        }

        let msg = crate::parse_line(line.trim_end()).map_err(IoError::Parse)?;

        let response = match server.handle(msg) {
            Output::Send(response) => response,
            Output::ToolCall { tool, responder } => responder.respond(handler(None, tool)),
            Output::ProtocolError(error) => return Err(IoError::Protocol(error)),
            Output::None => continue,
        };
        write_message(&mut output, response)?;
    }

    Ok(())
}

/// Writes a JSON-RPC message followed by a newline.
fn write_message(w: &mut impl Write, msg: OutgoingMessage) -> Result<(), IoError> {
    serde_json::to_writer(&mut *w, msg.as_inner()).map_err(IoError::Serialize)?;
    w.write_all(b"\n").map_err(IoError::Io)?;
    w.flush().map_err(IoError::Io)
}
