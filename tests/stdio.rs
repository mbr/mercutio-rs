//! Shared wire-contract tests for the synchronous and asynchronous stdio transports.

#![cfg(any(feature = "io-stdlib", feature = "io-tokio"))]

use std::io::Cursor;

use mercutio::{
    McpServer,
    io::{IoError, McpSessionId},
};
use serde_json::{Value, json};

mercutio::tool_registry! {
    enum TestTools {
        Echo("echo", "Echoes input") { value: String },
        Fail("fail", "Returns a domain error") {},
    }
}

/// Expected transport outcome for an input transcript.
enum Expected {
    /// A complete session including successful and unsuccessful tool calls.
    Session,
    /// Invalid JSON terminates the transport.
    ParseError,
    /// A handshake violation terminates the transport.
    ProtocolError,
}

/// Returns the same session and failure cases for both transport implementations.
fn cases() -> [(&'static str, Expected); 3] {
    [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"value":"hello"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fail"}}
{"jsonrpc":"2.0","id":4,"method":"ping"}"#,
            Expected::Session,
        ),
        ("not valid json\n", Expected::ParseError),
        (
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            Expected::ProtocolError,
        ),
    ]
}

/// Requires stdio's session-free handler context and exercises domain errors.
fn handle(session: Option<McpSessionId>, tool: TestTools) -> Result<String, &'static str> {
    assert!(session.is_none());
    match tool {
        TestTools::Echo(input) => Ok(input.value),
        TestTools::Fail(_) => Err("domain failure"),
    }
}

/// Checks framing, response content, continued operation after tool errors, and fatal errors.
fn assert_output(expected: Expected, result: Result<(), IoError>, output: &[u8]) {
    match expected {
        Expected::ParseError => assert!(matches!(result, Err(IoError::Parse(_)))),
        Expected::ProtocolError => assert!(matches!(result, Err(IoError::Protocol(_)))),
        Expected::Session => {
            result.expect("session completed at EOF");
            assert!(output.ends_with(b"\n"));
            let output = std::str::from_utf8(output).expect("UTF-8 output");
            let responses: Vec<Value> = output
                .lines()
                .map(|line| serde_json::from_str(line).expect("one JSON response per line"))
                .collect();
            assert_eq!(responses.len(), 4);
            assert_eq!(responses[0]["id"], 1);
            assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
            assert_eq!(
                responses[1..],
                [
                    json!({"jsonrpc": "2.0", "id": 2, "result": {
                        "content": [{"type": "text", "text": "hello"}], "isError": false
                    }}),
                    json!({"jsonrpc": "2.0", "id": 3, "result": {
                        "content": [{"type": "text", "text": "domain failure"}], "isError": true
                    }}),
                    json!({"jsonrpc": "2.0", "id": 4, "result": {}}),
                ]
            );
            return;
        }
    }
    assert!(
        output.is_empty(),
        "fatal input errors must not produce a response"
    );
}

/// Exercises the synchronous transport through its public stream API.
#[cfg(feature = "io-stdlib")]
#[test]
fn synchronous_stdio_contract() {
    for (input, expected) in cases() {
        let mut output = Vec::new();
        let result = mercutio::io::stdlib::run_on(
            Cursor::new(input),
            &mut output,
            McpServer::builder().build(),
            handle,
        );
        assert_output(expected, result, &output);
    }
}

/// Exercises the asynchronous transport through its public stream API.
#[cfg(feature = "io-tokio")]
#[tokio::test]
async fn asynchronous_stdio_contract() {
    for (input, expected) in cases() {
        let mut output = Vec::new();
        let result = mercutio::io::tokio::run_on(
            Cursor::new(input),
            &mut output,
            McpServer::builder().build(),
            |session, tool| async move { handle(session, tool) },
        )
        .await;
        assert_output(expected, result, &output);
    }
}
