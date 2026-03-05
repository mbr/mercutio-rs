# mercutio

An MCP server library that doesn't do IO.

You feed it JSON-RPC messages, it tells you what to do: send a response, handle a tool call, or close the connection. You handle transport (typically newline-delimited JSON over stdin/stdout).

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

For servers without tools, omit the type parameter:

```rust
let mut server = McpServer::builder()
    .name("my-server")
    .version("1.0.0")
    .build();
```
