#!/usr/bin/env bash
# Install the language toolchains needed to run the CSIL transport conformance
# suites (`cargo run -p xtask test-transports`).
#
# Design goals:
#   - Idempotent: re-running skips anything already present at the pinned version.
#   - No root, no system package manager: everything lands under one per-user dir
#     ($CSILGEN_TOOLS, default ~/.local/csilgen-tools) so it never touches the
#     system. Honors this repo's "avoid system dependencies" rule.
#   - Self-describing: writes an `env.sh` you can source to put every tool on PATH.
#
# It fully installs the four download-and-extract toolchains (Zig, a JDK for
# Java+Kotlin, the .NET SDK, the Dart SDK). The remaining four (Swift, OCaml,
# Ruby, Elixir/Erlang) need a compiler bootstrap or system libraries that vary by
# distro, so this script prints guidance for them rather than installing fragile
# copies — see the NOTES section it emits at the end.
#
# Usage:
#   tools/install-transport-toolchains.sh           # install/verify all four
#   CSILGEN_TOOLS=/opt/csil tools/install-transport-toolchains.sh
#   source ~/.local/csilgen-tools/env.sh            # then run the transport tests
set -euo pipefail

# --- pinned versions (override via env) ------------------------------------
ZIG_VER="${ZIG_VER:-0.14.1}"
JDK_VER="${JDK_VER:-17}"          # Java artifact floor; also drives Kotlin
DOTNET_CHANNEL="${DOTNET_CHANNEL:-8.0}"
DART_VER="${DART_VER:-latest}"    # a pinned x.y.z also works (uses that release path)
OCAML_VER="${OCAML_VER:-5.2.1}"   # opam switch compiler (needs cc/make/m4 to build)

TOOLS="${CSILGEN_TOOLS:-$HOME/.local/csilgen-tools}"
mkdir -p "$TOOLS"

# --- platform detection ----------------------------------------------------
OS="$(uname -s)"; ARCH="$(uname -m)"
[ "$OS" = "Linux" ] || { echo "This installer currently supports Linux only (saw $OS)." >&2; exit 1; }
case "$ARCH" in
  x86_64)  ZIG_ARCH=x86_64; JDK_ARCH=x64;     DART_ARCH=x64;     DOTNET_ARCH=x64 ;;
  aarch64) ZIG_ARCH=aarch64; JDK_ARCH=aarch64; DART_ARCH=arm64;   DOTNET_ARCH=arm64 ;;
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

# --------------------------------------------------------------------------- OCaml (opam, per-user)
install_ocaml() {
  if ! { have cc && have make && have m4; }; then
    echo "OCaml: skipped — opam builds a switch from source and needs cc, make, m4 (install your distro's base build tools, then re-run)."
    return 0
  fi
  local opam="$TOOLS/bin/opam"
  export OPAMROOT="$TOOLS/opam"
  if [ -x "$opam" ] && "$opam" var --root "$OPAMROOT" --switch csil prefix >/dev/null 2>&1; then
    echo "OCaml (opam switch 'csil') already installed."; return
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
  "$opam" switch list --root "$OPAMROOT" 2>/dev/null | grep -q csil \
    || "$opam" switch create csil "$OCAML_VER" --yes --root "$OPAMROOT" \
    || "$opam" switch create csil "$OCAML_VER" --yes --disable-sandboxing --root "$OPAMROOT"
  eval "$("$opam" env --root "$OPAMROOT" --switch csil)"
  "$opam" install dune --yes
  ocaml --version; dune --version
}

install_zig
install_jdk
install_dotnet
install_dart
install_ocaml

# --- write a sourceable env file ------------------------------------------
cat > "$TOOLS/env.sh" <<EOF
# Source this to put the CSIL transport toolchains on PATH:  source "$TOOLS/env.sh"
export JAVA_HOME="$TOOLS/jdk$JDK_VER"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_CLI_HOME="$TOOLS/dotnet"
export PATH="$TOOLS/bin:$TOOLS/zig-$ZIG_VER:\$JAVA_HOME/bin:$TOOLS/dotnet:$TOOLS/dart-sdk/bin:\$PATH"
# OCaml: load the opam switch env (no-op if opam/the switch isn't installed)
if [ -x "$TOOLS/bin/opam" ] && [ -d "$TOOLS/opam" ]; then
  eval "\$("$TOOLS/bin/opam" env --root "$TOOLS/opam" --switch csil 2>/dev/null)"
fi
EOF

say "Done — installed under $TOOLS"
cat <<EOF
Activate the toolchains, then run the transport suites:

    source "$TOOLS/env.sh"
    cargo run -p xtask test-transports

This enables: C (needs system cc + cmake), Java, Kotlin, Zig, Dart, C#, and OCaml
(per-user opam, when cc/make/m4 are present) — plus the always-on Rust/Go/TS/Python
when those are present. xtask skips any language whose toolchain is still absent.

NOTES — these three need distro packages or system libraries, so install them with your
package manager rather than here:
  - Ruby 3.2+      : pacman -Syu ruby   | apt install ruby   | rbenv/ruby-install
  - Elixir/OTP 27+ : pacman -Syu elixir | apt install elixir | asdf / mise
  - Swift 6        : swiftly (https://www.swift.org/install/linux/) or swift.org tarball; pulls some system libs
EOF
