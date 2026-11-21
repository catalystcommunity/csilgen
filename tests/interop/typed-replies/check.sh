#!/usr/bin/env bash
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
if [[ "$lang" == json || "$lang" == openapi ]]; then
    "$compiler" generate -q -i "$fixture/choices.csil" -t "$lang" -o "$work"
    cd "$work"
    python3 "$fixture/descriptions.py" "$lang"
    exit
fi
if [[ "$lang" == python ]]; then output="$work/api"; else output="$work"; fi
for target in "$lang" "$lang-client"; do
    "$compiler" generate -q -i "$fixture/replies.csil" -t "$target" -o "$output"
done
cd "$work"
case "$lang" in
    c) cp "$fixture/peer.c" .; cc -std=c11 -Wall -Wextra -Werror peer.c -o peer; ./peer ;;
    csharp)
        cp "$fixture/Peer.cs" .
        echo '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>' > Peer.csproj
        dotnet run --project Peer.csproj ;;
    dart) cp "$fixture/peer.dart" .; dart --enable-asserts peer.dart ;;
    elixir)
        mkdir -p beam
        elixirc -o beam ./*.gen.ex
        elixir -pa beam "$fixture/peer.exs" ;;
    java)
        cp "$fixture/Peer.java" csilgen/generated/
        mkdir -p classes
        javac -d classes csilgen/generated/*.java "$root"/transports/java/src/main/java/community/catalyst/csilgen/transport/*.java
        java -ea -cp classes csilgen.generated.Peer ;;
    kotlin)
        mkdir -p src/main/kotlin
        cp -r community src/main/kotlin/
        cp "$fixture/Peer.kt" src/main/kotlin/
        echo 'rootProject.name = "rpc-reply-test"' > settings.gradle.kts
        cat > build.gradle.kts <<'EOF'
plugins {
    kotlin("jvm") version "2.2.20"
    application
}
repositories { mavenCentral() }
application { mainClass.set("community.catalyst.csilgen.generated.PeerKt") }
EOF
        gradle --offline --console=plain run ;;
    ocaml)
        cp "$fixture/peer.ml" .
        ocamlc csil_cbor.ml types.mli types.ml codec.ml services.ml client.ml peer.ml -o peer
        ./peer ;;
    php) cp "$fixture/peer.php" .; CSILGEN_ROOT="$root" php -d zend.assertions=1 -d assert.exception=1 peer.php ;;
    python) cp "$fixture/peer.py" .; python3 peer.py ;;
    ruby) cp "$fixture/peer.rb" .; ruby peer.rb ;;
    swift)
        cp "$fixture/peer.swift" main.swift
        swiftc Types.swift Codec.swift Services.swift Client.swift main.swift -o peer
        ./peer ;;
    typescript)
        cp "$fixture/peer.ts" .
        node --test peer.ts
        if command -v tsc >/dev/null; then tsc_cmd=(tsc); else tsc_cmd=(npx --offline --package=typescript@5 tsc); fi
        for surface in typescript typescript-typesonly typescript-client typescript-server; do
            (cd "$root" && "$compiler" generate -q -i "$fixture/strip-types.csil" -t "$surface" -o "$work/$surface")
            cp "$fixture/strip-types.ts" "$surface/"
            (cd "$surface" && "${tsc_cmd[@]}" --noEmit --erasableSyntaxOnly && node --test strip-types.ts)
        done
        "${tsc_cmd[@]}" --erasableSyntaxOnly --target es2022 --module commonjs --strict --rewriteRelativeImportExtensions --outDir dist *.ts
        node dist/peer.js ;;
    zig) cp "$fixture/peer.zig" .; zig test peer.zig ;;
    *) echo "Unsupported target $lang" >&2; exit 1 ;;
esac
