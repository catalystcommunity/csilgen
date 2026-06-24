#!/usr/bin/env sh
# Full build + test for the CSIL Java transport library behind one command, invoked by the
# xtask transport runner. Uses the committed Gradle wrapper so only a JDK (>= 17) plus
# network access on first run are required. Exits non-zero on any failure.
set -e

cd "$(dirname "$0")"

exec ./gradlew --no-daemon test
