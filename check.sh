#!/bin/sh

#: Runs formatting, compilation, and linting with warnings as errors.

set -e

echo "rustc $(rustc --version) at $(which rustc), cargo $(cargo --version) at $(which cargo)"

./format.sh --check
RUSTFLAGS="-D warnings" cargo check
RUSTFLAGS="-D warnings" cargo check --features cli
RUSTFLAGS="-D warnings" cargo check --features cli,io-stdlib,io-tokio,io-axum,jiff
RUSTFLAGS="-D warnings" cargo check --features cli,io-stdlib,io-tokio,io-axum,chrono
cargo clippy -- -D warnings
cargo clippy --features cli -- -D warnings
cargo clippy --features cli,io-stdlib,io-tokio,io-axum,jiff -- -D warnings
cargo clippy --features cli,io-stdlib,io-tokio,io-axum,chrono -- -D warnings
