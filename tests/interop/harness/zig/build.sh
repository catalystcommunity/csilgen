#!/usr/bin/env bash
# Build the Zig interop harness: the single executable links the generated package
# (gen/) and the monorepo's Zig transport library. Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "${CATALYST_TOOLS:-$HOME/.local/catalyst-tools}/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

zig build

echo "zig harness built"
