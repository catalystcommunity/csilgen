#!/usr/bin/env bash
# Build the Elixir interop harness. Elixir is interpreted (main.exs runs directly), so
# the only "build" step is compiling the two libraries the harness loads: the generated
# package under gen/ and the official csilgen_transport library. Both are Mix projects
# with zero external deps, so `mix deps.get` is a no-op kept for clean-checkout parity.
# Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "$HOME/.local/csilgen-tools/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
TRANSPORT="$HERE/../../../../transports/elixir"

# Compile the official transport library (RPC/Events/Datagrams + CBOR codec).
( cd "$TRANSPORT" && mix deps.get >/dev/null && MIX_ENV=dev mix compile )

# Compile the generated interop_api package (types/codec/client/server/validation).
( cd "$HERE/gen" && mix deps.get >/dev/null && MIX_ENV=dev mix compile )

# Fail fast if either ebin is missing.
test -d "$HERE/gen/_build/dev/lib/interop_api/ebin"
test -d "$TRANSPORT/_build/dev/lib/csilgen_transport/ebin"

echo "elixir harness built"
