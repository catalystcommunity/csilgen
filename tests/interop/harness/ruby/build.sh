#!/usr/bin/env bash
# Build the Ruby interop harness. Ruby is interpreted, so there is nothing to compile:
# the only "build" step is making sure the gems the generated package needs are present.
# The generated CBOR codec is hand-rolled (no CBOR gem), but it uses BigDecimal for the
# decimal core type, which Ruby 3.4 no longer ships as a default gem. Idempotent.
set -euo pipefail

# shellcheck disable=SC1091
source "$HOME/.local/csilgen-tools/env.sh" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

# bigdecimal (decimal core type) and json (result protocol) must be loadable. Install a
# missing one into the per-user gem dir (already on RubyGems' default load path), the
# same approach transports/ruby/run-tests.sh uses, so no root and no GEM_PATH juggling.
ensure_gem() {
  local require_path="$1" gem_name="$2"
  if ruby -e "require '${require_path}'" >/dev/null 2>&1; then
    return 0
  fi
  echo "build.sh: '${gem_name}' is missing; installing it into the user gem dir..."
  gem install --user-install --no-document "${gem_name}" >/dev/null
}

ensure_gem bigdecimal bigdecimal
ensure_gem json json

# Fail fast if the generated package or the transport lib won't load.
ruby -I"$HERE/gen/lib" -I"$HERE/../../../../transports/ruby/lib" \
  -e 'require "interop_api"; require "csilgen/transport"' >/dev/null

echo "ruby harness built"
