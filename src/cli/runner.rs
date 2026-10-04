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
    /// Parses process arguments, invokes a synchronous handler, and renders to process streams.
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

    /// Parses process arguments, invokes an async handler, and renders to process streams.
    pub async fn run_async<H>(&self, handler: &mut H) -> Result<(), CliError>
    where
        H: MutToolHandler<R>,
    {
        let (tool, options) = self.try_parse()?.into_parts();
        render_process_result(&options, handler.handle(None, tool).await)
    }

    /// Parses explicit arguments, invokes an async handler, and renders to injected streams.
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
