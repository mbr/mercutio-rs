# mercutio

A Rust library for building [MCP](https://modelcontextprotocol.io/) servers. In MCP, *clients* are LLM host applications (IDEs, chat interfaces) that connect to *servers* to give models access to tools. `mercutio` handles the server-side protocol (parsing messages, managing the initialization handshake, dispatching tool calls), while you handle the transport. The core is a pure state machine: feed it JSON-RPC messages, and it returns what to send back.

This [sans-io](https://www.firezone.dev/blog/sans-io) design means you can run it over stdio, HTTP, WebSockets, or anything else without fighting the library.

## Defining Tools

Use `tool_registry!` to define your tools. Field doc comments become JSON Schema descriptions that the LLM sees:

```rust,ignore
mercutio::tool_registry! {
    enum MyTools {
        GetWeather("get_weather", "Gets current weather for a city") {
            /// City name, e.g. "Llanfairpwllgwyngyllgogerychwyrndrobwllllantysiliogogogoch".
            city: String,
        },
        SetReminder("set_reminder", "Sets a reminder") {
            /// What to remind about.
            message: String,
            /// When to trigger the reminder.
            at: mercutio::Rfc3339,
            /// Minutes to wait before reminding again.
            snooze_minutes: u32,
        },
    }
}
```

`Rfc3339` requires either the `jiff` or `chrono` feature. It emits `format: "date-time"` in JSON Schema, and deserialization errors include the current time as an example to help models self-correct.

## Native CLI

Enable the `cli` feature to expose the same registry and handler as a native command-line
application. Commands invoke the handler locally, without starting or connecting to an MCP server.
The generated command uses conventional kebab-case spellings while retaining the original tool
names internally:

```rust,ignore
use std::{convert::Infallible, process::ExitCode};
use mercutio::cli::{CliError, ToolRegistryExt as _};

/// Guides callers using weather queries and reminders.
const INSTRUCTIONS: &str = "Specify a city for weather queries. Reminder times must include a UTC offset.";

fn main() -> ExitCode {
    let cli = MyTools::cli("my-tools")
        .about("Check the weather and set reminders")
        .instructions(INSTRUCTIONS)
        .version("1.0.0")
        .build()
        .expect("tool definitions must produce a valid CLI");
    let result = cli.run(|_session_id, tool| -> Result<String, Infallible> {
        Ok(match tool {
            MyTools::GetWeather(input) => format!("Weather in {}: sunny", input.city),
            MyTools::SetReminder(input) => format!("Reminder set: {}", input.message),
        })
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_cli_error(error),
    }
}

/// Writes a CLI diagnostic or help request and preserves its exit status.
fn report_cli_error(error: CliError) -> ExitCode {
    match error.write_to(std::io::stdout().lock(), std::io::stderr().lock()) {
        Ok(()) => ExitCode::from(error.exit_code()),
        Err(_) => ExitCode::FAILURE,
    }
}
```

The registry above produces a complete standalone CLI:

```console
$ my-tools -h
Check the weather and set reminders

Usage: my-tools [OPTIONS] [COMMAND]

Commands:
  get-weather   Gets current weather for a city
  set-reminder  Sets a reminder

Options:
      --input-json <COMMAND>  Read arguments from stdin as JSON
  -h, --help                  Print help (see more with '--help')
  -V, --version               Print version

Output options (before the command):
      --output <MODE>  Choose output format

Use --help for the full reference.

$ my-tools get-weather -h
Gets current weather for a city

Usage: my-tools get-weather --city <STRING>

Options:
      --city <STRING>  City name, e.g. "Llanfairpwllgwyngyllgogerychwyrndrobwllllantysiliogogogoch".
  -h, --help           Print help (see more with '--help')
```

Both `.about(...)` and `.instructions(...)` are optional and recommended. Without them,
help starts with usage, commands, and options rather than a generic description. `-h`
is the compact command index; `--help` shows the summary and complete instructions first,
followed by usage, commands, and all options. Output defaults, accepted modes, `--images`,
and `--artifact-dir` are documented in long help. Command-specific help does not repeat
application instructions.

Reuse the same `INSTRUCTIONS` value for MCP initialization:

```rust,ignore
let server = mercutio::McpServer::<MyTools>::builder()
    .name("my-tools-mcp")
    .version("1.0.0")
    .instructions(INSTRUCTIONS)
    .build();
```

For longer references, define `INSTRUCTIONS` with `include_str!("instructions.md")` and
pass it to both builders. The document stays single-sourced; the CLI does not depend on
a server instance.

Schema properties become named options. Required options are enforced, string enums become exact
possible values, booleans accept `--recursive`, `--recursive=true`, and `--recursive=false`, and
scalar arrays repeat without comma splitting:

```console
my-tools search --query rust --tags mcp --tags rust --recursive=false
```

Statically described objects are flattened by property path. Dynamic objects and complex arrays
remain strict JSON values. A supported scalar-versus-object `oneOf` uses either the parent option
or its descendant options, never both:

```console
my-tools search --filter-range-min 1 --filter-range-max 10
my-tools deploy --environment '{"RUST_LOG":"info","PORT":"8080"}'
my-tools apply --rules '[{"path":"src","allow":true}]'
my-tools search --filter 'recent items'
my-tools search --filter-tags rust --filter-range-min 1
```

For stable scripting and schemas that cannot be represented completely as options, select the
original tool by its normalized name and pipe exactly one JSON object through stdin:

```console
printf '%s' '{"city":"Berlin"}' | my-tools --input-json get-weather
my-tools --output structured --input-json get-weather < arguments.json
```

Presentation options are root-scoped and must precede the tool command. Artifact output is the
default: text is printed normally, while images, audio, and embedded blobs are decoded into a
private unique temporary directory and represented by absolute paths. Agents can select a durable
parent and disable terminal image presentation explicitly:

```console
my-tools --artifact-dir .pi/artifacts --images off create-chart --title Quarterly
```

`--images kitty` forces inline PNG display while retaining the artifact path. `--images auto`
displays only on a compatible process terminal and remains off for pipes and injected writers.
The other output modes are intended for scripts:

```console
my-tools --output structured report > report.json
my-tools --output raw report > complete-mcp-result.json
my-tools --output binary create-chart > chart.png
```

`structured` writes only `structuredContent`; `raw` preserves the complete MCP result, including
base64; and `binary` requires exactly one binary block and writes its decoded bytes without
framing. In binary mode, accompanying text is written to stderr.

The parser and runners never terminate the process. [`CliError`](https://docs.rs/mercutio/latest/mercutio/cli/struct.CliError.html)
reports status `0` for help and version, `2` for usage and input failures, and `1` for handler,
rendering, decoding, filesystem, and stream failures. Successful payloads go to stdout and
diagnostics go to stderr. Returned errors, including help requests, remain unprinted until the
caller renders them. The `report_cli_error` helper above selects the correct stream and preserves
the exit status; a failed write returns status `1`. Do not propagate `CliError` with `?` from
`main() -> Result`: that reports help as an error and loses the distinction between usage and
runtime failures. The same helper works with synchronous runners, asynchronous runners, and
parse-only calls.

Parse-only applications can use `try_parse_from` or `try_parse_matches`, invoke the returned typed
tool directly, and provide custom rendering.

### Initialize after parsing

Keep help and input validation independent of application configuration and external services.
The runners parse arguments before invoking the handler, but cannot defer work done while
constructing it. Load configuration and connect inside the handler closure, not before calling
the runner. For example, with application-owned `Config` and `DatabaseHandler` types:

```rust,ignore
use mercutio::io::{McpSessionId, ToolHandler as _};

let mut handler = |session_id: Option<McpSessionId>, tool: MyTools| async move {
    let config = Config::load()?;
    let handler = DatabaseHandler::connect(&config).await?;
    handler.handle(session_id, tool).await
};
let result = cli.run_async(&mut handler).await;
```

Match `result` as in the standalone example, returning `ExitCode::SUCCESS` or
`report_cli_error(error)`. For custom dispatch or rendering, use
`try_parse()` or `try_parse_matches()` first, then initialize resources only after obtaining
an `Invocation`. This also keeps malformed arguments from opening a database connection.
Embedded instructions via `include_str!` require no runtime file access.

### Nesting in an application

Use `attach_to` when native tools share a binary with MCP transports. The entire generated tree is
placed under the name supplied to `cli`; collisions with application commands are construction
errors. The following fragment belongs in an entry point returning `ExitCode` and reuses
`report_cli_error` above. `dispatch_native`, `run_stdio_mcp`, and `run_http_mcp` stand for
application-owned functions returning `ExitCode`; they initialize resources and handle execution
failures after parsing:

```rust,ignore
use std::process::ExitCode;
use mercutio::cli::ToolRegistryExt as _;

let tools = MyTools::cli("tool")
    .about("Check the weather and set reminders")
    .instructions(INSTRUCTIONS)
    .version("1.0.0")
    .build()
    .expect("tool definitions must produce a valid CLI");
let command = tools
    .attach_to(
        clap::Command::new("my-app")
            .subcommand_required(true)
            .subcommand(clap::Command::new("mcp"))
            .subcommand(
                clap::Command::new("mcp-http")
                    .arg(clap::Arg::new("bind").long("bind").required(true)),
            ),
    )
    .expect("application commands must not collide");
let matches = match command.try_get_matches() {
    Ok(matches) => matches,
    Err(error) => {
        return match error.print() {
            Ok(()) => ExitCode::from(error.exit_code() as u8),
            Err(_) => ExitCode::FAILURE,
        };
    }
};

match matches.subcommand() {
    Some(("tool", matches)) => {
        let invocation = match tools.try_parse_matches(matches, std::io::stdin().lock()) {
            Ok(invocation) => invocation,
            Err(error) => return report_cli_error(error),
        };
        let (tool, output_options) = invocation.into_parts();
        dispatch_native(tool, output_options)
    }
    Some(("mcp", _)) => run_stdio_mcp(),
    Some(("mcp-http", matches)) => {
        let bind = matches.get_one::<String>("bind").expect("required bind address");
        run_http_mcp(bind)
    }
    _ => unreachable!("Clap validates subcommands"),
}
```

Construct the application handler inside the selected branch, after `try_parse_matches()`
succeeds for native commands. Do not load configuration or open connections before parsing
the application command tree; help for either the application or the `tool` subtree should
work without them.

A small test that calls `MyTools::cli("tool").build()` is recommended. It catches lossy naming
collisions such as `filter_tags` versus `filter.tags`, unsupported protocol names, and the reserved
`help` spelling when schemas change.

## Sans-IO Usage

The core API is a state machine. Pass in parsed messages, match on the output:

```rust,ignore
use mercutio::{McpServer, Output};

let mut server = McpServer::<MyTools>::builder()
    .name("my-server")
    .version("1.0")
    .build();

loop {
    let line = read_line_somehow();
    let msg = mercutio::parse_line(&line)?;

    match server.handle(msg) {
        Output::Send(response) => send(response.into_inner()),
        Output::ToolCall { tool, responder } => {
            let result = match tool {
                MyTools::GetWeather(input) => format!("Weather in {}: sunny", input.city),
                MyTools::SetReminder(input) => format!("Reminder set: {}", input.message),
            };
            send(responder.respond(Ok::<_, std::convert::Infallible>(result)).into_inner());
        }
        Output::ProtocolError(_) => break,
        Output::None => {}
    }
}
```

## Transports

If you'd rather not wire up I/O yourself, the `io-*` feature flags provide ready-made transports. These use handler traits to process tool calls:

```rust,ignore
use mercutio::{ToolOutput, io::{McpSessionId, ToolHandler}};

struct MyHandler;

impl ToolHandler<MyTools> for MyHandler {
    type Error = std::convert::Infallible;

    async fn handle(
        &self,
        _session_id: Option<McpSessionId>,
        tool: MyTools,
    ) -> Result<ToolOutput, Self::Error> {
        match tool {
            MyTools::GetWeather(input) => {
                Ok(format!("Weather in {}: sunny", input.city).into())
            }
            MyTools::SetReminder(input) => {
                Ok(format!("Reminder set: {}", input.message).into())
            }
        }
    }
}
```

`ToolHandler` takes `&self` for concurrent contexts; `MutToolHandler` takes `&mut self` for exclusive access. The session ID is `Some` for HTTP (multiple clients share one server), `None` for stdio (one process = one session). Closures work via blanket impl: `|_session_id, tool| async move { ... }`.

### io-tokio

Async stdin/stdout using Tokio:

```rust,ignore
let server = McpServer::<MyTools>::builder().name("my-server").version("1.0").build();
mercutio::io::tokio::run_stdio(server, MyHandler).await?;
```

### io-stdlib

Synchronous stdin/stdout (no async runtime):

```rust,ignore
let server = McpServer::<MyTools>::builder().name("my-server").version("1.0").build();
mercutio::io::stdlib::run_stdio(server, |_session_id, tool| handle_tool(tool))?;
```

### io-axum

HTTP transport with session management:

```rust,ignore
let mut builder = McpServer::<MyTools>::builder();
builder.name("my-server").version("1.0");

let router = mercutio::io::axum::mcp_router(builder, MyHandler);
let app = axum::Router::new().nest("/mcp", router);
```

For custom session storage, use `McpRouter::builder()` with `.storage()`.

## Testing

To test your handler, construct it with test fixtures and call `handle` directly:

```rust
use mercutio::{ToolOutput, ToolRegistry, io::ToolHandler};

mercutio::tool_registry! {
    enum Tools {
        Greet("greet", "Greets someone") {
            name: String,
        },
    }
}

struct Handler;

impl ToolHandler<Tools> for Handler {
    type Error = std::convert::Infallible;

    async fn handle(&self, _: Option<mercutio::io::McpSessionId>, tool: Tools) -> Result<ToolOutput, Self::Error> {
        match tool {
            Tools::Greet(g) => Ok(format!("Hello, {}!", g.name).into()),
        }
    }
}

# let rt = tokio::runtime::Runtime::new().unwrap();
# rt.block_on(async {
let handler = Handler;
let tool = Tools::Greet(Greet { name: "Alice".into() });

let output = handler.handle(None, tool).await.expect("handler failed");
assert_eq!(output.as_text(), Some("Hello, Alice!"));

// Tool outputs are text blocks that can grow large; insta snapshots help manage them:
insta::assert_snapshot!(output, @"Hello, Alice!");
# });
```

To test that invalid inputs produce useful error messages, use [`ToolRegistry::parse`]:

```rust
use mercutio::ToolRegistry;

mercutio::tool_registry! {
    enum Tools {
        Greet("greet", "Greets someone") { name: String },
    }
}

let err = Tools::parse("greet", serde_json::json!({})).err().expect("should fail");
assert!(err.to_string().contains("name"));
```

### Example

A complete server supporting both transports:

```rust,ignore
use clap::{Parser, Subcommand};
use mercutio::{McpServer, ToolOutput, io::{McpSessionId, ToolHandler}};

mercutio::tool_registry! {
    enum MyTools {
        Greet("greet", "Greets someone") { name: String },
    }
}

struct MyHandler;

impl ToolHandler<MyTools> for MyHandler {
    type Error = std::convert::Infallible;

    async fn handle(&self, _: Option<McpSessionId>, tool: MyTools) -> Result<ToolOutput, Self::Error> {
        match tool {
            MyTools::Greet(input) => Ok(format!("Hello, {}!", input.name).into()),
        }
    }
}

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Mcp,
    McpHttp { bind: std::net::SocketAddr },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut builder = McpServer::<MyTools>::builder();
    builder.name("greeter").version("1.0");

    match args.command {
        Command::Mcp => {
            mercutio::io::tokio::run_stdio(builder.build(), MyHandler).await?;
        }
        Command::McpHttp { bind } => {
            let router = mercutio::io::axum::mcp_router(builder, MyHandler);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            axum::serve(listener, router).await?;
        }
    }
    Ok(())
}
```

## Feature Flags

| Feature | Description |
|---------|-------------|
| `cli` | Native schema-driven command-line interface |
| `io-stdlib` | Synchronous stdin/stdout transport |
| `io-tokio` | Async stdin/stdout transport (Tokio) |
| `io-axum` | HTTP transport (Axum) with session management |
| `jiff` | `Rfc3339` timestamp type using jiff (mutually exclusive with `chrono`) |
| `chrono` | `Rfc3339` timestamp type using chrono (mutually exclusive with `jiff`) |
