#!/usr/bin/env bash
# Install repo-tracked git hooks into .git/hooks via symlink.
# Symlinks (rather than copies) so future hook updates are picked up
# without re-running this script.

set -euo pipefail
cd "$(dirname "$0")/.."

if [ ! -d .git ]; then
    echo "Not a git repo (no .git/) — nothing to install." >&2
    exit 1
fi

mkdir -p .git/hooks

ln -sf ../../scripts/pre-commit .git/hooks/pre-commit
chmod +x scripts/pre-commit

echo "Installed pre-commit hook → .git/hooks/pre-commit (runs scripts/check.sh)"
