//! IO-less MCP server library.
//!
//! Implements the server side of the Model Context Protocol (MCP), a JSON-RPC 2.0 based protocol
//! for LLM tool integration. This crate handles protocol logic without performing any IO; the
//! caller is responsible for transport (typically newline-delimited JSON over stdin/stdout).
//!
//! In MCP terminology, the *client* is the LLM host application (e.g., an IDE extension or chat
//! interface) that spawns and connects to MCP *servers* to give the model access to tools.
//!
//! # Protocol Flow
//!
//! The protocol proceeds through three phases:
//!
//! 1. **Initialization**: The client sends an `initialize` request containing its capabilities and
//!    implementation info. The server responds with its own capabilities, implementation info, and
//!    optional [`ServerConfig::instructions`] for the LLM.
//!
//! 2. **Initialized notification**: The client sends a `notifications/initialized` notification to
//!    confirm it received and processed the server's response. This completes the handshake.
//!
//! 3. **Ready**: The server can now handle tool requests. Supported methods are `tools/list` to
//!    enumerate available tools and `tools/call` to invoke a tool. The `ping` method is available
//!    in all phases for connection health checks.
//!
//! # Usage
//!
//! Create an [`McpServer`] with a [`ServerConfig`], then feed incoming [`JsonrpcMessage`]s to
//! [`McpServer::handle`]. Each call returns an [`Output`] indicating what action the caller should
//! take: send a response, handle a tool call, or react to a protocol error. For tool requests, the
//! caller receives a [`Responder`] to construct the response after executing the tool.

use std::marker::PhantomData;

use rust_mcp_schema::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, INTERNAL_ERROR, INVALID_PARAMS,
    Implementation, InitializeRequestParams, InitializeResult, JsonrpcError, JsonrpcMessage,
    JsonrpcRequestParams, JsonrpcResponse, LATEST_PROTOCOL_VERSION, ListToolsResult,
    METHOD_NOT_FOUND, RequestId, Result, RpcError, ServerCapabilities,
};
use serde::Serialize;

/// Server configuration for MCP initialization.
pub struct ServerConfig {
    /// Server implementation info.
    pub info: Implementation,
    /// Server capabilities.
    pub capabilities: ServerCapabilities,
    /// Optional instructions for the LLM on how to use this server.
    ///
    /// Sent to the client during initialization. The client may incorporate this text into the
    /// system prompt to help the LLM understand when and how to use the server's tools. Typical
    /// content includes tool selection guidance, required operation sequences, or domain-specific
    /// constraints. Note that client support for this field varies.
    pub instructions: Option<String>,
}

/// Client information received during initialization.
#[derive(Clone, Debug)]
pub struct ClientInfo {
    /// Client implementation info.
    pub info: Implementation,
    /// Client capabilities.
    pub capabilities: ClientCapabilities,
    /// Protocol version agreed upon.
    pub protocol_version: String,
}

/// Protocol phase tracking initialization state.
enum Phase {
    /// Waiting for `initialize` request.
    WaitingForInitialize,
    /// Received `initialize`, waiting for `notifications/initialized`.
    WaitingForInitialized(ClientInfo),
    /// Fully initialized and ready for requests.
    Ready(ClientInfo),
}

/// IO-less MCP server.
pub struct McpServer {
    /// Server configuration.
    config: ServerConfig,
    /// Current protocol phase.
    phase: Phase,
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
}

/// Tool call data for execution.
pub struct ToolCall {
    /// Tool name.
    pub name: String,
    /// Tool arguments as JSON.
    pub arguments: serde_json::Value,
}

/// Generic response builder for deferred request handling.
#[must_use = "request must be responded to"]
pub struct Responder<T> {
    /// Request ID to respond to.
    id: RequestId,
    /// Marker for the response type.
    _marker: PhantomData<fn(T)>,
}

impl<T: Serialize> Responder<T> {
    /// Creates a new responder for the given request ID.
    fn new(id: RequestId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// Creates a success response with the given value.
    pub fn success(self, value: T) -> OutgoingMessage {
        let json_value = serde_json::to_value(value).expect("response serialization failed");
        let extra = json_value.as_object().cloned();
        let result = Result { meta: None, extra };
        let response = JsonrpcResponse::new(self.id, result);
        OutgoingMessage(JsonrpcMessage::Response(response))
    }

    /// Creates an error response.
    pub fn error(self, error: impl Into<JsonRpcError>) -> OutgoingMessage {
        let err: JsonRpcError = error.into();
        let rpc_error = RpcError {
            code: err.code(),
            message: err.message().into(),
            data: None,
        };
        let error_msg = JsonrpcError::new(rpc_error, self.id);
        OutgoingMessage(JsonrpcMessage::Error(error_msg))
    }

    /// Creates a response from a result, converting errors to JSON-RPC errors.
    pub fn respond(
        self,
        result: std::result::Result<T, impl Into<JsonRpcError>>,
    ) -> OutgoingMessage {
        match result {
            Ok(v) => self.success(v),
            Err(e) => self.error(e),
        }
    }
}

/// Responder for tool call results.
pub type ToolCallResponder = Responder<CallToolResult>;

/// Responder for tool list results.
pub type ToolListResponder = Responder<ListToolsResult>;

/// Typed JSON-RPC error codes.
#[derive(Clone, Debug)]
pub enum JsonRpcError {
    /// Method not found.
    MethodNotFound(String),
    /// Invalid parameters.
    InvalidParams(String),
    /// Internal error.
    InternalError(String),
}

impl JsonRpcError {
    /// Returns the JSON-RPC error code.
    fn code(&self) -> i64 {
        match self {
            JsonRpcError::MethodNotFound(_) => METHOD_NOT_FOUND,
            JsonRpcError::InvalidParams(_) => INVALID_PARAMS,
            JsonRpcError::InternalError(_) => INTERNAL_ERROR,
        }
    }

    /// Returns the error message.
    fn message(&self) -> &str {
        match self {
            JsonRpcError::MethodNotFound(msg)
            | JsonRpcError::InvalidParams(msg)
            | JsonRpcError::InternalError(msg) => msg,
        }
    }
}

/// Protocol errors that indicate the connection should be closed.
#[derive(Clone, Debug)]
pub enum ProtocolError {
    /// Received unexpected message for current phase.
    UnexpectedMessage {
        /// What message was expected.
        expected: &'static str,
        /// What message was received.
        got: String,
    },
    /// Unsupported protocol version.
    UnsupportedVersion {
        /// Version requested by client.
        requested: String,
        /// Version supported by server.
        supported: String,
    },
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::UnexpectedMessage { expected, got } => {
                write!(f, "unexpected message: expected {expected}, got {got}")
            }
            ProtocolError::UnsupportedVersion {
                requested,
                supported,
            } => {
                write!(
                    f,
                    "unsupported protocol version: requested {requested}, supported {supported}"
                )
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Output from handling a message.
#[must_use = "output must be handled"]
pub enum Output {
    /// Send this message to the client.
    Send(OutgoingMessage),
    /// Send this message to the client then close due to protocol error.
    SendAndClose(OutgoingMessage, ProtocolError),
    /// Server transitioned to ready phase.
    Ready(ClientInfo),
    /// Tool call request that the caller must handle.
    ToolCall(ToolCall, ToolCallResponder),
    /// Tool list request that the caller must handle.
    ToolList(ToolListResponder),
    /// No action needed.
    None,
    /// Protocol error - caller should close connection.
    ProtocolError(ProtocolError),
}

/// Error returned when parsing a line fails.
#[derive(Debug)]
pub struct ParseError(serde_json::Error);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to parse JSON-RPC message: {}", self.0)
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Parses a line of input into a JSON-RPC message.
pub fn parse_line(line: &str) -> std::result::Result<JsonrpcMessage, ParseError> {
    serde_json::from_str(line).map_err(ParseError)
}

impl McpServer {
    /// Creates a new MCP server with the given configuration.
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            phase: Phase::WaitingForInitialize,
        }
    }

    /// Returns whether the server is in the ready phase.
    pub fn is_ready(&self) -> bool {
        matches!(self.phase, Phase::Ready(_))
    }

    /// Returns client info if initialization is complete.
    pub fn client(&self) -> Option<&ClientInfo> {
        match &self.phase {
            Phase::Ready(client) => Some(client),
            _ => None,
        }
    }

    /// Handles an incoming message and returns the appropriate output.
    pub fn handle(&mut self, msg: JsonrpcMessage) -> Output {
        match &self.phase {
            Phase::WaitingForInitialize => self.handle_waiting_for_initialize(msg),
            Phase::WaitingForInitialized(_) => self.handle_waiting_for_initialized(msg),
            Phase::Ready(_) => self.handle_ready(msg),
        }
    }

    /// Handles messages while waiting for the `initialize` request.
    fn handle_waiting_for_initialize(&mut self, msg: JsonrpcMessage) -> Output {
        match msg {
            JsonrpcMessage::Request(req) if req.method == "ping" => {
                Output::Send(Self::empty_response(req.id))
            }
            JsonrpcMessage::Request(req) if req.method == "initialize" => {
                self.handle_initialize(req.id, req.params)
            }
            _ => Output::ProtocolError(ProtocolError::UnexpectedMessage {
                expected: "initialize",
                got: Self::describe_message(&msg),
            }),
        }
    }

    /// Handles the `initialize` request.
    fn handle_initialize(&mut self, id: RequestId, params: Option<JsonrpcRequestParams>) -> Output {
        let params_value = params.and_then(|p| p.extra).unwrap_or_default();
        let params: InitializeRequestParams =
            match serde_json::from_value(serde_json::Value::Object(params_value)) {
                Ok(p) => p,
                Err(e) => {
                    return Output::Send(Self::error_response(
                        id,
                        INVALID_PARAMS,
                        &format!("invalid initialize params: {e}"),
                    ));
                }
            };

        if params.protocol_version != LATEST_PROTOCOL_VERSION {
            let msg = format!(
                "unsupported protocol version: {}, supported: {}",
                params.protocol_version, LATEST_PROTOCOL_VERSION
            );
            let error = ProtocolError::UnsupportedVersion {
                requested: params.protocol_version.clone(),
                supported: LATEST_PROTOCOL_VERSION.into(),
            };
            return Output::SendAndClose(Self::error_response(id, INVALID_PARAMS, &msg), error);
        }

        let client = ClientInfo {
            info: params.client_info,
            capabilities: params.capabilities,
            protocol_version: params.protocol_version,
        };

        let result = InitializeResult {
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
        Output::Send(OutgoingMessage(JsonrpcMessage::Response(response)))
    }

    /// Handles messages while waiting for the `notifications/initialized` notification.
    fn handle_waiting_for_initialized(&mut self, msg: JsonrpcMessage) -> Output {
        match msg {
            JsonrpcMessage::Request(req) if req.method == "ping" => {
                Output::Send(Self::empty_response(req.id))
            }
            JsonrpcMessage::Notification(notif) if notif.method == "notifications/initialized" => {
                let Phase::WaitingForInitialized(client) =
                    std::mem::replace(&mut self.phase, Phase::WaitingForInitialize)
                else {
                    unreachable!("phase should be WaitingForInitialized");
                };
                self.phase = Phase::Ready(client.clone());
                Output::Ready(client)
            }
            _ => Output::ProtocolError(ProtocolError::UnexpectedMessage {
                expected: "notifications/initialized",
                got: Self::describe_message(&msg),
            }),
        }
    }

    /// Handles messages in the ready phase.
    fn handle_ready(&mut self, msg: JsonrpcMessage) -> Output {
        match msg {
            JsonrpcMessage::Request(req) => match req.method.as_str() {
                "ping" => Output::Send(Self::empty_response(req.id)),
                "tools/list" => Output::ToolList(Responder::new(req.id)),
                "tools/call" => self.handle_tool_call(req.id, req.params),
                method => {
                    tracing::debug!(method, "unknown method");
                    Output::Send(Self::error_response(
                        req.id,
                        METHOD_NOT_FOUND,
                        &format!("Method not found: {method}"),
                    ))
                }
            },
            JsonrpcMessage::Notification(notif) => {
                tracing::debug!(method = %notif.method, "ignoring notification");
                Output::None
            }
            _ => Output::None,
        }
    }

    /// Handles a `tools/call` request.
    fn handle_tool_call(&self, id: RequestId, params: Option<JsonrpcRequestParams>) -> Output {
        let params_value = params.and_then(|p| p.extra).unwrap_or_default();
        let params: CallToolRequestParams =
            match serde_json::from_value(serde_json::Value::Object(params_value)) {
                Ok(p) => p,
                Err(e) => {
                    return Output::Send(Self::error_response(
                        id,
                        INVALID_PARAMS,
                        &format!("invalid tools/call params: {e}"),
                    ));
                }
            };

        let arguments = params
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        Output::ToolCall(
            ToolCall {
                name: params.name,
                arguments,
            },
            Responder::new(id),
        )
    }

    /// Creates an empty success response.
    fn empty_response(id: RequestId) -> OutgoingMessage {
        let result = Result {
            meta: None,
            extra: None,
        };
        let response = JsonrpcResponse::new(id, result);
        OutgoingMessage(JsonrpcMessage::Response(response))
    }

    /// Creates an error response.
    fn error_response(id: RequestId, code: i64, message: &str) -> OutgoingMessage {
        let error = RpcError {
            code,
            message: message.into(),
            data: None,
        };
        let error_msg = JsonrpcError::new(error, id);
        OutgoingMessage(JsonrpcMessage::Error(error_msg))
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
}

#[cfg(test)]
mod tests {
    use rust_mcp_schema::{
        Implementation, JsonrpcNotification, JsonrpcRequest, ServerCapabilities,
        ServerCapabilitiesTools,
    };

    use super::*;

    fn test_config() -> ServerConfig {
        ServerConfig {
            info: Implementation {
                name: "test".into(),
                version: "1.0".into(),
                title: None,
            },
            capabilities: ServerCapabilities {
                tools: Some(ServerCapabilitiesTools {
                    list_changed: Some(false),
                }),
                completions: None,
                experimental: None,
                logging: None,
                prompts: None,
                resources: None,
            },
            instructions: None,
        }
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
        let mut server = McpServer::new(test_config());
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
        let mut server = McpServer::new(test_config());

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
        assert!(matches!(output, Output::Ready(_)));
        assert!(server.is_ready());
    }

    #[test]
    fn tool_list_request() {
        let mut server = McpServer::new(test_config());
        initialize_server(&mut server);

        let list_req = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("2".into()),
            "tools/list".into(),
            None,
        ));
        let output = server.handle(list_req);
        assert!(matches!(output, Output::ToolList(_)));
    }

    #[test]
    fn tool_call_request() {
        let mut server = McpServer::new(test_config());
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
            Output::ToolCall(call, _responder) => {
                assert_eq!(call.name, "test_tool");
            }
            _ => panic!("expected ToolCall output"),
        }
    }

    #[test]
    fn unknown_method_returns_error() {
        let mut server = McpServer::new(test_config());
        initialize_server(&mut server);

        let unknown_req = JsonrpcMessage::Request(JsonrpcRequest::new(
            RequestId::String("4".into()),
            "unknown/method".into(),
            None,
        ));
        let output = server.handle(unknown_req);
        match output {
            Output::Send(msg) => {
                assert!(matches!(msg.as_inner(), JsonrpcMessage::Error(_)));
            }
            _ => panic!("expected Send with error"),
        }
    }

    fn initialize_server(server: &mut McpServer) {
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
