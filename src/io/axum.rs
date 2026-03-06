//! Axum HTTP transport for MCP Streamable HTTP.
//!
//! Implements the
//! [Streamable HTTP transport](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http)
//! from MCP specification 2025-03-26. Provides session management and an axum router for handling
//! MCP requests over HTTP.
//!
//! # Example
//!
//! ```no_run
//! use std::convert::Infallible;
//! use mercutio::{McpServer, io::axum::mcp_router};
//!
//! mercutio::tool_registry! {
//!     enum MyTools {
//!         GetWeather("get_weather", "Gets weather") { city: String },
//!     }
//! }
//!
//! let mut builder = McpServer::<MyTools>::builder();
//! builder.name("my-server").version("1.0.0");
//!
//! let router = mcp_router(builder, |tool: MyTools| async move {
//!     match tool {
//!         MyTools::GetWeather(input) => {
//!             Ok::<_, Infallible>(format!("Weather in {}: sunny", input.city))
//!         }
//!     }
//! });
//!
//! // Mount at your desired path
//! let app = axum::Router::new().nest("/mcp", router);
//! ```
//!
//! # Protocol
//!
//! The transport uses a single endpoint:
//!
//! - `POST /`: Client messages (requests, notifications, responses)
//! - `GET /`: Returns 405 Method Not Allowed (SSE streaming not implemented)
//! - `DELETE /`: Session termination
//!
//! Sessions are identified by the `Mcp-Session-Id` header. The server generates a session ID when
//! responding to an `initialize` request and the client must include it in subsequent requests.
//!
//! # Limitations
//!
//! - No SSE streaming for server-initiated messages
//! - No batch requests (JSON-RPC arrays)
//! - No session persistence across server restarts

mod session_id;

use std::{collections::HashMap, sync::Arc};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use rand::Rng;
use tokio::sync::{Mutex, RwLock};

pub use self::session_id::{
    McpSessionId, OptionalSessionId, ParseSessionIdError, SESSION_ID_HEADER, SessionIdRejection,
};
pub use super::ToolHandler;
use crate::{McpServer, McpServerBuilder, Output, ToolRegistry, parse_line};

/// Type alias for the session storage map.
type SessionMap<R> = Arc<RwLock<HashMap<McpSessionId, Mutex<McpServer<R>>>>>;

/// Shared state for axum handlers.
struct AppState<R: ToolRegistry, H: ToolHandler<R>> {
    /// Builder for creating new server instances.
    builder: Arc<McpServerBuilder<R>>,
    /// Session storage holding one [`McpServer`] per active session.
    sessions: SessionMap<R>,
    /// Handler for tool invocations, called when `Output::ToolCall` is returned.
    handler: H,
}

impl<R: ToolRegistry, H: ToolHandler<R> + Clone> Clone for AppState<R, H> {
    fn clone(&self) -> Self {
        Self {
            builder: Arc::clone(&self.builder),
            sessions: Arc::clone(&self.sessions),
            handler: self.handler.clone(),
        }
    }
}

/// Creates an axum [`Router`] for an MCP endpoint.
///
/// Returns a router handling `POST /` for client messages and `DELETE /` for session termination.
/// See the [module documentation](self) for a complete example.
///
/// Session storage is managed internally and not exposed. Sessions are lost on server restart.
pub fn mcp_router<R, H>(builder: McpServerBuilder<R>, handler: H) -> Router
where
    R: ToolRegistry + Send + Sync + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let state = AppState {
        builder: Arc::new(builder),
        sessions: Arc::new(RwLock::new(HashMap::new())),
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
    OptionalSessionId(session_id): OptionalSessionId,
    body: Bytes,
) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
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
        Some(id) => handle_existing_session(&state, id, msg).await,
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
    session_id: McpSessionId,
    msg: rust_mcp_schema::JsonrpcMessage,
) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    let sessions = state.sessions.read().await;
    let server_mutex = match sessions.get(&session_id) {
        Some(s) => s,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let mut server = server_mutex.lock().await;
    let output = server.handle(msg);
    drop(server);
    drop(sessions);

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
    let session_id: McpSessionId = rand::rng().random();
    let server = state.builder.build();

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, Mutex::new(server));
    }

    let sessions = state.sessions.read().await;
    let server_mutex = sessions.get(&session_id).expect("just created");
    let mut server = server_mutex.lock().await;
    let output = server.handle(msg);
    drop(server);
    drop(sessions);

    if let Output::ProtocolError(_) = &output {
        state.sessions.write().await.remove(&session_id);
    }

    output_to_response(output, &state.handler, Some(session_id)).await
}

/// Builds a JSON response with optional session ID header.
fn json_response(msg: &crate::OutgoingMessage, session_id: Option<McpSessionId>) -> Response {
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

    if let Some(id) = session_id {
        let value = HeaderValue::from_str(&id.to_string()).expect("hex is valid header");
        response.headers_mut().insert(SESSION_ID_HEADER, value);
    }

    response
}

/// Converts server output to an HTTP response.
async fn output_to_response<R, H>(
    output: Output<R>,
    handler: &H,
    session_id: Option<McpSessionId>,
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
async fn handle_delete<R, H>(
    State(state): State<AppState<R, H>>,
    session_id: McpSessionId,
) -> Response
where
    R: ToolRegistry + Send + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    if state.sessions.write().await.remove(&session_id).is_some() {
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

    use super::{SESSION_ID_HEADER, mcp_router};
    use crate::{McpServer, McpServerBuilder, NoTools};

    fn test_builder() -> McpServerBuilder<NoTools> {
        let mut builder = McpServer::builder();
        builder.name("test").version("1.0");
        builder
    }

    fn test_handler(_: NoTools) -> Result<String, std::convert::Infallible> {
        unreachable!("no tools")
    }

    #[tokio::test]
    async fn initialize_creates_session() {
        let router = mcp_router(test_builder(), |t| async { test_handler(t) });

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
        let router = mcp_router(test_builder(), |t| async { test_handler(t) });

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
        let router = mcp_router(test_builder(), |t| async { test_handler(t) });

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .header(SESSION_ID_HEADER, "00000000000000000000000000000000")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_removes_session() {
        let router = mcp_router(test_builder(), |t| async { test_handler(t) });

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
        let router = mcp_router(test_builder(), |t| async { test_handler(t) });

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
