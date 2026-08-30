#![cfg(feature = "cli")]

use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::{self, Cursor, Write},
};

use mercutio::{
    NoTools, ToolDef, ToolDefinition, ToolDefinitions, ToolOutput, ToolRegistry,
    cli::{Cli, CliBuildProblem, CliErrorKind, OutputMode, ToolRegistryExt as _},
};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Fast,
    Thorough,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Range {
    /// Smallest accepted value.
    min: i64,
    /// Largest accepted value.
    max: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Filter {
    /// Numeric range.
    range: Range,
    /// Labels to include.
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Rule {
    path: String,
    allow: bool,
}

mercutio::tool_registry! {
    enum TestTools {
        Search("search_items", "Searches indexed items") {
            /// Search expression.
            query: String,
            /// Search strategy.
            mode: Mode,
            /// Optional result limit.
            limit: Option<u64>,
            /// Optional score threshold.
            threshold: Option<f64>,
            /// Whether to recurse.
            recursive: Option<bool>,
            /// Structured filters.
            filter: Option<Filter>,
            /// Process environment.
            environment: Option<BTreeMap<String, String>>,
            /// Ordered matching rules.
            rules: Option<Vec<Rule>>,
            /// A property sharing the root adapter option's spelling.
            input_json: Option<String>,
        },
        Ping("HTTPServer", "Checks service health") {},
    }
}

fn cli() -> Cli<TestTools> {
    TestTools::cli("my-tools")
        .version("1.0.0")
        .build()
        .expect("valid CLI")
}

#[test]
fn representative_help_is_stable() {
    let mut root = cli().command();
    let root_help = root.render_long_help().to_string();
    let mut search = root
        .find_subcommand_mut("search-items")
        .expect("search command")
        .clone();
    let tool_help = search.render_long_help().to_string();

    insta::assert_snapshot!(format!("{root_help}\n--- TOOL ---\n{tool_help}"));
}

#[test]
fn parses_scalars_nested_objects_arrays_and_json_values() {
    let invocation = cli()
        .try_parse_from(
            [
                "my-tools",
                "search-items",
                "--query",
                "rust",
                "--mode",
                "thorough",
                "--limit",
                "20",
                "--threshold=-1.5e-2",
                "--recursive=false",
                "--filter-range-min=-3",
                "--filter-tags",
                "mcp",
                "--filter-tags",
                "rust,lang",
                "--environment",
                r#"{"RUST_LOG":"info"}"#,
                "--rules",
                r#"[{"path":"src","allow":true}]"#,
                "--input-json",
                "property-value",
            ],
            Cursor::new(Vec::new()),
        )
        .expect("valid typed invocation");

    let TestTools::Search(input) = invocation.into_tool() else {
        panic!("expected search tool");
    };
    assert_eq!(input.query, "rust");
    assert_eq!(input.mode, Mode::Thorough);
    assert_eq!(input.limit, Some(20));
    assert_eq!(input.threshold, Some(-1.5e-2));
    assert_eq!(input.recursive, Some(false));
    assert_eq!(
        input.filter,
        Some(Filter {
            range: Range { min: -3, max: None },
            tags: Some(vec!["mcp".into(), "rust,lang".into()]),
        })
    );
    assert_eq!(
        input.environment,
        Some(BTreeMap::from([("RUST_LOG".into(), "info".into())]))
    );
    assert_eq!(
        input.rules,
        Some(vec![Rule {
            path: "src".into(),
            allow: true,
        }])
    );
    assert_eq!(input.input_json.as_deref(), Some("property-value"));
}

#[test]
fn preserves_omitted_and_implicit_booleans() {
    let omitted = cli()
        .try_parse_from(
            ["my-tools", "search-items", "--query", "x", "--mode", "fast"],
            Cursor::new(Vec::new()),
        )
        .expect("valid invocation")
        .into_tool();
    let TestTools::Search(omitted) = omitted else {
        panic!("expected search tool");
    };
    assert_eq!(omitted.recursive, None);

    let implicit = cli()
        .try_parse_from(
            [
                "my-tools",
                "search-items",
                "--query",
                "x",
                "--mode",
                "fast",
                "--recursive",
            ],
            Cursor::new(Vec::new()),
        )
        .expect("valid invocation")
        .into_tool();
    let TestTools::Search(implicit) = implicit else {
        panic!("expected search tool");
    };
    assert_eq!(implicit.recursive, Some(true));
}

#[test]
fn enforces_conditionally_required_descendants() {
    let error = cli()
        .try_parse_from(
            [
                "my-tools",
                "search-items",
                "--query",
                "x",
                "--mode",
                "fast",
                "--filter-tags",
                "rust",
            ],
            Cursor::new(Vec::new()),
        )
        .err()
        .expect("missing range must fail");
    assert_eq!(error.kind(), CliErrorKind::Usage);
    assert!(error.to_string().contains("--filter-range-min"));
}

#[test]
fn rejects_non_json_numeric_and_mistyped_json_values() {
    let numeric = cli()
        .try_parse_from(
            [
                "my-tools",
                "search-items",
                "--query",
                "x",
                "--mode",
                "fast",
                "--limit",
                "+2",
            ],
            Cursor::new(Vec::new()),
        )
        .err()
        .expect("leading plus must fail");
    assert_eq!(numeric.exit_code(), 2);

    let object = cli()
        .try_parse_from(
            [
                "my-tools",
                "search-items",
                "--query",
                "x",
                "--mode",
                "fast",
                "--environment",
                "[]",
            ],
            Cursor::new(Vec::new()),
        )
        .err()
        .expect("array is not an object");
    assert!(object.to_string().contains("expected object"));
}

#[test]
fn dispatches_exact_whole_input_json() {
    let invocation = cli()
        .try_parse_from(
            [
                "my-tools",
                "--output",
                "raw",
                "--input-json",
                "search-items",
            ],
            Cursor::new(
                br#"{"query":"stdin","mode":"fast","recursive":false,"filter":{"range":{"min":1}}}"#,
            ),
        )
        .expect("valid JSON route");
    assert_eq!(invocation.output_options().mode, OutputMode::Raw);
    let TestTools::Search(input) = invocation.into_tool() else {
        panic!("expected search tool");
    };
    assert_eq!(input.query, "stdin");
    assert_eq!(input.recursive, Some(false));
}

#[test]
fn whole_input_rejects_invalid_shapes_and_mixed_dispatch() {
    for input in [b"".as_slice(), b"[]", b"{} trailing"] {
        let error = cli()
            .try_parse_from(
                ["my-tools", "--input-json", "search-items"],
                Cursor::new(input),
            )
            .err()
            .expect("invalid whole input must fail");
        assert_eq!(error.exit_code(), 2);
    }

    let mixed = cli()
        .try_parse_from(
            [
                "my-tools",
                "--input-json",
                "search-items",
                "search-items",
                "--query",
                "x",
                "--mode",
                "fast",
            ],
            Cursor::new(b"{}"),
        )
        .err()
        .expect("mixed routes must fail");
    assert!(mixed.to_string().contains("cannot be combined"));
}

#[derive(Debug, Deserialize, PartialEq)]
struct UnionInput {
    choice: Choice,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(untagged)]
enum Choice {
    Text(String),
    Object { label: String, count: Option<u64> },
}

impl JsonSchema for UnionInput {
    fn schema_name() -> Cow<'static, str> {
        "UnionInput".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "choice": {
                    "description": "Text or detailed selection.",
                    "oneOf": [
                        { "type": "string" },
                        {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string" },
                                "count": { "type": "integer", "minimum": 0 }
                            },
                            "required": ["label"]
                        }
                    ]
                }
            },
            "required": ["choice"]
        })
    }
}

impl ToolDef for UnionInput {
    const NAME: &'static str = "choose";
    const DESCRIPTION: &'static str = "Chooses one representation";
}

#[test]
fn parses_and_separates_supported_union_branches() {
    let cli = UnionInput::cli("chooser").build().expect("valid CLI");
    let scalar = cli
        .try_parse_from(
            ["chooser", "choose", "--choice", "plain"],
            Cursor::new(Vec::new()),
        )
        .expect("scalar branch")
        .into_tool();
    assert_eq!(scalar.choice, Choice::Text("plain".into()));

    let object = cli
        .try_parse_from(
            ["chooser", "choose", "--choice-label", "detailed"],
            Cursor::new(Vec::new()),
        )
        .expect("object branch")
        .into_tool();
    assert_eq!(
        object.choice,
        Choice::Object {
            label: "detailed".into(),
            count: None,
        }
    );

    let error = cli
        .try_parse_from(
            [
                "chooser",
                "choose",
                "--choice",
                "plain",
                "--choice-label",
                "detailed",
            ],
            Cursor::new(Vec::new()),
        )
        .err()
        .expect("mixed branches must fail");
    assert!(error.to_string().contains("cannot be combined"));
}

#[derive(Debug, Deserialize, PartialEq)]
struct FallbackInput {
    plain: String,
    choice: serde_json::Value,
    node: serde_json::Value,
    hybrid: serde_json::Value,
}

impl JsonSchema for FallbackInput {
    fn schema_name() -> Cow<'static, str> {
        "FallbackInput".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "plain": { "type": "string" },
                "choice": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "integer" }
                    ]
                },
                "node": { "$ref": "#/$defs/Node" },
                "hybrid": {
                    "type": "object",
                    "properties": { "fixed": { "type": "string" } },
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["plain", "choice", "node", "hybrid"],
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" },
                        "next": { "$ref": "#/$defs/Node" }
                    },
                    "required": ["value"]
                }
            }
        })
    }
}

impl ToolDef for FallbackInput {
    const NAME: &'static str = "fallback";
    const DESCRIPTION: &'static str = "Exercises localized JSON fallbacks";
}

#[test]
fn keeps_typed_siblings_with_localized_json_fallbacks() {
    let cli = FallbackInput::cli("fallbacks")
        .build()
        .expect("valid fallback CLI");
    let input = cli
        .try_parse_from(
            [
                "fallbacks",
                "fallback",
                "--plain",
                "typed",
                "--choice",
                "42",
                "--node",
                r#"{"value":"root","next":{"value":"leaf"}}"#,
                "--hybrid",
                r#"{"fixed":"known","extra":"dynamic"}"#,
            ],
            Cursor::new(Vec::new()),
        )
        .expect("localized JSON values")
        .into_tool();
    assert_eq!(input.plain, "typed");
    assert_eq!(input.choice, serde_json::json!(42));
    assert_eq!(input.node["next"]["value"], "leaf");
    assert_eq!(input.hybrid["extra"], "dynamic");
}

#[derive(Debug, Deserialize, JsonSchema)]
struct First;

impl ToolDef for First {
    const NAME: &'static str = "get_weather";
    const DESCRIPTION: &'static str = "First";
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Second;

impl ToolDef for Second {
    const NAME: &'static str = "get-weather";
    const DESCRIPTION: &'static str = "Second";
}

struct CollidingTools;

impl ToolRegistry for CollidingTools {
    fn parse(name: &str, _arguments: serde_json::Value) -> Result<Self, mercutio::JsonRpcError> {
        Err(mercutio::JsonRpcError::MethodNotFound { msg: name.into() })
    }

    fn definitions() -> ToolDefinitions {
        ToolDefinitions::new(vec![
            ToolDefinition::from_tool::<First>(),
            ToolDefinition::from_tool::<Second>(),
        ])
    }
}

#[test]
fn reports_normalization_and_application_collisions() {
    let error = CollidingTools::cli("tools")
        .build()
        .err()
        .expect("collision must fail");
    assert!(matches!(
        &error.problems()[0],
        CliBuildProblem::NameCollision { cli_name, originals }
            if cli_name == "get-weather" && originals.len() == 2
    ));

    let generated = cli();
    let parent = clap::Command::new("app").subcommand(clap::Command::new("my-tools"));
    let error = generated
        .attach_to(parent)
        .expect_err("application collision must fail");
    assert!(matches!(
        error.problems(),
        [CliBuildProblem::ApplicationCommandCollision { name }] if name == "my-tools"
    ));
}

#[test]
fn supports_nesting_and_empty_registries() {
    let generated = cli();
    let command = generated
        .attach_to(clap::Command::new("app").subcommand(clap::Command::new("mcp")))
        .expect("unique subtree");
    assert!(command.find_subcommand("my-tools").is_some());

    let empty = Cli::<NoTools>::builder("tools")
        .build()
        .expect("empty CLI builds");
    let error = empty
        .try_parse_from(["tools"], Cursor::new(Vec::new()))
        .err()
        .expect("empty CLI has no invocation");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn help_and_version_are_stdout_control_flow() {
    for argument in ["--help", "--version"] {
        let error = cli()
            .try_parse_from(["my-tools", argument], Cursor::new(Vec::new()))
            .err()
            .expect("display request");
        assert_eq!(error.kind(), CliErrorKind::Display);
        assert_eq!(error.exit_code(), 0);
        assert!(!error.targets_stderr());
    }
}

fn minimal_search_args(output: &str) -> Vec<&str> {
    vec![
        "my-tools",
        "--output",
        output,
        "search-items",
        "--query",
        "rust",
        "--mode",
        "fast",
    ]
}

#[test]
fn synchronous_runner_emits_structured_and_raw_json() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    cli()
        .run_on(
            minimal_search_args("structured"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |session, _tool| -> Result<ToolOutput, &str> {
                assert!(session.is_none());
                Ok(ToolOutput::new()
                    .text("fallback")
                    .structured(&serde_json::json!({"count": 2})))
            },
        )
        .expect("structured runner");
    assert_eq!(
        stdout,
        br#"{"count":2}
"#
    );
    assert!(stderr.is_empty());

    stdout.clear();
    cli()
        .run_on(
            minimal_search_args("raw"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |_, _| -> Result<ToolOutput, &str> {
                Ok(ToolOutput::new()
                    .text("fallback")
                    .structured(&serde_json::json!({"count": 2})))
            },
        )
        .expect("raw runner");
    assert_eq!(
        stdout,
        br#"{"content":[{"text":"fallback","type":"text"}],"isError":false,"structuredContent":{"count":2}}
"#
    );
}

#[test]
fn structured_and_handler_failures_write_no_stdout() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let missing = cli()
        .run_on(
            minimal_search_args("structured"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |_, _| -> Result<&str, &str> { Ok("text only") },
        )
        .expect_err("missing structured content");
    assert_eq!(missing.exit_code(), 1);
    assert!(stdout.is_empty());

    let handler = cli()
        .run_on(
            minimal_search_args("raw"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |_, _| -> Result<&str, &str> { Err("domain failure") },
        )
        .expect_err("handler error");
    assert_eq!(handler.kind(), CliErrorKind::Runtime);
    assert!(handler.to_string().contains("domain failure"));
    assert!(stdout.is_empty());
}

#[test]
fn binary_output_separates_bytes_and_text() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    cli()
        .run_on(
            minimal_search_args("binary"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |_, _| -> Result<ToolOutput, &str> {
                Ok(ToolOutput::new()
                    .text("generated image")
                    .image(b"\x89PNG\r\n", "image/png"))
            },
        )
        .expect("binary runner");
    assert_eq!(stdout, b"\x89PNG\r\n");
    assert_eq!(stderr, b"generated image\n");

    stdout.clear();
    stderr.clear();
    let invalid = mercutio::rust_mcp_schema::ImageContent::new(
        "not base64".into(),
        "image/png".into(),
        None,
        None,
    );
    let error = cli()
        .run_on(
            minimal_search_args("binary"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |_, _| -> Result<ToolOutput, &str> { Ok(ToolOutput::new().content(invalid)) },
        )
        .expect_err("invalid base64");
    assert_eq!(error.exit_code(), 1);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn artifact_output_creates_private_unique_files_and_forced_kitty() {
    let parent = tempfile::tempdir().expect("temporary artifact parent");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let args = vec![
        "my-tools",
        "--artifact-dir",
        parent.path().to_str().expect("UTF-8 path"),
        "--images",
        "kitty",
        "search-items",
        "--query",
        "rust",
        "--mode",
        "fast",
    ];
    cli()
        .run_on(
            args,
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            |_, _| -> Result<ToolOutput, &str> {
                Ok(ToolOutput::new()
                    .text("summary")
                    .image(b"png", "image/png")
                    .audio(b"wav", "audio/wav")
                    .embedded_blob(b"pdf", "file:///report.pdf", "application/pdf")
                    .embedded_text("notes", "file:///notes.txt", Some("text/plain"))
                    .resource_link("file:///source", "source")
                    .structured(&serde_json::json!({"ok": true})))
            },
        )
        .expect("artifact runner");
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("text output");
    assert!(rendered.contains("\u{1b}_Ga=T,f=100,m=0;"));
    assert!(rendered.contains("[image:"));
    assert!(rendered.contains("[audio:"));
    assert!(rendered.contains("[embedded blob: file:///report.pdf ->"));
    assert!(rendered.contains("[embedded text: file:///notes.txt (text/plain)]\nnotes"));
    assert!(rendered.contains("[resource: source (file:///source)]"));
    assert!(rendered.ends_with("{\n  \"ok\": true\n}\n"));

    let directories = std::fs::read_dir(parent.path())
        .expect("artifact parent")
        .collect::<Result<Vec<_>, _>>()
        .expect("directory entries");
    assert_eq!(directories.len(), 1);
    let directory = directories[0].path();
    assert!(directory.is_absolute());
    assert_eq!(
        std::fs::read(directory.join("image-1.png")).expect("image"),
        b"png"
    );
    assert_eq!(
        std::fs::read(directory.join("audio-2.wav")).expect("audio"),
        b"wav"
    );
    assert_eq!(
        std::fs::read(directory.join("blob-3.pdf")).expect("blob"),
        b"pdf"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            std::fs::metadata(&directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(directory.join("image-1.png"))
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn text_only_artifacts_create_no_directory() {
    let parent = tempfile::tempdir().expect("temporary artifact parent");
    let mut stdout = Vec::new();
    cli()
        .run_on(
            [
                "my-tools",
                "--artifact-dir",
                parent.path().to_str().expect("UTF-8 path"),
                "search-items",
                "--query",
                "rust",
                "--mode",
                "fast",
            ],
            Cursor::new(Vec::new()),
            &mut stdout,
            Vec::new(),
            |_, _| -> Result<&str, &str> { Ok("plain text") },
        )
        .expect("text artifact output");
    assert_eq!(stdout, b"plain text\n");
    assert_eq!(
        std::fs::read_dir(parent.path())
            .expect("artifact parent")
            .count(),
        0
    );
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected write failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn artifacts_survive_final_stdout_failure() {
    let parent = tempfile::tempdir().expect("temporary artifact parent");
    let error = cli()
        .run_on(
            [
                "my-tools",
                "--artifact-dir",
                parent.path().to_str().expect("UTF-8 path"),
                "search-items",
                "--query",
                "rust",
                "--mode",
                "fast",
            ],
            Cursor::new(Vec::new()),
            FailingWriter,
            Vec::new(),
            |_, _| -> Result<ToolOutput, &str> { Ok(ToolOutput::new().image(b"png", "image/png")) },
        )
        .expect_err("stdout failure");
    assert_eq!(error.exit_code(), 1);
    let directory = std::fs::read_dir(parent.path())
        .expect("artifact parent")
        .next()
        .expect("retained invocation directory")
        .expect("directory entry")
        .path();
    assert_eq!(
        std::fs::read(directory.join("image-1.png")).expect("image"),
        b"png"
    );
}

fn search_behavior(tool: TestTools) -> ToolOutput {
    match tool {
        TestTools::Search(input) => format!("handled {}", input.query).into(),
        TestTools::Ping(_) => "pong".into(),
    }
}

#[test]
fn native_and_mcp_paths_invoke_the_same_behavior() {
    let mut server = mercutio::McpServer::<TestTools>::builder()
        .name("test")
        .version("1.0")
        .build();
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let _ = server.handle(mercutio::parse_line(initialize).expect("initialize request"));
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let _ = server.handle(mercutio::parse_line(initialized).expect("initialized notification"));
    let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_items","arguments":{"query":"rust","mode":"fast"}}}"#;
    let mercutio::Output::ToolCall { tool, .. } =
        server.handle(mercutio::parse_line(call).expect("tool call"))
    else {
        panic!("expected MCP tool call");
    };
    let mcp_output = search_behavior(tool);

    let mut cli_output = Vec::new();
    cli()
        .run_on(
            minimal_search_args("artifacts"),
            Cursor::new(Vec::new()),
            &mut cli_output,
            Vec::new(),
            |_, tool| Ok::<_, &str>(search_behavior(tool)),
        )
        .expect("native tool call");
    assert_eq!(mcp_output.as_text(), Some("handled rust"));
    assert_eq!(cli_output, b"handled rust\n");
}

#[tokio::test]
async fn asynchronous_runner_invokes_mut_handler() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut handler = |session: Option<mercutio::io::McpSessionId>, _tool: TestTools| async move {
        assert!(session.is_none());
        Ok::<_, &str>("async output")
    };
    cli()
        .run_async_on(
            minimal_search_args("artifacts"),
            Cursor::new(Vec::new()),
            &mut stdout,
            &mut stderr,
            &mut handler,
        )
        .await
        .expect("async runner");
    assert_eq!(stdout, b"async output\n");
    assert!(stderr.is_empty());
}
