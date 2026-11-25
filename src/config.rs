//! MCP server configuration and builder.

use std::{marker::PhantomData, sync::Arc};

use rust_mcp_schema::{Implementation, ServerCapabilities, ServerCapabilitiesTools};

use crate::{McpServer, Phase, ToolRegistry};

/// Generates instructions from the initialization request's context.
pub(crate) type Instructions<C> = dyn Fn(&C) -> String + Send + Sync;

/// Server configuration for MCP initialization.
pub(crate) struct ServerConfig<C> {
    /// Server implementation info sent during initialization.
    pub info: Implementation,
    /// Server capabilities advertised to the client.
    pub capabilities: ServerCapabilities,
    /// Optional LLM instructions sent during initialization.
    pub instructions: Option<Arc<Instructions<C>>>,
}

/// Builder for constructing an [`McpServer`].
pub struct McpServerBuilder<R: ToolRegistry, C = ()> {
    /// Server name.
    name: String,
    /// Server version.
    version: String,
    /// Optional human-readable title.
    title: Option<String>,
    /// Optional LLM instructions.
    instructions: Option<Arc<Instructions<C>>>,
    /// Tool registry marker.
    _marker: PhantomData<R>,
}

impl<R: ToolRegistry, C> McpServerBuilder<R, C> {
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

    /// Generates server instructions from the initialization request's context.
    ///
    /// Replaces the previous callback and runs only during initialization; clients
    /// must reconnect to receive changed instructions. Client support varies.
    /// For fixed text, use `|_| "Instructions.".to_owned()`.
    pub fn instructions<F>(&mut self, callback: F) -> &mut Self
    where
        F: Fn(&C) -> String + Send + Sync + 'static,
    {
        self.instructions = Some(Arc::new(callback));
        self
    }

    /// Builds the [`McpServer`].
    pub fn build(&self) -> McpServer<R, C> {
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

    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

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
    fn advertised_configuration<R: ToolRegistry, C>(
        mut server: McpServer<R, C>,
        context: &C,
    ) -> Value {
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        let Output::Send(response) =
            server.handle_with_context(context, parse_line(request).expect("initialize request"))
        else {
            panic!("expected initialization response");
        };
        let response = serde_json::to_value(response.into_inner()).expect("serializable response");
        response["result"].clone()
    }

    /// Advertises the default identity without tool capabilities or instructions.
    #[test]
    fn advertises_defaults_without_tools() {
        let result = advertised_configuration(McpServer::<NoTools>::builder().build(), &());
        assert_eq!(
            result["serverInfo"],
            json!({"name": "unnamed-mcp-server", "version": "0.0.0"})
        );
        assert_eq!(result["capabilities"], json!({}));
        assert!(result.get("instructions").is_none());
    }

    /// Resolves callbacks only at initialization and honors the last setter.
    #[test]
    fn contextual_instructions() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let prefix = String::from("Project: ");
        let mut builder = McpServer::<NoTools, String>::builder();
        builder
            .instructions(|_| "replaced".to_owned())
            .instructions(move |context| {
                counter.fetch_add(1, Ordering::SeqCst);
                format!("{prefix}{context}")
            });
        assert!(
            builder
                .build()
                .to_string()
                .contains("[instructions callback]")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        for project in ["first", "second"] {
            let result = advertised_configuration(builder.build(), &project.to_owned());
            assert_eq!(result["instructions"], format!("Project: {project}"));
        }
        builder.instructions(|_| "fixed".to_owned());
        let result = advertised_configuration(builder.build(), &String::new());
        assert_eq!(result["instructions"], "fixed");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Sends configured metadata and enables the tool-list capability.
    #[test]
    fn advertises_configured_server_with_tools() {
        let server = McpServer::<TestTool>::builder()
            .name("test-server")
            .version("1.2.3")
            .title("Test Server")
            .instructions(|_| "Use this server for testing.".to_owned())
            .build();
        let result = advertised_configuration(server, &());
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
