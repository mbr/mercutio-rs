//! Axum HTTP transport for MCP Streamable HTTP.
//!
//! Implements the
//! [Streamable HTTP transport](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http)
//! from MCP specification 2025-03-26. Provides session management and an axum router for handling MCP requests over HTTP.
//!
//! # Overview
//!
//! The Streamable HTTP transport uses a single endpoint that accepts:
//!
//! - `POST`: Client messages (requests, notifications, responses)
//! - `GET`: Returns 405 Method Not Allowed (SSE streaming not implemented)
//! - `DELETE`: Session termination
//!
//! Sessions are identified by the `Mcp-Session-Id` header. The server generates a session ID
//! when responding to an `initialize` request and the client must include it in subsequent
//! requests.
//!
//! # Limitations
//!
//! This implementation does not support SSE streaming for server-initiated messages. The GET
//! endpoint returns 405 Method Not Allowed per the spec. Batch requests (JSON-RPC arrays) are
//! also not yet supported.

use std::{collections::HashMap, future::Future, sync::Arc};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use tokio::sync::RwLock;

use crate::{McpServer, Output, ToolOutput, ToolRegistry, parse_line};

/// Session ID header name per MCP spec.
pub const SESSION_ID_HEADER: &str = "mcp-session-id";

/// Handles tool invocations for an MCP server.
///
/// Similar to the tokio transport's `ToolHandler`, but takes `&self` instead of `&mut self`
/// because axum handlers must be `Clone + Send + Sync` for concurrent request handling. Use
/// interior mutability (e.g., `Arc<Mutex<...>>`) for mutable state.
pub trait ToolHandler<R: ToolRegistry>: Send + Sync {
    /// Error type returned by the handler.
    type Error: std::fmt::Display;

    /// Handles a tool invocation and returns the result.
    fn handle(&self, tool: R) -> impl Future<Output = Result<ToolOutput, Self::Error>> + Send;
}

impl<R, F, Fut, T, E> ToolHandler<R> for F
where
    R: ToolRegistry + Send,
    F: Fn(R) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, E>> + Send,
    T: Into<ToolOutput>,
    E: std::fmt::Display,
{
    type Error = E;

    async fn handle(&self, tool: R) -> Result<ToolOutput, E> {
        self(tool).await.map(Into::into)
    }
}

/// Session storage for MCP servers.
///
/// Each session has its own [`McpServer`] instance tracking protocol state. Sessions are created
/// on `initialize` requests and removed on `DELETE` or timeout.
pub struct Sessions<R: ToolRegistry> {
    /// Map of session ID to server instance.
    servers: RwLock<HashMap<String, tokio::sync::Mutex<McpServer<R>>>>,
    /// Factory function for creating new servers.
    server_fn: Arc<dyn Fn() -> McpServer<R> + Send + Sync>,
}

impl<R: ToolRegistry> Sessions<R> {
    /// Creates a new session store with the given server factory.
    ///
    /// The factory is called once per new session to create a fresh [`McpServer`].
    pub fn new<F>(server_fn: F) -> Self
    where
        F: Fn() -> McpServer<R> + Send + Sync + 'static,
    {
        Self {
            servers: RwLock::new(HashMap::new()),
            server_fn: Arc::new(server_fn),
        }
    }

    /// Creates a new session and returns its ID.
    async fn create_session(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let server = (self.server_fn)();
        self.servers
            .write()
            .await
            .insert(id.clone(), tokio::sync::Mutex::new(server));
        id
    }

    /// Removes a session by ID. Returns true if the session existed.
    pub async fn remove(&self, id: &str) -> bool {
        self.servers.write().await.remove(id).is_some()
    }

    /// Returns the number of active sessions.
    pub async fn len(&self) -> usize {
        self.servers.read().await.len()
    }

    /// Returns true if there are no active sessions.
    pub async fn is_empty(&self) -> bool {
        self.servers.read().await.is_empty()
    }
}

/// Shared state for axum handlers.
struct AppState<R: ToolRegistry, H: ToolHandler<R>> {
    /// Session storage holding one [`McpServer`] per active session.
    sessions: Arc<Sessions<R>>,
    /// Handler for tool invocations, called when `Output::ToolCall` is returned.
    handler: H,
}

impl<R: ToolRegistry, H: ToolHandler<R> + Clone> Clone for AppState<R, H> {
    fn clone(&self) -> Self {
        Self {
            sessions: Arc::clone(&self.sessions),
            handler: self.handler.clone(),
        }
    }
}

/// Creates an axum [`Router`] for an MCP endpoint.
///
/// The router handles:
/// - `POST /`: Client messages (initialize, requests, notifications)
/// - `DELETE /`: Session termination
///
/// # Arguments
///
/// * `sessions` - Session storage created with [`Sessions::new`]
/// * `handler` - Tool handler implementing [`ToolHandler`]
///
/// # Example
///
/// ```ignore
/// use std::convert::Infallible;
/// use mercutio::{McpServer, io::axum::{Sessions, mcp_router}};
///
/// mercutio::tool_registry! {
///     enum MyTools {
///         GetWeather("get_weather", "Gets weather") { city: String },
///     }
/// }
///
/// let sessions = Sessions::new(|| {
///     McpServer::<MyTools>::builder()
///         .name("my-server")
///         .version("1.0.0")
///         .build()
/// });
///
/// let router = mcp_router(sessions, |tool: MyTools| async move {
///     match tool {
///         MyTools::GetWeather(input) => {
///             Ok::<_, Infallible>(format!("Weather in {}: sunny", input.city))
///         }
///     }
/// });
///
/// // Mount at your desired path
/// let app = axum::Router::new().nest("/mcp", router);
/// ```
pub fn mcp_router<R, H>(sessions: Sessions<R>, handler: H) -> Router
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let state = AppState {
        sessions: Arc::new(sessions),
        handler,
    };

    Router::new()
        .route("/", post(handle_post::<R, H>))
        .route("/", get(handle_get))
        .route("/", delete(handle_delete::<R, H>))
        .with_state(state)
}

/// Handles POST requests (client messages).
async fn handle_post<R, H>(
    State(state): State<AppState<R, H>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let session_id = headers
        .get(SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UTF-8").into_response(),
    };

    let msg = match parse_line(body_str) {
        Ok(m) => m,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON-RPC: {e}")).into_response();
        }
    };

    match session_id {
        Some(id) => handle_existing_session(&state, &id, msg).await,
        None => handle_new_session(&state, msg).await,
    }
}

/// Handles GET requests (SSE streams).
///
/// Returns 405 Method Not Allowed since SSE streaming is not implemented.
async fn handle_get() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

/// Handles a message for an existing session.
async fn handle_existing_session<R, H>(
    state: &AppState<R, H>,
    session_id: &str,
    msg: rust_mcp_schema::JsonrpcMessage,
) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let servers = state.sessions.servers.read().await;
    let server_mutex = match servers.get(session_id) {
        Some(s) => s,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let mut server = server_mutex.lock().await;
    let output = server.handle(msg);
    drop(server);
    drop(servers);

    output_to_response(output, &state.handler, Some(session_id)).await
}

/// Handles a message for a new session (no session ID header).
async fn handle_new_session<R, H>(
    state: &AppState<R, H>,
    msg: rust_mcp_schema::JsonrpcMessage,
) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let session_id = state.sessions.create_session().await;

    let servers = state.sessions.servers.read().await;
    let server_mutex = servers.get(&session_id).expect("just created");
    let mut server = server_mutex.lock().await;
    let output = server.handle(msg);
    drop(server);
    drop(servers);

    if let Output::ProtocolError(_) = &output {
        state.sessions.remove(&session_id).await;
    }

    output_to_response(output, &state.handler, Some(&session_id)).await
}

/// Builds a JSON response with optional session ID header.
fn json_response(msg: &crate::OutgoingMessage, session_id: Option<&str>) -> Response {
    let json = match serde_json::to_vec(msg.as_inner()) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialization error: {e}"),
            )
                .into_response();
        }
    };

    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json,
    )
        .into_response();

    if let Some(id) = session_id
        && let Ok(value) = HeaderValue::from_str(id)
    {
        response.headers_mut().insert(SESSION_ID_HEADER, value);
    }

    response
}

/// Converts server output to an HTTP response.
async fn output_to_response<R, H>(
    output: Output<R>,
    handler: &H,
    session_id: Option<&str>,
) -> Response
where
    R: ToolRegistry,
    H: ToolHandler<R>,
{
    match output {
        Output::Send(msg) => json_response(&msg, session_id),
        Output::ToolCall { tool, responder } => {
            let result = handler.handle(tool).await;
            let msg = responder.respond(result);
            json_response(&msg, session_id)
        }
        Output::None => StatusCode::ACCEPTED.into_response(),
        Output::ProtocolError(e) => {
            (StatusCode::BAD_REQUEST, format!("protocol error: {e}")).into_response()
        }
    }
}

/// Handles DELETE requests (session termination).
async fn handle_delete<R, H>(State(state): State<AppState<R, H>>, headers: HeaderMap) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let session_id = match headers.get(SESSION_ID_HEADER).and_then(|v| v.to_str().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "missing session ID").into_response(),
    };

    if state.sessions.remove(session_id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::util::ServiceExt;

    use super::{SESSION_ID_HEADER, Sessions, mcp_router};
    use crate::{McpServer, NoTools};

    fn test_sessions() -> Sessions<NoTools> {
        Sessions::new(|| McpServer::builder().name("test").version("1.0").build())
    }

    fn test_handler(_: NoTools) -> Result<String, std::convert::Infallible> {
        unreachable!("no tools")
    }

    #[tokio::test]
    async fn initialize_creates_session() {
        let router = mcp_router(test_sessions(), |t| async { test_handler(t) });

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(SESSION_ID_HEADER));

        let session_id = response
            .headers()
            .get(SESSION_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(!session_id.is_empty());
    }

    #[tokio::test]
    async fn subsequent_request_requires_session() {
        let sessions = test_sessions();
        let router = mcp_router(sessions, |t| async { test_handler(t) });

        let init_body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;

        let init_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(init_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        let session_id = init_response
            .headers()
            .get(SESSION_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let initialized_body = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .header(SESSION_ID_HEADER, &session_id)
                    .body(Body::from(initialized_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let ping_body = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .header(SESSION_ID_HEADER, &session_id)
                    .body(Body::from(ping_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_session_returns_404() {
        let router = mcp_router(test_sessions(), |t| async { test_handler(t) });

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .header(SESSION_ID_HEADER, "nonexistent-session")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_removes_session() {
        let sessions = Sessions::new(|| McpServer::builder().name("test").version("1.0").build());
        let router = mcp_router(sessions, |t| async { test_handler(t) });

        let init_body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;

        let init_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(init_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        let session_id = init_response
            .headers()
            .get(SESSION_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let delete_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/")
                    .header(SESSION_ID_HEADER, &session_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        let ping_body = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .header(SESSION_ID_HEADER, &session_id)
                    .body(Body::from(ping_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_returns_405() {
        let router = mcp_router(test_sessions(), |t| async { test_handler(t) });

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
