#!/usr/bin/env bash
# Pre-commit gate: build + clippy + tests.
# Workspace lints declare clippy::pedantic = "deny" so we don't override on
# the CLI here; -D warnings catches any other warning that slips through.

set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== cargo build --workspace ==="
cargo build --workspace

echo
echo "=== cargo clippy (pedantic enforced, deny warnings) ==="
cargo clippy --workspace --all-targets -- -D warnings

echo
echo "=== cargo test --workspace ==="
cargo test --workspace

echo
echo "All checks passed."
