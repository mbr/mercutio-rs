//! Exercises extraction and delivery of explicit application context.

#![cfg(any(
    feature = "cli",
    feature = "io-stdlib",
    feature = "io-tokio",
    feature = "io-axum"
))]

#[cfg(any(feature = "cli", feature = "io-tokio", feature = "io-axum"))]
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};

use mercutio::ToolDef;
#[cfg(any(feature = "cli", feature = "io-tokio", feature = "io-axum"))]
use mercutio::{
    ToolOutput,
    io::{McpSessionId, ToolHandler},
};
use schemars::JsonSchema;
use serde::Deserialize;

/// Invokes a handler without putting application context in tool arguments.
#[derive(Deserialize, JsonSchema)]
struct ReadContext {}

impl ToolDef for ReadContext {
    const NAME: &'static str = "read_context";
    const DESCRIPTION: &'static str = "Reports invocation context";
}

/// Non-cloneable context whose call counter reveals reuse across invocations.
struct Context {
    /// Application-defined selection.
    project: String,
    /// Calls dispatched against this exact context value.
    calls: AtomicUsize,
}

impl Context {
    /// Constructs fresh context for one entry point.
    fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            calls: AtomicUsize::new(0),
        }
    }

    /// Reports the selection and increments the local call counter.
    fn report(&self) -> String {
        format!(
            "{}:{}",
            self.project,
            self.calls.fetch_add(1, Ordering::SeqCst)
        )
    }
}

/// Borrows request context across an await without owning it as handler state.
#[cfg(any(feature = "cli", feature = "io-tokio", feature = "io-axum"))]
#[derive(Clone)]
struct Handler;

#[cfg(any(feature = "cli", feature = "io-tokio", feature = "io-axum"))]
impl ToolHandler<ReadContext, Context> for Handler {
    type Error = Infallible;

    async fn handle(
        &self,
        context: &Context,
        _session: Option<McpSessionId>,
        _tool: ReadContext,
    ) -> Result<ToolOutput, Self::Error> {
        tokio::task::yield_now().await;
        Ok(context.report().into())
    }
}

#[cfg(feature = "cli")]
mod cli {
    //! Checks extraction from generated and nested command matches.

    use std::{
        convert::Infallible,
        io::{self, Cursor},
    };

    use clap::{Arg, ArgMatches, Command};
    use mercutio::cli::{FromCli, ToolRegistryExt as _};

    use super::{Context, Handler, ReadContext};

    impl FromCli for Context {
        type Error = io::Error;

        fn augment_command(command: Command) -> Command {
            command.arg(Arg::new("project").long("project").required(true))
        }

        fn from_cli(matches: &ArgMatches) -> Result<Self, Self::Error> {
            let project = matches
                .get_one::<String>("project")
                .expect("required option");
            if project == "rejected" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "project rejected",
                ));
            }
            Ok(Self::new(project))
        }
    }

    /// Delivers context through sync, async, JSON, and nested entry points.
    #[tokio::test]
    async fn invocation_context() {
        let cli = ReadContext::cli("tools")
            .context::<Context>()
            .build()
            .expect("CLI");
        let args = ["tools", "--project", "marc/general", "read-context"];
        let mut output = Vec::new();
        cli.run_on(
            args,
            io::empty(),
            &mut output,
            io::sink(),
            |context, session, _| {
                assert!(session.is_none());
                Ok::<_, Infallible>(context.report())
            },
        )
        .expect("sync invocation");
        assert_eq!(output, b"marc/general:0\n");

        output.clear();
        cli.run_async_on(args, io::empty(), &mut output, io::sink(), &mut Handler)
            .await
            .expect("async invocation");
        assert_eq!(output, b"marc/general:0\n");

        let invocation = cli
            .try_parse_from(
                [
                    "tools",
                    "--project",
                    "travel/japan",
                    "--input-json",
                    "read-context",
                ],
                Cursor::new("{}"),
            )
            .expect("JSON invocation");
        assert_eq!(invocation.context().project, "travel/japan");
        let (_, context, _) = invocation.into_parts();
        assert_eq!(context.report(), "travel/japan:0");

        let matches = cli
            .attach_to(Command::new("app"))
            .expect("nested CLI")
            .try_get_matches_from([
                "app",
                "tools",
                "--project",
                "nested/project",
                "read-context",
            ])
            .expect("nested matches");
        let invocation = cli
            .try_parse_matches(
                matches.subcommand_matches("tools").expect("subtree"),
                io::empty(),
            )
            .expect("nested invocation");
        assert_eq!(invocation.context().project, "nested/project");
    }

    /// Returns help and extraction failures without dispatching the handler.
    #[test]
    fn extraction_failure_and_help() {
        let cli = ReadContext::cli("tools")
            .context::<Context>()
            .build()
            .expect("CLI");
        for (args, status) in [
            (vec!["tools", "--project", "rejected", "read-context"], 2),
            (vec!["tools", "--project", "rejected", "--help"], 0),
            (vec!["tools", "read-context"], 2),
        ] {
            let mut output = Vec::new();
            let error = cli
                .run_on(
                    args,
                    io::empty(),
                    &mut output,
                    io::sink(),
                    |_, _, _| -> Result<String, Infallible> {
                        panic!("invalid context or help must not dispatch")
                    },
                )
                .expect_err("no invocation");
            assert_eq!(error.exit_code(), status);
            assert!(output.is_empty());
        }
    }
}

#[cfg(any(feature = "io-stdlib", feature = "io-tokio"))]
mod stdio {
    //! Checks startup extraction, reuse, and rejection before reading input.

    use std::io::{self, Cursor};

    use mercutio::{
        McpServer,
        io::{FromStdio, IoError},
    };
    use serde_json::Value;

    #[cfg(feature = "io-tokio")]
    use super::Handler;
    use super::{Context, ReadContext};

    /// One connection with two calls sharing startup context.
    const INPUT: &str = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"read_context\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"read_context\"}}\n",
    );

    impl FromStdio for Context {
        type Error = io::Error;

        fn from_stdio() -> Result<Self, Self::Error> {
            Ok(Self::new("stdio"))
        }
    }

    /// Rejects startup independently of input contents.
    struct Rejected;

    impl FromStdio for Rejected {
        type Error = io::Error;

        fn from_stdio() -> Result<Self, Self::Error> {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "context unavailable",
            ))
        }
    }

    /// Checks successive calls were made against the same startup context.
    fn assert_reports(output: &[u8], project: &str) {
        let responses: Vec<Value> = std::str::from_utf8(output)
            .expect("UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON"))
            .collect();
        assert_eq!(responses.len(), 3);
        for (index, response) in responses[1..].iter().enumerate() {
            assert_eq!(
                response["result"]["content"][0]["text"],
                format!("{project}:{index}")
            );
        }
    }

    /// Supports startup and supplied context, rejecting before reading input.
    #[cfg(feature = "io-stdlib")]
    #[test]
    fn synchronous_context() {
        let mut output = Vec::new();
        mercutio::io::stdlib::run_on(
            Cursor::new(INPUT),
            &mut output,
            McpServer::builder().build(),
            |context: &Context, session, _: ReadContext| {
                assert!(session.is_none());
                Ok::<_, io::Error>(context.report())
            },
        )
        .expect("stdio context");
        assert_reports(&output, "stdio");

        output.clear();
        mercutio::io::stdlib::run_on_with_context(
            Cursor::new(INPUT),
            &mut output,
            McpServer::builder().build(),
            &Context::new("provided"),
            |context, _, _: ReadContext| Ok::<_, io::Error>(context.report()),
        )
        .expect("provided context");
        assert_reports(&output, "provided");

        let mut input = Cursor::new(INPUT);
        let error = mercutio::io::stdlib::run_on(
            &mut input,
            io::sink(),
            McpServer::builder().build(),
            |_: &Rejected, _, _: ReadContext| -> Result<String, io::Error> {
                panic!("rejected context must not dispatch")
            },
        )
        .expect_err("rejected startup");
        assert!(matches!(error, IoError::Context(_)));
        assert_eq!(input.position(), 0);
    }

    /// Delivers borrowed startup context through the mutable async handler adapter.
    #[cfg(feature = "io-tokio")]
    #[tokio::test]
    async fn asynchronous_context() {
        let mut output = Vec::new();
        mercutio::io::tokio::run_on(
            Cursor::new(INPUT),
            &mut output,
            McpServer::builder().build(),
            Handler,
        )
        .await
        .expect("stdio context");
        assert_reports(&output, "stdio");

        output.clear();
        mercutio::io::tokio::run_on_with_context(
            Cursor::new(INPUT),
            &mut output,
            McpServer::builder().build(),
            &Context::new("provided"),
            Handler,
        )
        .await
        .expect("provided context");
        assert_reports(&output, "provided");

        let mut input = Cursor::new(INPUT);
        let error = mercutio::io::tokio::run_on(
            &mut input,
            tokio::io::sink(),
            McpServer::builder().build(),
            |_: &Rejected, _, _: ReadContext| async { Err::<String, _>("must not dispatch") },
        )
        .await
        .expect_err("rejected startup");
        assert!(matches!(error, IoError::Context(_)));
        assert_eq!(input.position(), 0);
    }
}

#[cfg(feature = "io-axum")]
mod http {
    //! Checks request extraction against application-owned state.

    use std::time::Duration;

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::{FromRequestParts, Query},
        http::{Request, StatusCode, request::Parts},
        response::Response,
    };
    use mercutio::{
        McpServer,
        io::axum::{HTTP_SESSION_ID_HEADER, InMemoryStorage, McpRouter},
    };
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{Context, Handler};

    /// State deliberately distinct from the tool handler and private router state.
    struct ApplicationState {
        /// Prefix applied while constructing request context.
        prefix: &'static str,
    }

    /// Application selection supplied afresh on each request.
    #[derive(Deserialize)]
    struct Selection {
        /// Fully qualified project.
        project: String,
    }

    impl FromRequestParts<ApplicationState> for Context {
        type Rejection = StatusCode;

        async fn from_request_parts(
            parts: &mut Parts,
            state: &ApplicationState,
        ) -> Result<Self, Self::Rejection> {
            let Query(selection) = Query::<Selection>::from_request_parts(parts, state)
                .await
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            Ok(Self::new(format!("{}{}", state.prefix, selection.project)))
        }
    }

    /// Sends a request through the public router without binding a listener.
    async fn request(router: &Router, uri: &str, session: Option<&str>, body: &str) -> Response {
        let mut request = Request::post(uri).header("content-type", "application/json");
        if let Some(session) = session {
            request = request.header(HTTP_SESSION_ID_HEADER, session);
        }
        router
            .clone()
            .oneshot(request.body(Body::from(body.to_owned())).expect("request"))
            .await
            .expect("response")
    }

    /// Re-extracts per request and isolates overlapping calls.
    #[tokio::test]
    async fn request_context_is_not_session_state() {
        let router = McpRouter::builder(McpServer::builder(), Handler)
            .context_state(ApplicationState {
                prefix: "selected:",
            })
            .storage(InMemoryStorage::new(1, Duration::from_secs(60)))
            .build();
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;
        let rejected = request(&router, "/", None, initialize).await;
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        assert!(!rejected.headers().contains_key(HTTP_SESSION_ID_HEADER));

        let response = request(&router, "/?project=marc%2Fgeneral", None, initialize).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "rejected requests must not consume session capacity"
        );
        let session = response.headers()[HTTP_SESSION_ID_HEADER]
            .to_str()
            .expect("session")
            .to_owned();
        let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(
            request(
                &router,
                "/?project=marc%2Fgeneral",
                Some(&session),
                initialized
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        let call =
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_context"}}"#;
        let (first, second) = tokio::join!(
            request(&router, "/?project=marc%2Fgeneral", Some(&session), call),
            request(&router, "/?project=travel%2Fjapan", Some(&session), call),
        );
        for (response, project) in [(first, "marc/general"), (second, "travel/japan")] {
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let result: Value = serde_json::from_slice(&bytes).expect("JSON");
            assert_eq!(
                result["result"]["content"][0]["text"],
                format!("selected:{project}:0")
            );
        }
        assert_eq!(
            request(&router, "/", Some(&session), call).await.status(),
            StatusCode::BAD_REQUEST,
            "a prior request must not supply a missing project"
        );
        for (uri, status) in [
            ("/", StatusCode::BAD_REQUEST),
            ("/?project=marc%2Fgeneral", StatusCode::NO_CONTENT),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::delete(uri)
                        .header(HTTP_SESSION_ID_HEADER, &session)
                        .body(Body::empty())
                        .expect("delete request"),
                )
                .await
                .expect("delete response");
            assert_eq!(response.status(), status);
        }
    }
}
