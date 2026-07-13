#!/usr/bin/env bash
# Select a JDK that can run the committed Gradle wrapper in the current
# transport directory. Source this file before invoking ./gradlew.

set -euo pipefail

gradle_wrapper_version() {
  local props="${1:-gradle/wrapper/gradle-wrapper.properties}"
  local url

  if [ ! -f "$props" ]; then
    echo "Gradle wrapper properties not found: $props" >&2
    return 1
  fi

  url="$(grep '^distributionUrl=' "$props" | cut -d= -f2-)"
  sed -n 's/.*gradle-\([0-9][0-9.]*\)-.*/\1/p' <<<"$url"
}

version_ge() {
  local left="$1"
  local right="$2"
  local left_major left_minor right_major right_minor

  IFS=. read -r left_major left_minor _ <<<"$left"
  IFS=. read -r right_major right_minor _ <<<"$right"
  left_minor="${left_minor:-0}"
  right_minor="${right_minor:-0}"

  if (( left_major > right_major )); then
    return 0
  fi
  if (( left_major == right_major && left_minor >= right_minor )); then
    return 0
  fi
  return 1
}

gradle_max_java() {
  local gradle_version="$1"

  if version_ge "$gradle_version" "9.4"; then
    echo 26
  elif version_ge "$gradle_version" "9.1"; then
    echo 25
  elif version_ge "$gradle_version" "8.14"; then
    echo 24
  elif version_ge "$gradle_version" "8.10"; then
    echo 23
  elif version_ge "$gradle_version" "8.8"; then
    echo 22
  elif version_ge "$gradle_version" "8.5"; then
    echo 21
  elif version_ge "$gradle_version" "7.3"; then
    echo 17
  else
    echo 16
  fi
}

java_major() {
  local java_bin="$1"
  "$java_bin" -version 2>&1 | awk '
    /version/ {
      for (i = 1; i <= NF; i++) {
        if ($i == "version") {
          v = $(i + 1)
          gsub(/"/, "", v)
          split(v, parts, ".")
          if (parts[1] == "1") print parts[2]
          else print parts[1]
          exit
        }
      }
    }
  '
}

append_candidate() {
  local -n out_ref="$1"
  local label="$2"
  local java_bin="$3"
  local java_home="${4:-}"
  local existing

  [ -x "$java_bin" ] || return 0

  for existing in "${out_ref[@]}"; do
    if [ "${existing#*|}" = "$java_bin|$java_home" ]; then
      return 0
    fi
  done
  out_ref+=("$label|$java_bin|$java_home")
}

collect_jdk_candidates() {
  local array_name="$1"
  local tools system_java
  local tool_roots=()

  if [ -n "${JAVA_HOME:-}" ]; then
    append_candidate "$array_name" "JAVA_HOME=$JAVA_HOME" "$JAVA_HOME/bin/java" "$JAVA_HOME"
  fi

  if [ -n "${CATALYST_TOOLS:-}" ]; then
    tool_roots+=("$CATALYST_TOOLS")
  fi
  tool_roots+=("$HOME/.config/catalyst-tools" "$HOME/.local/catalyst-tools")

  for tools in "${tool_roots[@]}"; do
    append_candidate "$array_name" "$tools/jdk17" "$tools/jdk17/bin/java" "$tools/jdk17"
  done

  if system_java="$(command -v java 2>/dev/null)"; then
    append_candidate "$array_name" "PATH java" "$system_java" ""
  fi
}

select_compatible_gradle_jdk() {
  local gradle_version min_java max_java
  local candidates=()
  local candidate label java_bin java_home major
  local found=()

  gradle_version="$(gradle_wrapper_version)"
  if [ -z "$gradle_version" ]; then
    echo "Could not parse Gradle version from gradle/wrapper/gradle-wrapper.properties" >&2
    return 1
  fi

  min_java=17
  max_java="$(gradle_max_java "$gradle_version")"
  collect_jdk_candidates candidates

  for candidate in "${candidates[@]}"; do
    IFS='|' read -r label java_bin java_home <<<"$candidate"
    major="$(java_major "$java_bin" || true)"
    if [ -z "$major" ]; then
      found+=("$label: unreadable version")
      continue
    fi
    found+=("$label: Java $major")

    if (( major >= min_java && major <= max_java )); then
      if [ -n "$java_home" ]; then
        export JAVA_HOME="$java_home"
        export PATH="$JAVA_HOME/bin:$PATH"
      else
        unset JAVA_HOME
        export PATH="$(dirname "$java_bin"):$PATH"
      fi
      return 0
    fi
  done

  {
    echo "No compatible JDK found for Gradle $gradle_version."
    echo "Gradle $gradle_version can run on Java $min_java through Java $max_java for these tests."
    if [ "${#found[@]}" -gt 0 ]; then
      echo "Detected JDKs:"
      printf '  - %s\n' "${found[@]}"
    else
      echo "Detected JDKs: none"
    fi
    echo "Install a compatible JDK or run tools/install-transport-toolchains.sh, then retry."
  } >&2
  return 1
}
