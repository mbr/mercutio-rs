//! IO-less MCP protocol implementation.

pub mod protocol;

pub use protocol::{
    Client, JsonRpcError, McpServer, McpServerBuilder, NoTools, OutgoingMessage, Output,
    ParseError, ProtocolError, Responder, ToolDef, ToolDefinition, ToolRegistry, ToolResult,
    parse_line,
};
pub use rust_mcp_schema;
pub use schemars;
pub use serde;
pub use serde_json;
