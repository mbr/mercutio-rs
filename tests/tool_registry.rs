//! Integration tests for the `tool_registry!` macro.

use mercutio::{McpServer, Output, ToolRegistry, parse_line};

mercutio::tool_registry! {
    enum TestTools {
        GetWeather("get_weather", "Gets weather for a city") {
            /// City name.
            city: String,
        },
        Ping("ping", "Health check") {},
    }
}

#[test]
fn macro_generates_valid_registry() {
    let definitions = TestTools::definitions();
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0].name, "get_weather");
    assert_eq!(definitions[1].name, "ping");
}

#[test]
fn macro_generated_tools_work_with_server() {
    let mut server = McpServer::<TestTools>::builder()
        .name("test")
        .version("1.0")
        .build();

    // Initialize
    let init = r#"{"jsonrpc":"2.0","id":"1","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let msg = parse_line(init).expect("valid json");
    let output = server.handle(msg);
    assert!(matches!(output, Output::Send(_)));

    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let msg = parse_line(initialized).expect("valid json");
    let _ = server.handle(msg);
    assert!(server.is_ready());

    // Call tool
    let call = r#"{"jsonrpc":"2.0","id":"2","method":"tools/call","params":{"name":"get_weather","arguments":{"city":"Berlin"}}}"#;
    let msg = parse_line(call).expect("valid json");
    let output = server.handle(msg);

    match output {
        Output::ToolCall {
            tool: TestTools::GetWeather(input),
            responder,
        } => {
            assert_eq!(input.city, "Berlin");
            let response = responder.respond("Sunny, 22C");
            assert!(matches!(
                response.as_inner(),
                mercutio::rust_mcp_schema::JsonrpcMessage::Response(_)
            ));
        }
        _ => panic!("expected ToolCall"),
    }
}

#[test]
fn server_display() {
    let server = McpServer::<TestTools>::builder()
        .name("weather-service")
        .version("1.0.0")
        .instructions("You help users check the weather.")
        .build();

    insta::assert_snapshot!(server.to_string(), @r"
    # weather-service

    Version: 1.0.0

    ## Instructions

    You help users check the weather.

    # Tools

    ## get_weather

    Gets weather for a city

    Parameters:
      city (string, required)
        City name.

    ## ping

    Health check
    ");
}
