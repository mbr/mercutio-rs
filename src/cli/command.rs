//! Generates Clap commands and schema-derived help.

use clap::{
    Arg, ArgAction, Command,
    builder::{PossibleValuesParser, ValueParser},
};
use serde_json::Value;

use super::{
    CliBuilder,
    schema::{NodeSpec, ObjectSpec, ScalarKind, ToolSpec, ValueKind, ValueSpec},
};
use crate::ToolRegistry;

/// Generates the root command and its typed tool subcommands.
pub(super) fn root_command<R: ToolRegistry>(
    name: String,
    builder: CliBuilder<R>,
    tools: &[ToolSpec],
) -> Command {
    let about = builder.about.filter(|text| !text.trim().is_empty());
    let long_about = builder
        .instructions
        .filter(|text| !text.trim().is_empty())
        .map(|instructions| match &about {
            Some(about) => format!("{about}\n\n{instructions}"),
            None => instructions,
        });
    let mut command = Command::new(name)
        .after_help("Use --help for the full reference.")
        .after_long_help("")
        .disable_help_subcommand(true)
        .arg(output_arg())
        .arg(image_arg())
        .arg(
            Arg::new("artifact-dir")
                .long("artifact-dir")
                .value_name("DIR")
                .help_heading("Output options (before the command)")
                .hide_short_help(true)
                .help("Save generated files under DIR")
                .long_help(
                    "Save generated files beneath this directory (default: system temporary \
                     directory). Each invocation uses its own directory and prints absolute \
                     file paths.\n\nOnly available with --output artifacts.",
                ),
        );
    if let Some(about) = about {
        command = command.about(about);
    }
    if let Some(long_about) = long_about {
        command = command.long_about(long_about);
    }
    if let Some(version) = builder.version {
        command = command.version(version);
    }
    if !tools.is_empty() {
        let whole_input_only = tools
            .iter()
            .filter(|tool| tool.root.is_none())
            .map(|tool| tool.cli_name.as_str())
            .collect::<Vec<_>>();
        let mut input_help = String::from(
            "Select a command and read its arguments as one complete JSON object from stdin \
             through EOF. Cannot be combined with a subcommand or its argument options.",
        );
        if !whole_input_only.is_empty() {
            input_help.push_str("\n\nWhole-input-only commands: ");
            input_help.push_str(&whole_input_only.join(", "));
            input_help.push('.');
        }
        let values = tools
            .iter()
            .map(|tool| tool.cli_name.clone())
            .collect::<Vec<_>>();
        input_help.push_str("\n\nCommands: ");
        input_help.push_str(&values.join(", "));
        command = command.arg(
            Arg::new("input-json-route")
                .long("input-json")
                .value_name("COMMAND")
                .value_parser(PossibleValuesParser::new(values))
                .hide_possible_values(true)
                .help("Read arguments from stdin as JSON")
                .long_help(input_help),
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
    let mut notes = Vec::new();
    collect_union_help(root, None, &mut notes);
    if !notes.is_empty() {
        command = command.after_long_help(format!(
            "Alternatives (do not combine forms):\n{}",
            notes.join("\n")
        ));
    }
    command
}

/// Collects union selection rules with their enclosing activation conditions.
fn collect_union_help(object: &ObjectSpec, required_when: Option<&str>, notes: &mut Vec<String>) {
    for child in &object.children {
        match child {
            NodeSpec::Object(object) => {
                let condition =
                    (!object.required).then(|| format!("`{}` is activated", object.path));
                collect_union_help(object, condition.as_deref().or(required_when), notes);
            }
            NodeSpec::Union(union) => {
                let mut values = Vec::new();
                collect_values(&union.object, &mut values);
                let options = values
                    .iter()
                    .map(|value| format!("`--{}`", value.cli_name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let selection = match (union.required, required_when) {
                    (false, _) => "optional".into(),
                    (true, None) => "required".into(),
                    (true, Some(condition)) => format!("required when {condition}"),
                };
                notes.push(format!(
                    "  `{}`: `--{}` OR object options ({options}); {selection}",
                    union.scalar.path, union.scalar.cli_name,
                ));
                let condition = format!("the object form of `{}` is selected", union.scalar.path);
                collect_union_help(&union.object, Some(&condition), notes);
            }
            NodeSpec::Value(_) => {}
        }
    }
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
        .long_help(option_long_help(value))
        .value_parser(ValueParser::string())
        .required(value.required && value.conditional_parent.is_none());

    let scalar = match &value.kind {
        ValueKind::Scalar(kind) => Some(kind),
        ValueKind::Array(kind) => {
            arg = arg.action(ArgAction::Append);
            Some(kind)
        }
        ValueKind::Json(_) => None,
    };
    match scalar {
        Some(ScalarKind::Boolean) => arg
            .num_args(0..=1)
            .default_missing_value("true")
            .value_parser(PossibleValuesParser::new(["true", "false"])),
        Some(ScalarKind::String(values)) if !values.is_empty() => {
            arg.value_parser(PossibleValuesParser::new(values.clone()))
        }
        Some(ScalarKind::Integer | ScalarKind::Number) => arg.allow_negative_numbers(true),
        _ => arg,
    }
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
        ValueKind::Scalar(kind) | ValueKind::Array(kind) => match kind {
            ScalarKind::String(_) => "STRING",
            ScalarKind::Integer => "INTEGER",
            ScalarKind::Number => "NUMBER",
            ScalarKind::Boolean => "BOOL",
        },
        ValueKind::Json(_) => "JSON",
    }
}

/// Generates the root output option.
fn output_arg() -> Arg {
    Arg::new("output-mode")
        .long("output")
        .value_name("MODE")
        .default_value("artifacts")
        .help_heading("Output options (before the command)")
        .hide_default_value(true)
        .hide_possible_values(true)
        .value_parser(PossibleValuesParser::new([
            "artifacts",
            "structured",
            "raw",
            "binary",
        ]))
        .help("Choose output format")
        .long_help(
            "Choose output format:\n\n\
             artifacts   Readable text and generated file paths (default)\n\
             structured  JSON data only; requires structured output\n\
             raw         Complete MCP result as JSON, including encoded file data\n\
             binary      Bytes of exactly one image, audio item, or embedded file\n\n\
             In binary mode, bytes go to stdout and accompanying text goes to stderr.",
        )
}

/// Generates the root image policy option.
fn image_arg() -> Arg {
    Arg::new("image-mode")
        .long("images")
        .value_name("MODE")
        .default_value("auto")
        .help_heading("Output options (before the command)")
        .hide_short_help(true)
        .hide_default_value(true)
        .hide_possible_values(true)
        .value_parser(PossibleValuesParser::new(["auto", "kitty", "off"]))
        .help("Control inline image display")
        .long_help(
            "Control inline PNG image display:\n\n\
             auto   Display on recognized compatible terminals (default)\n\
             kitty  Force Kitty image display\n\
             off    Do not display inline images\n\n\
             Automatic display is disabled for pipes, redirected output, and custom output \
             writers. Files are saved in every mode. Only available with --output artifacts.",
        )
}
