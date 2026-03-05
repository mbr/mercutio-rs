//! Synchronous stdin/stdout transport.
//!
//! Runs an MCP server using newline-delimited JSON over stdin/stdout. This is the standard
//! transport for local MCP servers spawned as child processes.
//!
//! # Deadlock Warning
//!
//! This function locks stdout for the duration of the call. Using [`println!`] or other macros
//! that lock stdout inside the handler will deadlock. Use [`eprintln!`] for debug output.

use std::io::{BufRead, BufReader, Write};

use thiserror::Error;

use crate::{McpServer, OutgoingMessage, Output, ParseError, ProtocolError, ToolRegistry};

/// Errors from the stdio transport.
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

impl From<std::io::Error> for IoError {
    fn from(err: std::io::Error) -> Self {
        IoError::Io(err)
    }
}

impl From<ParseError> for IoError {
    fn from(err: ParseError) -> Self {
        IoError::Parse(err)
    }
}

impl From<ProtocolError> for IoError {
    fn from(err: ProtocolError) -> Self {
        IoError::Protocol(err)
    }
}

/// Runs an MCP server over stdin/stdout.
///
/// Reads newline-delimited JSON-RPC messages from stdin and writes responses to stdout. The
/// handler is called for each tool invocation and must return an [`OutgoingMessage`] to send
/// back to the client.
///
/// Returns when stdin reaches EOF or a protocol error occurs.
///
/// # Deadlock Warning
///
/// Stdout is locked for the duration of this call. Using [`println!`], [`print!`], or other
/// stdout-locking macros inside the handler will deadlock. Use [`eprintln!`] for debug output.
///
/// # Example
///
/// ```ignore
/// mercutio::tool_registry! {
///     enum MyTools {
///         GetWeather("get_weather", "Gets weather") { city: String },
///     }
/// }
///
/// let server = McpServer::<MyTools>::builder()
///     .name("my-server")
///     .version("1.0.0")
///     .build();
///
/// run_stdio(server, |tool| match tool {
///     MyTools::GetWeather(input, responder) => {
///         responder.success(format!("Weather in {}: sunny", input.city))
///     }
/// })?;
/// ```
pub fn run_stdio<R, H>(mut server: McpServer<R>, mut handler: H) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: FnMut(R) -> OutgoingMessage,
{
    let stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }

        let msg = crate::parse_line(line.trim_end())?;

        match server.handle(msg) {
            Output::Send(response) => {
                write_message(&mut stdout, response)?;
            }
            Output::ToolCall(tool) => {
                let response = handler(tool);
                write_message(&mut stdout, response)?;
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
fn write_message(w: &mut impl Write, msg: OutgoingMessage) -> std::io::Result<()> {
    serde_json::to_writer(&mut *w, msg.as_inner())?;
    w.write_all(b"\n")?;
    w.flush()
}
