# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `ToolHandler` trait for async tool handlers with mutable state.

### Changed

- Tool enum variants no longer contain `Responder`; moved to `Output::ToolCall { tool, responder }`.
- `ToolHandler::handle` and `io::stdlib` handler now return `ToolResult` directly.
- `ToolRegistry::parse` no longer takes `id` parameter.
- `io::tokio::run_stdio` now takes `impl ToolHandler<R>` instead of async closures.
- Flattened module structure: removed `protocol` module, types now at crate root.

## [0.1.0] - 2026-03-06

Initial release.

[Unreleased]: https://github.com/mbr/mercutio/compare/0.1.0...HEAD
[0.1.0]: https://github.com/mbr/mercutio/releases/tag/0.1.0
