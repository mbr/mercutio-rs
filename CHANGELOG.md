# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.1] - 2026-03-08

### Added

- `Display` implementation for `ToolOutput` for snapshot testing tool responses.

## [0.5.0] - 2026-03-08

### Added

- `ToolDefinitions` newtype returned by `ToolRegistry::definitions()` with `Display` impl for human-readable output.
- `Debug` and `Display` implementations for `ToolDefinition`.
- Documentation on text vs JSON tool outputs with research reference.

### Changed

- `ToolRegistry::definitions()` returns `ToolDefinitions` instead of `Vec<ToolDefinition>`.

## [0.4.1] - 2026-03-07

### Changed

- Restructured README with sans-IO usage shown before convenience transports.
- Crate documentation now uses README via `include_str!`.
- Protocol flow documentation moved to `McpServer` struct docs.

## [0.4.0] - 2026-03-07

### Added

- `McpSessionId` type in `io` module for session identification across transports.
- `SessionStorage` trait for custom session storage implementations.
- `InMemoryStorage` with configurable capacity and LRU eviction.
- `McpRouter::builder()` for configuring axum routers with custom storage.

### Changed

- `ToolHandler::handle` and `MutToolHandler::handle` now take `session_id: Option<McpSessionId>` as first parameter.
- `io::stdlib` handler closure signature changed to `FnMut(Option<McpSessionId>, R) -> Result<T, E>`.
- `io::axum` now uses `InMemoryStorage` with bounded capacity (default 10,000 sessions) instead of unbounded `HashMap`.

## [0.3.0] - 2026-03-06

### Added

- `io-axum` feature: HTTP transport implementing MCP Streamable HTTP with session management.
- `ToolHandler` trait for concurrent contexts (`&self`), with blanket impl for async closures.
- `MutToolHandler` trait for exclusive-access contexts (`&mut self`), with blanket impl from `ToolHandler`.

### Changed

- `io` module is now always available; only submodules (`io::tokio`, `io::axum`, `io::stdlib`) are feature-gated.
- `io::tokio` now uses `MutToolHandler` instead of `ToolHandler`.

## [0.2.0] - 2026-03-06

### Added

- `ToolHandler` trait for async tool handlers with mutable state.
- `ToolOutput` type for building successful tool responses.
- `IntoToolResponse` trait for ergonomic handler return types.
- `io::stdlib::run_on` and `io::tokio::run_on` for custom transports.

### Changed

- Tool enum variants no longer contain `Responder`; moved to `Output::ToolCall { tool, responder }`.
- `Responder` simplified: `respond()` accepts bare values or `Result<T, E>`, `rpc_error()` for protocol errors.
- Handlers return `impl IntoToolResponse` (e.g., `String`, `ToolOutput`, `Result<T, E>`).
- `ToolRegistry::parse` no longer takes `id` parameter.
- `ToolHandler` trait now uses `type Error` and returns `Result<ToolOutput, Self::Error>`.
- `io::stdlib::run_stdio` now requires handlers to return `Result<T, E>`.
- `io::tokio::run_stdio` now takes `impl ToolHandler<R>` instead of async closures.
- Flattened module structure: removed `protocol` module, types now at crate root.

### Removed

- `ToolResult` type (replaced by `ToolOutput` and `IntoToolResponse`).

## [0.1.0] - 2026-03-06

Initial release.

[Unreleased]: https://github.com/mbr/mercutio/compare/0.5.1...HEAD
[0.5.1]: https://github.com/mbr/mercutio/compare/0.5.0...0.5.1
[0.5.0]: https://github.com/mbr/mercutio/compare/0.4.1...0.5.0
[0.4.1]: https://github.com/mbr/mercutio/compare/0.4.0...0.4.1
[0.4.0]: https://github.com/mbr/mercutio/compare/0.3.0...0.4.0
[0.3.0]: https://github.com/mbr/mercutio/compare/0.2.0...0.3.0
[0.2.0]: https://github.com/mbr/mercutio/compare/0.1.0...0.2.0
[0.1.0]: https://github.com/mbr/mercutio/releases/tag/0.1.0
