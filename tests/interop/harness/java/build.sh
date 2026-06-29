#!/usr/bin/env bash
# Build the Java interop harness: compile the generated package (gen/), the monorepo's
# Java transport library, and the harness sources with javac. RPC/Events ride loopback
# TCP and datagrams ride UDP (both in the JDK stdlib), so no native shim is needed.
# Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "$HOME/.local/csilgen-tools/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
cd "$HERE"

TRANSPORT_SRC="$ROOT/transports/java/src/main/java"

# Compile every Java source (transport lib + generated package + harness) into classes/.
rm -rf classes
mkdir -p classes
mapfile -t SOURCES < <(find "$TRANSPORT_SRC" "$HERE/gen/src/main/java" -name '*.java')
javac -d classes "${SOURCES[@]}" "$HERE/Main.java"

echo "java harness built"
