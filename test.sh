#!/bin/sh

#: Runs tests with no features and all features.

set -e

cargo test
cargo test --all-features
