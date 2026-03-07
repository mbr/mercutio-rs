# mercutio

A Rust library for building [MCP](https://modelcontextprotocol.io/) servers. In MCP, *clients* are LLM host applications (IDEs, chat interfaces) that connect to *servers* to give models access to tools. `mercutio` handles the server-side protocol (parsing messages, managing the initialization handshake, dispatching tool calls), while you handle the transport. The core is a pure state machine: feed it JSON-RPC messages, and it returns what to send back.

This [sans-io](https://www.firezone.dev/blog/sans-io) design means you can run it over stdio, HTTP, WebSockets, or anything else without fighting the library.

## Defining Tools

Use `tool_registry!` to define your tools. Field doc comments become JSON Schema descriptions that the LLM sees:

```rust,no_run
mercutio::tool_registry! {
    enum MyTools {
        GetWeather("get_weather", "Gets current weather for a city") {
            /// City name, e.g. "San Francisco".
            city: String,
        },
        SetReminder("set_reminder", "Sets a reminder") {
            /// What to remind about.
            message: String,
            /// Minutes from now.
            minutes: u32,
        },
    }
}
```

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
                MyTools::SetReminder(input) => format!("Reminder: {} in {} min", input.message, input.minutes),
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
                Ok(format!("Reminder: {} in {} min", input.message, input.minutes).into())
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
| `io-stdlib` | Synchronous stdin/stdout transport |
| `io-tokio` | Async stdin/stdout transport (Tokio) |
| `io-axum` | HTTP transport (Axum) with session management |
