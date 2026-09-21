#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
testfile=crates/pc-cli/tests/audit_regressions.rs
if [ -e "$testfile" ]; then echo "Refusing to overwrite $testfile" >&2; exit 2; fi
trap 'rm -f "$testfile"' EXIT HUP INT TERM
cp audit-artifacts/regressions.rs "$testfile"
cargo test -p pc-cli --test audit_regressions --locked -- --nocapture
