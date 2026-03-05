# mercutio

An MCP server library that doesn't do IO.

You feed it JSON-RPC messages, it tells you what to do: send a response, handle a tool call, or close the connection. You handle transport (typically newline-delimited JSON over stdin/stdout).

## Usage

```rust
use mercutio::{McpServer, Output};

let mut server = McpServer::new(config);

loop {
    let line = read_line_somehow();
    let msg = mercutio::parse_line(&line)?;

    match server.handle(msg) {
        Output::Send(msg) => send(msg.into_inner()),
        Output::ToolCall(call, responder) => {
            let result = execute_tool(&call.name, &call.arguments);
            send(responder.respond(result).into_inner());
        }
        Output::ToolList(responder) => {
            send(responder.success(my_tools()).into_inner());
        }
        Output::Ready(client) => log::info!("connected: {}", client.info.name),
        Output::SendAndClose(msg, err) => {
            send(msg.into_inner());
            break;
        }
        Output::ProtocolError(err) => break,
        Output::None => {}
    }
}
```
