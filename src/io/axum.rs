//! Axum HTTP transport for MCP Streamable HTTP.
//!
//! Implements the
//! [Streamable HTTP transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#streamable-http)
//! from MCP specification 2025-11-25. Provides session management and an axum router for handling
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
//! let router = mcp_router(builder, |_session_id, tool: MyTools| async move {
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

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Bytes,
    extract::{FromRequestParts, State},
    http::{HeaderValue, StatusCode, header, header::ToStrError, request::Parts},
    response::{IntoResponse, Response},
    routing::post,
};
use rand::Rng;
use thiserror::Error;

pub use super::{
    ToolHandler,
    session_id::{HTTP_SESSION_ID_HEADER, McpSessionId, ParseSessionIdError},
};
use crate::{McpServer, McpServerBuilder, Output, ToolRegistry, parse_line};

/// Rejection type when session ID extraction fails.
#[derive(Debug, Error)]
pub enum SessionIdRejection {
    /// The `Mcp-Session-Id` header is missing.
    #[error("missing session ID header `{HTTP_SESSION_ID_HEADER}`")]
    Missing,
    /// The header value is not valid UTF-8.
    #[error("session ID header not valid UTF-8")]
    InvalidUtf8(#[source] ToStrError),
    /// The header value failed to parse as a session ID.
    #[error("invalid session ID")]
    InvalidFormat(#[source] ParseSessionIdError),
}

impl IntoResponse for SessionIdRejection {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.to_string()).into_response()
    }
}

impl<S> FromRequestParts<S> for McpSessionId
where
    S: Send + Sync,
{
    type Rejection = SessionIdRejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let value = parts
            .headers
            .get(HTTP_SESSION_ID_HEADER)
            .ok_or(SessionIdRejection::Missing)?;

        let s = value.to_str().map_err(SessionIdRejection::InvalidUtf8)?;
        s.parse().map_err(SessionIdRejection::InvalidFormat)
    }
}

/// Extractor for an optional session ID.
///
/// Returns `None` if the header is missing, `Some(id)` if valid, or rejects with
/// [`SessionIdRejection`] if the header is present but malformed.
#[derive(Clone, Copy, Debug)]
pub struct OptionalSessionId(pub Option<McpSessionId>);

impl<S> FromRequestParts<S> for OptionalSessionId
where
    S: Send + Sync,
{
    type Rejection = SessionIdRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if !parts.headers.contains_key(HTTP_SESSION_ID_HEADER) {
            return Ok(Self(None));
        }

        let id = McpSessionId::from_request_parts(parts, state).await?;
        Ok(Self(Some(id)))
    }
}

/// Manages session lifecycle for the HTTP transport.
///
/// Implementations handle session creation, access, and removal.
pub trait SessionStorage<R: ToolRegistry>: Send + Sync + 'static {
    /// Error type for storage operations.
    type Error: std::fmt::Display + Send;

    /// Creates a new session with the given server, returning its ID.
    fn create(
        &self,
        server: McpServer<R>,
    ) -> impl Future<Output = Result<McpSessionId, Self::Error>> + Send;

    /// Calls a function with exclusive access to a session's server.
    ///
    /// Returns `Ok(None)` if the session does not exist, `Ok(Some(result))` on success.
    fn with_session<T: Send>(
        &self,
        id: McpSessionId,
        f: impl FnOnce(&mut McpServer<R>) -> T + Send,
    ) -> impl Future<Output = Result<Option<T>, Self::Error>> + Send;

    /// Removes a session, returning `true` if it existed.
    fn remove(&self, id: McpSessionId) -> impl Future<Output = bool> + Send;
}

/// Error from [`InMemoryStorage`] operations.
#[derive(Clone, Copy, Debug, Error)]
pub enum InMemoryStorageError {
    /// Storage is at capacity and no sessions are old enough to evict.
    #[error("session storage at capacity")]
    AtCapacity,
}

/// Entry in the in-memory session storage.
struct SessionEntry<R: ToolRegistry> {
    /// The MCP server instance for this session.
    server: McpServer<R>,
    /// When this session was last accessed.
    last_accessed: Instant,
}

/// In-memory session storage with LRU eviction.
///
/// Stores sessions in memory with a configurable capacity limit. When full, evicts the
/// least-recently-used session that is older than the minimum eviction age. If no sessions
/// qualify for eviction, returns [`InMemoryStorageError::AtCapacity`].
pub struct InMemoryStorage<R: ToolRegistry> {
    /// Session entries keyed by session ID.
    sessions: RwLock<HashMap<McpSessionId, SessionEntry<R>>>,
    /// Maximum number of sessions to store.
    capacity: usize,
    /// Minimum age before a session can be evicted.
    min_eviction_age: Duration,
}

impl<R: ToolRegistry> InMemoryStorage<R> {
    /// Creates a new in-memory storage with the given capacity and minimum eviction age.
    ///
    /// Sessions younger than `min_eviction_age` will not be evicted even when at capacity,
    /// causing [`InMemoryStorageError::AtCapacity`] errors instead.
    pub fn new(capacity: usize, min_eviction_age: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            capacity,
            min_eviction_age,
        }
    }
}

impl<R: ToolRegistry + Send + Sync + 'static> SessionStorage<R> for InMemoryStorage<R> {
    type Error = InMemoryStorageError;

    async fn create(&self, server: McpServer<R>) -> Result<McpSessionId, Self::Error> {
        let id: McpSessionId = rand::rng().random();
        let now = Instant::now();

        let mut sessions = self.sessions.write().expect("lock poisoned");

        if sessions.len() >= self.capacity {
            let eviction_threshold = now - self.min_eviction_age;
            let oldest = sessions
                .iter()
                .filter(|(_, entry)| entry.last_accessed < eviction_threshold)
                .min_by_key(|(_, entry)| entry.last_accessed)
                .map(|(id, _)| *id);

            match oldest {
                Some(oldest_id) => {
                    sessions.remove(&oldest_id);
                }
                None => return Err(InMemoryStorageError::AtCapacity),
            }
        }

        sessions.insert(
            id,
            SessionEntry {
                server,
                last_accessed: now,
            },
        );

        Ok(id)
    }

    async fn with_session<T: Send>(
        &self,
        id: McpSessionId,
        f: impl FnOnce(&mut McpServer<R>) -> T + Send,
    ) -> Result<Option<T>, Self::Error> {
        let mut sessions = self.sessions.write().expect("lock poisoned");
        let Some(entry) = sessions.get_mut(&id) else {
            return Ok(None);
        };

        entry.last_accessed = Instant::now();
        Ok(Some(f(&mut entry.server)))
    }

    async fn remove(&self, id: McpSessionId) -> bool {
        self.sessions
            .write()
            .expect("lock poisoned")
            .remove(&id)
            .is_some()
    }
}

/// Default capacity for [`InMemoryStorage`].
pub const DEFAULT_CAPACITY: usize = 10_000;

/// Default minimum eviction age for [`InMemoryStorage`].
pub const DEFAULT_MIN_EVICTION_AGE: Duration = Duration::from_secs(120);

/// Shared state for axum handlers.
struct AppState<R: ToolRegistry, H: ToolHandler<R>, S: SessionStorage<R>> {
    /// Builder for creating new server instances.
    builder: Arc<McpServerBuilder<R>>,
    /// Session storage.
    storage: Arc<S>,
    /// Handler for tool invocations.
    handler: H,
}

impl<R: ToolRegistry, H: ToolHandler<R> + Clone, S: SessionStorage<R>> Clone for AppState<R, H, S> {
    fn clone(&self) -> Self {
        Self {
            builder: Arc::clone(&self.builder),
            storage: Arc::clone(&self.storage),
            handler: self.handler.clone(),
        }
    }
}

/// Builder for creating an MCP router with custom configuration.
///
/// Use [`McpRouter::builder`] to create a builder, then call `.build()` to get the axum
/// [`Router`].
pub struct McpRouter;

impl McpRouter {
    /// Creates a builder with the required server builder and handler.
    pub fn builder<R, H>(
        builder: McpServerBuilder<R>,
        handler: H,
    ) -> McpRouterBuilder<R, H, InMemoryStorage<R>>
    where
        R: ToolRegistry + Send + Sync + 'static,
        H: ToolHandler<R> + Clone + Send + Sync + 'static,
    {
        McpRouterBuilder {
            builder,
            handler,
            storage: InMemoryStorage::new(DEFAULT_CAPACITY, DEFAULT_MIN_EVICTION_AGE),
        }
    }
}

/// Builder for configuring an MCP router.
pub struct McpRouterBuilder<R: ToolRegistry, H, S> {
    /// Builder for creating new server instances.
    builder: McpServerBuilder<R>,
    /// Handler for tool invocations.
    handler: H,
    /// Session storage.
    storage: S,
}

impl<R, H, S> McpRouterBuilder<R, H, S>
where
    R: ToolRegistry + Send + Sync + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
    S: SessionStorage<R>,
{
    /// Sets a custom session storage implementation.
    pub fn storage<S2: SessionStorage<R>>(self, storage: S2) -> McpRouterBuilder<R, H, S2> {
        McpRouterBuilder {
            builder: self.builder,
            handler: self.handler,
            storage,
        }
    }

    /// Builds the axum [`Router`].
    pub fn build(self) -> Router {
        let state = AppState {
            builder: Arc::new(self.builder),
            storage: Arc::new(self.storage),
            handler: self.handler,
        };

        Router::new()
            .route(
                "/",
                post(handle_post::<R, H, S>)
                    .get(handle_get)
                    .delete(handle_delete::<R, H, S>),
            )
            .with_state(state)
    }
}

/// Creates an axum [`Router`] for an MCP endpoint.
///
/// Returns a router handling `POST /` for client messages and `DELETE /` for session termination.
/// See the [module documentation](self) for a complete example.
///
/// Uses [`InMemoryStorage`] with [`DEFAULT_CAPACITY`] and [`DEFAULT_MIN_EVICTION_AGE`]. For
/// custom storage, use [`McpRouter::builder`].
pub fn mcp_router<R, H>(builder: McpServerBuilder<R>, handler: H) -> Router
where
    R: ToolRegistry + Send + Sync + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
{
    McpRouter::builder(builder, handler).build()
}

/// Handles POST requests (client messages).
async fn handle_post<R, H, S>(
    State(state): State<AppState<R, H, S>>,
    OptionalSessionId(session_id): OptionalSessionId,
    body: Bytes,
) -> Response
where
    R: ToolRegistry + Send + Sync + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
    S: SessionStorage<R>,
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

    let session_id = match session_id {
        Some(id) => id,
        None => match state.storage.create(state.builder.build()).await {
            Ok(id) => id,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("storage error: {e}"),
                )
                    .into_response();
            }
        },
    };

    handle_session(&state, session_id, msg).await
}

/// Handles GET requests (SSE streams).
///
/// Returns 405 Method Not Allowed since SSE streaming is not implemented.
async fn handle_get() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

/// Handles a message for a session.
async fn handle_session<R, H, S>(
    state: &AppState<R, H, S>,
    session_id: McpSessionId,
    msg: rust_mcp_schema::JsonrpcMessage,
) -> Response
where
    R: ToolRegistry + Send + Sync + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
    S: SessionStorage<R>,
{
    let output = match state
        .storage
        .with_session(session_id, |server| server.handle(msg))
        .await
    {
        Ok(Some(output)) => output,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("storage error: {e}"),
            )
                .into_response();
        }
    };

    match output {
        Output::Send(msg) => json_response(&msg, session_id),
        Output::ToolCall { tool, responder } => {
            let result = state.handler.handle(Some(session_id), tool).await;
            json_response(&responder.respond(result), session_id)
        }
        Output::None => StatusCode::ACCEPTED.into_response(),
        Output::ProtocolError(e) => {
            state.storage.remove(session_id).await;
            (StatusCode::BAD_REQUEST, format!("protocol error: {e}")).into_response()
        }
    }
}

/// Builds a JSON response with session ID header.
fn json_response(msg: &crate::OutgoingMessage, session_id: McpSessionId) -> Response {
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

    let value = HeaderValue::from_str(&session_id.to_string()).expect("hex is valid header");
    response.headers_mut().insert(HTTP_SESSION_ID_HEADER, value);

    response
}

/// Handles DELETE requests (session termination).
async fn handle_delete<R, H, S>(
    State(state): State<AppState<R, H, S>>,
    session_id: McpSessionId,
) -> Response
where
    R: ToolRegistry + Send + Sync + 'static,
    H: ToolHandler<R> + Clone + Send + Sync + 'static,
    S: SessionStorage<R>,
{
    if state.storage.remove(session_id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    //! Exercises HTTP session lifecycle and deterministic storage eviction.

    use std::{
        convert::Infallible,
        time::{Duration, Instant},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        response::Response,
    };
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tower::util::ServiceExt;

    use super::{
        HTTP_SESSION_ID_HEADER, InMemoryStorage, InMemoryStorageError, McpSessionId,
        SessionStorage, mcp_router,
    };
    use crate::{McpServer, NoTools, ToolDef, parse_line};

    /// Starts an MCP session.
    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    /// Completes the initialization handshake.
    const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    /// Checks an existing session.
    const PING: &str = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;

    /// Supplies text to a session-aware handler.
    #[derive(Deserialize, JsonSchema)]
    struct Echo {
        /// Text to echo.
        value: String,
    }

    impl ToolDef for Echo {
        const NAME: &'static str = "echo";
        const DESCRIPTION: &'static str = "Echoes text";
    }

    /// Creates a router whose result identifies the handler's session.
    fn test_router() -> Router {
        mcp_router(
            McpServer::builder(),
            |session: Option<McpSessionId>, input: Echo| async move {
                Ok::<_, Infallible>(format!(
                    "{}: {}",
                    session.expect("HTTP session"),
                    input.value
                ))
            },
        )
    }

    /// Sends one HTTP request with an optional session header.
    async fn request(
        router: &Router,
        method: &str,
        session: Option<&str>,
        body: &'static str,
    ) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri("/")
            .header("content-type", "application/json");
        if let Some(session) = session {
            request = request.header(HTTP_SESSION_ID_HEADER, session);
        }
        router
            .clone()
            .oneshot(request.body(Body::from(body)).expect("valid request"))
            .await
            .expect("router response")
    }

    /// Dispatches with the assigned session and removes it on deletion or protocol failure.
    #[tokio::test]
    async fn session_dispatch_and_cleanup() {
        for (method, body, status) in [
            ("DELETE", "", StatusCode::NO_CONTENT),
            (
                "POST",
                r#"{"jsonrpc":"2.0","id":99,"result":{}}"#,
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let router = test_router();
            let response = request(&router, "POST", None, INITIALIZE).await;
            assert_eq!(response.status(), StatusCode::OK);
            let session = response
                .headers()
                .get(HTTP_SESSION_ID_HEADER)
                .expect("assigned session")
                .to_str()
                .expect("ASCII session")
                .to_string();
            let _: McpSessionId = session.parse().expect("valid session identifier");

            let response = request(&router, "POST", Some(&session), INITIALIZED).await;
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"value":"hello"}}}"#;
            let response = request(&router, "POST", Some(&session), call).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[HTTP_SESSION_ID_HEADER], session);
            let body_bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body");
            let result: Value = serde_json::from_slice(&body_bytes).expect("JSON response");
            assert_eq!(
                result,
                json!({
                    "jsonrpc": "2.0", "id": 3,
                    "result": {"content": [{"type": "text", "text": format!("{session}: hello")}], "isError": false}
                })
            );

            assert_eq!(
                request(&router, method, Some(&session), body)
                    .await
                    .status(),
                status
            );
            assert_eq!(
                request(&router, "POST", Some(&session), PING)
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                request(&router, "DELETE", Some(&session), "")
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
    }

    /// Rejects unsupported methods, invalid bodies, and missing or malformed session headers.
    #[tokio::test]
    async fn rejects_invalid_http_requests() {
        let router = test_router();
        for (method, session, body, status) in [
            ("GET", None, "", StatusCode::METHOD_NOT_ALLOWED),
            ("DELETE", None, "", StatusCode::BAD_REQUEST),
            ("POST", None, "invalid JSON", StatusCode::BAD_REQUEST),
            ("POST", Some("not-hex"), PING, StatusCode::BAD_REQUEST),
            (
                "POST",
                Some("00000000000000000000000000000000"),
                PING,
                StatusCode::NOT_FOUND,
            ),
        ] {
            assert_eq!(
                request(&router, method, session, body).await.status(),
                status
            );
        }
    }

    /// Retains server mutations and reports a removed session as absent.
    #[tokio::test]
    async fn storage_session_lifecycle() {
        let storage = InMemoryStorage::new(1, Duration::ZERO);
        let id = storage
            .create(McpServer::<NoTools>::builder().build())
            .await
            .expect("new session");
        storage
            .with_session(id, |server| {
                let _ = server.handle(parse_line(INITIALIZE).expect("initialize request"));
                let _ = server.handle(parse_line(INITIALIZED).expect("initialized notification"));
            })
            .await
            .expect("access session");
        assert_eq!(
            storage
                .with_session(id, |server| server.is_ready())
                .await
                .expect("read state"),
            Some(true)
        );
        assert!(storage.remove(id).await);
        assert!(!storage.remove(id).await);
        assert_eq!(
            storage
                .with_session(id, |_| ())
                .await
                .expect("missing session"),
            None
        );
    }

    /// Protects recent sessions and evicts by last access rather than creation order.
    #[tokio::test]
    async fn storage_capacity_and_lru_eviction() {
        let storage = InMemoryStorage::new(2, Duration::from_secs(60));
        let first = storage
            .create(McpServer::<NoTools>::builder().build())
            .await
            .expect("first session");
        let second = storage
            .create(McpServer::builder().build())
            .await
            .expect("second session");
        assert!(matches!(
            storage.create(McpServer::builder().build()).await,
            Err(InMemoryStorageError::AtCapacity)
        ));

        {
            let mut sessions = storage.sessions.write().expect("session lock");
            let stale = Instant::now() - Duration::from_secs(120);
            sessions.get_mut(&first).expect("first entry").last_accessed =
                stale - Duration::from_secs(1);
            sessions
                .get_mut(&second)
                .expect("second entry")
                .last_accessed = stale;
        }
        assert_eq!(
            storage
                .with_session(first, |_| ())
                .await
                .expect("refresh first session"),
            Some(())
        );
        let third = storage
            .create(McpServer::builder().build())
            .await
            .expect("evict stale session");
        for (id, expected) in [(first, Some(())), (second, None), (third, Some(()))] {
            assert_eq!(
                storage
                    .with_session(id, |_| ())
                    .await
                    .expect("session lookup"),
                expected
            );
        }
    }
}
