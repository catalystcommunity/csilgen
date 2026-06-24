#!/usr/bin/env bash
# Full setup + build + test for the OCaml transport library behind one command, so
# xtask can drive it uniformly. Exits non-zero on any failure.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The conformance vectors live one level up (transports/conformance), outside this
# dune project, so the test reads them through this absolute path at runtime.
export CSILGEN_CONFORMANCE_DIR="$(cd "$here/../conformance" && pwd)"

cd "$here"
dune build
dune runtest --force
