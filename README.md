# mercutio

A Rust library for building [MCP](https://modelcontextprotocol.io/) servers. In MCP, _clients_ are LLM host applications (IDEs, chat interfaces) that connect to _servers_ to give models access to tools. `mercutio` handles the server-side protocol (parsing messages, managing the initialization handshake, dispatching tool calls), while you handle the transport. The core is a pure state machine: feed it JSON-RPC messages, and it returns what to send back.

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

Enable `cli` to expose the same registry and handler as native commands, without an MCP connection:

```rust,ignore
use mercutio::cli::ToolRegistryExt as _;

let cli = MyTools::cli("my-tools")
    .about("Check the weather and set reminders")
    .instructions(INSTRUCTIONS)
    .build()?;

cli.run_async_or_exit(&mut handler).await;
```

This handles help, diagnostics, and exit codes. Use `run_or_exit()` for synchronous handlers.

```console
my-tools get-weather --city Berlin
printf '%s' '{"city":"Berlin"}' | my-tools --input-json get-weather
my-tools --output raw get-weather --city Berlin
```

Schemas supply flags, nested object flattening, and repeatable arrays; whole-input JSON covers
complex inputs. Output defaults to readable text and paths to generated files, with structured
JSON, raw JSON, and binary modes for scripts.

Optional `.about()` and `.instructions()` supply application help. Reuse the MCP instructions
constant, or embed a document with `include_str!`. `-h` stays compact; `--help` includes the full
reference and output controls.

For a multi-interface application, `attach_to()` nests the generated commands below an
application-owned Clap command. Parse-only APIs let you handle dispatch and rendering yourself.
See the [`cli` API](https://docs.rs/mercutio/latest/mercutio/cli/) for composition and the
non-exiting `run()` and `run_async()` variants.

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
        _context: &(),
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

`ToolHandler` takes `&self` for concurrent contexts; `MutToolHandler` takes `&mut self` for exclusive access. The session ID is `Some` for HTTP (multiple clients share one server), `None` for stdio (one process = one session). The additional context argument defaults to `&()`. Closures work via blanket impl: `|_context: &(), _session_id, tool| async move { ... }`.

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
mercutio::io::stdlib::run_stdio(server, |_context: &(), _session_id, tool| handle_tool(tool))?;
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

## Application context

Implement `ToolHandler<Tools, Context>` (or `MutToolHandler<Tools, Context>`) to receive
application-defined context separately from the handler's long-lived state:

```rust,ignore
async fn handle(
    &self,
    context: &Context,
    session: Option<McpSessionId>,
    tool: Tools,
) -> Result<ToolOutput, Self::Error> {
    self.execute(context, session, tool).await
}
```

Context is borrowed for execution and need not implement `Clone`. Trait implementations can
borrow it across awaits; closure adapters should copy or clone needed data before creating an
`async move` block. The protocol core and static registry remain independent of context:
context does not currently alter tool discovery, parsing, or server instructions. Applications
must enforce their own scope and policy in the handler; extraction alone is not authorization.

**HTTP:** Implement Axum's `FromRequestParts<S>` for your context, using query parameters,
headers, extensions, or application state. `mcp_router` uses `S = ()`; for application state use:

```rust,ignore
let router = mercutio::io::axum::McpRouter::builder(builder, handler)
    .context_state(application_state)
    .build();
```

The extractor receives your application state, not private router state. It runs on every POST
and DELETE, before session processing. Rejections are ordinary Axum responses. Context is never
saved in the MCP session: for example, `?project=marc%2Fgeneral` must be supplied on each request
when your extractor requires it. Different clients and concurrent calls cannot inherit a prior
request's context.

**Stdio:** Implement `io::FromStdio` to construct context synchronously once at startup, usually
from configuration. Both stdio transports reuse that value for the stream's lifetime and return
`IoError::Context` on extraction failure without reading input. For async setup or caller-owned
context, use `run_on_with_context(input, output, server, &context, handler)` instead. Extractors
must not read stdin or write stdout.

**CLI:** Implement `cli::FromCli`: `augment_command` adds context options to the generated root
command, and `from_cli` extracts context from its `ArgMatches`. Select it with
`Tools::cli("tools").context::<Context>().build()?`. Extraction runs once for a successfully
parsed invocation, including the whole-input JSON route, but not for help or version requests.
Extraction errors are usage errors and prevent dispatch. For nested commands, use
`try_parse_matches` on the generated subtree. Parse-only callers can access `Invocation::context`
or obtain `(tool, context, output_options)` from `into_parts()`.

Context-free applications use `()`, which implements both extraction traits and Axum's
extractor. Existing handlers and runner closures must add the first `&()` argument; direct
handler calls become `handler.handle(&(), session, tool)`.

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

    async fn handle(&self, _: &(), _: Option<mercutio::io::McpSessionId>, tool: Tools) -> Result<ToolOutput, Self::Error> {
        match tool {
            Tools::Greet(g) => Ok(format!("Hello, {}!", g.name).into()),
        }
    }
}

# let rt = tokio::runtime::Runtime::new().unwrap();
# rt.block_on(async {
let handler = Handler;
let tool = Tools::Greet(Greet { name: "Alice".into() });

let output = handler.handle(&(), None, tool).await.expect("handler failed");
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

    async fn handle(&self, _: &(), _: Option<McpSessionId>, tool: MyTools) -> Result<ToolOutput, Self::Error> {
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

| Feature     | Description                                                            |
| ----------- | ---------------------------------------------------------------------- |
| `cli`       | Native schema-driven command-line interface                            |
| `io-stdlib` | Synchronous stdin/stdout transport                                     |
| `io-tokio`  | Async stdin/stdout transport (Tokio)                                   |
| `io-axum`   | HTTP transport (Axum) with session management                          |
| `jiff`      | `Rfc3339` timestamp type using jiff (mutually exclusive with `chrono`) |
| `chrono`    | `Rfc3339` timestamp type using chrono (mutually exclusive with `jiff`) |
