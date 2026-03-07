# mercutio

A Rust library for building [MCP](https://modelcontextprotocol.io/) servers. `mercutio` handles the protocol (parsing messages, managing the initialization handshake, dispatching tool calls), while you handle the transport. The core is a pure state machine: feed it JSON-RPC messages, and it returns what to send back.

This [sans-io](https://www.firezone.dev/blog/sans-io) design means you can run it over stdio, HTTP, WebSockets, or anything else without fighting the library.

## Defining Tools

Use `tool_registry!` to define your tools. Field doc comments become JSON Schema descriptions that the LLM sees:

```rust
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

## Handlers

Handlers process tool invocations. There are two traits: `ToolHandler` (`&self`, for concurrent contexts) and `MutToolHandler` (`&mut self`, for exclusive access). Both receive an optional session ID—`Some` for HTTP transports, `None` for stdio.

```rust
use mercutio::{ToolOutput, io::{McpSessionId, MutToolHandler}};

struct MyHandler;

impl MutToolHandler<MyTools> for MyHandler {
    type Error = std::convert::Infallible;

    async fn handle(
        &mut self,
        _session_id: Option<McpSessionId>,
        tool: MyTools,
    ) -> Result<ToolOutput, Self::Error> {
        match tool {
            MyTools::GetWeather(input) => {
                Ok(format!("Weather in {}: sunny", input.city).into())
            }
            MyTools::SetReminder(input) => {
                Ok(format!("Reminder set: {} in {} min", input.message, input.minutes).into())
            }
        }
    }
}
```

Closures work too via blanket impl: `|_session_id, tool| async move { ... }`.

## Transports

### io-tokio

Async stdin/stdout using Tokio:

```rust
let server = McpServer::<MyTools>::builder().name("my-server").version("1.0").build();
mercutio::io::tokio::run_stdio(server, MyHandler).await?;
```

### io-stdlib

Synchronous stdin/stdout (no async runtime):

```rust
let server = McpServer::<MyTools>::builder().name("my-server").version("1.0").build();
mercutio::io::stdlib::run_stdio(server, |_session_id, tool| handle_tool(tool))?;
```

### io-axum

HTTP transport with session management:

```rust
let mut builder = McpServer::<MyTools>::builder();
builder.name("my-server").version("1.0");

let router = mercutio::io::axum::mcp_router(builder, MyHandler);
let app = axum::Router::new().nest("/mcp", router);
```

For custom session storage, use `McpRouter::builder()` with `.storage()`.

## Custom Transport

For WebSockets or other transports, use `McpServer` directly:

```rust
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
            let result = handle_tool(tool);
            send(responder.respond(result).into_inner());
        }
        Output::ProtocolError(_) => break,
        Output::None => {}
    }
}
```

## Example

A complete server supporting both stdio and HTTP transports:

```rust
use clap::Parser;
use mercutio::{McpServer, ToolOutput, io::{McpSessionId, ToolHandler}};

mercutio::tool_registry! {
    enum MyTools {
        Greet("greet", "Greets someone") {
            /// Name to greet.
            name: String,
        },
    }
}

struct MyHandler;

impl ToolHandler<MyTools> for MyHandler {
    type Error = std::convert::Infallible;

    async fn handle(
        &self,
        _session_id: Option<McpSessionId>,
        tool: MyTools,
    ) -> Result<ToolOutput, Self::Error> {
        match tool {
            MyTools::Greet(input) => Ok(format!("Hello, {}!", input.name).into()),
        }
    }
}

#[derive(Parser)]
struct Args {
    /// Run as HTTP server instead of stdio.
    #[arg(long)]
    http: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut builder = McpServer::<MyTools>::builder();
    builder.name("greeter").version("1.0");

    if args.http {
        let router = mercutio::io::axum::mcp_router(builder, MyHandler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
        axum::serve(listener, router).await?;
    } else {
        mercutio::io::tokio::run_stdio(builder.build(), MyHandler).await?;
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
