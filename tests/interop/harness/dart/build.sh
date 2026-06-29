#!/usr/bin/env bash
# Build the Dart interop harness: just resolve dependencies. The harness runs under
# the Dart VM (JIT) via `run`, so there is no compile step; sockets are pure dart:io
# (RawSynchronousSocket / ServerSocket / RawDatagramSocket), so there is no native
# addon to build. Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "${CATALYST_TOOLS:-$HOME/.local/catalyst-tools}/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

dart pub get

echo "dart harness built"
