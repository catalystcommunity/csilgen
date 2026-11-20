#!/usr/bin/env bash
set -euo pipefail
fixture=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$fixture/../../.." && pwd)
compiler=${CSILGEN:-"$root/target/release/csilgen"}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$root"

for lang in rust go; do
    for surface in "$lang" "$lang-client"; do
        "$compiler" generate -q -i "$fixture/replies.csil" -t "$surface" -o "$work/$lang"
    done
done
"$compiler" generate -q -i "$fixture/choices.csil" -t rust-client -o "$work/choices"
cd "$work"
cp "$fixture/peer.rs" "$fixture/peer.go" .
cat > Cargo.toml <<EOF
[package]
name = "reply-check"
edition = "2024"
version = "0.0.0"

[dependencies]
csilgen-transport = { path = "$root/transports/rust" }

[[bin]]
name = "peer"
path = "peer.rs"
EOF
go mod init replycheck
go mod edit -require=github.com/catalystcommunity/csilgen/transports/go@v0.0.0 \
    -replace=github.com/catalystcommunity/csilgen/transports/go="$root/transports/go"
cargo run --quiet
go run peer.go
cargo run --quiet -- read-go
