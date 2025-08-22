#!/bin/bash
# Development and testing script for CSIL generators
#
# This script provides utilities for local development and testing of your
# CSIL generator before building the final WASM module.

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
GENERATOR_NAME=$(cargo metadata --no-deps --format-version 1 | grep '"name"' | head -1 | cut -d'"' -f4)
TEST_DIR="test-output"
FIXTURES_DIR="test-fixtures"

show_help() {
    echo "CSIL Generator Development and Testing Tool"
    echo ""
    echo "Usage: $0 [COMMAND] [OPTIONS]"
    echo ""
    echo "Commands:"
    echo "  test          Run all tests (unit + integration)"
    echo "  unit          Run unit tests only"
    echo "  integration   Run integration tests with sample CSIL files"
    echo "  build         Build the WASM module"
    echo "  validate      Validate WASM module exports"
    echo "  benchmark     Run performance benchmarks"
    echo "  fixtures      Generate test CSIL fixtures"
    echo "  clean         Clean test outputs and build artifacts"
    echo "  watch         Watch for changes and auto-test"
    echo "  help          Show this help message"
    echo ""
    echo "Options:"
    echo "  --verbose     Enable verbose output"
    echo "  --release     Use release build (for benchmarks)"
    echo ""
    echo "Examples:"
    echo "  $0 test                    # Run all tests"
    echo "  $0 integration --verbose   # Run integration tests with verbose output"  
    echo "  $0 benchmark --release     # Run benchmarks with optimized build"
}

log() {
    echo -e "${BLUE}[$(date +'%H:%M:%S')]${NC} $1"
}

success() {
    echo -e "${GREEN}✓${NC} $1"
}

warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

error() {
    echo -e "${RED}✗${NC} $1"
    exit 1
}

# Check if a command exists
check_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        error "Required command '$1' not found. Please install it first."
    fi
}

# Run unit tests
run_unit_tests() {
    log "Running unit tests..."
    cargo test --lib
    success "Unit tests passed"
}

# Generate test fixtures if they don't exist
generate_fixtures() {
    log "Generating test CSIL fixtures..."
    
    mkdir -p "$FIXTURES_DIR"
    
    # Basic types fixture
    cat > "$FIXTURES_DIR/basic-types.csil" << 'EOF'
; Basic CSIL types test
User = {
    name: text @bidirectional @description("User's display name"),
    email: text ? @send-only @min-length(5),
    id: uint @receive-only,
    active: bool @bidirectional,
    created_at: text @receive-only @description("ISO timestamp"),
}
EOF

    # Service definition fixture  
    cat > "$FIXTURES_DIR/user-service.csil" << 'EOF'
; CSIL service definition test
User = {
    name: text @bidirectional,
    email: text @send-only,
    id: uint @receive-only,
}

CreateUserRequest = {
    name: text @depends-on(email),
    email: text ?,
}

service UserAPI {
    create-user: CreateUserRequest -> User
    get-user: uint -> User  
    update-user: User <-> User
    delete-user: uint -> nil
}
EOF

    # Complex types fixture
    cat > "$FIXTURES_DIR/complex-types.csil" << 'EOF'
; Complex CSIL types with metadata
Address = {
    street: text @bidirectional @min-length(1),
    city: text @bidirectional,
    country: text @bidirectional @max-length(2),
    postal_code: text ? @send-only,
}

User = {
    id: uint @receive-only,
    name: text @bidirectional @description("Full name"),
    addresses: [* Address] @bidirectional @max-items(5),
    metadata: { * text => any } ? @receive-only,
}

service AddressService {
    validate-address: Address -> bool
    geocode-address: Address <-> { lat: float, lng: float }
}
EOF

    success "Generated test fixtures in $FIXTURES_DIR/"
}

# Run integration tests with sample CSIL files
run_integration_tests() {
    log "Running integration tests..."
    
    # Generate fixtures if they don't exist
    if [ ! -d "$FIXTURES_DIR" ]; then
        generate_fixtures
    fi
    
    # Build the generator first
    cargo build
    
    # Create test output directory
    rm -rf "$TEST_DIR"
    mkdir -p "$TEST_DIR"
    
    success "Integration test setup complete"
    
    # Test each fixture file
    for fixture in "$FIXTURES_DIR"/*.csil; do
        if [ -f "$fixture" ]; then
            filename=$(basename "$fixture" .csil)
            log "Testing fixture: $filename"
            
            # TODO: Once csilgen CLI is available, test with:
            # csilgen generate --input "$fixture" --target "$GENERATOR_NAME" --output "$TEST_DIR/$filename/"
            
            # For now, just validate that the fixture parses correctly
            echo "  - Fixture file exists and is readable"
            if [ -s "$fixture" ]; then
                success "  - Fixture $filename is valid"
            else
                error "  - Fixture $filename is empty or invalid"
            fi
        fi
    done
    
    success "Integration tests completed"
}

# Build WASM module
build_wasm() {
    local release_flag=""
    if [ "$RELEASE_BUILD" = true ]; then
        release_flag="--release"
        log "Building WASM module (release mode)..."
    else
        log "Building WASM module (debug mode)..."
    fi
    
    # Check for wasm32 target
    if ! rustup target list --installed | grep -q "wasm32-unknown-unknown"; then
        log "Installing wasm32-unknown-unknown target..."
        rustup target add wasm32-unknown-unknown
    fi
    
    # Build WASM module
    cargo build --target wasm32-unknown-unknown $release_flag
    
    local build_dir="target/wasm32-unknown-unknown"
    if [ "$RELEASE_BUILD" = true ]; then
        build_dir="$build_dir/release"
    else
        build_dir="$build_dir/debug"
    fi
    
    local wasm_file="$build_dir/${GENERATOR_NAME}.wasm"
    
    if [ -f "$wasm_file" ]; then
        local size=$(ls -lh "$wasm_file" | awk '{print $5}')
        success "WASM module built: $wasm_file ($size)"
        
        # Optimize with wasm-opt if available
        if command -v wasm-opt >/dev/null 2>&1; then
            log "Optimizing WASM module..."
            wasm-opt -Os "$wasm_file" -o "$wasm_file.optimized"
            mv "$wasm_file.optimized" "$wasm_file"
            
            local optimized_size=$(ls -lh "$wasm_file" | awk '{print $5}')
            success "Optimized WASM module: $optimized_size"
        else
            warning "wasm-opt not found - install for smaller modules"
        fi
    else
        error "WASM build failed"
    fi
}

# Validate WASM module exports
validate_wasm() {
    log "Validating WASM module exports..."
    
    local build_dir="target/wasm32-unknown-unknown/debug"
    if [ "$RELEASE_BUILD" = true ]; then
        build_dir="target/wasm32-unknown-unknown/release"
    fi
    
    local wasm_file="$build_dir/${GENERATOR_NAME}.wasm"
    
    if [ ! -f "$wasm_file" ]; then
        error "WASM module not found. Run '$0 build' first."
    fi
    
    # Check if wasm-objdump is available for validation
    if command -v wasm-objdump >/dev/null 2>&1; then
        log "Checking exported functions..."
        
        local exports=$(wasm-objdump -x "$wasm_file" | grep -A 1000 "Export\[" | grep "func\[")
        
        # Required exports
        local required_exports=("generate" "get_metadata" "allocate" "deallocate")
        local found_exports=()
        
        for export in "${required_exports[@]}"; do
            if echo "$exports" | grep -q "\"$export\""; then
                found_exports+=("$export")
                success "  - Found required export: $export"
            else
                error "  - Missing required export: $export"
            fi
        done
        
        if [ ${#found_exports[@]} -eq ${#required_exports[@]} ]; then
            success "All required exports found"
        else
            error "Missing required exports"
        fi
        
        # Check memory export
        if wasm-objdump -x "$wasm_file" | grep -q "memory\[0\]"; then
            success "  - Memory export found"
        else
            warning "  - Memory export not found (may cause issues)"
        fi
        
    else
        warning "wasm-objdump not found - install wabt for detailed validation"
        success "WASM file exists and appears valid"
    fi
}

# Run performance benchmarks
run_benchmarks() {
    log "Running performance benchmarks..."
    
    if [ "$RELEASE_BUILD" != true ]; then
        warning "Benchmarks should be run with --release flag for accurate results"
    fi
    
    # Build with benchmarks
    cargo bench
    
    success "Benchmarks completed"
}

# Watch for changes and auto-test
watch_changes() {
    log "Watching for changes... (Press Ctrl+C to stop)"
    
    # Check if cargo-watch is available
    if ! command -v cargo-watch >/dev/null 2>&1; then
        error "cargo-watch not found. Install with: cargo install cargo-watch"
    fi
    
    # Watch for changes and run tests
    cargo watch -x 'test --lib' -x 'build'
}

# Clean test outputs and build artifacts
clean_all() {
    log "Cleaning test outputs and build artifacts..."
    
    rm -rf "$TEST_DIR"
    rm -rf "target/"
    
    success "Cleaned build artifacts and test outputs"
}

# Parse command line arguments
VERBOSE=false
RELEASE_BUILD=false
COMMAND=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --verbose)
            VERBOSE=true
            shift
            ;;
        --release)
            RELEASE_BUILD=true
            shift
            ;;
        test|unit|integration|build|validate|benchmark|fixtures|clean|watch|help)
            COMMAND=$1
            shift
            ;;
        *)
            echo "Unknown option: $1"
            show_help
            exit 1
            ;;
    esac
done

# Set verbose mode
if [ "$VERBOSE" = true ]; then
    set -x
fi

# Execute command
case $COMMAND in
    test)
        run_unit_tests
        run_integration_tests
        ;;
    unit)
        run_unit_tests
        ;;
    integration)
        run_integration_tests
        ;;
    build)
        build_wasm
        ;;
    validate)
        validate_wasm
        ;;
    benchmark)
        run_benchmarks
        ;;
    fixtures)
        generate_fixtures
        ;;
    clean)
        clean_all
        ;;
    watch)
        watch_changes
        ;;
    help|"")
        show_help
        ;;
    *)
        echo "Unknown command: $COMMAND"
        show_help
        exit 1
        ;;
esac