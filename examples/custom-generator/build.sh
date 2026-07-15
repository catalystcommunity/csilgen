#!/bin/bash
# Build script for the mdsummary example CSIL generator.
#
# Produces target/wasm32-unknown-unknown/release/csilgen_mdsummary_generator.wasm
# using the real generator ABI (docs/generator-plugin-contract.md) — plain
# `cargo build`, not wasm-pack/wasm-bindgen, which target a browser-JS
# calling convention this host doesn't speak.

set -e

SCRIPT_DIR="$(dirname "$(realpath "${BASH_SOURCE[0]}")")"
cd "$SCRIPT_DIR"

WASM_FILE="target/wasm32-unknown-unknown/release/csilgen_mdsummary_generator.wasm"

echo "Building mdsummary example CSIL generator..."
cargo build --release --target wasm32-unknown-unknown

if [ ! -f "$WASM_FILE" ]; then
    echo "Build failed - $WASM_FILE not found" >&2
    exit 1
fi

echo "Built: $SCRIPT_DIR/$WASM_FILE"
echo ""
echo "To use it with the CLI:"
echo "  mkdir -p $SCRIPT_DIR/../../.generators"
echo "  cp $SCRIPT_DIR/$WASM_FILE $SCRIPT_DIR/../../.generators/"
echo "  csilgen generate --input $SCRIPT_DIR/example-input.csil --target mdsummary --output <output-dir>"
