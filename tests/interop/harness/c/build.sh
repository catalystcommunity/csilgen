#!/usr/bin/env bash
# Build the C interop harness: a single executable that links the generated
# package (gen/) and the monorepo's C reference transport library (compiled
# directly from its sources, no install step). Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "${CATALYST_TOOLS:-$HOME/.local/catalyst-tools}/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
TLIB="$ROOT/transports/c"
cd "$HERE"

CC="${CC:-cc}"
"$CC" -std=c11 -O2 -Wall -Wextra \
    -I"$TLIB/include" -I"$TLIB/src" -I"$HERE" \
    main.c "$TLIB"/src/*.c \
    -o csil-interop-c

echo "c harness built"
