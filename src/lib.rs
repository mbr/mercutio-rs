#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod config;
mod tools;

pub mod io;

use std::{marker::PhantomData, mem};

pub use config::McpServerBuilder;
use config::ServerConfig;
#[doc(hidden)]
pub use rust_mcp_schema;
use rust_mcp_schema::{
    CallToolRequestParams, ClientCapabilities, INTERNAL_ERROR, INVALID_PARAMS, Implementation,
    InitializeRequestParams, InitializeResult, JsonrpcError, JsonrpcMessage, JsonrpcRequestParams,
    JsonrpcResponse, LATEST_PROTOCOL_VERSION, ListToolsResult, METHOD_NOT_FOUND, RequestId, Result,
    RpcError,
};
#[doc(hidden)]
pub use schemars;
#[doc(hidden)]
pub use serde;
#[doc(hidden)]
pub use serde_json;
use thiserror::Error;
pub use tools::{
    IntoToolResponse, NoTools, ToolDef, ToolDefinition, ToolDefinitions, ToolOutput, ToolRegistry,
};

/// The connected MCP client.
///
/// Represents the client on the other end of the connection after successful initialization.
/// Available via [`McpServer::client`] once the handshake completes.
#[derive(Clone, Debug)]
pub struct Client {
    /// Client implementation details (`clientInfo` in the MCP spec).
    pub info: Implementation,
    /// Features the client supports, such as sampling or roots.
    pub capabilities: ClientCapabilities,
}

/// Protocol phase tracking initialization state.
enum Phase {
    /// Waiting for `initialize` request.
    WaitingForInitialize,
    /// Received `initialize`, waiting for `notifications/initialized`.
    WaitingForInitialized(Client),
    /// Fully initialized and ready for requests.
    Ready(Client),
}

/// IO-less MCP server state machine.
///
/// Generic over `R: ToolRegistry` which defines the available tools. Defaults to [`NoTools`] for
/// servers without tools. The registry handles tool parsing and provides type-safe dispatch.
///
/// # Protocol Flow
///
/// The server proceeds through three phases, with state transitions handled internally:
///
/// 1. **Initialization**: The client sends an `initialize` request containing its capabilities and
///    implementation info. The server responds with its own capabilities, implementation info, and
///    optional instructions for the LLM (see [`McpServerBuilder::instructions`]).
///
/// 2. **Initialized notification**: The client sends a `notifications/initialized` notification to
///    confirm it received and processed the server's response. This completes the handshake.
///
/// 3. **Ready**: The server can now handle tool requests. Supported methods are `tools/list` to
///    enumerate available tools and `tools/call` to invoke a tool.
///
/// The `ping` method is available in all phases for connection health checks. Other requests are
/// rejected until the handshake completes; [`McpServer::is_ready`] can check this.
pub struct McpServer<R: ToolRegistry = NoTools> {
    /// Server configuration.
    config: ServerConfig,
    /// Current protocol phase.
    phase: Phase,
    /// Tool registry marker.
    _marker: PhantomData<R>,
}

/// Outgoing message that must be sent to the client.
#[must_use = "message must be sent to client"]
pub struct OutgoingMessage(JsonrpcMessage);

impl OutgoingMessage {
    /// Consumes the wrapper and returns the inner message.
    pub fn into_inner(self) -> JsonrpcMessage {
        self.0
    }

    /// Returns a reference to the inner message.
    pub fn as_inner(&self) -> &JsonrpcMessage {
        &self.0
    }

    /// Creates an empty success response.
    fn empty_response(id: RequestId) -> Self {
        let response = JsonrpcResponse::new(id, Default::default());
        Self(JsonrpcMessage::Response(response))
    }
}

/// Response builder for tool calls.
///
/// Provides two methods for sending responses:
///
/// - [`respond`](Self::respond): Handles both success values and domain errors. Accepts bare
///   values (`String`, [`ToolOutput`]) or `Result<T, E>` where `Err` becomes a tool error
///   (`is_error: true`). This is the common case.
///
/// - [`rpc_error`](Self::rpc_error): Sends a JSON-RPC error response. Use this only for
///   protocol-level failures (rare after successful tool parsing).
///
/// # Example
///
/// ```ignore
/// // Direct value
/// responder.respond("Success!")
///
/// // Result - Ok becomes success, Err becomes tool error
/// responder.respond(std::fs::read_to_string(&path))
///
/// // With ToolOutput builder
/// responder.respond(ToolOutput::json(&my_data))
///
/// // Rare: protocol-level error
/// responder.rpc_error(JsonRpcError::InternalError { msg: "..." })
/// ```
#[must_use = "request must be responded to"]
pub struct Responder {
    /// Request ID to respond to.
    id: RequestId,
}

impl Responder {
    /// Creates a new responder for the given request ID.
    pub fn new(id: RequestId) -> Self {
        Self { id }
    }

    /// Sends a tool response.
    ///
    /// Accepts bare values (`String`, `&str`, [`ToolOutput`]) or `Result<T, E>`:
    /// - Bare values and `Ok(v)` become successful responses (`is_error: false`)
    /// - `Err(e)` becomes a tool error (`is_error: true`) with the error's display text
    pub fn respond(self, value: impl IntoToolResponse) -> OutgoingMessage {
        let call_result = value.into_tool_response();
        let json_value =
            serde_json::to_value(&call_result).expect("CallToolResult serialization failed");
        let extra = json_value.as_object().cloned();
        let response = JsonrpcResponse::new(self.id, Result { meta: None, extra });
        OutgoingMessage(JsonrpcMessage::Response(response))
    }

    /// Sends a JSON-RPC error response.
    ///
    /// Use this for protocol-level failures only. After successful tool parsing, this is rarely
    /// needed; domain errors should go through [`respond`](Self::respond) with a `Result::Err`.
    pub fn rpc_error(self, error: impl Into<JsonRpcError>) -> OutgoingMessage {
        let err: JsonRpcError = error.into();
        let rpc_error = RpcError {
            code: err.code(),
            message: err.to_string(),
            data: None,
        };
        let error_msg = JsonrpcError::new(rpc_error, self.id);
        OutgoingMessage(JsonrpcMessage::Error(error_msg))
    }
}

/// Request-level JSON-RPC errors.
///
/// These represent errors in handling a specific request. They are converted to JSON-RPC error
/// responses and sent to the client; the connection remains open for further requests.
#[derive(Clone, Debug, Error)]
pub enum JsonRpcError {
    /// Method not found.
    #[error("method not found: {msg}")]
    MethodNotFound {
        /// Error message to show.
        msg: String,
    },
    /// Invalid parameters.
    #[error("invalid params: {msg}")]
    InvalidParams {
        /// Error message to show.
        msg: String,
    },
    /// Internal error.
    #[error("internal error: {msg}")]
    InternalError {
        /// Error message to show.
        msg: String,
    },
}

impl JsonRpcError {
    /// Returns the JSON-RPC error code.
    fn code(&self) -> i64 {
        match self {
            JsonRpcError::MethodNotFound { msg: _ } => METHOD_NOT_FOUND,
            JsonRpcError::InvalidParams { msg: _ } => INVALID_PARAMS,
            JsonRpcError::InternalError { msg: _ } => INTERNAL_ERROR,
        }
    }

    /// Converts this error into an outgoing JSON-RPC error response.
    pub fn into_response(self, id: RequestId) -> OutgoingMessage {
        let error = RpcError {
            code: self.code(),
            message: self.to_string(),
            data: None,
        };
        OutgoingMessage(JsonrpcMessage::Error(JsonrpcError::new(error, id)))
    }
}

/// Protocol-level errors that terminate the connection.
///
/// Unlike [`JsonRpcError`], these indicate the connection is in an unrecoverable state and must be
/// closed. They may or may not result in an error response being sent before closing.
#[derive(Clone, Debug, Error)]
pub enum ProtocolError {
    /// Received unexpected message for current phase.
    #[error("unexpected message: expected {expected}, got {got}")]
    UnexpectedMessage {
        /// What message was expected.
        expected: &'static str,
        /// What message was received.
        got: String,
    },
}

/// Output from handling a message.
///
/// Generic over the tool registry type. The [`ToolCall`](Output::ToolCall) variant contains
/// the parsed tool input and a [`Responder`] for sending the result.
#[must_use = "output must be handled"]
pub enum Output<R: ToolRegistry> {
    /// Send this message to the client.
    Send(OutgoingMessage),
    /// Tool call with parsed input and responder for the result.
    ToolCall {
        /// Parsed tool input.
        tool: R,
        /// Responder for sending the tool result.
        responder: Responder,
    },
    /// No action needed.
    None,
    /// Protocol error - caller should close connection.
    ProtocolError(ProtocolError),
}

/// Error returned when parsing a line fails.
#[derive(Debug, Error)]
#[error("failed to parse JSON-RPC message: {0}")]
pub struct ParseError(#[source] serde_json::Error);

/// Parses a line of input into a JSON-RPC message.
pub fn parse_line(line: &str) -> std::result::Result<JsonrpcMessage, ParseError> {
    serde_json::from_str(line).map_err(ParseError)
}

/// Parses JSON-RPC request params into a typed struct.
fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<JsonrpcRequestParams>,
) -> std::result::Result<T, serde_json::Error> {
    let params_value = params.and_then(|p| p.extra).unwrap_or_default();
    serde_json::from_value(serde_json::Value::Object(params_value))
}

impl<R: ToolRegistry> McpServer<R> {
    /// Returns a builder for constructing an [`McpServer`].
    pub fn builder() -> McpServerBuilder<R> {
        McpServerBuilder::new()
    }

    /// Returns whether the server is in the ready phase.
    pub fn is_ready(&self) -> bool {
        matches!(self.phase, Phase::Ready(_))
    }

    /// Returns the connected client after initialization completes.
    pub fn client(&self) -> Option<&Client> {
        match &self.phase {
            Phase::Ready(client) => Some(client),
            _ => None,
        }
    }

    fn expectation(&self) -> &'static str {
        match self.phase {
            Phase::WaitingForInitialize => "initialize",
            Phase::WaitingForInitialized(_) => "notifications/initialized",
            Phase::Ready(_) => "anything, really",
        }
    }

    /// Handles an incoming message and returns the appropriate output.
    pub fn handle(&mut self, msg: JsonrpcMessage) -> Output<R> {
        match (&mut self.phase, msg) {
            // Always respond to pings.
            (_, JsonrpcMessage::Request(req)) if req.method == "ping" => {
                Output::Send(OutgoingMessage::empty_response(req.id))
            }
            // Waiting for completion, only legal thing (besides ping) is receiving the
            // `initialized` notification.
            (Phase::WaitingForInitialized(_), JsonrpcMessage::Notification(notif))
                if notif.method == "notifications/initialized" =>
            {
                let Phase::WaitingForInitialized(client) =
                    // Note: Replacing with `WaitingForInitialize` is not semantically correct
                    //       here, we're just using it as an "empty" value to make our life easier.
                    mem::replace(&mut self.phase, Phase::WaitingForInitialize)
                else {
                    unreachable!("already verified phase");
                };
                self.phase = Phase::Ready(client);
                Output::None
            }
            (Phase::Ready(_), JsonrpcMessage::Notification(notif)) => {
                tracing::debug!(method = %notif.method, "ignoring notification");
                Output::None
            }
            (_, JsonrpcMessage::Request(req)) => {
                let id = req.id;
                self.handle_request(id.clone(), &req.method, req.params)
                    .unwrap_or_else(|e| Output::Send(e.into_response(id)))
            }
            (_, msg) => Output::ProtocolError(ProtocolError::UnexpectedMessage {
                expected: self.expectation(),
                got: describe_message(&msg),
            }),
        }
    }

    /// Handles the `initialize` request.
    fn handle_initialize(
        &mut self,
        id: RequestId,
        params: Option<JsonrpcRequestParams>,
    ) -> std::result::Result<Output<R>, JsonRpcError> {
        let params: InitializeRequestParams =
            parse_params(params).map_err(|e| JsonRpcError::InvalidParams {
                msg: format!("initialize: {e}"),
            })?;

        let client = Client {
            info: params.client_info,
            capabilities: params.capabilities,
        };

        let result = InitializeResult {
            // Note: Version negotiation according to the MCP is pretty much a client-side
            //       affair. We simply report our supported version.
            protocol_version: LATEST_PROTOCOL_VERSION.into(),
            capabilities: self.config.capabilities.clone(),
            server_info: self.config.info.clone(),
            instructions: self.config.instructions.clone(),
            meta: None,
        };

        self.phase = Phase::WaitingForInitialized(client);

        let json_value = serde_json::to_value(result).expect("InitializeResult serialization");
        let extra = json_value.as_object().cloned();
        let response = JsonrpcResponse::new(id, Result { meta: None, extra });
        Ok(Output::Send(OutgoingMessage(JsonrpcMessage::Response(
            response,
        ))))
    }

    /// Handles a `tools/call` request.
    fn handle_tool_call(
        &self,
        id: RequestId,
        params: Option<JsonrpcRequestParams>,
    ) -> std::result::Result<Output<R>, JsonRpcError> {
        let params: CallToolRequestParams =
            parse_params(params).map_err(|e| JsonRpcError::InvalidParams {
                msg: format!("tools/call: {e}"),
            })?;

        let arguments = params
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        match R::parse(&params.name, arguments) {
            Ok(tool) => Ok(Output::ToolCall {
                tool,
                responder: Responder::new(id),
            }),
            Err(e) => Ok(Output::Send(e.into_response(id))),
        }
    }

    /// Handles a `tools/list` request.
    fn handle_tool_list(&self, id: RequestId) -> Output<R> {
        let definitions = R::definitions();
        let tools: Vec<_> = definitions.into_iter().map(|d| d.into_mcp_tool()).collect();
        let result = ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        };
        let json_value = serde_json::to_value(result).expect("ListToolsResult serialization");
        let extra = json_value.as_object().cloned();
        let response = JsonrpcResponse::new(id, Result { meta: None, extra });
        Output::Send(OutgoingMessage(JsonrpcMessage::Response(response)))
    }

    /// Dispatches a request to the appropriate handler based on method and phase.
    fn handle_request(
        &mut self,
        id: RequestId,
        method: &str,
        params: Option<JsonrpcRequestParams>,
    ) -> std::result::Result<Output<R>, JsonRpcError> {
        match (&mut self.phase, method) {
            (Phase::WaitingForInitialize, "initialize") => self.handle_initialize(id, params),
            (Phase::Ready(_), "tools/list") if R::ENABLED => Ok(self.handle_tool_list(id)),
            (Phase::Ready(_), "tools/call") if R::ENABLED => self.handle_tool_call(id, params),
            (Phase::Ready(_), method) => Err(JsonRpcError::MethodNotFound {
                msg: method.to_string(),
            }),
            _ => Ok(Output::ProtocolError(ProtocolError::UnexpectedMessage {
                expected: self.expectation(),
                got: format!("request:{method}"),
            })),
        }
    }
}

/// Describes a message for error reporting.
fn describe_message(msg: &JsonrpcMessage) -> String {
    match msg {
        JsonrpcMessage::Request(req) => format!("request:{}", req.method),
        JsonrpcMessage::Response(_) => "response".into(),
        JsonrpcMessage::Notification(notif) => format!("notification:{}", notif.method),
        JsonrpcMessage::Error(_) => "error".into(),
    }
}

#[cfg(test)]
mod tests {
    use rust_mcp_schema::{
        JsonrpcMessage, JsonrpcNotification, JsonrpcRequest, JsonrpcRequestParams, RequestId,
    };

    use crate::{McpServer, NoTools, Output};

    fn test_server() -> McpServer<NoTools> {
        McpServer::<NoTools>::builder()
            .name("test")
            .version("1.0")
            .build()
    }

    fn initialize_params() -> JsonrpcRequestParams {
        let extra: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1.0" }
            }"#,
        )
        .expect("valid JSON");
        JsonrpcRequestParams {
            meta: None,
            extra: Some(extra),
        }
    }

    #[test]
    fn ping_during_init() {
        let mut server = test_server();
        let ping = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("1".into()),
            "ping".into(),
            None,
        ));
        let output = server.handle(ping);
        assert!(matches!(output, Output::Send(_)));
        assert!(!server.is_ready());
    }

    #[test]
    fn full_initialization() {
        let mut server = test_server();

        let init_req = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("1".into()),
            "initialize".into(),
            Some(initialize_params()),
        ));
        let output = server.handle(init_req);
        assert!(matches!(output, Output::Send(_)));
        assert!(!server.is_ready());

        let initialized = JsonrpcMessage::Notification(JsonrpcNotification::new(
            "notifications/initialized".into(),
            None,
        ));
        let output = server.handle(initialized);
        assert!(matches!(output, Output::None));
        assert!(server.is_ready());
    }

    #[test]
    fn tool_list_returns_error_for_no_tools() {
        let mut server = test_server();
        initialize_server(&mut server);

        let list_req = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("2".into()),
            "tools/list".into(),
            None,
        ));
        let output = server.handle(list_req);
        match output {
            Output::Send(msg) => {
                assert!(matches!(msg.as_inner(), JsonrpcMessage::Error(_)));
            }
            _ => panic!("expected Send with error"),
        }
    }

    #[test]
    fn tool_call_returns_error_for_no_tools() {
        let mut server = test_server();
        initialize_server(&mut server);

        let call_params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{ "name": "test_tool", "arguments": { "arg1": "value1" } }"#)
                .expect("valid JSON");

        let call_req = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("3".into()),
            "tools/call".into(),
            Some(JsonrpcRequestParams {
                meta: None,
                extra: Some(call_params),
            }),
        ));
        let output = server.handle(call_req);
        match output {
            Output::Send(msg) => {
                assert!(matches!(msg.as_inner(), JsonrpcMessage::Error(_)));
            }
            _ => panic!("expected Send with error"),
        }
    }

    fn initialize_server(server: &mut McpServer<NoTools>) {
        let init_req = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("init".into()),
            "initialize".into(),
            Some(initialize_params()),
        ));
        let _ = server.handle(init_req);

        let initialized = JsonrpcMessage::Notification(JsonrpcNotification::new(
            "notifications/initialized".into(),
            None,
        ));
        let _ = server.handle(initialized);
    }
}
