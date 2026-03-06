//! Synchronous stdin/stdout transport.
//!
//! Runs an MCP server using newline-delimited JSON over stdin/stdout. This is the standard
//! transport for local MCP servers spawned as child processes.

use std::io::{BufRead, BufReader, Write};

pub use super::IoError;
use crate::{JsonRpcError, McpServer, OutgoingMessage, Output, ToolRegistry, ToolResult};

/// Runs an MCP server over stdin/stdout.
///
/// Reads newline-delimited JSON-RPC messages from stdin and writes responses to stdout. The
/// handler is called for each tool invocation and must return a [`ToolResult`] or error.
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
/// ```no_run
/// use mercutio::{McpServer, io::stdlib::run_stdio};
///
/// mercutio::tool_registry! {
///     enum MyTools {
///         GetWeather("get_weather", "Gets weather") { city: String },
///     }
/// }
///
/// fn main() -> Result<(), mercutio::io::stdlib::IoError> {
///     let server = McpServer::<MyTools>::builder()
///         .name("my-server")
///         .version("1.0.0")
///         .build();
///
///     run_stdio(server, |tool| match tool {
///         MyTools::GetWeather(input) => {
///             Ok(format!("Weather in {}: sunny", input.city).into())
///         }
///     })
/// }
/// ```
pub fn run_stdio<R, H>(server: McpServer<R>, handler: H) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: FnMut(R) -> Result<ToolResult, JsonRpcError>,
{
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    run_on(BufReader::new(stdin), stdout, server, handler)
}

/// Runs an MCP server on arbitrary buffered input/output streams.
fn run_on<R, H, I, O>(
    mut input: I,
    mut output: O,
    mut server: McpServer<R>,
    mut handler: H,
) -> Result<(), IoError>
where
    R: ToolRegistry,
    H: FnMut(R) -> Result<ToolResult, JsonRpcError>,
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

        match server.handle(msg) {
            Output::Send(response) => {
                write_message(&mut output, response)?;
            }
            Output::ToolCall { tool, responder } => {
                let response = responder.respond(handler(tool));
                write_message(&mut output, response)?;
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
fn write_message(w: &mut impl Write, msg: OutgoingMessage) -> Result<(), IoError> {
    serde_json::to_writer(&mut *w, msg.as_inner()).map_err(IoError::Serialize)?;
    w.write_all(b"\n").map_err(IoError::Io)?;
    w.flush().map_err(IoError::Io)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{IoError, run_on};
    use crate::{McpServer, NoTools};

    fn test_server() -> McpServer<NoTools> {
        McpServer::builder().name("test").version("1.0").build()
    }

    #[test]
    fn full_session() {
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
        );

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

    #[test]
    fn parse_error() {
        let input = "not valid json\n";
        let mut output = Vec::new();

        let result = run_on(
            Cursor::new(input),
            &mut output,
            test_server(),
            |_: NoTools| unreachable!("no tools"),
        );

        assert!(matches!(result, Err(IoError::Parse(_))));
    }

    #[test]
    fn protocol_error_on_unexpected_message() {
        let input = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}
"#;
        let mut output = Vec::new();

        let result = run_on(
            Cursor::new(input),
            &mut output,
            test_server(),
            |_: NoTools| unreachable!("no tools"),
        );

        assert!(matches!(result, Err(IoError::Protocol(_))));
    }
}
