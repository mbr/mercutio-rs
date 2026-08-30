#![cfg(feature = "cli")]

use std::{borrow::Cow, collections::BTreeMap, io::Cursor};

use mercutio::{
    NoTools, ToolDef, ToolDefinition, ToolDefinitions, ToolRegistry,
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
        .err()
        .expect("application collision must fail");
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
