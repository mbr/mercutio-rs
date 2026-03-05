# mercutio

A Rust library for building [MCP](https://modelcontextprotocol.io/) servers. `mercutio` handles the protocol (parsing messages, managing the initialization handshake, dispatching tool calls), while you handle the transport. The core is a pure state machine: feed it JSON-RPC messages, and it returns what to send back.

This [sans-io](https://www.firezone.dev/blog/sans-io) design means you can run it over stdio, HTTP, WebSockets, or anything else without fighting the library.

If you'd rather not wire up your own transport, the `io-*` feature flags provide ready-made integrations.

## Usage

```rust
use mercutio::{McpServer, Output};

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

loop {
    let line = read_line_somehow();
    let msg = mercutio::parse_line(&line)?;

    match server.handle(msg) {
        Output::Send(msg) => send(msg.into_inner()),
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

- `io-stdlib` - Synchronous stdin/stdout transport using standard library I/O

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
