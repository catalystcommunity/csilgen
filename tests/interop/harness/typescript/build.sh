#!/usr/bin/env bash
# Build the TypeScript interop harness: compile the generated package (gen/ ->
# commonjs dist/). The harness itself (main.ts) runs under Node's native type
# stripping, so it needs no compile step. Transports ride loopback TCP/UDP via
# node:net and node:dgram — pure stdlib, no native addon. Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "${CATALYST_TOOLS:-$HOME/.local/catalyst-tools}/env.sh" 2>/dev/null || true
# Node is the one explicitly on PATH; prepend it so it wins over any tool shims.
export PATH="$HOME/.local/opt/node/bin:$PATH"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

# Compile the generated package to commonjs dist/ (its own tsc).
if [ -d gen ]; then
  (
    cd gen
    npm install --no-audit --no-fund --silent
    npm run build
  )
fi

echo "typescript harness built"
