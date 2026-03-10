//! Tool registration types and macros.
//!
//! Provides type-safe tool registration for MCP servers. The [`tool_registry!`] macro generates
//! input structs, a dispatch enum, and [`ToolRegistry`] implementation from a single declaration.
//!
//! # Tool Output Format
//!
//! Prefer plain text over JSON for content the LLM will reason about. Research shows JSON-mode
//! degrades LLM reasoning performance (see <https://arxiv.org/abs/2408.02442>). Use JSON
//! ([`ToolOutput::json`]) only when the output needs programmatic parsing downstream. For tool
//! results the LLM will interpret and relay to users, return human-readable text:
//!
//! ```ignore
//! // Good: readable text the LLM can reason about
//! Ok(format!("Temperature: {}F\nConditions: {}", temp, conditions))
//!
//! // Avoid: JSON for LLM consumption
//! Ok(ToolOutput::json(&WeatherData { temp, conditions }))
//! ```
//!
//! # Snapshot Testing
//!
//! Both [`ToolDefinitions`] and [`ToolOutput`] implement [`Display`] for snapshot testing with
//! `insta`. Use this to verify tool schemas and outputs:
//!
//! ```ignore
//! #[test]
//! fn tool_schemas_are_stable() {
//!     insta::assert_snapshot!(MyTools::definitions().to_string(), @r"
//!     # Tools
//!
//!     ## get_weather
//!     ...
//!     ");
//! }
//!
//! #[test]
//! fn weather_output_format() {
//!     let output = get_weather("Berlin").await?;
//!     insta::assert_snapshot!(output.to_string(), @r"
//!     Temperature: 72F
//!
//!     Conditions: Sunny
//!     ");
//! }
//! ```

use std::{collections::HashMap, fmt, ops::Index};

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
/// # Text vs JSON
///
/// Prefer plain text for tool outputs the LLM will reason about. Research shows JSON-mode
/// degrades reasoning performance (see [module docs](self) for details). Reserve
/// [`ToolOutput::json`] for data that needs programmatic parsing downstream.
///
/// ```ignore
/// // Recommended: human-readable text
/// Ok(format!("Temperature: {}F\nConditions: {}", temp, conditions))
///
/// // Use only when structured parsing is needed downstream
/// Ok(ToolOutput::json(&data))
/// ```
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
/// the JSON as escaped text for backwards compatibility (per MCP spec). Only use this when the
/// output requires programmatic parsing:
/// ```ignore
/// Ok(ToolOutput::json(&api_response))
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
///
/// # Snapshot Testing
///
/// `ToolOutput` implements [`Display`] for snapshot testing with `insta`. This renders text
/// blocks directly and shows placeholders for binary content (images, audio, embedded resources):
///
/// ```ignore
/// #[test]
/// fn weather_tool_output() {
///     let output = get_weather_handler(input).await?;
///     insta::assert_snapshot!(output.to_string(), @r"
///     Temperature: 72F
///
///     Conditions: Sunny
///     ");
/// }
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

impl fmt::Display for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, block) in self.content.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
                writeln!(f)?;
            }
            match block {
                ContentBlock::TextContent(text) => {
                    write!(f, "{}", text.text)?;
                }
                ContentBlock::ImageContent(img) => {
                    write!(f, "[Image: {}, {} bytes]", img.mime_type, img.data.len())?;
                }
                ContentBlock::AudioContent(audio) => {
                    write!(
                        f,
                        "[Audio: {}, {} bytes]",
                        audio.mime_type,
                        audio.data.len()
                    )?;
                }
                ContentBlock::ResourceLink(link) => {
                    write!(f, "[Resource: {} ({})]", link.name, link.uri)?;
                }
                ContentBlock::EmbeddedResource(res) => {
                    use rust_mcp_schema::EmbeddedResourceResource;
                    match &res.resource {
                        EmbeddedResourceResource::TextResourceContents(text) => {
                            write!(f, "[Embedded Text: {}]", text.uri)?;
                        }
                        EmbeddedResourceResource::BlobResourceContents(blob) => {
                            let mime = blob.mime_type.as_deref().unwrap_or("unknown");
                            write!(
                                f,
                                "[Embedded Blob: {}, {}, {} bytes]",
                                blob.uri,
                                mime,
                                blob.blob.len()
                            )?;
                        }
                    }
                }
            }
        }

        if let Some(structured) = &self.structured_content {
            if !self.content.is_empty() {
                writeln!(f)?;
                writeln!(f)?;
            }
            writeln!(f, "Structured Content:")?;
            let json = serde_json::to_string_pretty(structured).unwrap_or_default();
            write!(f, "{}", json)?;
        }

        Ok(())
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
#[derive(Debug)]
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
        let settings = schemars::r#gen::SchemaSettings::draft07().with(|s| {
            s.option_add_null_type = false;
        });
        let schema = settings.into_generator().into_root_schema_for::<T>();
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

impl fmt::Display for ToolDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## {}", self.name)?;
        writeln!(f)?;
        writeln!(f, "{}", self.description)?;

        let Some(props) = &self.input_schema.properties else {
            return Ok(());
        };
        if props.is_empty() {
            return Ok(());
        }

        writeln!(f)?;
        writeln!(f, "Parameters:")?;

        let mut names: Vec<_> = props.keys().collect();
        names.sort();

        for name in names {
            let prop = &props[name];
            let required = self.input_schema.required.contains(name);
            let req_str = if required { "required" } else { "optional" };

            let type_str = prop.get("type").and_then(|v| v.as_str()).unwrap_or("any");

            write!(f, "  {name} ({type_str}, {req_str})")?;

            if let Some(desc) = prop.get("description").and_then(|v| v.as_str()) {
                writeln!(f)?;
                write!(f, "    {desc}")?;
            }

            if let Some(enum_vals) = prop.get("enum").and_then(|v| v.as_array()) {
                let vals: Vec<_> = enum_vals.iter().filter_map(|v| v.as_str()).collect();
                if !vals.is_empty() {
                    writeln!(f)?;
                    write!(f, "    Values: {}", vals.join(", "))?;
                }
            }

            writeln!(f)?;
        }

        Ok(())
    }
}

/// Collection of tool definitions returned by [`ToolRegistry::definitions`].
///
/// Implements [`Display`] to render all tools as a human-readable document. This lets you see
/// exactly what the LLM receives when it queries your MCP server's available tools, making it
/// easy to verify tool names, descriptions, and parameter schemas.
///
/// # Snapshot Testing with Insta
///
/// Use `insta` inline snapshots to catch unintended changes to your tool schemas:
///
/// ```ignore
/// use mercutio::ToolRegistry;
///
/// #[test]
/// fn tool_schemas_are_stable() {
///     // The snapshot is stored inline - run `cargo insta test` to update
///     insta::assert_snapshot!(MyTools::definitions().to_string(), @r"
///     # Tools
///
///     ## get_weather
///
///     Gets current weather for a location
///
///     Parameters:
///       location (string, required)
///         City name or address
///     ");
/// }
/// ```
///
/// When you change a tool's name, description, or parameters, the test fails and `cargo insta
/// review` shows the diff. This ensures schema changes are intentional and documented.
#[derive(Debug)]
pub struct ToolDefinitions(Vec<ToolDefinition>);

impl ToolDefinitions {
    /// Creates a new collection from a vector of definitions.
    pub fn new(definitions: Vec<ToolDefinition>) -> Self {
        Self(definitions)
    }

    /// Returns the number of tool definitions.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true if there are no tool definitions.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over the tool definitions.
    pub fn iter(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.0.iter()
    }
}

impl Index<usize> for ToolDefinitions {
    type Output = ToolDefinition;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl IntoIterator for ToolDefinitions {
    type Item = ToolDefinition;
    type IntoIter = std::vec::IntoIter<ToolDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ToolDefinitions {
    type Item = &'a ToolDefinition;
    type IntoIter = std::slice::Iter<'a, ToolDefinition>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl fmt::Display for ToolDefinitions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "# Tools")?;
        writeln!(f)?;

        for (i, def) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{def}")?;
        }

        Ok(())
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
    fn definitions() -> ToolDefinitions;
}

/// Empty tool registry for servers that don't expose tools.
///
/// This is the default type parameter for [`McpServer`](crate::McpServer), so the turbofish can
/// be omitted: `McpServer::builder()` instead of `McpServer::<NoTools>::builder()`.
#[derive(Debug)]
pub enum NoTools {}

impl ToolRegistry for NoTools {
    const ENABLED: bool = false;

    fn parse(name: &str, _arguments: serde_json::Value) -> std::result::Result<Self, JsonRpcError> {
        Err(JsonRpcError::MethodNotFound {
            msg: format!("unknown tool: {name}"),
        })
    }

    fn definitions() -> ToolDefinitions {
        ToolDefinitions::new(vec![])
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

            fn definitions() -> $crate::ToolDefinitions {
                $crate::ToolDefinitions::new(vec![
                    $(
                        $crate::ToolDefinition::from_tool::<$variant>(),
                    )*
                ])
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

    #[test]
    fn tool_definition_display() {
        #[allow(dead_code)]
        #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
        struct TestInput {
            /// The city to look up.
            city: String,
            /// Temperature unit preference.
            units: Option<String>,
        }

        impl super::ToolDef for TestInput {
            const NAME: &'static str = "get_weather";
            const DESCRIPTION: &'static str = "Gets weather for a city";
        }

        let def = ToolDefinition::from_tool::<TestInput>();
        insta::assert_snapshot!(def.to_string(), @r"
        ## get_weather

        Gets weather for a city

        Parameters:
          city (string, required)
            The city to look up.
          units (string, optional)
            Temperature unit preference.
        ");
    }

    #[test]
    fn tool_definition_schema_json() {
        #[allow(dead_code)]
        #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
        struct TestInput {
            /// Required field.
            name: String,
            /// Optional field.
            count: Option<u32>,
        }

        impl super::ToolDef for TestInput {
            const NAME: &'static str = "test";
            const DESCRIPTION: &'static str = "Test tool";
        }

        let def = ToolDefinition::from_tool::<TestInput>();
        insta::assert_json_snapshot!(def.input_schema, {
            ".properties" => insta::sorted_redaction()
        }, @r#"
        {
          "properties": {
            "count": {
              "description": "Optional field.",
              "format": "uint32",
              "minimum": 0.0,
              "type": "integer"
            },
            "name": {
              "description": "Required field.",
              "type": "string"
            }
          },
          "required": [
            "name"
          ],
          "type": "object"
        }
        "#);
    }

    #[test]
    fn tool_definitions_display() {
        #[allow(dead_code)]
        #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
        struct GetWeather {
            /// City or address to look up.
            location: String,
        }

        impl super::ToolDef for GetWeather {
            const NAME: &'static str = "get_weather";
            const DESCRIPTION: &'static str = "Gets weather for a location";
        }

        #[allow(dead_code)]
        #[derive(Debug, schemars::JsonSchema, serde::Deserialize)]
        struct SetReminder {
            /// Reminder message.
            message: String,
            /// Minutes from now.
            delay_minutes: u32,
        }

        impl super::ToolDef for SetReminder {
            const NAME: &'static str = "set_reminder";
            const DESCRIPTION: &'static str = "Sets a reminder";
        }

        let defs = super::ToolDefinitions::new(vec![
            ToolDefinition::from_tool::<GetWeather>(),
            ToolDefinition::from_tool::<SetReminder>(),
        ]);
        insta::assert_snapshot!(defs.to_string(), @r"
        # Tools

        ## get_weather

        Gets weather for a location

        Parameters:
          location (string, required)
            City or address to look up.

        ## set_reminder

        Sets a reminder

        Parameters:
          delay_minutes (integer, required)
            Minutes from now.
          message (string, required)
            Reminder message.
        ");
    }

    #[test]
    fn tool_output_display_text() {
        let output = ToolOutput::new()
            .text("Temperature: 72F")
            .text("Conditions: Sunny");
        insta::assert_snapshot!(output.to_string(), @r"
        Temperature: 72F

        Conditions: Sunny
        ");
    }

    #[test]
    fn tool_output_display_with_structured() {
        #[derive(serde::Serialize)]
        struct Weather {
            temp: i32,
            conditions: String,
        }

        let output = ToolOutput::new()
            .text("Current weather")
            .structured(&Weather {
                temp: 72,
                conditions: "Sunny".into(),
            });
        insta::assert_snapshot!(output.to_string(), @r#"
        Current weather

        Structured Content:
        {
          "conditions": "Sunny",
          "temp": 72
        }
        "#);
    }
}
