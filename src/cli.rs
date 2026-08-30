//! Native command-line interfaces for tool registries.
//!
//! This module turns a [`ToolRegistry`] into a dynamic `clap` command tree and reconstructs the
//! selected command's arguments into the same JSON object accepted by [`ToolRegistry::parse`].

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt,
    io::{self, Read, Write},
    marker::PhantomData,
    path::PathBuf,
};

use clap::{
    Arg, ArgAction, ArgMatches, Command,
    builder::{PossibleValuesParser, ValueParser},
    error::ErrorKind as ClapErrorKind,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{ToolDefinition, ToolRegistry};

/// Controls the successful result representation written by a runner.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputMode {
    /// Renders text and materializes binary content as files.
    #[default]
    Artifacts,
    /// Writes only `structuredContent` as JSON.
    Structured,
    /// Writes the complete MCP tool result as JSON.
    Raw,
    /// Writes the single binary content block as bytes.
    Binary,
}

/// Controls inline image presentation in artifact output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImageMode {
    /// Uses Kitty graphics only for a compatible process terminal.
    #[default]
    Auto,
    /// Forces Kitty graphics output for PNG images.
    Kitty,
    /// Disables inline image output.
    Off,
}

/// Presentation settings selected by root command options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputOptions {
    /// Selected output representation.
    pub mode: OutputMode,
    /// Selected inline image policy.
    pub images: ImageMode,
    /// Optional parent directory for filesystem artifacts.
    pub artifact_dir: Option<PathBuf>,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            mode: OutputMode::Artifacts,
            images: ImageMode::Auto,
            artifact_dir: None,
        }
    }
}

/// One parsed native tool invocation.
pub struct Invocation<R: ToolRegistry> {
    /// Parsed registry value.
    tool: R,
    /// Selected output settings.
    output: OutputOptions,
}

impl<R: ToolRegistry> Invocation<R> {
    /// Consumes the invocation and returns its parsed tool.
    pub fn into_tool(self) -> R {
        self.tool
    }

    /// Consumes the invocation and returns its tool and output settings.
    pub fn into_parts(self) -> (R, OutputOptions) {
        (self.tool, self.output)
    }

    /// Returns the selected output settings.
    pub fn output_options(&self) -> &OutputOptions {
        &self.output
    }
}

/// One reason a CLI could not be constructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliBuildProblem {
    /// A protocol name contains unsupported characters or has no command-line spelling.
    InvalidName {
        /// Kind of protocol name.
        kind: &'static str,
        /// Original protocol name or property path.
        name: String,
    },
    /// Multiple protocol names have the same normalized spelling.
    NameCollision {
        /// Conflicting command-line spelling.
        cli_name: String,
        /// Original protocol names or property paths.
        originals: Vec<String>,
    },
    /// A protocol name collides with Clap's help interface.
    ReservedName {
        /// Kind of protocol name.
        kind: &'static str,
        /// Original protocol name or property path.
        name: String,
    },
    /// The generated subtree collides with an application command.
    ApplicationCommandCollision {
        /// Conflicting command name.
        name: String,
    },
}

impl fmt::Display for CliBuildProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { kind, name } => {
                write!(f, "unsupported {kind} name `{name}`")
            }
            Self::NameCollision {
                cli_name,
                originals,
            } => write!(
                f,
                "`{cli_name}` is shared by {}",
                originals
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::ReservedName { kind, name } => {
                write!(f, "{kind} name `{name}` is reserved for help")
            }
            Self::ApplicationCommandCollision { name } => {
                write!(f, "application already has a `{name}` subcommand")
            }
        }
    }
}

/// Error returned while constructing a generated command tree.
#[derive(Debug, Error)]
#[error("cannot construct CLI: {message}")]
pub struct CliBuildError {
    /// Human-readable aggregate message.
    message: String,
    /// Every detected construction problem.
    problems: Vec<CliBuildProblem>,
}

impl CliBuildError {
    /// Creates an aggregate construction error.
    fn new(problems: Vec<CliBuildProblem>) -> Self {
        let message = problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        Self { message, problems }
    }

    /// Returns every detected construction problem.
    pub fn problems(&self) -> &[CliBuildProblem] {
        &self.problems
    }
}

/// Category of a native CLI error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliErrorKind {
    /// Help or version output requested by the caller.
    Display,
    /// Invalid command-line or tool input.
    Usage,
    /// Handler, rendering, filesystem, or stream failure.
    Runtime,
}

/// Error or non-exiting display request returned by native CLI operations.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct CliError {
    /// Error category.
    kind: CliErrorKind,
    /// Fully rendered message.
    message: String,
}

impl CliError {
    /// Converts a non-exiting Clap error.
    fn from_clap(error: clap::Error) -> Self {
        let kind = match error.kind() {
            ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion => CliErrorKind::Display,
            _ => CliErrorKind::Usage,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }

    /// Creates a stream or runtime failure.
    fn runtime(message: impl Into<String>) -> Self {
        Self {
            kind: CliErrorKind::Runtime,
            message: format!("error: {}\n", message.into()),
        }
    }

    /// Returns the error category.
    pub fn kind(&self) -> CliErrorKind {
        self.kind
    }

    /// Returns the process exit status associated with this value.
    pub fn exit_code(&self) -> u8 {
        match self.kind {
            CliErrorKind::Display => 0,
            CliErrorKind::Usage => 2,
            CliErrorKind::Runtime => 1,
        }
    }

    /// Returns whether this value should be written to stderr.
    pub fn targets_stderr(&self) -> bool {
        !matches!(self.kind, CliErrorKind::Display)
    }

    /// Writes the rendered value to its selected stream.
    pub fn write_to(&self, mut stdout: impl Write, mut stderr: impl Write) -> io::Result<()> {
        if self.targets_stderr() {
            stderr.write_all(self.message.as_bytes())
        } else {
            stdout.write_all(self.message.as_bytes())
        }
    }
}

/// Builder for a generated native CLI.
pub struct CliBuilder<R: ToolRegistry> {
    /// Root command name.
    name: String,
    /// Optional root version.
    version: Option<String>,
    /// Registry marker.
    marker: PhantomData<R>,
}

impl<R: ToolRegistry> CliBuilder<R> {
    /// Sets the root command version.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Builds the generated command and schema reconstruction data.
    pub fn build(self) -> Result<Cli<R>, CliBuildError> {
        Cli::from_builder(self)
    }
}

/// Generated native command-line adapter for a [`ToolRegistry`].
pub struct Cli<R: ToolRegistry> {
    /// Generated root command.
    command: Command,
    /// Analyzed tool definitions.
    tools: Vec<ToolSpec>,
    /// Registry marker.
    marker: PhantomData<R>,
}

impl<R: ToolRegistry> Cli<R> {
    /// Creates a builder for a standalone or nested generated command.
    pub fn builder(name: impl Into<String>) -> CliBuilder<R> {
        CliBuilder {
            name: name.into(),
            version: None,
            marker: PhantomData,
        }
    }

    /// Builds a CLI from its public builder.
    fn from_builder(builder: CliBuilder<R>) -> Result<Self, CliBuildError> {
        let mut problems = Vec::new();
        let root_name = normalize_name(&builder.name).map_err(|()| {
            CliBuildError::new(vec![CliBuildProblem::InvalidName {
                kind: "command",
                name: builder.name.clone(),
            }])
        })?;
        let mut tool_names = BTreeMap::<String, Vec<String>>::new();
        let mut tools = Vec::new();

        for definition in R::definitions() {
            let cli_name = match normalize_name(&definition.name) {
                Ok(name) => name,
                Err(()) => {
                    problems.push(CliBuildProblem::InvalidName {
                        kind: "tool",
                        name: definition.name,
                    });
                    continue;
                }
            };
            if cli_name == "help" {
                problems.push(CliBuildProblem::ReservedName {
                    kind: "tool",
                    name: definition.name.clone(),
                });
            }
            tool_names
                .entry(cli_name.clone())
                .or_default()
                .push(definition.name.clone());
            tools.push(analyze_tool(definition, cli_name, &mut problems));
        }

        append_collisions(&mut problems, tool_names);
        if !problems.is_empty() {
            return Err(CliBuildError::new(problems));
        }

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
        let mut command = Command::new(root_name)
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
        if let Some(version) = builder.version {
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
        for tool in &tools {
            if tool.root.is_some() {
                command = command.subcommand(tool_command(tool));
            }
        }

        Ok(Self {
            command,
            tools,
            marker: PhantomData,
        })
    }

    /// Returns a clone of the generated Clap command.
    pub fn command(&self) -> Command {
        self.command.clone()
    }

    /// Attaches the complete generated tree below an application-owned command.
    pub fn attach_to(&self, command: Command) -> Result<Command, CliBuildError> {
        let name = self.command.get_name();
        if command
            .get_subcommands()
            .any(|child| child.get_name() == name)
        {
            return Err(CliBuildError::new(vec![
                CliBuildProblem::ApplicationCommandCollision {
                    name: name.to_string(),
                },
            ]));
        }
        Ok(command.subcommand(self.command.clone()))
    }

    /// Parses an explicit argument iterator and injected whole-input reader.
    pub fn try_parse_from<I, T, S>(&self, args: I, input: S) -> Result<Invocation<R>, CliError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
        S: Read,
    {
        let matches = self
            .command
            .clone()
            .try_get_matches_from(args)
            .map_err(CliError::from_clap)?;
        self.try_parse_matches(&matches, input)
    }

    /// Parses process arguments and uses standard input for whole-input JSON.
    pub fn try_parse(&self) -> Result<Invocation<R>, CliError> {
        self.try_parse_from(std::env::args_os(), std::io::stdin().lock())
    }

    /// Reconstructs an invocation from matches rooted at the generated command.
    pub fn try_parse_matches(
        &self,
        matches: &ArgMatches,
        mut input: impl Read,
    ) -> Result<Invocation<R>, CliError> {
        let output = parse_output_options(matches).map_err(|message| self.usage_error(message))?;
        let json_route = matches
            .try_get_one::<String>("input-json-route")
            .ok()
            .flatten();
        let subcommand = matches.subcommand();

        if json_route.is_some() && subcommand.is_some() {
            return Err(self.usage_error("--input-json cannot be combined with a tool subcommand"));
        }

        let (tool_name, arguments) = if let Some(selector) = json_route {
            let tool = self
                .tools
                .iter()
                .find(|tool| &tool.cli_name == selector)
                .expect("Clap validates JSON route tool names");
            let value = read_json_input(&mut input)?;
            (tool.original_name.as_str(), value)
        } else if let Some((selected, tool_matches)) = subcommand {
            let tool = self
                .tools
                .iter()
                .find(|tool| tool.cli_name == selected)
                .expect("Clap validates tool subcommands");
            let root = tool
                .root
                .as_ref()
                .expect("typed subcommand has root schema");
            let arguments = root
                .reconstruct(tool_matches, true)
                .map_err(|message| self.usage_error(message))?
                .expect("root object is always active");
            (tool.original_name.as_str(), arguments)
        } else {
            return Err(self.usage_error("no tool selected"));
        };

        let tool =
            R::parse(tool_name, arguments).map_err(|error| self.usage_error(error.to_string()))?;
        Ok(Invocation { tool, output })
    }

    /// Creates a consistently rendered usage error.
    fn usage_error(&self, message: impl Into<String>) -> CliError {
        CliError::from_clap(
            self.command
                .clone()
                .error(ClapErrorKind::ValueValidation, message.into()),
        )
    }
}

/// Adds registry-oriented CLI construction shorthand.
pub trait ToolRegistryExt: ToolRegistry {
    /// Creates a native CLI builder for this registry type.
    fn cli(name: impl Into<String>) -> CliBuilder<Self> {
        Cli::builder(name)
    }
}

impl<R: ToolRegistry> ToolRegistryExt for R {}

/// Analyzed command and schema for one tool.
struct ToolSpec {
    /// Original MCP tool name.
    original_name: String,
    /// Normalized command name.
    cli_name: String,
    /// Tool description.
    description: String,
    /// Typed root object, or `None` for whole-input-only tools.
    root: Option<ObjectSpec>,
}

/// One statically enumerable object.
struct ObjectSpec {
    /// Original property name, absent for a tool root.
    name: Option<String>,
    /// Display path from the tool root.
    path: String,
    /// Whether this object is required when its parent is active.
    required: bool,
    /// Child properties.
    children: Vec<NodeSpec>,
}

impl ObjectSpec {
    /// Returns whether any descendant option was supplied.
    fn has_input(&self, matches: &ArgMatches) -> bool {
        self.children.iter().any(|child| child.has_input(matches))
    }

    /// Reconstructs this object when selected or required.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<Value>, String> {
        let active =
            self.name.is_none() || (self.required && parent_active) || self.has_input(matches);
        if !active {
            return Ok(None);
        }

        let mut object = Map::new();
        for child in &self.children {
            if let Some((name, value)) = child.reconstruct(matches, true)? {
                object.insert(name, value);
            }
        }
        Ok(Some(Value::Object(object)))
    }
}

/// One schema property represented in a command.
enum NodeSpec {
    /// Statically flattened object.
    Object(ObjectSpec),
    /// Scalar, repeated scalar, or JSON-valued option.
    Value(ValueSpec),
    /// Scalar-versus-object union.
    Union(UnionSpec),
}

impl NodeSpec {
    /// Returns whether this node has supplied input.
    fn has_input(&self, matches: &ArgMatches) -> bool {
        match self {
            Self::Object(object) => object.has_input(matches),
            Self::Value(value) => value.is_present(matches),
            Self::Union(union) => {
                union.scalar.is_present(matches) || union.object.has_input(matches)
            }
        }
    }

    /// Reconstructs this node and its original property name.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<(String, Value)>, String> {
        match self {
            Self::Object(object) => Ok(object
                .reconstruct(matches, parent_active)?
                .map(|value| (object.name.clone().expect("child object has a name"), value))),
            Self::Value(spec) => spec
                .reconstruct(matches, parent_active)
                .map(|result| result.map(|value| (spec.name.clone(), value))),
            Self::Union(union) => union.reconstruct(matches, parent_active),
        }
    }
}

/// One command option and its JSON representation.
struct ValueSpec {
    /// Original property name.
    name: String,
    /// Original dotted property path.
    path: String,
    /// Normalized option name and Clap identifier.
    cli_name: String,
    /// Whether this value is required when its parent is active.
    required: bool,
    /// First optional ancestor that conditionally activates this value.
    conditional_parent: Option<String>,
    /// Input encoding.
    kind: ValueKind,
    /// Source JSON Schema.
    schema: Value,
}

impl ValueSpec {
    /// Returns whether this option occurred.
    fn is_present(&self, matches: &ArgMatches) -> bool {
        matches.value_source(&self.cli_name).is_some()
    }

    /// Reconstructs this option's JSON value.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<Value>, String> {
        let values = matches
            .get_many::<String>(&self.cli_name)
            .map(|values| values.map(String::as_str).collect::<Vec<_>>());
        let Some(values) = values else {
            if self.required && parent_active {
                return Err(format!("missing required option `--{}`", self.cli_name));
            }
            return Ok(None);
        };

        match &self.kind {
            ValueKind::Scalar(kind) => parse_scalar(kind, values[0], &self.cli_name).map(Some),
            ValueKind::Array(kind) => values
                .into_iter()
                .map(|value| parse_scalar(kind, value, &self.cli_name))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
                .map(Some),
            ValueKind::Json(expected) => {
                let value: Value = serde_json::from_str(values[0])
                    .map_err(|error| format!("invalid JSON for `--{}`: {error}", self.cli_name))?;
                if let Some(expected) = expected
                    && !json_has_type(&value, expected)
                {
                    return Err(format!(
                        "invalid JSON for `--{}`: expected {expected}",
                        self.cli_name
                    ));
                }
                Ok(Some(value))
            }
        }
    }
}

/// Encoding used by one option.
enum ValueKind {
    /// One scalar occurrence.
    Scalar(ScalarKind),
    /// Ordered repeated scalar occurrences.
    Array(ScalarKind),
    /// One strict JSON value with an optional expected type.
    Json(Option<&'static str>),
}

/// Supported scalar JSON types.
enum ScalarKind {
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
struct UnionSpec {
    /// Original property name.
    name: String,
    /// Whether one branch is required when the parent is active.
    required: bool,
    /// Scalar parent option.
    scalar: ValueSpec,
    /// Flattened object branch.
    object: ObjectSpec,
}

impl UnionSpec {
    /// Reconstructs exactly one selected union branch.
    fn reconstruct(
        &self,
        matches: &ArgMatches,
        parent_active: bool,
    ) -> Result<Option<(String, Value)>, String> {
        let scalar_selected = self.scalar.is_present(matches);
        let object_selected = self.object.has_input(matches);
        if scalar_selected && object_selected {
            return Err(format!(
                "`--{}` cannot be combined with its object branch options",
                self.scalar.cli_name
            ));
        }
        if scalar_selected {
            let value = self
                .scalar
                .reconstruct(matches, true)?
                .expect("selected scalar union branch has a value");
            return Ok(Some((self.name.clone(), value)));
        }
        if object_selected {
            let value = self
                .object
                .reconstruct(matches, true)?
                .expect("selected object union branch is active");
            return Ok(Some((self.name.clone(), value)));
        }
        if self.required && parent_active {
            return Err(format!(
                "one of `--{}` or its object branch options is required",
                self.scalar.cli_name
            ));
        }
        Ok(None)
    }
}

/// Analyzes one tool definition.
fn analyze_tool(
    definition: ToolDefinition,
    cli_name: String,
    problems: &mut Vec<CliBuildProblem>,
) -> ToolSpec {
    let schema = definition.input_schema_json();
    let mut option_names = BTreeMap::<String, Vec<String>>::new();
    let root = if is_static_object(&schema) {
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
                (!child_required && is_static_object(child_schema)).then(|| child_path.clone())
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
    if let Some(union) = analyze_union(
        name,
        schema,
        &path,
        required,
        optional_parent.clone(),
        option_names,
        problems,
    ) {
        return NodeSpec::Union(union);
    }

    if is_static_object(schema) {
        return NodeSpec::Object(analyze_object(
            schema,
            Some(name.to_string()),
            path,
            required,
            optional_parent,
            option_names,
            problems,
        ));
    }

    NodeSpec::Value(analyze_value(
        name,
        schema,
        path,
        required,
        optional_parent,
        option_names,
        problems,
    ))
}

/// Analyzes a supported scalar-versus-object union.
#[allow(clippy::too_many_arguments)]
fn analyze_union(
    name: &str,
    schema: &Value,
    path: &str,
    required: bool,
    optional_parent: Option<String>,
    option_names: &mut BTreeMap<String, Vec<String>>,
    problems: &mut Vec<CliBuildProblem>,
) -> Option<UnionSpec> {
    let choices = schema.get("oneOf")?.as_array()?;
    if choices.len() != 2 {
        return None;
    }
    let (scalar_schema, object_schema) =
        if scalar_kind(&choices[0]).is_some() && is_nonempty_static_object(&choices[1]) {
            (&choices[0], &choices[1])
        } else if scalar_kind(&choices[1]).is_some() && is_nonempty_static_object(&choices[0]) {
            (&choices[1], &choices[0])
        } else {
            return None;
        };

    let mut scalar_schema = scalar_schema.clone();
    merge_annotations(&mut scalar_schema, schema);
    let scalar = analyze_value(
        name,
        &scalar_schema,
        path.to_string(),
        false,
        optional_parent.clone(),
        option_names,
        problems,
    );
    let object = analyze_object(
        object_schema,
        Some(name.to_string()),
        path.to_string(),
        false,
        Some(optional_parent.unwrap_or_else(|| path.to_string())),
        option_names,
        problems,
    );
    Some(UnionSpec {
        name: name.to_string(),
        required,
        scalar,
        object,
    })
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
fn analyze_value(
    name: &str,
    schema: &Value,
    path: String,
    required: bool,
    optional_parent: Option<String>,
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
        kind: value_kind(schema),
        schema: schema.clone(),
    }
}

/// Returns the option encoding for a schema.
fn value_kind(schema: &Value) -> ValueKind {
    if let Some(kind) = scalar_kind(schema) {
        return ValueKind::Scalar(kind);
    }
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        if let Some(items) = schema.get("items")
            && let Some(kind) = scalar_kind(items)
        {
            return ValueKind::Array(kind);
        }
        return ValueKind::Json(Some("array"));
    }
    let expected = schema
        .get("type")
        .and_then(Value::as_str)
        .and_then(static_json_type);
    ValueKind::Json(expected)
}

/// Returns a supported scalar kind.
fn scalar_kind(schema: &Value) -> Option<ScalarKind> {
    match schema.get("type").and_then(Value::as_str)? {
        "string" => Some(ScalarKind::String(
            schema
                .get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect(),
        )),
        "integer" => Some(ScalarKind::Integer),
        "number" => Some(ScalarKind::Number),
        "boolean" => Some(ScalarKind::Boolean),
        _ => None,
    }
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

/// Returns whether an object can be flattened without losing dynamic keys.
fn is_static_object(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
        && schema.get("properties").is_none_or(Value::is_object)
        && schema.get("$ref").is_none()
        && schema.get("oneOf").is_none()
        && schema.get("anyOf").is_none()
        && schema
            .get("additionalProperties")
            .is_none_or(|value| value == &Value::Bool(false))
}

/// Returns whether a flattenable object has at least one property.
fn is_nonempty_static_object(schema: &Value) -> bool {
    is_static_object(schema)
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty())
}

/// Appends every normalization collision to construction problems.
fn append_collisions(problems: &mut Vec<CliBuildProblem>, names: BTreeMap<String, Vec<String>>) {
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

/// Parses and validates root output options.
fn parse_output_options(matches: &ArgMatches) -> Result<OutputOptions, String> {
    let mode = match matches
        .get_one::<String>("output-mode")
        .map(String::as_str)
        .expect("output mode has a default")
    {
        "artifacts" => OutputMode::Artifacts,
        "structured" => OutputMode::Structured,
        "raw" => OutputMode::Raw,
        "binary" => OutputMode::Binary,
        _ => unreachable!("Clap validates output modes"),
    };
    let images = match matches
        .get_one::<String>("image-mode")
        .map(String::as_str)
        .expect("image mode has a default")
    {
        "auto" => ImageMode::Auto,
        "kitty" => ImageMode::Kitty,
        "off" => ImageMode::Off,
        _ => unreachable!("Clap validates image modes"),
    };
    let artifact_dir = matches.get_one::<String>("artifact-dir").map(PathBuf::from);
    if mode != OutputMode::Artifacts
        && (artifact_dir.is_some()
            || matches.value_source("image-mode") == Some(clap::parser::ValueSource::CommandLine))
    {
        return Err("--artifact-dir and --images require --output artifacts".into());
    }
    Ok(OutputOptions {
        mode,
        images,
        artifact_dir,
    })
}

/// Reads exactly one whole-input JSON object.
fn read_json_input(input: &mut impl Read) -> Result<Value, CliError> {
    let mut deserializer = serde_json::Deserializer::from_reader(input);
    let value = Value::deserialize(&mut deserializer).map_err(|error| {
        if error.is_io() {
            CliError::runtime(format!("failed to read JSON input: {error}"))
        } else {
            CliError {
                kind: CliErrorKind::Usage,
                message: format!("error: invalid JSON input: {error}\n"),
            }
        }
    })?;
    deserializer.end().map_err(|error| CliError {
        kind: CliErrorKind::Usage,
        message: format!("error: trailing JSON input: {error}\n"),
    })?;
    if !value.is_object() {
        return Err(CliError {
            kind: CliErrorKind::Usage,
            message: "error: whole-input JSON must be an object\n".into(),
        });
    }
    Ok(value)
}

/// Parses one strict scalar value.
fn parse_scalar(kind: &ScalarKind, input: &str, cli_name: &str) -> Result<Value, String> {
    match kind {
        ScalarKind::String(_) => Ok(Value::String(input.to_string())),
        ScalarKind::Boolean => match input {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("invalid boolean for `--{cli_name}`")),
        },
        ScalarKind::Integer => {
            let value: Value = serde_json::from_str(input)
                .map_err(|error| format!("invalid integer for `--{cli_name}`: {error}"))?;
            if value.as_i64().is_none() && value.as_u64().is_none() {
                return Err(format!("invalid integer for `--{cli_name}`"));
            }
            Ok(value)
        }
        ScalarKind::Number => {
            let value: Value = serde_json::from_str(input)
                .map_err(|error| format!("invalid number for `--{cli_name}`: {error}"))?;
            if !value.is_number() {
                return Err(format!("invalid number for `--{cli_name}`"));
            }
            Ok(value)
        }
    }
}

/// Returns whether a JSON value has the expected basic type.
fn json_has_type(value: &Value, expected: &str) -> bool {
    match expected {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        _ => true,
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
fn normalize_name(name: &str) -> Result<String, ()> {
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
