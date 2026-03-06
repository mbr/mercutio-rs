# mercutio

A Rust library for building [MCP](https://modelcontextprotocol.io/) servers. `mercutio` handles the protocol (parsing messages, managing the initialization handshake, dispatching tool calls), while you handle the transport. The core is a pure state machine: feed it JSON-RPC messages, and it returns what to send back.

This [sans-io](https://www.firezone.dev/blog/sans-io) design means you can run it over stdio, HTTP, WebSockets, or anything else without fighting the library.

If you'd rather not wire up your own transport, the `io-*` feature flags provide ready-made integrations.

## Side-effect free usage

```rust
use mercutio::{McpServer, Output};

// Define your tools with the macro - generates input structs and dispatch enum.
// Field docstrings become JSON Schema descriptions that the LLM sees.
mercutio::tool_registry! {
    enum MyTools {
        GetWeather("get_weather", "Gets weather for a city") {
            /// City name, e.g. "San Francisco".
            city: String,
        },
    }
}

let mut server = McpServer::<MyTools>::builder()
    .name("my-server")
    .version("1.0.0")
    .build();

// You provide the transport - MCP uses newline-delimited JSON
loop {
    let line = read_line_somehow();
    let msg = mercutio::parse_line(&line)?;

    match server.handle(msg) {
        // Protocol responses (init, tool list, etc.)
        Output::Send(msg) => send(msg.into_inner()),
        // Tool invocation - handle it and send the response
        Output::ToolCall(MyTools::GetWeather(input, responder)) => {
            let weather = get_weather(&input.city);
            send(responder.success(format!("{}C", weather.temp)).into_inner());
        }
        Output::ProtocolError(err) => break,
        Output::None => {}
    }
}
```

## Feature Flags

If you do not want/need to implement the IO harness yourself, `mercutio` has a few implementations ready:

- `io-stdlib` - Synchronous stdin/stdout transport using standard library I/O
- `io-tokio` - Async stdin/stdout transport using Tokio

### io-stdlib

Runs an MCP server over stdin/stdout with newline-delimited JSON:

```rust
use mercutio::{io::stdlib::run_stdio, McpServer};

mercutio::tool_registry! {
    enum MyTools {
        GetWeather("get_weather", "Gets weather") { city: String },
    }
}

let server = McpServer::<MyTools>::builder()
    .name("my-server")
    .version("1.0.0")
    .build();

run_stdio(server, |tool| match tool {
    MyTools::GetWeather(input, responder) => {
        responder.success(format!("Weather in {}: sunny", input.city))
    }
})?;
```

### io-tokio

Async version using Tokio. Implement `ToolHandler` on a struct to use async operations:

```rust
use mercutio::{McpServer, OutgoingMessage, io::tokio::{run_stdio, ToolHandler}};

mercutio::tool_registry! {
    enum MyTools {
        GetWeather("get_weather", "Gets weather") { city: String },
    }
}

struct Handler;

impl ToolHandler<MyTools> for Handler {
    async fn handle(&mut self, tool: MyTools) -> OutgoingMessage {
        match tool {
            MyTools::GetWeather(input, responder) => {
                let weather = fetch_weather(&input.city).await;
                responder.success(format!("Weather in {}: {}", input.city, weather))
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), mercutio::io::tokio::IoError> {
    let server = McpServer::<MyTools>::builder()
        .name("my-server")
        .version("1.0.0")
        .build();

    run_stdio(server, Handler).await
}
```
