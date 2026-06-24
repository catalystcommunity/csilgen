#!/usr/bin/env bash
# Full build + test for the Kotlin/JVM transport library behind one command, invoked by
# xtask's transport test runner. The Gradle wrapper downloads its own Gradle distribution
# (first run needs network); the only system requirement is a JDK 17+ on PATH (`java`).
# Exits non-zero on any failure.
set -euo pipefail
cd "$(dirname "$0")"
exec ./gradlew test --quiet
