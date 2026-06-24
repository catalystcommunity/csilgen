#!/usr/bin/env bash
# Full build+test for the Zig CSIL transport library. xtask invokes this; it must
# do everything behind one command and exit non-zero on any failure.
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v zig >/dev/null 2>&1; then
    echo "zig not found on PATH; install Zig 0.14.x (see README.md)" >&2
    exit 127
fi

# The std/build API drifts across minor versions, so require the 0.14 line. A 0.13 or
# 0.15+ toolchain will not build this unchanged.
ver="$(zig version)"
case "$ver" in
    0.14.*) ;;
    *)
        echo "this library is pinned to Zig 0.14.x; found $ver" >&2
        exit 1
        ;;
esac

# Builds the public module, runs colocated unit tests, then the conformance tests
# against transports/conformance/*.json.
zig build test
