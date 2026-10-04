//! Regression tests for schema-driven CLI input encodings.

#![cfg(feature = "cli")]

use std::{borrow::Cow, collections::BTreeMap, io::Cursor};

use mercutio::{ToolDef, cli::ToolRegistryExt as _};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;
use serde_json::{Value, json};

/// Returns unsupported property shapes and representative JSON inputs.
fn fallback_cases() -> Vec<(&'static str, Value, Value)> {
    let scalar_union = json!({
        "type": "string",
        "anyOf": [{ "enum": ["first"] }, { "enum": ["second"] }]
    });
    let referenced_scalar = json!({ "type": "string", "$ref": "#/$defs/Label" });
    let object = json!({
        "type": "object",
        "properties": { "label": { "type": "string" } },
        "required": ["label"]
    });
    vec![
        ("scalar_union", scalar_union.clone(), json!("first")),
        (
            "mixed_union",
            json!({ "anyOf": [{ "type": "string" }, { "type": "integer" }] }),
            json!(42),
        ),
        (
            "recursive",
            json!({ "$ref": "#/$defs/Node" }),
            json!({ "value": "root", "next": { "value": "leaf" } }),
        ),
        (
            "hybrid",
            json!({
                "type": "object",
                "properties": { "fixed": { "type": "string" } },
                "additionalProperties": { "type": "string" }
            }),
            json!({ "fixed": "known", "extra": "dynamic" }),
        ),
        (
            "scalar_one_of",
            json!({
                "type": "string",
                "oneOf": [{ "enum": ["first"] }, { "enum": ["second"] }]
            }),
            json!("first"),
        ),
        (
            "referenced_scalar",
            referenced_scalar.clone(),
            json!("first"),
        ),
        (
            "referenced_array",
            json!({ "type": "array", "items": { "type": "string" }, "$ref": "#/$defs/Labels" }),
            json!(["first"]),
        ),
        (
            "array_union",
            json!({
                "type": "array",
                "items": { "type": "string" },
                "anyOf": [{ "maxItems": 1 }, { "minItems": 3 }]
            }),
            json!(["first"]),
        ),
        (
            "item_union",
            json!({ "type": "array", "items": scalar_union }),
            json!(["first"]),
        ),
        (
            "item_ref",
            json!({ "type": "array", "items": referenced_scalar }),
            json!(["first"]),
        ),
        (
            "tuple",
            json!({ "type": "array", "prefixItems": [{ "type": "integer" }], "items": { "type": "string" } }),
            json!([1, "first"]),
        ),
        (
            "referenced_branch",
            json!({ "oneOf": [referenced_scalar, object.clone()] }),
            json!({ "label": "first" }),
        ),
        (
            "competing_union",
            json!({
                "oneOf": [{ "type": "string" }, object.clone()],
                "anyOf": [{ "type": "string" }, { "type": "object" }]
            }),
            json!({ "label": "first" }),
        ),
        (
            "restricted_union",
            json!({ "type": "string", "oneOf": [{ "type": "string" }, object.clone()] }),
            json!("first"),
        ),
        (
            "all_of",
            json!({ "type": "object", "properties": { "label": { "type": "string" } }, "allOf": [object] }),
            json!({ "label": "first" }),
        ),
        (
            "empty_branch",
            json!({
                "oneOf": [
                    { "type": "string" },
                    { "type": "object", "properties": { "empty": { "type": "object", "properties": {} } } }
                ]
            }),
            json!({ "empty": {} }),
        ),
        (
            "dynamic",
            json!({ "type": "object" }),
            json!({ "PORT": "8080" }),
        ),
        (
            "pattern_object",
            json!({
                "type": "object",
                "properties": { "fixed": { "type": "string" } },
                "patternProperties": { "^extra": { "type": "string" } }
            }),
            json!({ "fixed": "first", "extra_key": "second" }),
        ),
    ]
}

/// Captures reconstructed properties without imposing extra schema validation.
#[derive(Deserialize)]
struct EncodingInput {
    /// A supported sibling option.
    typed: String,
    /// Properties whose encodings are under test.
    #[serde(flatten)]
    values: BTreeMap<String, Value>,
}

impl JsonSchema for EncodingInput {
    fn schema_name() -> Cow<'static, str> {
        "EncodingInput".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let mut schema = json_schema!({
            "type": "object",
            "properties": {
                "typed": { "type": "string" },
                "empty": { "type": "object", "properties": {} },
                "closed": { "type": "object", "additionalProperties": false }
            },
            "required": ["typed", "empty", "closed"],
            "$defs": {
                "Label": { "type": "string" },
                "Labels": { "type": "array", "items": { "type": "string" } },
                "Node": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" },
                        "next": { "$ref": "#/$defs/Node" }
                    },
                    "required": ["value"]
                }
            }
        });
        let properties = schema.as_object_mut().expect("object schema")["properties"]
            .as_object_mut()
            .expect("property map");
        for (name, property, _) in fallback_cases() {
            properties.insert(name.into(), property);
        }
        schema
    }
}

impl ToolDef for EncodingInput {
    const NAME: &'static str = "encoding";
    const DESCRIPTION: &'static str = "Exercises property input encodings";
}

/// Preserves unsupported shapes as JSON while keeping supported siblings typed.
#[test]
fn unsupported_shapes_use_localized_json_options() {
    let cli = EncodingInput::cli("tools").build().expect("valid CLI");
    let command = cli.command();
    let tool = command.find_subcommand("encoding").expect("typed tool");
    let mut args = vec![
        "tools".into(),
        "encoding".into(),
        "--typed".into(),
        "verbatim".into(),
    ];
    let mut expected = BTreeMap::from([
        ("empty".to_string(), json!({})),
        ("closed".to_string(), json!({})),
    ]);
    for (name, _, value) in fallback_cases() {
        let spelling = name.replace('_', "-");
        let arg = tool
            .get_arguments()
            .find(|arg| arg.get_long() == Some(&spelling))
            .expect("localized option");
        assert_eq!(
            arg.get_value_names().expect("value name")[0],
            "JSON",
            "{name}"
        );
        args.extend([format!("--{spelling}"), value.to_string()]);
        expected.insert(name.to_string(), value);
    }
    let input = cli
        .try_parse_from(args, Cursor::new([]))
        .expect("valid JSON options")
        .into_tool();
    assert_eq!(input.typed, "verbatim");
    assert_eq!(input.values, expected);

    for (option, value) in [
        ("--scalar-union", "unquoted"),
        ("--array-union", "{}"),
        ("--dynamic", "[]"),
    ] {
        let error = cli
            .try_parse_from(
                ["tools", "encoding", "--typed", "x", option, value],
                Cursor::new([]),
            )
            .err()
            .expect("invalid JSON encoding");
        assert_eq!(error.exit_code(), 2);
    }
}

/// Accepts arbitrary object properties without enumerating them in the schema.
#[derive(Deserialize)]
#[serde(transparent)]
struct DynamicRoot {
    /// Captured input properties.
    values: BTreeMap<String, Value>,
}

impl JsonSchema for DynamicRoot {
    fn schema_name() -> Cow<'static, str> {
        "DynamicRoot".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "object" })
    }
}

impl ToolDef for DynamicRoot {
    const NAME: &'static str = "dynamic_root";
    const DESCRIPTION: &'static str = "Accepts arbitrary object input";
}

/// Preserves empty-object invocations and whole-input dispatch for bare root schemas.
#[test]
fn bare_roots_support_empty_and_whole_input_invocations() {
    let cli = DynamicRoot::cli("tools").build().expect("valid CLI");
    let empty = cli
        .try_parse_from(["tools", "dynamic-root"], Cursor::new([]))
        .expect("empty invocation")
        .into_tool();
    assert!(empty.values.is_empty());
    let input = cli
        .try_parse_from(
            ["tools", "--input-json", "dynamic-root"],
            Cursor::new(br#"{"key":42}"#),
        )
        .expect("whole-input invocation")
        .into_tool();
    assert_eq!(input.values["key"], json!(42));
}

/// Accepts arbitrary property names with string values.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
struct MapRoot {
    /// Captured input properties.
    values: BTreeMap<String, String>,
}

impl ToolDef for MapRoot {
    const NAME: &'static str = "map_root";
    const DESCRIPTION: &'static str = "Accepts string-valued object input";
}

/// Requires whole-input dispatch for explicitly dynamic root schemas.
#[test]
fn map_roots_are_whole_input_only() {
    let cli = MapRoot::cli("tools").build().expect("valid CLI");
    let mut command = cli.command();
    assert!(command.find_subcommand("map-root").is_none());
    assert!(
        command
            .render_long_help()
            .to_string()
            .contains("Whole-input-only tools: map-root")
    );
    let input = cli
        .try_parse_from(
            ["tools", "--input-json", "map-root"],
            Cursor::new(br#"{"PORT":"8080"}"#),
        )
        .expect("whole-input invocation")
        .into_tool();
    assert_eq!(input.values["PORT"], "8080");
    let error = cli
        .try_parse_from(["tools", "map-root"], Cursor::new([]))
        .err()
        .expect("no typed command for a map root");
    assert_eq!(error.exit_code(), 2);
}

/// Describes a zero-field input with the derived schema.
#[derive(Deserialize, JsonSchema)]
struct EmptyInput {}

impl ToolDef for EmptyInput {
    const NAME: &'static str = "empty";
    const DESCRIPTION: &'static str = "Accepts empty input";
}

/// Describes a closed object without a properties map.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedInput {}

impl JsonSchema for ClosedInput {
    fn schema_name() -> Cow<'static, str> {
        "ClosedInput".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "object", "additionalProperties": false })
    }
}

impl ToolDef for ClosedInput {
    const NAME: &'static str = "closed";
    const DESCRIPTION: &'static str = "Accepts only empty input";
}

/// Keeps derived empty and explicitly closed inputs callable without arguments.
#[test]
fn empty_tools_keep_typed_subcommands() {
    EmptyInput::cli("tools")
        .build()
        .expect("valid empty CLI")
        .try_parse_from(["tools", "empty"], Cursor::new([]))
        .expect("empty invocation");
    ClosedInput::cli("tools")
        .build()
        .expect("valid closed CLI")
        .try_parse_from(["tools", "closed"], Cursor::new([]))
        .expect("closed invocation");
}
