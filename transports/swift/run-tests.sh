#!/usr/bin/env sh
# Build and run the CsilgenTransport test suite, returning non-zero on any failure. The
# xtask runner shells out to this single script (and skips it when `swift` is absent).
# Required toolchain: a Swift 6.0+ toolchain on PATH — nothing else (no Foundation system
# package, no networking libs; the codec + conformance suite are stdlib-only).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
cd "$here"

# `swift test` compiles and runs the XCTest suite in one step; its exit code reflects any
# test failure, so the runner just propagates it.
swift test
