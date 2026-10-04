//! Invokes local handlers and binds parsing and rendering to caller-owned streams.

use std::{
    ffi::OsString,
    fmt,
    io::{IsTerminal, Read, Write},
};

use super::{
    Cli, CliError,
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
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let stderr = std::io::stderr();
        let terminal_images = stdout.is_terminal() && kitty_terminal();
        self.run_sync_with(
            std::env::args_os(),
            stdin.lock(),
            stdout.lock(),
            stderr.lock(),
            handler,
            terminal_images,
        )
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
        self.run_sync_with(args, input, stdout, stderr, handler, false)
    }

    /// Shares synchronous runner behavior with process and injected stream forms.
    #[allow(clippy::too_many_arguments)]
    fn run_sync_with<I, A, S, O, D, H, T, E>(
        &self,
        args: I,
        input: S,
        mut stdout: O,
        mut stderr: D,
        handler: H,
        terminal_images: bool,
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
        let invocation = self.try_parse_from(args, input)?;
        let (tool, options) = invocation.into_parts();
        let output = handler(None, tool)
            .map(Into::into)
            .map_err(|error| CliError::runtime(format!("tool handler failed: {error}")))?;
        render_output(&options, &output, &mut stdout, &mut stderr, terminal_images)
    }

    /// Parses process arguments, invokes an async handler, and renders to process streams.
    pub async fn run_async<H>(&self, handler: &mut H) -> Result<(), CliError>
    where
        H: MutToolHandler<R>,
    {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let stderr = std::io::stderr();
        let terminal_images = stdout.is_terminal() && kitty_terminal();
        self.run_async_with(
            std::env::args_os(),
            stdin.lock(),
            stdout.lock(),
            stderr.lock(),
            handler,
            terminal_images,
        )
        .await
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
        self.run_async_with(args, input, stdout, stderr, handler, false)
            .await
    }

    /// Shares asynchronous runner behavior with process and injected stream forms.
    #[allow(clippy::too_many_arguments)]
    async fn run_async_with<I, A, S, O, D, H>(
        &self,
        args: I,
        input: S,
        mut stdout: O,
        mut stderr: D,
        handler: &mut H,
        terminal_images: bool,
    ) -> Result<(), CliError>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString> + Clone,
        S: Read,
        O: Write,
        D: Write,
        H: MutToolHandler<R>,
    {
        let invocation = self.try_parse_from(args, input)?;
        let (tool, options) = invocation.into_parts();
        let output = handler
            .handle(None, tool)
            .await
            .map_err(|error| CliError::runtime(format!("tool handler failed: {error}")))?;
        render_output(&options, &output, &mut stdout, &mut stderr, terminal_images)
    }
}
