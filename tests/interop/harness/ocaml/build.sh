#!/usr/bin/env bash
# Build the OCaml interop harness: one executable linking the generated package
# (gen/, the `interop_api` library) and the monorepo's OCaml transport library.
# The transport lib's own dune declares a `public_name` (needing an opam package),
# so it is vendored here as a private (public_name-less) copy to keep this harness a
# self-contained dune workspace. Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "$HOME/.local/csilgen-tools/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

LIB="$HERE/../../../../transports/ocaml/lib"
rm -rf transport
mkdir -p transport
cp "$LIB"/*.ml "$LIB"/*.mli transport/
cat > transport/dune <<'EOF'
(library
 (name csilgen_transport)
 (libraries unix))
EOF

# Release profile: the generated package and the transport library are warning-clean
# under it (warnings-as-errors off), matching how the generator's own dune tests build.
dune build --profile release

echo "ocaml harness built"
