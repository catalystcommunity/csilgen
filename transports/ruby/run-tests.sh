#!/usr/bin/env bash
# Full build+test entrypoint for the Ruby transport gem. There is nothing to compile and
# no third-party dependencies: the codec is hand-rolled and the suite uses only minitest
# and json. xtask invokes this and skips it when `ruby` is absent.
set -euo pipefail

cd "$(dirname "$0")"

# minitest and json are "default gems" — bundled with most CRuby builds, but some distro
# packagings (notably Arch's `ruby`) ship neither. Install whatever is missing into the
# per-user gem dir (already on RubyGems' default load path, so no GEM_PATH juggling and
# no root) so a fresh box runs the suite with just an interpreter and a network.
ensure_gem() {
  local require_path="$1" gem_name="$2"
  if ruby -e "require '${require_path}'" >/dev/null 2>&1; then
    return 0
  fi
  echo "run-tests.sh: '${gem_name}' is missing; installing it into the user gem dir..."
  if ! gem install --user-install --no-document "${gem_name}" >/dev/null; then
    echo "run-tests.sh: failed to install '${gem_name}'. Install it manually with:" >&2
    echo "    gem install --user-install ${gem_name}" >&2
    exit 1
  fi
}

ensure_gem minitest/autorun minitest
ensure_gem json json

exec ruby -Ilib -Itest test/all_tests.rb
