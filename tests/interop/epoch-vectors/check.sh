#!/usr/bin/env bash
# Decode every row of vectors.tsv as a `Moment` in one target, then re-encode the
# rows that must decode. Usage: check.sh <target> [work-dir]
set -euo pipefail
fixture=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$fixture/../../.." && pwd)
cd "$root"
compiler=${CSILGEN:-"$root/target/release/csilgen"}
lang=${1:?supply a generator target}
work=${2:-$(mktemp -d)}
if [[ $# == 1 ]]; then trap 'rm -rf "$work"' EXIT; fi
mkdir -p "$work"
work=$(cd "$work" && pwd)
vectors="$fixture/vectors.tsv"
if [[ "$lang" == python ]]; then output="$work/api"; else output="$work"; fi
"$compiler" generate -q -i "$fixture/epoch.csil" -t "$lang" -o "$output"
cd "$work"
case "$lang" in
    c) cp "$fixture/peer.c" .; cc -std=c11 -Wall -Wextra -Werror peer.c -o peer; ./peer "$vectors" ;;
    csharp)
        cp "$fixture/Peer.cs" .
        echo '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework><Nullable>enable</Nullable></PropertyGroup></Project>' > Peer.csproj
        dotnet run --project Peer.csproj -- "$vectors" ;;
    dart) cp "$fixture/peer.dart" .; dart --enable-asserts peer.dart "$vectors" ;;
    elixir)
        mkdir -p beam
        elixirc -o beam ./*.gen.ex
        elixir -pa beam "$fixture/peer.exs" "$vectors" ;;
    go)
        mkdir -p api
        mv ./*.gen.go api/
        cp "$fixture/peer.go" .
        go mod init momentcheck >/dev/null 2>&1
        go run . "$vectors" ;;
    java)
        cp "$fixture/Peer.java" csilgen/generated/
        mkdir -p classes
        javac -d classes csilgen/generated/*.java
        java -ea -cp classes csilgen.generated.Peer "$vectors" ;;
    kotlin)
        mkdir -p src/main/kotlin
        cp -r community src/main/kotlin/
        cp "$fixture/Peer.kt" src/main/kotlin/
        echo 'rootProject.name = "epoch-check"' > settings.gradle.kts
        cat > build.gradle.kts <<'KTS'
plugins {
    kotlin("jvm") version "2.2.20"
    application
}
repositories { mavenCentral() }
application { mainClass.set("community.catalyst.csilgen.generated.PeerKt") }
KTS
        gradle --offline --console=plain -q run --args="$vectors" ;;
    ocaml)
        cp "$fixture/peer.ml" .
        ocamlc csil_cbor.ml types.mli types.ml codec.ml peer.ml -o peer
        ./peer "$vectors" ;;
    php) cp "$fixture/peer.php" .; CSILGEN_ROOT="$root" php -d zend.assertions=1 -d assert.exception=1 peer.php "$vectors" ;;
    python) cp "$fixture/peer.py" .; python3 peer.py "$vectors" ;;
    ruby) cp "$fixture/peer.rb" .; ruby peer.rb "$vectors" ;;
    rust)
        mkdir -p src
        mv ./*.rs src/ 2>/dev/null || true
        cp "$fixture/peer.rs" src/main.rs
        cat > Cargo.toml <<TOML
[package]
name = "epoch-check"
edition = "2024"
version = "0.0.0"

[workspace]

[dependencies]
time = { version = "0.3.47", features = ["formatting", "parsing"] }
TOML
        cargo run --quiet -- "$vectors" ;;
    swift)
        cp "$fixture/peer.swift" main.swift
        cp "$vectors" vectors.tsv
        build='swiftc Types.swift Codec.swift main.swift -o peer && ./peer vectors.tsv'
        if command -v swiftc >/dev/null; then sh -c "$build"
        else docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$work:/w" -w /w swift:6.0-bookworm sh -c "$build"; fi ;;
    typescript) cp "$fixture/peer.ts" .; node peer.ts "$vectors" ;;
    zig) cp "$fixture/peer.zig" .; zig run peer.zig -- "$vectors" ;;
    *) echo "Unsupported target $lang" >&2; exit 1 ;;
esac
