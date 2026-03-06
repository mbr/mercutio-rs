# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `ToolHandler` trait for async tool handlers with mutable state.
- `ToolOutput` type for building successful tool responses.
- `IntoToolResponse` trait for ergonomic handler return types.

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

[Unreleased]: https://github.com/mbr/mercutio/compare/0.1.0...HEAD
[0.1.0]: https://github.com/mbr/mercutio/releases/tag/0.1.0
