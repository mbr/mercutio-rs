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
application. The generated command uses conventional kebab-case spellings while retaining the
original MCP names internally:

```rust,ignore
use std::convert::Infallible;
use mercutio::cli::ToolRegistryExt as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = MyTools::cli("my-tools").version("1.0.0").build()?;
    cli.run(|_session_id, tool| -> Result<String, Infallible> {
        Ok(match tool {
            MyTools::GetWeather(input) => format!("Weather in {}: sunny", input.city),
            MyTools::SetReminder(input) => format!("Reminder set: {}", input.message),
        })
    })?;
    Ok(())
}
```

The registry above produces a complete standalone CLI:

```console
$ my-tools --help
Invokes local MCP tool handlers as native commands

Usage: my-tools [OPTIONS] [COMMAND]

Commands:
  get-weather  Gets current weather for a city
  set-reminder Sets a reminder

Options:
      --output <MODE>       [default: artifacts] [possible values: artifacts, structured, raw, binary]
      --images <MODE>       [default: auto] [possible values: auto, kitty, off]
      --artifact-dir <DIR>  Parent directory for artifact output
      --input-json <TOOL>   Reads the selected tool's JSON object from stdin
  -h, --help                Print help
  -V, --version             Print version

$ my-tools get-weather --help
Usage: my-tools get-weather --city <STRING>

Options:
      --city <STRING>  City name, e.g. "Llanfairpwllgwyngyllgogerychwyrndrobwllllantysiliogogogoch".
  -h, --help           Print help
```

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
diagnostics go to stderr. Parse-only applications can use `try_parse_from` or
`try_parse_matches`, invoke the returned typed tool directly, and provide custom rendering.

### Nesting in an application

Use `attach_to` when native tools share a binary with MCP transports. The entire generated tree is
placed under the name supplied to `cli`; collisions with application commands are construction
errors:

```rust,ignore
use mercutio::cli::ToolRegistryExt as _;

let tools = MyTools::cli("tool").version("1.0.0").build()?;
let command = tools.attach_to(
    clap::Command::new("my-app")
        .subcommand(clap::Command::new("mcp"))
        .subcommand(
            clap::Command::new("mcp-http")
                .arg(clap::Arg::new("bind").long("bind").required(true)),
        ),
)?;
let matches = command.get_matches();

match matches.subcommand() {
    Some(("tool", matches)) => {
        let invocation = tools.try_parse_matches(matches, std::io::stdin().lock())?;
        let (tool, output_options) = invocation.into_parts();
        // Invoke the same handler used by the MCP branches, then render as appropriate.
    }
    Some(("mcp", _)) => { /* run stdio MCP */ }
    Some(("mcp-http", _)) => { /* run HTTP MCP */ }
    _ => unreachable!("Clap validates subcommands"),
}
```

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
