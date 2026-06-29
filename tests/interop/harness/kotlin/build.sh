#!/usr/bin/env bash
# Build the Kotlin interop harness (generated package + transport lib) into a runnable
# distribution. All three transports ride loopback TCP/UDP from the JDK stdlib, so there
# is no native shim to compile.
set -euo pipefail

# shellcheck disable=SC1091
source "$HOME/.local/csilgen-tools/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

# Build the harness distribution (no daemon: the orchestrator builds once per run).
gradle --no-daemon -q installDist

echo "kotlin harness built"
