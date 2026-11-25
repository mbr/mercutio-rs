//! Native command-line interfaces for tool registries.
//!
//! This module turns a [`ToolRegistry`] into a dynamic `clap` command tree and reconstructs the
//! selected command's arguments into the same JSON object accepted by [`ToolRegistry::parse`].

mod command;
mod input;
mod output;
mod runner;
mod schema;

use std::{
    collections::BTreeMap,
    convert::Infallible,
    error::Error as StdError,
    ffi::OsString,
    fmt,
    io::{self, Read, Write},
    marker::PhantomData,
    path::PathBuf,
};

use clap::{ArgMatches, Command, error::ErrorKind as ClapErrorKind};
use thiserror::Error;

use self::{
    command::root_command,
    input::{parse_output_options, read_json_input},
    schema::{ToolSpec, analyze_tool, append_collisions, normalize_name},
};
use crate::ToolRegistry;

/// Extracts application context from a native CLI invocation.
///
/// [`augment_command`](Self::augment_command) adds context options to the root
/// command, separate from tool arguments. [`from_cli`](Self::from_cli) reads the
/// parsed matches and constructs the context passed by reference to the handler.
/// Extraction runs once after successful tool parsing, not for help or version
/// requests; an extraction error prevents dispatch. `()` adds no options.
///
/// Select the context type with [`CliBuilder::context`]:
///
/// ```
/// use std::{convert::Infallible, io};
///
/// use clap::{Arg, ArgMatches, Command};
/// use mercutio::cli::{FromCli, ToolRegistryExt as _};
///
/// mercutio::tool_registry! {
///     enum Tools {
///         List("list", "Lists project issues") {},
///     }
/// }
///
/// struct ProjectContext {
///     project: String,
/// }
///
/// impl FromCli for ProjectContext {
///     type Error = Infallible;
///
///     fn augment_command(command: Command) -> Command {
///         command.arg(Arg::new("project").long("project").required(true))
///     }
///
///     fn from_cli(matches: &ArgMatches) -> Result<Self, Self::Error> {
///         Ok(Self {
///             project: matches.get_one::<String>("project")
///                 .expect("required by Clap").clone(),
///         })
///     }
/// }
///
/// let cli = Tools::cli("issues").context::<ProjectContext>().build()?;
/// cli.run_on(
///     ["issues", "--project", "marc/general", "list"],
///     io::empty(), io::sink(), io::sink(),
///     |context, _session, _tool| {
///         assert_eq!(context.project, "marc/general");
///         Ok::<_, Infallible>("listed")
///     },
/// )?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait FromCli: Sized {
    /// Error returned when invocation context cannot be constructed.
    type Error: StdError + Send + Sync + 'static;

    /// Adds application context options to the generated root command.
    ///
    /// The default leaves the command unchanged.
    fn augment_command(command: Command) -> Command {
        command
    }

    /// Constructs context from matches rooted at the generated command.
    ///
    /// Errors become CLI usage errors and prevent handler execution.
    fn from_cli(matches: &ArgMatches) -> Result<Self, Self::Error>;
}

impl FromCli for () {
    type Error = Infallible;

    fn from_cli(_matches: &ArgMatches) -> Result<Self, Self::Error> {
        Ok(())
    }
}

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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OutputOptions {
    /// Selected output representation.
    pub mode: OutputMode,
    /// Selected inline image policy.
    pub images: ImageMode,
    /// Optional parent directory for filesystem artifacts.
    pub artifact_dir: Option<PathBuf>,
}

/// One parsed native tool invocation.
pub struct Invocation<R: ToolRegistry, C = ()> {
    /// Parsed registry value.
    tool: R,
    /// Application context extracted for this invocation.
    context: C,
    /// Selected output settings.
    output: OutputOptions,
}

impl<R: ToolRegistry, C> Invocation<R, C> {
    /// Returns the tool, discarding context and output settings.
    pub fn into_tool(self) -> R {
        self.tool
    }

    /// Consumes the invocation and returns its tool, context, and output settings.
    pub fn into_parts(self) -> (R, C, OutputOptions) {
        (self.tool, self.context, self.output)
    }

    /// Returns the application context extracted for this invocation.
    pub fn context(&self) -> &C {
        &self.context
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
                    .join(", "),
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
pub struct CliBuildError {
    /// Every detected construction problem.
    problems: Vec<CliBuildProblem>,
}

impl fmt::Display for CliBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cannot construct CLI: ")?;
        for (index, problem) in self.problems.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            fmt::Display::fmt(problem, f)?;
        }
        Ok(())
    }
}

impl CliBuildError {
    /// Creates an aggregate construction error.
    fn new(problems: Vec<CliBuildProblem>) -> Self {
        Self { problems }
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
pub struct CliBuilder<R: ToolRegistry, C = ()> {
    /// Root command name.
    name: String,
    /// Optional root version.
    version: Option<String>,
    /// Optional application summary.
    about: Option<String>,
    /// Optional application reference.
    instructions: Option<String>,
    /// Registry and invocation context types.
    marker: PhantomData<fn() -> (R, C)>,
}

impl<R: ToolRegistry, C> CliBuilder<R, C> {
    /// Selects the application context extracted from native invocations.
    pub fn context<C2: FromCli>(self) -> CliBuilder<R, C2> {
        CliBuilder {
            name: self.name,
            version: self.version,
            about: self.about,
            instructions: self.instructions,
            marker: PhantomData,
        }
    }

    /// Sets an application summary for short and long root help.
    ///
    /// Recommended for application-specific help. Omitted or whitespace-only summaries
    /// produce no introductory description.
    pub fn about(mut self, about: impl Into<String>) -> Self {
        self.about = Some(about.into());
        self
    }

    /// Sets the application reference shown after the summary in long root help.
    ///
    /// Recommended for usage guidance; the same text can be passed to
    /// [`crate::McpServerBuilder::instructions`]. Omitted or whitespace-only instructions
    /// produce no reference. Nonempty text is retained without Markdown rendering.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Sets the root command version.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Builds the generated command and schema reconstruction data.
    pub fn build(self) -> Result<Cli<R, C>, CliBuildError>
    where
        C: FromCli,
    {
        Cli::from_builder(self)
    }
}

/// Generated native command-line adapter for a [`ToolRegistry`].
pub struct Cli<R: ToolRegistry, C = ()> {
    /// Generated root command.
    command: Command,
    /// Analyzed tool definitions.
    tools: Vec<ToolSpec>,
    /// Registry and invocation context types.
    marker: PhantomData<fn() -> (R, C)>,
}

impl<R: ToolRegistry, C: FromCli> Cli<R, C> {
    /// Creates a builder for a standalone or nested generated command.
    pub fn builder(name: impl Into<String>) -> CliBuilder<R, C> {
        CliBuilder {
            name: name.into(),
            version: None,
            about: None,
            instructions: None,
            marker: PhantomData,
        }
    }

    /// Builds a CLI from its public builder.
    fn from_builder(builder: CliBuilder<R, C>) -> Result<Self, CliBuildError> {
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

        let command = C::augment_command(root_command(root_name, builder, &tools));
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
        if command.get_subcommands().any(|child| {
            child.get_name() == name || child.get_all_aliases().any(|alias| alias == name)
        }) {
            return Err(CliBuildError::new(vec![
                CliBuildProblem::ApplicationCommandCollision {
                    name: name.to_string(),
                },
            ]));
        }
        Ok(command.subcommand(self.command.clone()))
    }

    /// Parses an explicit argument iterator and injected whole-input reader.
    pub fn try_parse_from<I, T, S>(&self, args: I, input: S) -> Result<Invocation<R, C>, CliError>
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
    pub fn try_parse(&self) -> Result<Invocation<R, C>, CliError> {
        self.try_parse_from(std::env::args_os(), std::io::stdin().lock())
    }

    /// Reconstructs an invocation from matches rooted at the generated command.
    pub fn try_parse_matches(
        &self,
        matches: &ArgMatches,
        mut input: impl Read,
    ) -> Result<Invocation<R, C>, CliError> {
        let output = parse_output_options(matches).map_err(|message| self.usage_error(message))?;
        let json_route = matches
            .try_get_one::<String>("input-json-route")
            .ok()
            .flatten();
        let (selected, tool_matches) = match (json_route, matches.subcommand()) {
            (Some(_), Some(_)) => {
                return Err(
                    self.usage_error("--input-json cannot be combined with a tool subcommand")
                );
            }
            (Some(selector), None) => (selector.as_str(), None),
            (None, Some((selected, tool_matches))) => (selected, Some(tool_matches)),
            (None, None) => return Err(self.usage_error("no tool selected")),
        };
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.cli_name == selected)
            .expect("Clap validates tool selectors");
        let arguments = if let Some(tool_matches) = tool_matches {
            tool.root
                .as_ref()
                .expect("typed subcommand has root schema")
                .reconstruct(tool_matches, true)
                .map_err(|message| self.usage_error(message))?
                .expect("root object is always active")
        } else {
            read_json_input(&mut input)?
        };

        let tool = R::parse(&tool.original_name, arguments)
            .map_err(|error| self.usage_error(error.to_string()))?;
        let context = C::from_cli(matches).map_err(|error| self.usage_error(error.to_string()))?;
        Ok(Invocation {
            tool,
            context,
            output,
        })
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
