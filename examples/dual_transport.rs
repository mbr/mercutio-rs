//! Example MCP server supporting both stdio and HTTP transports.

use clap::Parser;
use mercutio::{
    McpServer, ToolOutput,
    io::{McpSessionId, ToolHandler},
};

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
