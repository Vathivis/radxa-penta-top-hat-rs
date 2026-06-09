#!/usr/bin/env sh
set -eu

export CARGO_HOME="${CARGO_HOME:-/srv/storage/development/rust/cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-/srv/storage/development/rust/rustup}"
export PATH="$CARGO_HOME/bin:$PATH"

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
