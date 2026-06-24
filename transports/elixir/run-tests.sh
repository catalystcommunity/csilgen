#!/usr/bin/env bash
# Full build+test entrypoint for the Elixir transport library. xtask invokes this
# when `elixir` is present and skips it otherwise. Exits non-zero on any failure.
#
# `mix deps.get` is a no-op here (zero external deps), but it is kept so a clean
# checkout works identically; `mix test` compiles and runs ExUnit. Required
# toolchain: Elixir 1.18+ on Erlang/OTP 27+ (the conformance tests use the
# OTP-native JSON module).
set -euo pipefail

cd "$(dirname "$0")"

mix deps.get
exec mix test
