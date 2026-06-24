#!/usr/bin/env bash
# Full setup + test for the Dart transport library, behind one command. xtask's
# transport runner invokes this (and skips it when the Dart SDK is absent).
#
# `dart test` needs the dev-dependencies fetched first, so `dart pub get` runs
# before it. Any failure aborts (set -e) with a non-zero exit so the runner sees it.
set -euo pipefail

cd "$(dirname "$0")"

dart pub get
dart test
