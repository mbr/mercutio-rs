//! Analyzes tool schemas into native input encodings and retains protocol names.

use std::collections::BTreeMap;

use serde_json::Value;

use super::CliBuildProblem;
use crate::ToolDefinition;

/// Analyzed command and schema for one tool.
pub(super) struct ToolSpec {
    /// Original MCP tool name.
    pub(super) original_name: String,
    /// Normalized command name.
    pub(super) cli_name: String,
    /// Tool description.
    pub(super) description: String,
    /// Typed root object, or `None` for whole-input-only tools.
    pub(super) root: Option<ObjectSpec>,
}

/// One statically enumerable object.
pub(super) struct ObjectSpec {
    /// Original property name, absent for a tool root.
    pub(super) name: Option<String>,
    /// Display path from the tool root.
    pub(super) path: String,
    /// Whether this object is required when its parent is active.
    pub(super) required: bool,
    /// Child properties.
    pub(super) children: Vec<NodeSpec>,
}

/// One schema property represented in a command.
pub(super) enum NodeSpec {
    /// Statically flattened object.
    Object(ObjectSpec),
    /// Scalar, repeated scalar, or JSON-valued option.
    Value(ValueSpec),
    /// Scalar-versus-object union.
    Union(UnionSpec),
}

/// One command option and its JSON representation.
pub(super) struct ValueSpec {
    /// Original property name.
    pub(super) name: String,
    /// Original dotted property path.
    pub(super) path: String,
    /// Normalized option name and Clap identifier.
    pub(super) cli_name: String,
    /// Whether this value is required when its parent is active.
    pub(super) required: bool,
    /// First optional ancestor that conditionally activates this value.
    pub(super) conditional_parent: Option<String>,
    /// Input encoding.
    pub(super) kind: ValueKind,
    /// Source JSON Schema.
    pub(super) schema: Value,
}

/// Encoding used by one option.
pub(super) enum ValueKind {
    /// One scalar occurrence.
    Scalar(ScalarKind),
    /// Ordered repeated scalar occurrences.
    Array(ScalarKind),
    /// One strict JSON value with an optional expected type.
    Json(Option<&'static str>),
}

/// Supported scalar JSON types.
pub(super) enum ScalarKind {
    /// Verbatim UTF-8 string with optional exact values.
    String(Vec<String>),
    /// JSON integer.
    Integer,
    /// JSON number.
    Number,
    /// Explicit or implicit JSON boolean.
    Boolean,
}

/// Supported scalar-versus-object union.
pub(super) struct UnionSpec {
    /// Original property name.
    pub(super) name: String,
    /// Whether one branch is required when the parent is active.
    pub(super) required: bool,
    /// Scalar parent option.
    pub(super) scalar: ValueSpec,
    /// Flattened object branch.
    pub(super) object: ObjectSpec,
}

/// Analyzes one tool definition.
pub(super) fn analyze_tool(
    definition: ToolDefinition,
    cli_name: String,
    problems: &mut Vec<CliBuildProblem>,
) -> ToolSpec {
    let schema = definition.input_schema_json();
    let mut option_names = BTreeMap::<String, Vec<String>>::new();
    let root = if matches!(
        classify_schema(&schema, SchemaPosition::Root),
        SchemaShape::Object
    ) {
        Some(analyze_object(
            &schema,
            None,
            String::new(),
            false,
            None,
            &mut option_names,
            problems,
        ))
    } else {
        None
    };
    append_collisions(problems, option_names);

    ToolSpec {
        original_name: definition.name,
        cli_name,
        description: definition.description,
        root,
    }
}

/// Analyzes one statically enumerable object.
#[allow(clippy::too_many_arguments)]
fn analyze_object(
    schema: &Value,
    name: Option<String>,
    path: String,
    required: bool,
    optional_parent: Option<String>,
    option_names: &mut BTreeMap<String, Vec<String>>,
    problems: &mut Vec<CliBuildProblem>,
) -> ObjectSpec {
    let required_names = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let mut children = Vec::new();

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (child_name, child_schema) in properties {
            let child_path = if path.is_empty() {
                child_name.clone()
            } else {
                format!("{path}.{child_name}")
            };
            let child_required = required_names.contains(&child_name.as_str());
            let child_optional_parent = optional_parent.clone().or_else(|| {
                (!child_required
                    && matches!(
                        classify_schema(child_schema, SchemaPosition::Nested),
                        SchemaShape::Object
                    ))
                .then(|| child_path.clone())
            });
            let node = analyze_node(
                child_name,
                child_schema,
                child_path,
                child_required,
                child_optional_parent,
                option_names,
                problems,
            );
            children.push(node);
        }
    }

    children.sort_by(|left, right| node_sort_key(left).cmp(&node_sort_key(right)));
    ObjectSpec {
        name,
        path,
        required,
        children,
    }
}

/// Analyzes one property schema.
#[allow(clippy::too_many_arguments)]
fn analyze_node(
    name: &str,
    schema: &Value,
    path: String,
    required: bool,
    optional_parent: Option<String>,
    option_names: &mut BTreeMap<String, Vec<String>>,
    problems: &mut Vec<CliBuildProblem>,
) -> NodeSpec {
    match classify_schema(schema, SchemaPosition::Nested) {
        SchemaShape::Object => NodeSpec::Object(analyze_object(
            schema,
            Some(name.to_string()),
            path,
            required,
            optional_parent,
            option_names,
            problems,
        )),
        SchemaShape::Value(kind) => NodeSpec::Value(analyze_value(
            name,
            schema,
            path,
            required,
            optional_parent,
            kind,
            option_names,
            problems,
        )),
        SchemaShape::ScalarObject {
            scalar_schema,
            scalar_kind,
            object_schema,
        } => {
            let mut scalar_schema = scalar_schema.clone();
            merge_annotations(&mut scalar_schema, schema);
            let scalar = analyze_value(
                name,
                &scalar_schema,
                path.clone(),
                false,
                optional_parent.clone(),
                ValueKind::Scalar(scalar_kind),
                option_names,
                problems,
            );
            let object = analyze_object(
                object_schema,
                Some(name.to_string()),
                path.clone(),
                false,
                Some(optional_parent.unwrap_or(path)),
                option_names,
                problems,
            );
            NodeSpec::Union(UnionSpec {
                name: name.to_string(),
                required,
                scalar,
                object,
            })
        }
    }
}

/// Copies descriptive parent annotations onto a union branch.
fn merge_annotations(branch: &mut Value, parent: &Value) {
    let (Some(branch), Some(parent)) = (branch.as_object_mut(), parent.as_object()) else {
        return;
    };
    for key in ["description", "default", "examples", "title"] {
        if let Some(value) = parent.get(key) {
            branch
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
}

/// Analyzes one command option.
#[allow(clippy::too_many_arguments)]
fn analyze_value(
    name: &str,
    schema: &Value,
    path: String,
    required: bool,
    optional_parent: Option<String>,
    kind: ValueKind,
    option_names: &mut BTreeMap<String, Vec<String>>,
    problems: &mut Vec<CliBuildProblem>,
) -> ValueSpec {
    let cli_name = match normalize_path(&path) {
        Ok(cli_name) => cli_name,
        Err(()) => {
            problems.push(CliBuildProblem::InvalidName {
                kind: "property",
                name: path.clone(),
            });
            format!("invalid-{}", option_names.len())
        }
    };
    if cli_name == "help" {
        problems.push(CliBuildProblem::ReservedName {
            kind: "property",
            name: path.clone(),
        });
    }
    option_names
        .entry(cli_name.clone())
        .or_default()
        .push(path.clone());

    ValueSpec {
        name: name.to_string(),
        path,
        cli_name,
        required,
        conditional_parent: optional_parent,
        kind,
        schema: schema.clone(),
    }
}

/// Schema shapes supported by native input encodings.
enum SchemaShape<'a> {
    /// An object with statically enumerable properties.
    Object,
    /// A single typed, repeatable, or JSON-valued option.
    Value(ValueKind),
    /// A scalar-versus-object union with selectable object descendants.
    ScalarObject {
        /// Source schema for the scalar branch.
        scalar_schema: &'a Value,
        /// Encoding of the scalar branch.
        scalar_kind: ScalarKind,
        /// Source schema for the object branch.
        object_schema: &'a Value,
    },
}

/// Distinguishes the tool input root from nested schema values.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SchemaPosition {
    /// Tool input, where bare object schemas retain zero-argument commands.
    Root,
    /// A property, array item, or union branch.
    Nested,
}

/// Classifies input encoding without validating general JSON Schema constraints.
fn classify_schema(schema: &Value, position: SchemaPosition) -> SchemaShape<'_> {
    let schema_type = schema.get("type").and_then(Value::as_str);
    let json = SchemaShape::Value(ValueKind::Json(schema_type.and_then(static_json_type)));
    if ["$ref", "$dynamicRef", "anyOf", "allOf"]
        .iter()
        .any(|keyword| schema.get(keyword).is_some())
    {
        return json;
    }
    if let Some(choices) = schema.get("oneOf") {
        if schema.get("type").is_none()
            && let Some([first, second]) = choices.as_array().map(Vec::as_slice)
        {
            for (scalar_schema, object_schema) in [(first, second), (second, first)] {
                if let SchemaShape::Value(ValueKind::Scalar(scalar_kind)) =
                    classify_schema(scalar_schema, SchemaPosition::Nested)
                    && matches!(
                        classify_schema(object_schema, SchemaPosition::Nested),
                        SchemaShape::Object
                    )
                    && object_has_options(object_schema)
                {
                    return SchemaShape::ScalarObject {
                        scalar_schema,
                        scalar_kind,
                        object_schema,
                    };
                }
            }
        }
        return json;
    }

    match schema_type {
        Some("object") => {
            let properties = schema.get("properties");
            let additional = schema.get("additionalProperties");
            // Schemars also emits bare object schemas for zero-field structs.
            // Preserve their root commands; nested bare objects need JSON input.
            let enumerable = properties.is_some_and(Value::is_object)
                || (properties.is_none()
                    && (additional == Some(&Value::Bool(false))
                        || position == SchemaPosition::Root));
            if enumerable
                && additional.is_none_or(|value| value == &Value::Bool(false))
                && schema.get("patternProperties").is_none()
            {
                SchemaShape::Object
            } else {
                json
            }
        }
        Some("array") => {
            if schema.get("prefixItems").is_none()
                && let Some(items) = schema.get("items")
                && let SchemaShape::Value(ValueKind::Scalar(kind)) =
                    classify_schema(items, SchemaPosition::Nested)
            {
                SchemaShape::Value(ValueKind::Array(kind))
            } else {
                json
            }
        }
        Some("string") => SchemaShape::Value(ValueKind::Scalar(ScalarKind::String(
            schema
                .get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect(),
        ))),
        Some("integer") => SchemaShape::Value(ValueKind::Scalar(ScalarKind::Integer)),
        Some("number") => SchemaShape::Value(ValueKind::Scalar(ScalarKind::Number)),
        Some("boolean") => SchemaShape::Value(ValueKind::Scalar(ScalarKind::Boolean)),
        _ => json,
    }
}

/// Returns whether a flattened object branch has an option that can select it.
fn object_has_options(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties.values().any(|property| {
                match classify_schema(property, SchemaPosition::Nested) {
                    SchemaShape::Object => object_has_options(property),
                    SchemaShape::Value(_) | SchemaShape::ScalarObject { .. } => true,
                }
            })
        })
}

/// Maps a schema type to a stable expected JSON type.
fn static_json_type(schema_type: &str) -> Option<&'static str> {
    match schema_type {
        "array" => Some("array"),
        "boolean" => Some("boolean"),
        "integer" => Some("integer"),
        "null" => Some("null"),
        "number" => Some("number"),
        "object" => Some("object"),
        "string" => Some("string"),
        _ => None,
    }
}

/// Appends every normalization collision to construction problems.
pub(super) fn append_collisions(
    problems: &mut Vec<CliBuildProblem>,
    names: BTreeMap<String, Vec<String>>,
) {
    for (cli_name, originals) in names {
        if originals.len() > 1 {
            problems.push(CliBuildProblem::NameCollision {
                cli_name,
                originals,
            });
        }
    }
}

/// Returns a deterministic required-first option ordering key.
fn node_sort_key(node: &NodeSpec) -> (bool, &str) {
    match node {
        NodeSpec::Object(object) => (!object.required, &object.path),
        NodeSpec::Value(value) => (!value.required, &value.cli_name),
        NodeSpec::Union(union) => (!union.required, &union.scalar.cli_name),
    }
}

/// Normalizes one dotted property path.
fn normalize_path(path: &str) -> Result<String, ()> {
    path.split('.')
        .map(normalize_name)
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("-"))
}

/// Normalizes one protocol name to common command-line spelling.
pub(super) fn normalize_name(name: &str) -> Result<String, ()> {
    if name.is_empty()
        || name
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.'))
    {
        return Err(());
    }

    let chars = name.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut separator = false;
    for (index, ch) in chars.iter().copied().enumerate() {
        if matches!(ch, '_' | '-' | '.') {
            separator = !output.is_empty();
            continue;
        }
        let previous = index
            .checked_sub(1)
            .and_then(|index| chars.get(index))
            .copied();
        let next = chars.get(index + 1).copied();
        let camel_boundary = ch.is_ascii_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_ascii_uppercase()
                        && next.is_some_and(|next| next.is_ascii_lowercase()))
            });
        if (separator || camel_boundary) && !output.ends_with('-') {
            output.push('-');
        }
        separator = false;
        output.push(ch.to_ascii_lowercase());
    }
    while output.ends_with('-') {
        output.pop();
    }
    (!output.is_empty()).then_some(output).ok_or(())
}
