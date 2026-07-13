#!/usr/bin/env bash
# Full build + test for the CSIL Java transport library behind one command, invoked by the
# xtask transport runner. Uses the committed Gradle wrapper and selects a JDK compatible
# with that wrapper before launching Gradle. Exits non-zero on any failure.
set -euo pipefail

cd "$(dirname "$0")"

source ../jvm-gradle-env.sh
select_compatible_gradle_jdk

exec ./gradlew --no-daemon test
