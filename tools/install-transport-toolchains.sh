#!/usr/bin/env bash
# Install the language toolchains catalystcommunity projects use to build & test
# generated code across languages.
#
# Design goals:
#   - Idempotent, PER TOOL: each tool checks whether it is already present at the
#     pinned version and skips if so, so re-running (or copy-pasting this into
#     another repo) only installs what is missing.
#   - No root, no system package manager: everything self-contained lands under one
#     per-user dir ($CATALYST_TOOLS, default ~/.local/catalyst-tools), shared by all
#     catalyst projects. Anything that can be installed without system libraries
#     lives here; tools that need a distro toolchain (Ruby, Elixir/Erlang, Swift)
#     are left to the system and noted at the end.
#   - Self-describing: writes an `env.sh` you can source to put every tool on PATH.
#
# Usage:
#   tools/install-transport-toolchains.sh                  # install/verify everything
#   CATALYST_TOOLS=/opt/catalyst tools/install-transport-toolchains.sh
#   source ~/.local/catalyst-tools/env.sh                  # then build/test
set -euo pipefail

# --- pinned versions (override via env) ------------------------------------
ZIG_VER="${ZIG_VER:-0.14.1}"
JDK_VER="${JDK_VER:-17}"            # Java artifact floor; also drives Kotlin
GRADLE_VER="${GRADLE_VER:-8.10.2}"  # Kotlin/Java builds (`gradle`, not the wrapper)
DOTNET_CHANNEL="${DOTNET_CHANNEL:-8.0}"
DART_VER="${DART_VER:-latest}"      # a pinned x.y.z also works (uses that release path)
GO_VER="${GO_VER:-1.26.4}"
NODE_VER="${NODE_VER:-26.1.0}"
OCAML_VER="${OCAML_VER:-5.2.1}"     # opam switch compiler (needs cc/make/m4 to build)
OPAM_SWITCH="${OPAM_SWITCH:-catalyst}"

TOOLS="${CATALYST_TOOLS:-$HOME/.local/catalyst-tools}"
mkdir -p "$TOOLS"

# --- platform detection ----------------------------------------------------
OS="$(uname -s)"; ARCH="$(uname -m)"
[ "$OS" = "Linux" ] || { echo "This installer currently supports Linux only (saw $OS)." >&2; exit 1; }
case "$ARCH" in
  x86_64)  ZIG_ARCH=x86_64; JDK_ARCH=x64;     DART_ARCH=x64;   DOTNET_ARCH=x64;   GO_ARCH=amd64; NODE_ARCH=x64 ;;
  aarch64) ZIG_ARCH=aarch64; JDK_ARCH=aarch64; DART_ARCH=arm64; DOTNET_ARCH=arm64; GO_ARCH=arm64; NODE_ARCH=arm64 ;;
  *) echo "Unsupported arch: $ARCH" >&2; exit 1 ;;
esac

say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

fetch() { # fetch URL DEST  (follows redirects, fails on HTTP error, atomic on success)
  curl -fSL --retry 3 --retry-delay 2 -o "$2.part" "$1" && mv -f "$2.part" "$2"
}

# --------------------------------------------------------------------------- Zig
install_zig() {
  local bin="$TOOLS/zig-$ZIG_VER/zig"
  if [ -x "$bin" ] && [ "$("$bin" version 2>/dev/null)" = "$ZIG_VER" ]; then
    echo "Zig $ZIG_VER already installed."; return
  fi
  say "Installing Zig $ZIG_VER"
  # Zig 0.14+ names archives zig-<arch>-linux-<ver>; older releases use zig-linux-<arch>-<ver>.
  local base="https://ziglang.org/download/$ZIG_VER" tarball
  for name in "zig-$ZIG_ARCH-linux-$ZIG_VER" "zig-linux-$ZIG_ARCH-$ZIG_VER"; do
    if curl -fsI "$base/$name.tar.xz" >/dev/null 2>&1; then tarball="$name"; break; fi
  done
  [ -n "${tarball:-}" ] || { echo "Could not resolve a Zig $ZIG_VER archive for $ZIG_ARCH" >&2; return 1; }
  fetch "$base/$tarball.tar.xz" "$TOOLS/zig.tar.xz"
  rm -rf "$TOOLS/zig-$ZIG_VER"
  tar xf "$TOOLS/zig.tar.xz" -C "$TOOLS"
  mv "$TOOLS/$tarball" "$TOOLS/zig-$ZIG_VER"
  rm -f "$TOOLS/zig.tar.xz"
  "$bin" version
}

# --------------------------------------------------------------------------- JDK (Java + Kotlin)
install_jdk() {
  local bin="$TOOLS/jdk$JDK_VER/bin/java"
  if [ -x "$bin" ] && "$bin" -version 2>&1 | grep -q "\"$JDK_VER\."; then
    echo "JDK $JDK_VER already installed."; return
  fi
  say "Installing Temurin JDK $JDK_VER (for Java + Kotlin)"
  fetch "https://api.adoptium.net/v3/binary/latest/$JDK_VER/ga/linux/$JDK_ARCH/jdk/hotspot/normal/eclipse" "$TOOLS/jdk.tar.gz"
  rm -rf "$TOOLS/jdk$JDK_VER"; mkdir -p "$TOOLS/jdk$JDK_VER"
  tar xf "$TOOLS/jdk.tar.gz" -C "$TOOLS/jdk$JDK_VER" --strip-components=1
  rm -f "$TOOLS/jdk.tar.gz"
  "$bin" -version 2>&1 | head -1
}

# --------------------------------------------------------------------------- Gradle (Kotlin/Java builds)
install_gradle() {
  local bin="$TOOLS/gradle-$GRADLE_VER/bin/gradle"
  if [ -x "$bin" ] && "$bin" --version 2>/dev/null | grep -q "Gradle $GRADLE_VER"; then
    echo "Gradle $GRADLE_VER already installed."; return
  fi
  say "Installing Gradle $GRADLE_VER"
  fetch "https://services.gradle.org/distributions/gradle-$GRADLE_VER-bin.zip" "$TOOLS/gradle.zip"
  rm -rf "$TOOLS/gradle-$GRADLE_VER"
  unzip -q "$TOOLS/gradle.zip" -d "$TOOLS"
  rm -f "$TOOLS/gradle.zip"
  "$bin" --version | grep -i "^Gradle"
}

# --------------------------------------------------------------------------- .NET (C#)
install_dotnet() {
  local bin="$TOOLS/dotnet/dotnet"
  if [ -x "$bin" ] && "$bin" --version 2>/dev/null | grep -q "^${DOTNET_CHANNEL%.*}\."; then
    echo ".NET $DOTNET_CHANNEL already installed."; return
  fi
  say "Installing .NET SDK $DOTNET_CHANNEL (for C#)"
  fetch "https://dot.net/v1/dotnet-install.sh" "$TOOLS/dotnet-install.sh"
  bash "$TOOLS/dotnet-install.sh" --channel "$DOTNET_CHANNEL" --architecture "$DOTNET_ARCH" --install-dir "$TOOLS/dotnet"
  "$bin" --version
}

# --------------------------------------------------------------------------- Dart
install_dart() {
  local bin="$TOOLS/dart-sdk/bin/dart"
  if [ -x "$bin" ]; then
    if [ "$DART_VER" = "latest" ] || "$bin" --version 2>&1 | grep -q "$DART_VER"; then
      echo "Dart SDK already installed ($("$bin" --version 2>&1))."; return
    fi
  fi
  say "Installing Dart SDK ($DART_VER)"
  local path
  if [ "$DART_VER" = "latest" ]; then path="channels/stable/release/latest"; else path="channels/stable/release/$DART_VER"; fi
  fetch "https://storage.googleapis.com/dart-archive/$path/sdk/dartsdk-linux-$DART_ARCH-release.zip" "$TOOLS/dart.zip"
  rm -rf "$TOOLS/dart-sdk"
  unzip -q "$TOOLS/dart.zip" -d "$TOOLS"
  rm -f "$TOOLS/dart.zip"
  "$bin" --version 2>&1
}

# --------------------------------------------------------------------------- Go
install_go() {
  local bin="$TOOLS/go/bin/go"
  if [ -x "$bin" ] && "$bin" version 2>/dev/null | grep -q "go$GO_VER "; then
    echo "Go $GO_VER already installed."; return
  fi
  say "Installing Go $GO_VER"
  fetch "https://go.dev/dl/go$GO_VER.linux-$GO_ARCH.tar.gz" "$TOOLS/go.tar.gz"
  rm -rf "$TOOLS/go"
  tar xf "$TOOLS/go.tar.gz" -C "$TOOLS"   # extracts to $TOOLS/go
  rm -f "$TOOLS/go.tar.gz"
  "$bin" version
}

# --------------------------------------------------------------------------- Node (TypeScript)
install_node() {
  local bin="$TOOLS/node/bin/node"
  if [ -x "$bin" ] && [ "$("$bin" --version 2>/dev/null)" = "v$NODE_VER" ]; then
    echo "Node v$NODE_VER already installed."; return
  fi
  say "Installing Node v$NODE_VER"
  fetch "https://nodejs.org/dist/v$NODE_VER/node-v$NODE_VER-linux-$NODE_ARCH.tar.xz" "$TOOLS/node.tar.xz"
  rm -rf "$TOOLS/node"; mkdir -p "$TOOLS/node"
  tar xf "$TOOLS/node.tar.xz" -C "$TOOLS/node" --strip-components=1
  rm -f "$TOOLS/node.tar.xz"
  "$bin" --version
}

# --------------------------------------------------------------------------- OCaml (opam, per-user)
install_ocaml() {
  if ! { have cc && have make && have m4; }; then
    echo "OCaml: skipped — opam builds a switch from source and needs cc, make, m4 (install your distro's base build tools, then re-run)."
    return 0
  fi
  local opam="$TOOLS/bin/opam"
  export OPAMROOT="$TOOLS/opam"
  if [ -x "$opam" ] && "$opam" var --root "$OPAMROOT" --switch "$OPAM_SWITCH" prefix >/dev/null 2>&1; then
    echo "OCaml (opam switch '$OPAM_SWITCH') already installed."; return
  fi
  say "Installing OCaml $OCAML_VER via opam (per-user; may compile a switch)"
  mkdir -p "$TOOLS/bin"
  if [ ! -x "$opam" ]; then
    local tag; tag="$(curl -fsSL https://api.github.com/repos/ocaml/opam/releases/latest | grep -m1 '"tag_name"' | cut -d'"' -f4)"
    fetch "https://github.com/ocaml/opam/releases/download/$tag/opam-$tag-$ARCH-linux" "$opam"; chmod +x "$opam"
  fi
  export PATH="$TOOLS/bin:$PATH" OPAMJOBS="$(nproc 2>/dev/null || echo 2)" OPAMYES=1
  [ -d "$OPAMROOT" ] || "$opam" init --bare --yes --no-setup --root "$OPAMROOT" \
    || "$opam" init --bare --yes --no-setup --disable-sandboxing --root "$OPAMROOT"
  "$opam" switch list --root "$OPAMROOT" 2>/dev/null | grep -q "$OPAM_SWITCH" \
    || "$opam" switch create "$OPAM_SWITCH" "$OCAML_VER" --yes --root "$OPAMROOT" \
    || "$opam" switch create "$OPAM_SWITCH" "$OCAML_VER" --yes --disable-sandboxing --root "$OPAMROOT"
  eval "$("$opam" env --root "$OPAMROOT" --switch "$OPAM_SWITCH")"
  "$opam" install dune --yes
  ocaml --version; dune --version
}

install_zig
install_jdk
install_gradle
install_dotnet
install_dart
install_go
install_node
install_ocaml

# --- write a sourceable env file (per-tool opt-out via $CATALYST_TOOLS_SKIP) --
cat > "$TOOLS/env.sh" <<EOF
# Source this to put the catalyst toolchains on PATH:  source "$TOOLS/env.sh"
#
# Opt out of any tool — to use your own install instead — by listing it,
# space-separated, in CATALYST_TOOLS_SKIP before sourcing. The opted-out tool is
# not prepended, so your own copy already on \$PATH wins. Example:
#   export CATALYST_TOOLS_SKIP="zig go"
# Keys: zig java gradle dotnet dart go node opam
export CATALYST_TOOLS="$TOOLS"

# True when \$1 is listed in CATALYST_TOOLS_SKIP (whole word, space-separated).
_catalyst_skip() { case " \${CATALYST_TOOLS_SKIP:-} " in *" \$1 "*) return 0 ;; *) return 1 ;; esac; }

# Each tool is prepended (so it wins over a system copy) unless opted out.
_catalyst_skip opam || PATH="$TOOLS/bin:\$PATH"
_catalyst_skip node || PATH="$TOOLS/node/bin:\$PATH"
_catalyst_skip go   || PATH="$TOOLS/go/bin:\$PATH"
_catalyst_skip dart || PATH="$TOOLS/dart-sdk/bin:\$PATH"
if ! _catalyst_skip dotnet; then
  export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_CLI_HOME="$TOOLS/dotnet"
  PATH="$TOOLS/dotnet:\$PATH"
fi
if ! _catalyst_skip java; then
  export JAVA_HOME="$TOOLS/jdk$JDK_VER"
  PATH="\$JAVA_HOME/bin:\$PATH"
fi
_catalyst_skip gradle || PATH="$TOOLS/gradle-$GRADLE_VER/bin:\$PATH"
_catalyst_skip zig    || PATH="$TOOLS/zig-$ZIG_VER:\$PATH"
export PATH

# OCaml: load the opam switch env (skipped if opam opted out, or not installed).
if ! _catalyst_skip opam && [ -x "$TOOLS/bin/opam" ] && [ -d "$TOOLS/opam" ]; then
  eval "\$("$TOOLS/bin/opam" env --root "$TOOLS/opam" --switch "$OPAM_SWITCH" 2>/dev/null)"
fi
EOF

say "Done — installed under $TOOLS"
cat <<EOF
Activate the toolchains:

    source "$TOOLS/env.sh"

Self-contained tools installed here: Zig, Java (+ Kotlin via Gradle), Gradle, .NET,
Dart, Go, Node, and OCaml (per-user opam, when cc/make/m4 are present). Rust is the
host toolchain (use your existing rustup/system cargo).

NOTES — these need distro packages or system libraries, so install them with your
package manager rather than here:
  - Ruby 3.2+      : pacman -Syu ruby   | apt install ruby   | rbenv/ruby-install
  - Elixir/OTP 27+ : pacman -Syu elixir | apt install elixir | asdf / mise
  - Swift 6        : swiftly (https://www.swift.org/install/linux/) or swift.org tarball; pulls some system libs
EOF
