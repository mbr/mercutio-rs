//! Tests for complete tool input schemas advertised over MCP.

use std::collections::BTreeMap;

use mercutio::{McpServer, Output, ToolDef, parse_line};
use schemars::JsonSchema;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Accepts an empty input and rejects unknown fields.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClosedInput {}

impl ToolDef for ClosedInput {
    const NAME: &'static str = "closed";
    const DESCRIPTION: &'static str = "Accepts only empty input";
}

/// Accepts an empty input while ignoring unknown fields.
#[derive(Deserialize, JsonSchema)]
struct OpenInput {}

impl ToolDef for OpenInput {
    const NAME: &'static str = "open";
    const DESCRIPTION: &'static str = "Accepts input without field restrictions";
}

/// Accepts arbitrary property names with a shared value type.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
struct MapInput<T> {
    /// Values supplied by the caller.
    #[allow(dead_code)]
    values: BTreeMap<String, T>,
}

impl<T: DeserializeOwned + JsonSchema + 'static> ToolDef for MapInput<T> {
    const NAME: &'static str = "map";
    const DESCRIPTION: &'static str = "Accepts a map of values";
}

/// Retrieves the serialized input schema through an initialized MCP connection.
fn listed_input_schema<T: ToolDef>() -> Value {
    let mut server = McpServer::<T>::builder()
        .name("schema-test")
        .version("1.0")
        .build();
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let _ = server.handle(parse_line(initialize).expect("valid initialize request"));
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let _ = server.handle(parse_line(initialized).expect("valid initialized notification"));
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let Output::Send(response) = server.handle(parse_line(list).expect("valid tools/list request"))
    else {
        panic!("expected tools/list response");
    };
    let response = serde_json::to_value(response.into_inner()).expect("serializable response");
    response["result"]["tools"][0]["inputSchema"].clone()
}

/// Preserves explicit rejection of unknown input properties.
#[test]
fn advertises_closed_input() {
    let schema = listed_input_schema::<ClosedInput>();
    assert_eq!(schema["additionalProperties"], json!(false));
}

/// Preserves schema-valued and explicitly unrestricted additional properties.
#[test]
fn advertises_map_value_schemas() {
    let strings = listed_input_schema::<MapInput<String>>();
    assert_eq!(strings["additionalProperties"], json!({"type": "string"}));

    let arbitrary = listed_input_schema::<MapInput<Value>>();
    assert_eq!(arbitrary["additionalProperties"], json!(true));
}

/// Leaves implicit additional-property behavior unchanged.
#[test]
fn does_not_invent_additional_properties() {
    let schema = listed_input_schema::<OpenInput>();
    assert_eq!(schema["type"], "object");
    assert!(schema.get("additionalProperties").is_none());
}
