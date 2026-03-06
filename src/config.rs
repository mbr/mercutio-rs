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

impl<R: ToolRegistry> Clone for McpServerBuilder<R> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            version: self.version.clone(),
            title: self.title.clone(),
            instructions: self.instructions.clone(),
            _marker: PhantomData,
        }
    }
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
        };

        McpServer {
            config: ServerConfig {
                info: Implementation {
                    name: self.name.clone(),
                    version: self.version.clone(),
                    title: self.title.clone(),
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
    use crate::{JsonRpcError, McpServer, ToolDefinition, ToolRegistry};

    /// Minimal tool registry for testing builder behavior.
    enum TestTools {}

    impl ToolRegistry for TestTools {
        fn parse(name: &str, _arguments: serde_json::Value) -> Result<Self, JsonRpcError> {
            Err(JsonRpcError::MethodNotFound {
                msg: format!("unknown tool: {name}"),
            })
        }

        fn definitions() -> Vec<ToolDefinition> {
            vec![]
        }
    }

    #[test]
    fn default_values() {
        let server = McpServer::<TestTools>::builder().build();
        assert!(!server.is_ready());
    }

    #[test]
    fn tools_disabled_for_no_tools() {
        use crate::NoTools;
        let server = McpServer::<NoTools>::builder().build();
        assert!(server.config.capabilities.tools.is_none());
    }

    #[test]
    fn tools_enabled_for_registry_with_tools() {
        let server = McpServer::<TestTools>::builder().build();
        assert!(server.config.capabilities.tools.is_some());
        let tools = server
            .config
            .capabilities
            .tools
            .as_ref()
            .expect("tools capability");
        assert_eq!(tools.list_changed, Some(false));
    }

    #[test]
    fn builder_sets_fields() {
        let server = McpServer::<TestTools>::builder()
            .name("test-server")
            .version("1.2.3")
            .title("Test Server")
            .instructions("Use this server for testing.")
            .build();

        assert_eq!(server.config.info.name, "test-server");
        assert_eq!(server.config.info.version, "1.2.3");
        assert_eq!(server.config.info.title.as_deref(), Some("Test Server"));
        assert_eq!(
            server.config.instructions.as_deref(),
            Some("Use this server for testing.")
        );
    }
}
