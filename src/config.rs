//! MCP server configuration and builder.

use std::marker::PhantomData;

use rust_mcp_schema::{Implementation, ServerCapabilities, ServerCapabilitiesTools};

use crate::{McpServer, Phase, ToolRegistry};

/// Server configuration for MCP initialization.
pub(crate) struct ServerConfig {
    /// Server implementation info sent during initialization.
    pub info: Implementation,
    /// Server capabilities advertised to the client.
    pub capabilities: ServerCapabilities,
    /// Optional LLM instructions sent during initialization.
    pub instructions: Option<String>,
}

/// Builder for constructing an [`McpServer`].
pub struct McpServerBuilder<R: ToolRegistry> {
    /// Server name.
    name: String,
    /// Server version.
    version: String,
    /// Optional human-readable title.
    title: Option<String>,
    /// Optional LLM instructions.
    instructions: Option<String>,
    /// Tool registry marker.
    _marker: PhantomData<R>,
}

impl<R: ToolRegistry> McpServerBuilder<R> {
    /// Creates a new builder with default values.
    pub(crate) fn new() -> Self {
        Self {
            name: "unnamed-mcp-server".into(),
            version: "0.0.0".into(),
            title: None,
            instructions: None,
            _marker: PhantomData,
        }
    }

    /// Sets the server name sent to clients during initialization.
    pub fn name<S: Into<String>>(&mut self, name: S) -> &mut Self {
        self.name = name.into();
        self
    }

    /// Sets the server version sent to clients during initialization.
    pub fn version<S: Into<String>>(&mut self, version: S) -> &mut Self {
        self.version = version.into();
        self
    }

    /// Sets a human-readable title sent to clients during initialization.
    pub fn title<S: Into<String>>(&mut self, title: S) -> &mut Self {
        self.title = Some(title.into());
        self
    }

    /// Sets instructions for the LLM on how to use this server.
    ///
    /// Sent to the client during initialization. The client may incorporate this text into the
    /// system prompt to help the LLM understand when and how to use the server's tools. Typical
    /// content includes tool selection guidance, required operation sequences, or domain-specific
    /// constraints. Note that client support for this field varies.
    ///
    /// See <https://blog.modelcontextprotocol.io/posts/2025-11-03-using-server-instructions/> for
    /// examples.
    pub fn instructions<S: Into<String>>(&mut self, instructions: S) -> &mut Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Builds the [`McpServer`].
    pub fn build(&self) -> McpServer<R> {
        let capabilities = ServerCapabilities {
            tools: if R::ENABLED {
                Some(ServerCapabilitiesTools {
                    list_changed: Some(false),
                })
            } else {
                None
            },
            completions: None,
            experimental: None,
            logging: None,
            prompts: None,
            resources: None,
            tasks: None,
        };

        McpServer {
            config: ServerConfig {
                info: Implementation {
                    description: None,
                    icons: Vec::new(),
                    name: self.name.clone(),
                    title: self.title.clone(),
                    version: self.version.clone(),
                    website_url: None,
                },
                capabilities,
                instructions: self.instructions.clone(),
            },
            phase: Phase::WaitingForInitialize,
            _marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Verifies configuration as advertised to MCP clients.

    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::{Value, json};

    use crate::{McpServer, NoTools, Output, ToolDef, ToolRegistry, parse_line};

    /// Enables tool capabilities through a single-tool registry.
    #[derive(Deserialize, JsonSchema)]
    struct TestTool {}

    impl ToolDef for TestTool {
        const NAME: &'static str = "test";
        const DESCRIPTION: &'static str = "Test tool";
    }

    /// Captures the configuration sent in the initialization response.
    fn advertised_configuration<R: ToolRegistry>(mut server: McpServer<R>) -> Value {
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        let Output::Send(response) =
            server.handle(parse_line(request).expect("initialize request"))
        else {
            panic!("expected initialization response");
        };
        let response = serde_json::to_value(response.into_inner()).expect("serializable response");
        response["result"].clone()
    }

    /// Advertises the default identity without tool capabilities or instructions.
    #[test]
    fn advertises_defaults_without_tools() {
        let result = advertised_configuration(McpServer::<NoTools>::builder().build());
        assert_eq!(
            result["serverInfo"],
            json!({"name": "unnamed-mcp-server", "version": "0.0.0"})
        );
        assert_eq!(result["capabilities"], json!({}));
        assert!(result.get("instructions").is_none());
    }

    /// Sends configured metadata and enables the tool-list capability.
    #[test]
    fn advertises_configured_server_with_tools() {
        let server = McpServer::<TestTool>::builder()
            .name("test-server")
            .version("1.2.3")
            .title("Test Server")
            .instructions("Use this server for testing.")
            .build();
        let result = advertised_configuration(server);
        assert_eq!(
            result["serverInfo"],
            json!({"name": "test-server", "version": "1.2.3", "title": "Test Server"})
        );
        assert_eq!(result["instructions"], "Use this server for testing.");
        assert_eq!(
            result["capabilities"],
            json!({"tools": {"listChanged": false}})
        );
    }
}
