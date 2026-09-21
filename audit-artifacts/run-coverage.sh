#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
# One-time setup: cargo install cargo-llvm-cov --locked
#                rustup component add llvm-tools-preview
# Do not run together with the temporary regression runners: those intentionally fail.
cargo llvm-cov --workspace --locked --html --output-dir audit-artifacts/coverage
cargo llvm-cov report --json --output-path audit-artifacts/coverage.json
cargo llvm-cov report --summary-only > audit-artifacts/coverage-summary.txt
