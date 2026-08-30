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

fn initialized_server() -> McpServer<TestTools> {
    let mut server = McpServer::<TestTools>::builder()
        .name("test")
        .version("1.0")
        .build();

    let init = r#"{"jsonrpc":"2.0","id":"1","method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let Output::Send(response) = server.handle(parse_line(init).expect("valid initialize request"))
    else {
        panic!("expected initialize response");
    };
    let response = serde_json::to_value(response.into_inner()).expect("serializable response");
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");

    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let output = server.handle(parse_line(initialized).expect("valid initialized notification"));
    assert!(matches!(output, Output::None));
    assert!(server.is_ready());

    server
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
    let mut server = initialized_server();
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
                mercutio::rust_mcp_schema::JsonrpcMessage::ResultResponse(_)
            ));
        }
        _ => panic!("expected ToolCall"),
    }

    let call = r#"{"jsonrpc":"2.0","id":"3","method":"tools/call","params":{"name":"ping"}}"#;
    let output = server.handle(parse_line(call).expect("valid argument-free tool request"));
    assert!(matches!(
        output,
        Output::ToolCall {
            tool: TestTools::Ping(_),
            ..
        }
    ));
}

#[test]
fn unknown_tool_is_invalid_params_error() {
    let mut server = initialized_server();

    let call = r#"{"jsonrpc":"2.0","id":"2","method":"tools/call","params":{"name":"missing"}}"#;
    let Output::Send(response) = server.handle(parse_line(call).expect("valid tool request"))
    else {
        panic!("expected protocol error response");
    };
    let response = serde_json::to_value(response.into_inner()).expect("serializable response");

    assert_eq!(response["error"]["code"], -32602);
    assert!(response.get("result").is_none());
}

#[test]
fn invalid_tool_input_is_execution_error() {
    let mut server = initialized_server();

    let call = r#"{"jsonrpc":"2.0","id":"2","method":"tools/call","params":{"name":"get_weather","arguments":{}}}"#;
    let Output::Send(response) = server.handle(parse_line(call).expect("valid tool request"))
    else {
        panic!("expected tool error response");
    };
    let response = serde_json::to_value(response.into_inner()).expect("serializable response");

    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("text error content")
            .contains("city")
    );
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
