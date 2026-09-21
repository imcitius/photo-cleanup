#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
testfile=crates/pc-api/tests/audit_regressions.rs
if [ -e "$testfile" ]; then echo "Refusing to overwrite $testfile" >&2; exit 2; fi
mkdir -p crates/pc-api/tests
trap 'rm -f "$testfile"; rmdir crates/pc-api/tests 2>/dev/null || true' EXIT HUP INT TERM
cp audit-artifacts/http-regressions.rs "$testfile"
cargo test -p pc-api --test audit_regressions --locked -- --nocapture
