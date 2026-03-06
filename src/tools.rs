//! Tool registration types and macros.
//!
//! Provides type-safe tool registration for MCP servers. The [`tool_registry!`] macro generates
//! input structs, a dispatch enum, and [`ToolRegistry`] implementation from a single declaration.

use std::collections::HashMap;

use rust_mcp_schema::{CallToolResult, ContentBlock, ToolInputSchema};
use serde::Serialize;

use crate::JsonRpcError;

/// Defines a tool's input type and metadata.
///
/// Implement this trait on tool input structs to associate them with MCP metadata. The
/// [`tool_registry`](crate::tool_registry) macro generates this implementation automatically.
pub trait ToolDef: schemars::JsonSchema + serde::de::DeserializeOwned + 'static {
    /// Tool name as it appears in the MCP protocol.
    const NAME: &'static str;
    /// Human-readable description of what the tool does.
    const DESCRIPTION: &'static str;
}

/// Successful output from a tool invocation.
///
/// Provides a builder API and ergonomic conversions for constructing tool output. For domain
/// errors (tool ran but failed), return an `Err` from your handler instead of using this type.
///
/// # Simple Text
///
/// Return a text response (accepts `&str`, [`String`], or [`format!`] results):
/// ```ignore
/// Ok("Operation completed")
/// Ok(format!("Found {} items", count))
/// ```
///
/// # Structured JSON
///
/// Return structured data with [`ToolOutput::json`]. This sets `structuredContent` and adds
/// the JSON as escaped text for backwards compatibility (per MCP spec):
/// ```ignore
/// Ok(ToolOutput::json(&weather_data))
/// ```
///
/// # Multiple Content Blocks
///
/// Combine multiple content blocks using the builder:
/// ```ignore
/// Ok(ToolOutput::new()
///     .text("Query results:")
///     .text(format!("Found {} matches", results.len())))
/// ```
#[derive(Debug, Default)]
pub struct ToolOutput {
    /// Content blocks.
    content: Vec<ContentBlock>,
    /// Structured content.
    structured_content: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ToolOutput {
    /// Creates an empty output for building.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates output with structured JSON content and text representation.
    ///
    /// Sets `structuredContent` and adds the JSON as escaped text to `content` for backwards
    /// compatibility (per MCP spec recommendation).
    ///
    /// # Panics
    ///
    /// Will panic if serialization of `T` fails.
    pub fn json<T: Serialize>(value: &T) -> Self {
        let text = serde_json::to_string(value).expect("serialization failed");
        Self::new().text(text).structured(value)
    }

    /// Adds a text content block.
    pub fn text<I: Into<String>>(mut self, text: I) -> Self {
        self.content.push(ContentBlock::text_content(text.into()));
        self
    }

    /// Sets structured content.
    ///
    /// Sets `structuredContent` to the serialized value. Does not modify `content`; use
    /// [`Self::json`] or add a text block manually for backwards compatibility.
    pub fn structured<T: Serialize>(mut self, value: &T) -> Self {
        let json_value = serde_json::to_value(value).expect("serialization failed");
        if let serde_json::Value::Object(map) = json_value {
            self.structured_content = Some(map);
        }
        self
    }

    /// Converts to the MCP [`CallToolResult`] with the given error flag.
    fn into_call_result(self, is_error: bool) -> CallToolResult {
        CallToolResult {
            content: self.content,
            is_error: Some(is_error),
            structured_content: self.structured_content,
            meta: None,
        }
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::new().text(text)
    }
}

impl From<&str> for ToolOutput {
    fn from(text: &str) -> Self {
        Self::new().text(text)
    }
}

/// Converts a value into a tool response ([`CallToolResult`]).
///
/// This trait enables [`Responder::respond`](crate::Responder::respond) to accept both direct
/// values and `Result` types:
///
/// - **Direct values** (`String`, `&str`, [`ToolOutput`]): Converted to a successful response
///   with `is_error: false`.
/// - **`Result<T, E>`**: `Ok(v)` becomes a successful response; `Err(e)` becomes a domain error
///   response with `is_error: true` and the error's display text as content.
pub trait IntoToolResponse {
    /// Converts this value into a [`CallToolResult`].
    fn into_tool_response(self) -> CallToolResult;
}

impl IntoToolResponse for ToolOutput {
    fn into_tool_response(self) -> CallToolResult {
        self.into_call_result(false)
    }
}

impl IntoToolResponse for String {
    fn into_tool_response(self) -> CallToolResult {
        ToolOutput::from(self).into_call_result(false)
    }
}

impl IntoToolResponse for &str {
    fn into_tool_response(self) -> CallToolResult {
        ToolOutput::from(self).into_call_result(false)
    }
}

impl<T, E> IntoToolResponse for Result<T, E>
where
    T: Into<ToolOutput>,
    E: std::fmt::Display,
{
    fn into_tool_response(self) -> CallToolResult {
        match self {
            Ok(v) => v.into().into_call_result(false),
            Err(e) => ToolOutput::new().text(e.to_string()).into_call_result(true),
        }
    }
}

/// MCP tool definition for `tools/list` responses.
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON Schema for the input parameters.
    pub input_schema: ToolInputSchema,
}

impl ToolDefinition {
    /// Creates a definition from a type implementing [`ToolDef`].
    pub fn from_tool<T: ToolDef>() -> Self {
        let schema = schemars::schema_for!(T);
        let json = serde_json::to_value(&schema).expect("schema serialization failed");
        let input_schema = convert_schema_to_tool_input(&json);
        Self {
            name: T::NAME.to_string(),
            description: T::DESCRIPTION.to_string(),
            input_schema,
        }
    }

    /// Converts to the MCP schema [`Tool`](rust_mcp_schema::Tool) type.
    pub fn into_mcp_tool(self) -> rust_mcp_schema::Tool {
        rust_mcp_schema::Tool {
            name: self.name,
            description: Some(self.description),
            input_schema: self.input_schema,
            annotations: None,
            meta: None,
            output_schema: None,
            title: None,
        }
    }
}

/// Converts a schemars JSON Schema to MCP's [`ToolInputSchema`].
///
/// MCP tools use standard JSON Schema for `inputSchema`. We use `schemars` to derive schemas from
/// Rust types, but [`ToolInputSchema`] only models `properties` and `required`, discarding metadata
/// like `$schema`, `title`, and `definitions`. This breaks nested struct types since schemars emits
/// `$ref` pointers into the discarded `definitions`. Workaround: annotate nested types with
/// `#[schemars(inline)]` to force inlining, or keep tool inputs flat.
fn convert_schema_to_tool_input(schema: &serde_json::Value) -> ToolInputSchema {
    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let properties = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    let map = v.as_object().cloned().unwrap_or_default();
                    (k.clone(), map)
                })
                .collect::<HashMap<_, _>>()
        });

    ToolInputSchema::new(required, properties)
}

/// Registry of available tools.
///
/// Implemented by enums representing the set of tools a server supports. Each variant
/// corresponds to a tool and contains its parsed input. The
/// [`tool_registry`](crate::tool_registry) macro generates this implementation automatically.
pub trait ToolRegistry: Sized {
    /// Whether tools are enabled. Used to advertise tool capabilities during init.
    const ENABLED: bool = true;

    /// Parses a tool call into a typed enum variant.
    fn parse(name: &str, arguments: serde_json::Value) -> std::result::Result<Self, JsonRpcError>;

    /// Returns tool definitions for `tools/list`.
    fn definitions() -> Vec<ToolDefinition>;
}

/// Empty tool registry for servers that don't expose tools.
#[derive(Debug)]
pub enum NoTools {}

impl ToolRegistry for NoTools {
    const ENABLED: bool = false;

    fn parse(name: &str, _arguments: serde_json::Value) -> std::result::Result<Self, JsonRpcError> {
        Err(JsonRpcError::MethodNotFound {
            msg: format!("unknown tool: {name}"),
        })
    }

    fn definitions() -> Vec<ToolDefinition> {
        vec![]
    }
}

/// Generates tool input structs, a dispatch enum, and [`ToolRegistry`] implementation.
///
/// Doc comments (`///`) on struct fields become JSON Schema descriptions, which are sent to
/// clients during `tools/list` and help the LLM understand how to use each parameter.
///
/// # Nested Types
///
/// Field types that are custom structs must be annotated with `#[schemars(inline)]`, otherwise
/// the generated JSON Schema will contain unresolved `$ref` pointers. Enums and primitive types
/// work without this annotation.
///
/// ```ignore
/// #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
/// #[schemars(inline)]  // Required for nested struct types
/// struct Location {
///     city: String,
///     country: String,
/// }
/// ```
///
/// # Example
///
/// ```ignore
/// tool_registry! {
///     enum MyTools {
///         GetWeather("get_weather", "Gets weather for a city") {
///             /// City name, e.g. "San Francisco"
///             city: String,
///             /// Temperature units (celsius or fahrenheit)
///             units: Option<Units>,
///         },
///
///         SetReminder("set_reminder", "Sets a reminder") {
///             /// Reminder text
///             text: String,
///         },
///     }
/// }
/// ```
#[macro_export]
macro_rules! tool_registry {
    (
        enum $enum_name:ident {
            $(
                $variant:ident($tool_name:literal, $description:literal) {
                    $(
                        $(#[$field_meta:meta])*
                        $field_name:ident : $field_type:ty
                    ),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        $(
            #[doc = concat!("Input parameters for the `", $tool_name, "` tool.")]
            #[derive(Debug, $crate::schemars::JsonSchema, $crate::serde::Deserialize)]
            #[schemars(crate = "::mercutio::schemars")]
            #[serde(crate = "::mercutio::serde")]
            pub struct $variant {
                $(
                    $(#[$field_meta])*
                    pub $field_name: $field_type,
                )*
            }

            impl $crate::ToolDef for $variant {
                const NAME: &'static str = $tool_name;
                const DESCRIPTION: &'static str = $description;
            }
        )*

        #[doc = concat!("Tool dispatch enum for this server.")]
        pub enum $enum_name {
            $(
                #[doc = concat!("The `", $tool_name, "` tool.")]
                $variant($variant),
            )*
        }

        impl $crate::ToolRegistry for $enum_name {
            fn parse(
                name: &str,
                arguments: $crate::serde_json::Value,
            ) -> std::result::Result<Self, $crate::JsonRpcError> {
                match name {
                    $(
                        $tool_name => {
                            let input: $variant = $crate::serde_json::from_value(arguments)
                                .map_err(|e| $crate::JsonRpcError::InvalidParams {
                                    msg: format!("{}: {}", $tool_name, e),
                                })?;
                            Ok(Self::$variant(input))
                        }
                    )*
                    _ => Err($crate::JsonRpcError::MethodNotFound {
                        msg: format!("unknown tool: {name}"),
                    }),
                }
            }

            fn definitions() -> Vec<$crate::ToolDefinition> {
                vec![
                    $(
                        $crate::ToolDefinition::from_tool::<$variant>(),
                    )*
                ]
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::{IntoToolResponse, NoTools, ToolDefinition, ToolOutput, ToolRegistry};
    use crate::JsonRpcError;

    #[test]
    fn no_tools_definitions_empty() {
        assert!(NoTools::definitions().is_empty());
    }

    #[test]
    fn no_tools_parse_returns_error() {
        let result = NoTools::parse("anything", serde_json::Value::Null);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, JsonRpcError::MethodNotFound { .. }));
    }

    #[test]
    fn tool_definition_from_tool() {
        #[allow(dead_code)]
        #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
        struct TestInput {
            value: String,
        }

        impl super::ToolDef for TestInput {
            const NAME: &'static str = "test_tool";
            const DESCRIPTION: &'static str = "A test tool";
        }

        let def = ToolDefinition::from_tool::<TestInput>();
        assert_eq!(def.name, "test_tool");
        assert_eq!(def.description, "A test tool");
        assert_eq!(def.input_schema.type_(), "object");
        assert!(def.input_schema.properties.is_some());
    }

    #[test]
    fn field_docstrings_become_schema_descriptions() {
        #[allow(dead_code)]
        #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
        struct TestInput {
            /// The city to look up.
            city: String,
            /// Temperature unit preference.
            units: Option<String>,
        }

        impl super::ToolDef for TestInput {
            const NAME: &'static str = "test";
            const DESCRIPTION: &'static str = "Test";
        }

        let def = ToolDefinition::from_tool::<TestInput>();
        let props = def.input_schema.properties.expect("properties");
        let city_prop = props.get("city").expect("city property");
        let city_desc = city_prop.get("description").and_then(|v| v.as_str());
        assert_eq!(city_desc, Some("The city to look up."));

        let units_prop = props.get("units").expect("units property");
        let units_desc = units_prop.get("description").and_then(|v| v.as_str());
        assert_eq!(units_desc, Some("Temperature unit preference."));
    }

    #[test]
    fn tool_output_from_string() {
        let result = "hello".into_tool_response();
        let json = serde_json::to_value(&result).expect("serialize");
        let content = json.get("content").expect("content field");
        assert!(content.is_array());
        assert_eq!(content.as_array().expect("array").len(), 1);
        assert_eq!(json.get("isError").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn tool_output_from_owned_string() {
        let result = String::from("hello").into_tool_response();
        let json = serde_json::to_value(&result).expect("serialize");
        let content = json.get("content").expect("content field");
        assert!(content.is_array());
    }

    #[test]
    fn tool_output_json_sets_structured_content() {
        #[derive(serde::Serialize)]
        struct Data {
            value: i32,
        }
        let result = ToolOutput::json(&Data { value: 42 }).into_tool_response();
        let json = serde_json::to_value(&result).expect("serialize");
        assert!(json.get("structuredContent").is_some());
        assert!(json.get("content").expect("content").is_array());
    }

    #[test]
    fn result_err_sets_is_error() {
        let result: Result<String, &str> = Err("something failed");
        let json = serde_json::to_value(&result.into_tool_response()).expect("serialize");
        assert_eq!(json.get("isError").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn result_ok_sets_is_error_false() {
        let result: Result<&str, &str> = Ok("success");
        let json = serde_json::to_value(&result.into_tool_response()).expect("serialize");
        assert_eq!(json.get("isError").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn tool_output_builder_multiple_text_blocks() {
        let result = ToolOutput::new()
            .text("first")
            .text("second")
            .into_tool_response();
        let json = serde_json::to_value(&result).expect("serialize");
        let content = json.get("content").expect("content");
        assert_eq!(content.as_array().expect("array").len(), 2);
    }

    #[test]
    fn tool_output_builder_text_and_structured() {
        #[derive(serde::Serialize)]
        struct Data {
            value: i32,
        }
        let result = ToolOutput::new()
            .text("summary")
            .structured(&Data { value: 1 })
            .into_tool_response();
        let json = serde_json::to_value(&result).expect("serialize");
        assert!(json.get("structuredContent").is_some());
        assert_eq!(
            json.get("content")
                .expect("content")
                .as_array()
                .expect("array")
                .len(),
            1
        );
    }
}
