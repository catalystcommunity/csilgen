#!/bin/bash
# Build script for CSIL generator WASM module
#
# This script compiles your generator to a WASM module that can be used
# by the csilgen CLI tool.

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Building CSIL Generator WASM Module${NC}"

# Check if wasm32-unknown-unknown target is installed
if ! rustup target list --installed | grep -q "wasm32-unknown-unknown"; then
    echo -e "${YELLOW}Installing wasm32-unknown-unknown target...${NC}"
    rustup target add wasm32-unknown-unknown
fi

# Build the WASM module
echo -e "${YELLOW}Compiling to WASM...${NC}"
cargo build --target wasm32-unknown-unknown --release

# Get the generator name from Cargo.toml
GENERATOR_NAME=$(cargo metadata --no-deps --format-version 1 | grep '"name"' | head -1 | cut -d'"' -f4)
WASM_FILE="target/wasm32-unknown-unknown/release/${GENERATOR_NAME}.wasm"

if [ -f "$WASM_FILE" ]; then
    echo -e "${GREEN}✓ WASM module built successfully: $WASM_FILE${NC}"
    
    # Display file size
    SIZE=$(ls -lh "$WASM_FILE" | awk '{print $5}')
    echo -e "${GREEN}  File size: $SIZE${NC}"
    
    # Optional: Optimize with wasm-opt if available
    if command -v wasm-opt >/dev/null 2>&1; then
        echo -e "${YELLOW}Optimizing WASM module...${NC}"
        wasm-opt -Os "$WASM_FILE" -o "$WASM_FILE.optimized"
        mv "$WASM_FILE.optimized" "$WASM_FILE"
        
        OPTIMIZED_SIZE=$(ls -lh "$WASM_FILE" | awk '{print $5}')
        echo -e "${GREEN}  Optimized size: $OPTIMIZED_SIZE${NC}"
    else
        echo -e "${YELLOW}  Tip: Install wasm-opt for smaller modules: cargo install wasm-opt${NC}"
    fi
    
    echo ""
    echo -e "${GREEN}Your generator is ready to use!${NC}"
    echo -e "Copy ${WASM_FILE} to your csilgen generators directory."
    echo ""
    echo "Test your generator with:"
    echo "  csilgen generate --input example.csil --target ${GENERATOR_NAME} --output ./generated/"
else
    echo -e "${RED}✗ Build failed - WASM file not found${NC}"
    exit 1
fi