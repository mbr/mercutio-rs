//! Exercises process-facing CLI runners without passing test-harness arguments to Clap.

#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::{
    env,
    io::Write,
    process::{Command, Output, Stdio},
};

use mercutio::{ToolDef, cli::ToolRegistryExt as _, io::McpSessionId};
use schemars::JsonSchema;
use serde::Deserialize;

/// Selects a child runner without changing the arguments seen by the CLI.
const RUNNER_ENV: &str = "MERCUTIO_TEST_CLI_RUNNER";

/// Echoes text or requests a handler failure.
#[derive(Deserialize, JsonSchema)]
struct Echo {
    /// Text to echo.
    message: String,
}

impl ToolDef for Echo {
    const NAME: &'static str = "echo";
    const DESCRIPTION: &'static str = "Echoes a message";
}

/// Makes normal stack cleanup observable without altering tool output.
struct CleanupMarker;

impl Drop for CleanupMarker {
    fn drop(&mut self) {
        eprintln!("cleanup ran");
    }
}

/// Shares handler behavior between synchronous and asynchronous child runners.
fn handle(session: Option<McpSessionId>, input: Echo) -> Result<String, &'static str> {
    assert!(session.is_none());
    if input.message == "fail" {
        Err("handler failed")
    } else {
        Ok(input.message)
    }
}

/// Runs the CLI as the child process's application entry point.
fn run_child(mode: &str) {
    let _cleanup = CleanupMarker;
    let cli = Echo::cli("echo-tools")
        .version("1.0")
        .build()
        .expect("valid test CLI");
    match mode {
        "sync" => cli.run_or_exit(handle),
        "async" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("test runtime");
            let mut handler = |session, input| async move { handle(session, input) };
            runtime.block_on(cli.run_async_or_exit(&mut handler));
        }
        _ => panic!("unknown test runner"),
    }
}

/// Creates an isolated child with captured output and no interactive input.
fn command(mode: &str) -> Command {
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .env(RUNNER_ENV, mode)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Runs a child with the selected arguments and whole-input JSON source.
fn invoke(mode: &str, args: &[&str], input: &str) -> Output {
    let mut child = command(mode)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .expect("child process");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write child input");
    child.wait_with_output().expect("child output")
}

/// Checks exit statuses, diagnostic destinations, and normal cleanup on success.
fn check_runner(mode: &str) {
    for (args, input, status, stdout, stderr) in [
        (vec!["-h"], "", 0, "Usage:", ""),
        (vec!["--help"], "", 0, "Usage:", ""),
        (vec!["--version"], "", 0, "1.0", ""),
        (vec!["echo"], "", 2, "", "--message"),
        (
            vec!["--input-json", "echo"],
            r#"{"message":42}"#,
            2,
            "",
            "error:",
        ),
        (
            vec!["echo", "--message", "fail"],
            "",
            1,
            "",
            "handler failed",
        ),
        (
            vec!["--output", "structured", "echo", "--message", "hello"],
            "",
            1,
            "",
            "no structuredContent",
        ),
        (
            vec!["echo", "--message", "hello"],
            "",
            0,
            "hello\n",
            "cleanup ran\n",
        ),
        (
            vec!["--input-json", "echo"],
            r#"{"message":"hello"}"#,
            0,
            "hello\n",
            "cleanup ran\n",
        ),
    ] {
        let output = invoke(mode, &args, input);
        assert_eq!(
            output.status.code(),
            Some(status),
            "{mode} {args:?}: {output:?}"
        );
        for (actual, expected) in [(output.stdout, stdout), (output.stderr, stderr)] {
            let actual = String::from_utf8(actual).expect("UTF-8 output");
            if expected.is_empty() {
                assert!(actual.is_empty(), "{mode} {args:?}: {actual}");
            } else {
                assert!(actual.contains(expected), "{mode} {args:?}: {actual}");
            }
        }
    }
}

/// Overrides the intended status when the selected diagnostic stream cannot write.
#[cfg(target_os = "linux")]
fn check_write_failures(mode: &str) {
    for args in [vec!["--help"], vec!["echo"]] {
        let full = OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("failing output device");
        let mut child = command(mode);
        child.args(&args);
        if args == ["--help"] {
            child.stdout(full);
        } else {
            child.stderr(full);
        }
        let output = child.output().expect("child output");
        assert_eq!(output.status.code(), Some(1), "{mode} {args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

/// Runs the subprocess checks, or acts as their CLI fixture when selected.
fn main() {
    if let Ok(mode) = env::var(RUNNER_ENV) {
        run_child(&mode);
    } else {
        for mode in ["sync", "async"] {
            check_runner(mode);
            #[cfg(target_os = "linux")]
            check_write_failures(mode);
        }
    }
}
