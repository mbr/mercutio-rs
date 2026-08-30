#!/bin/sh

#: Runs tests with no features and each timestamp backend.

set -e

cargo test
cargo test --features cli
cargo test --features cli,io-stdlib,io-tokio,io-axum,jiff
cargo test --features cli,io-stdlib,io-tokio,io-axum,chrono
