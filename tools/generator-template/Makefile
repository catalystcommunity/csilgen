# Makefile for CSIL Generator Development
# 
# This Makefile provides convenient shortcuts for common development tasks.
# It wraps the test.sh script and provides additional targets for development workflow.

.PHONY: all build test unit integration validate benchmark fixtures clean watch help install dev-deps format lint check

# Default target
all: format lint test build validate

# Build the WASM module
build:
	@./build.sh

# Build release version
build-release:
	@./test.sh build --release

# Run all tests
test:
	@./test.sh test

# Run only unit tests
unit:
	@./test.sh unit

# Run only integration tests  
integration:
	@./test.sh integration

# Validate WASM module exports
validate:
	@./test.sh validate

# Run performance benchmarks
benchmark:
	@./test.sh benchmark --release

# Generate test fixtures
fixtures:
	@./test.sh fixtures

# Clean build artifacts and test outputs
clean:
	@./test.sh clean

# Watch for changes and auto-test
watch:
	@./test.sh watch

# Install development dependencies
dev-deps:
	@echo "Installing development dependencies..."
	@rustup target add wasm32-unknown-unknown
	@rustup component add clippy rustfmt
	@cargo install cargo-watch || true
	@cargo install cargo-audit || true
	@echo "Development dependencies installed!"
	@echo ""
	@echo "Optional tools (install manually if needed):"
	@echo "  - wabt (for wasm-objdump): https://github.com/WebAssembly/wabt"
	@echo "  - binaryen (for wasm-opt): https://github.com/WebAssembly/binaryen"

# Format code
format:
	@echo "Formatting code..."
	@cargo fmt

# Run linter
lint:
	@echo "Running clippy..."
	@cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings

# Check code (format + lint + test)
check: format lint test

# Install the generator locally (for testing with csilgen CLI)
install:
	@echo "Installing generator..."
	@./build.sh
	@mkdir -p ~/.csilgen/generators
	@cp target/wasm32-unknown-unknown/release/*.wasm ~/.csilgen/generators/
	@echo "Generator installed to ~/.csilgen/generators/"

# Development workflow - check everything before committing
pre-commit: format lint test build validate
	@echo "All checks passed! Ready to commit."

# Create a new generator from this template
# Usage: make new-generator NAME=my-generator TARGET=go
new-generator:
	@if [ -z "$(NAME)" ]; then echo "Error: NAME is required. Usage: make new-generator NAME=my-generator TARGET=go"; exit 1; fi
	@if [ -z "$(TARGET)" ]; then echo "Error: TARGET is required. Usage: make new-generator NAME=my-generator TARGET=go"; exit 1; fi
	@echo "Creating new generator: $(NAME) (target: $(TARGET))"
	@mkdir -p ../$(NAME)
	@cp -r * ../$(NAME)/ || true
	@cp -r .github ../$(NAME)/ || true  
	@cp -r .vscode ../$(NAME)/ || true
	@sed -i 's/csilgen-generator-template/$(NAME)/g' ../$(NAME)/Cargo.toml || true
	@sed -i 's/my-custom-generator/$(NAME)/g' ../$(NAME)/src/lib.rs || true
	@sed -i 's/my-target/$(TARGET)/g' ../$(NAME)/src/lib.rs || true
	@echo "New generator created at ../$(NAME)/"
	@echo "Next steps:"
	@echo "  1. cd ../$(NAME)"
	@echo "  2. Update Cargo.toml with your details"
	@echo "  3. Implement your code generation logic in src/lib.rs"
	@echo "  4. Run 'make test' to verify everything works"

# Show help
help:
	@echo "CSIL Generator Development Makefile"
	@echo ""
	@echo "Available targets:"
	@echo "  all           - Format, lint, test, build, and validate"
	@echo "  build         - Build WASM module (debug)"
	@echo "  build-release - Build WASM module (release)"
	@echo "  test          - Run all tests"
	@echo "  unit          - Run unit tests only"
	@echo "  integration   - Run integration tests only"
	@echo "  validate      - Validate WASM module exports"
	@echo "  benchmark     - Run performance benchmarks"
	@echo "  fixtures      - Generate test CSIL fixtures"
	@echo "  clean         - Clean build artifacts"
	@echo "  watch         - Watch for changes and auto-test"
	@echo "  dev-deps      - Install development dependencies"
	@echo "  format        - Format code with rustfmt"
	@echo "  lint          - Run clippy linter"
	@echo "  check         - Format + lint + test"
	@echo "  install       - Install generator locally for testing"
	@echo "  pre-commit    - Run all checks before committing"
	@echo "  new-generator - Create new generator from template"
	@echo "  help          - Show this help message"
	@echo ""
	@echo "Examples:"
	@echo "  make test                                    # Run all tests"
	@echo "  make build-release                          # Build optimized WASM"
	@echo "  make new-generator NAME=go-gen TARGET=go    # Create Go generator"