#!/bin/bash

# Build script for custom CSIL generator WASM module

set -e

# Get the directory where this script is located
SCRIPT_DIR="$(dirname "$(realpath "${BASH_SOURCE[0]}")")"

echo "Building custom CSIL generator WASM module..."

# Clean previous builds
rm -rf "$SCRIPT_DIR/pkg/"

# Build the WASM module with wasm-pack
wasm-pack build --target web --out-dir "$SCRIPT_DIR/pkg"

echo "WASM module built successfully!"
echo "Output: $SCRIPT_DIR/pkg/custom_csil_generator.wasm"
echo ""
echo "To test the generator:"
echo "  csilgen generate --input $SCRIPT_DIR/example-input.csil --target $SCRIPT_DIR/pkg/custom_csil_generator.wasm --output $SCRIPT_DIR/generated/"