//! Invokes local handlers and binds parsing and rendering to caller-owned streams.

use std::{
    ffi::OsString,
    fmt,
    io::{IsTerminal, Read, Write},
};

use super::{
    Cli, CliError, OutputOptions,
    output::{kitty_terminal, render_output},
};
use crate::{
    ToolOutput, ToolRegistry,
    io::{McpSessionId, MutToolHandler},
};

impl<R: ToolRegistry> Cli<R> {
    /// Runs a synchronous handler, exiting after help or errors.
    ///
    /// Returns normally on success. Prints help and version to stdout before
    /// exiting with status `0`, or errors to stderr with status `2` for invalid
    /// input and `1` for runtime failures. A diagnostic write failure uses `1`.
    /// Exiting skips destructors; use [`Self::run`] to retain control and cleanup.
    pub fn run_or_exit<H, T, E>(&self, handler: H)
    where
        H: FnOnce(Option<McpSessionId>, R) -> Result<T, E>,
        T: Into<ToolOutput>,
        E: fmt::Display,
    {
        exit_on_error(self.run(handler));
    }

    /// Parses process arguments, invokes a synchronous handler, and renders to process streams.
    ///
    /// Parsing completes before invoking the handler. Initialize resources in the
    /// closure to keep help and input validation independent of configuration and
    /// external services.
    pub fn run<H, T, E>(&self, handler: H) -> Result<(), CliError>
    where
        H: FnOnce(Option<McpSessionId>, R) -> Result<T, E>,
        T: Into<ToolOutput>,
        E: fmt::Display,
    {
        let (tool, options) = self.try_parse()?.into_parts();
        render_process_result(&options, handler(None, tool))
    }

    /// Parses explicit arguments, invokes a synchronous handler, and renders to injected streams.
    ///
    /// Like [`Self::run`], invokes the handler only after successful parsing.
    pub fn run_on<I, A, S, O, D, H, T, E>(
        &self,
        args: I,
        input: S,
        stdout: O,
        stderr: D,
        handler: H,
    ) -> Result<(), CliError>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString> + Clone,
        S: Read,
        O: Write,
        D: Write,
        H: FnOnce(Option<McpSessionId>, R) -> Result<T, E>,
        T: Into<ToolOutput>,
        E: fmt::Display,
    {
        let (tool, options) = self.try_parse_from(args, input)?.into_parts();
        render_result(&options, handler(None, tool), stdout, stderr, false)
    }

    /// Runs an async handler, exiting after help or errors.
    ///
    /// Uses the same output and exit semantics as [`Self::run_or_exit`], returning
    /// normally on success. Use [`Self::run_async`] to retain control and cleanup.
    pub async fn run_async_or_exit<H>(&self, handler: &mut H)
    where
        H: MutToolHandler<R>,
    {
        exit_on_error(self.run_async(handler).await);
    }

    /// Parses process arguments, invokes an async handler, and renders to process streams.
    ///
    /// Parsing completes before invocation, but not before handler construction.
    /// Initialize resources inside a handler closure, or use [`Self::try_parse`]
    /// before constructing a handler for custom dispatch and rendering.
    pub async fn run_async<H>(&self, handler: &mut H) -> Result<(), CliError>
    where
        H: MutToolHandler<R>,
    {
        let (tool, options) = self.try_parse()?.into_parts();
        render_process_result(&options, handler.handle(None, tool).await)
    }

    /// Parses explicit arguments, invokes an async handler, and renders to injected streams.
    ///
    /// Like [`Self::run_async`], invokes the handler only after successful parsing.
    pub async fn run_async_on<I, A, S, O, D, H>(
        &self,
        args: I,
        input: S,
        stdout: O,
        stderr: D,
        handler: &mut H,
    ) -> Result<(), CliError>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString> + Clone,
        S: Read,
        O: Write,
        D: Write,
        H: MutToolHandler<R>,
    {
        let (tool, options) = self.try_parse_from(args, input)?.into_parts();
        let result = handler.handle(None, tool).await;
        render_result(&options, result, stdout, stderr, false)
    }
}

/// Prints a returned diagnostic or display request and exits with its status.
fn exit_on_error(result: Result<(), CliError>) {
    let Err(error) = result else {
        return;
    };
    let status = {
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let written = error.write_to(&mut stdout, &mut stderr).and_then(|()| {
            if error.targets_stderr() {
                stderr.flush()
            } else {
                stdout.flush()
            }
        });
        if written.is_ok() {
            error.exit_code()
        } else {
            1
        }
    };
    std::process::exit(i32::from(status));
}

/// Binds rendering to process streams without holding their locks during handler execution.
fn render_process_result(
    options: &OutputOptions,
    result: Result<impl Into<ToolOutput>, impl fmt::Display>,
) -> Result<(), CliError> {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let terminal_images = stdout.is_terminal() && kitty_terminal();
    render_result(
        options,
        result,
        stdout.lock(),
        stderr.lock(),
        terminal_images,
    )
}

/// Converts a handler result and renders it with shared error semantics.
fn render_result(
    options: &OutputOptions,
    result: Result<impl Into<ToolOutput>, impl fmt::Display>,
    mut stdout: impl Write,
    mut stderr: impl Write,
    terminal_images: bool,
) -> Result<(), CliError> {
    let output = result
        .map(Into::into)
        .map_err(|error| CliError::runtime(format!("tool handler failed: {error}")))?;
    render_output(options, &output, &mut stdout, &mut stderr, terminal_images)
}
