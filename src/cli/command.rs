//! Generates Clap commands and schema-derived help.

use clap::{
    Arg, ArgAction, Command,
    builder::{PossibleValuesParser, ValueParser},
};
use serde_json::Value;

use super::schema::{NodeSpec, ObjectSpec, ScalarKind, ToolSpec, ValueKind, ValueSpec};

/// Generates the root command and its typed tool subcommands.
pub(super) fn root_command(name: String, version: Option<String>, tools: &[ToolSpec]) -> Command {
    let whole_input_only = tools
        .iter()
        .filter(|tool| tool.root.is_none())
        .map(|tool| tool.cli_name.as_str())
        .collect::<Vec<_>>();
    let mut long_about = String::from(
        "Invokes local MCP tool handlers as native commands. Presentation options must \
         precede the tool command. --input-json <TOOL> reads one complete JSON object from \
         standard input instead of using typed tool options.",
    );
    if !whole_input_only.is_empty() {
        long_about.push_str(" Whole-input-only tools: ");
        long_about.push_str(&whole_input_only.join(", "));
        long_about.push('.');
    }
    let mut command = Command::new(name)
        .about("Invokes local MCP tool handlers as native commands")
        .long_about(long_about)
        .disable_help_subcommand(true)
        .arg(output_arg())
        .arg(image_arg())
        .arg(
            Arg::new("artifact-dir")
                .long("artifact-dir")
                .value_name("DIR")
                .help("Parent directory for artifact output")
                .long_help(
                    "Parent directory for artifact output. A private unique invocation \
                     directory is created beneath it.",
                ),
        );
    if let Some(version) = version {
        command = command.version(version);
    }
    if !tools.is_empty() {
        let values = tools
            .iter()
            .map(|tool| tool.cli_name.clone())
            .collect::<Vec<_>>();
        command = command.arg(
            Arg::new("input-json-route")
                .long("input-json")
                .value_name("TOOL")
                .value_parser(PossibleValuesParser::new(values))
                .help("Reads the selected tool's JSON object from stdin")
                .long_help(
                    "Selects a tool and reads exactly one complete JSON object from standard \
                     input through EOF. This route cannot be combined with a tool subcommand.",
                ),
        );
    }
    for tool in tools {
        if tool.root.is_some() {
            command = command.subcommand(tool_command(tool));
        }
    }
    command
}

/// Generates one tool subcommand.
fn tool_command(tool: &ToolSpec) -> Command {
    let mut command = Command::new(tool.cli_name.clone())
        .about(tool.description.clone())
        .disable_help_subcommand(true);
    let root = tool.root.as_ref().expect("typed tool has a root schema");
    let mut values = Vec::new();
    collect_values(root, &mut values);
    for value in values {
        command = command.arg(value_arg(value));
    }
    command
}

/// Collects flattened options from an object tree.
fn collect_values<'a>(object: &'a ObjectSpec, values: &mut Vec<&'a ValueSpec>) {
    for child in &object.children {
        match child {
            NodeSpec::Object(object) => collect_values(object, values),
            NodeSpec::Value(value) => values.push(value),
            NodeSpec::Union(union) => {
                values.push(&union.scalar);
                collect_values(&union.object, values);
            }
        }
    }
}

/// Generates one Clap option.
fn value_arg(value: &ValueSpec) -> Arg {
    let mut arg = Arg::new(value.cli_name.clone())
        .long(value.cli_name.clone())
        .value_name(value_name_for_kind(&value.kind))
        .help(option_help(value))
        .long_help(option_long_help(value));

    match &value.kind {
        ValueKind::Scalar(ScalarKind::Boolean) => {
            arg = arg
                .action(ArgAction::Set)
                .num_args(0..=1)
                .default_missing_value("true")
                .value_parser(PossibleValuesParser::new(["true", "false"]));
        }
        ValueKind::Scalar(ScalarKind::String(values)) if !values.is_empty() => {
            arg = arg.value_parser(PossibleValuesParser::new(values.clone()));
        }
        ValueKind::Scalar(ScalarKind::Integer | ScalarKind::Number) => {
            arg = arg.allow_negative_numbers(true);
        }
        ValueKind::Array(ScalarKind::Boolean) => {
            arg = arg
                .action(ArgAction::Append)
                .num_args(0..=1)
                .default_missing_value("true")
                .value_parser(PossibleValuesParser::new(["true", "false"]));
        }
        ValueKind::Array(ScalarKind::String(values)) if !values.is_empty() => {
            arg = arg
                .action(ArgAction::Append)
                .value_parser(PossibleValuesParser::new(values.clone()));
        }
        ValueKind::Array(ScalarKind::Integer | ScalarKind::Number) => {
            arg = arg.action(ArgAction::Append).allow_negative_numbers(true);
        }
        ValueKind::Array(_) => {
            arg = arg.action(ArgAction::Append);
        }
        ValueKind::Json(_) | ValueKind::Scalar(_) => {
            arg = arg.value_parser(ValueParser::string());
        }
    }

    if value.required && value.conditional_parent.is_none() {
        arg = arg.required(true);
    }
    arg
}

/// Returns concise option help.
fn option_help(value: &ValueSpec) -> String {
    value
        .schema
        .get("description")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| format!("Sets `{}`", value.path))
}

/// Returns self-contained option encoding and schema guidance.
fn option_long_help(value: &ValueSpec) -> String {
    let mut parts = vec![option_help(value)];
    match &value.kind {
        ValueKind::Scalar(ScalarKind::Boolean) => {
            parts.push("Accepts --option, --option=true, or --option=false forms.".into());
        }
        ValueKind::Array(_) => {
            parts.push("Repeat this option once per array item; order is preserved.".into());
        }
        ValueKind::Json(expected) => {
            let expected = expected.unwrap_or("value");
            parts.push(format!(
                "Value is strict JSON ({expected}); JSON strings require quotes."
            ));
        }
        ValueKind::Scalar(_) => {}
    }
    if let Some(parent) = &value.conditional_parent
        && value.required
    {
        parts.push(format!("Required when `{parent}` is activated."));
    }
    append_schema_annotations(&mut parts, &value.schema);
    parts.join(" ")
}

/// Adds descriptive JSON Schema annotations to long help.
fn append_schema_annotations(parts: &mut Vec<String>, schema: &Value) {
    let annotations = [
        ("format", "Format"),
        ("default", "Schema default"),
        ("examples", "Examples"),
        ("minimum", "Minimum"),
        ("maximum", "Maximum"),
        ("exclusiveMinimum", "Exclusive minimum"),
        ("exclusiveMaximum", "Exclusive maximum"),
        ("minLength", "Minimum length"),
        ("maxLength", "Maximum length"),
        ("pattern", "Pattern"),
        ("minItems", "Minimum items"),
        ("maxItems", "Maximum items"),
        ("uniqueItems", "Unique items"),
        ("minProperties", "Minimum properties"),
        ("maxProperties", "Maximum properties"),
    ];
    for (key, label) in annotations {
        if let Some(value) = schema.get(key) {
            let rendered = value
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| value.to_string());
            parts.push(format!("{label}: {rendered}."));
        }
    }
}

/// Returns the Clap value name for an option encoding.
fn value_name_for_kind(kind: &ValueKind) -> &'static str {
    match kind {
        ValueKind::Scalar(ScalarKind::String(_)) => "STRING",
        ValueKind::Scalar(ScalarKind::Integer) => "INTEGER",
        ValueKind::Scalar(ScalarKind::Number) => "NUMBER",
        ValueKind::Scalar(ScalarKind::Boolean) => "BOOL",
        ValueKind::Array(ScalarKind::String(_)) => "STRING",
        ValueKind::Array(ScalarKind::Integer) => "INTEGER",
        ValueKind::Array(ScalarKind::Number) => "NUMBER",
        ValueKind::Array(ScalarKind::Boolean) => "BOOL",
        ValueKind::Json(_) => "JSON",
    }
}

/// Generates the root output option.
fn output_arg() -> Arg {
    Arg::new("output-mode")
        .long("output")
        .value_name("MODE")
        .default_value("artifacts")
        .value_parser(PossibleValuesParser::new([
            "artifacts",
            "structured",
            "raw",
            "binary",
        ]))
        .help("Selects artifacts, structured, raw, or binary output")
        .long_help(
            "Selects output: artifacts renders text and saves binary blocks; structured writes \
             structuredContent JSON; raw writes the complete MCP result as JSON; binary writes \
             exactly one decoded binary block.",
        )
}

/// Generates the root image policy option.
fn image_arg() -> Arg {
    Arg::new("image-mode")
        .long("images")
        .value_name("MODE")
        .default_value("auto")
        .value_parser(PossibleValuesParser::new(["auto", "kitty", "off"]))
        .help("Controls Kitty image display in artifact output")
}
