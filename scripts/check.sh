#!/usr/bin/env bash
# Pre-commit gate: build + clippy + tests.
# Pedantic clippy is suppressed because the workspace lints have it at warn
# for guidance; the gate denies the remaining default-level lints.

set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== cargo build --workspace ==="
cargo build --workspace

echo
echo "=== cargo clippy (no pedantic, deny warnings) ==="
cargo clippy --workspace --all-targets -- -A clippy::pedantic -D warnings

echo
echo "=== cargo test --workspace ==="
cargo test --workspace

echo
echo "All checks passed."
