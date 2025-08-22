# CSIL Generator Packaging and Distribution Guide

This guide explains how to package, distribute, and publish your custom CSIL generators built with this template.

## Overview

CSIL generators are distributed as WebAssembly (WASM) modules that can be easily shared, installed, and used by anyone with the `csilgen` CLI tool. This guide covers the complete workflow from development to publication.

## Building for Distribution

### 1. Prepare Your Generator

Before packaging, ensure your generator is ready:

```bash
# Run all tests
make test

# Format and lint code
make format lint

# Build optimized WASM module
make build-release

# Validate WASM exports
make validate
```

### 2. Optimize the WASM Module

Your generator should be built with optimizations enabled:

```bash
# Build with optimizations
cargo build --target wasm32-unknown-unknown --release

# Further optimize with wasm-opt (if available)
wasm-opt -Os target/wasm32-unknown-unknown/release/your-generator.wasm \
         -o your-generator-optimized.wasm
```

Optimization goals:
- **Size**: Smaller modules load faster
- **Performance**: Optimized code runs faster  
- **Compatibility**: Ensure compatibility across WASM runtimes

### 3. Test with Real CSIL Files

Create comprehensive test cases with realistic CSIL specifications:

```csil
; Example comprehensive CSIL file for testing
User = {
    id: uint @receive-only @description("Unique user identifier"),
    name: text @bidirectional @min-length(1) @max-length(100),
    email: text @send-only @description("User's email address"),
    created_at: text @receive-only @description("ISO timestamp"),
    roles: [* text] @bidirectional @max-items(10),
    metadata: { * text => any } ? @receive-only,
}

CreateUserRequest = {
    name: text @depends-on(email) @min-length(1),
    email: text @depends-on(name) @description("Required email"),
    initial_roles: [* text] ? @max-items(5),
}

service UserAPI {
    ; Standard CRUD operations
    create-user: CreateUserRequest -> User
    get-user: uint -> User
    update-user: User -> User
    delete-user: uint -> nil
    
    ; Advanced operations
    list-users: { page: uint, size: uint } -> { users: [* User], total: uint }
    search-users: { query: text, filters: { * text => any } } <-> [* User]
}

service AuthAPI {
    login: { email: text, password: text } -> { user: User, token: text }
    logout: { token: text } -> nil
    refresh: { refresh_token: text } <-> { access_token: text }
}
```

Test your generator against this file to ensure it handles:
- Services with multiple operations
- Complex field metadata
- Field dependencies
- Validation constraints
- Bidirectional operations
- Various data types

## Distribution Methods

### 1. Single WASM File Distribution

The simplest distribution method is sharing the WASM file directly:

**Advantages:**
- Simple to share and install
- No dependency management
- Works across platforms
- Single file deployment

**Steps:**
1. Build optimized WASM module
2. Share the `.wasm` file via:
   - GitHub releases
   - Package registries
   - Direct download
   - Email/messaging

**Installation:**
```bash
# User downloads and installs
curl -L https://github.com/you/your-generator/releases/latest/download/your-generator.wasm \
     -o ~/.csilgen/generators/your-generator.wasm

# Test the generator
csilgen generate --input example.csil --target your-generator --output ./generated/
```

### 2. GitHub Releases

GitHub provides excellent infrastructure for distributing WASM modules:

**Setup GitHub Actions for Releases:**
```yaml
# .github/workflows/release.yml
name: Release

on:
  push:
    tags:
      - 'v*'

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown
      
      - name: Build WASM
        run: |
          cargo build --target wasm32-unknown-unknown --release
          # Optional: optimize with wasm-opt
      
      - name: Create Release
        uses: softprops/action-gh-release@v1
        with:
          files: |
            target/wasm32-unknown-unknown/release/*.wasm
            README.md
            GENERATOR_INTERFACE.md
          body: |
            ## Changes
            - See commit history for changes
            
            ## Installation
            ```bash
            curl -L ${{ github.event.release.assets[0].browser_download_url }} \
                 -o ~/.csilgen/generators/${{ github.event.repository.name }}.wasm
            ```
            
            ## Usage
            ```bash
            csilgen generate --input api.csil --target ${{ github.event.repository.name }} --output ./gen/
            ```
```

**Creating a Release:**
```bash
# Tag and push
git tag v1.0.0
git push origin v1.0.0

# GitHub Actions will automatically build and create the release
```

### 3. Package Registries

Consider publishing to package registries for easier discovery:

**Cargo Registry (crates.io):**
While you can't publish WASM files directly to crates.io, you can publish source code:

```toml
# Cargo.toml
[package]
name = "csilgen-my-generator"
version = "1.0.0"
edition = "2021"
description = "My custom CSIL generator"
license = "Apache-2.0"
repository = "https://github.com/you/my-generator"
keywords = ["csilgen", "code-generation", "wasm"]
categories = ["development-tools", "template-engine"]

[[bin]]
name = "build-wasm"
path = "src/build.rs"
```

Users can then install and build:
```bash
cargo install csilgen-my-generator
build-wasm  # Builds the WASM file
```

**NPM Registry (for web distribution):**
```json
{
  "name": "@your-org/csilgen-my-generator",
  "version": "1.0.0",
  "description": "My custom CSIL generator",
  "main": "my-generator.wasm",
  "files": ["my-generator.wasm", "README.md"],
  "keywords": ["csilgen", "code-generation", "wasm"],
  "repository": "https://github.com/you/my-generator",
  "license": "Apache-2.0"
}
```

### 4. Docker Distribution

Package your generator in a Docker container for isolated execution:

```dockerfile
# Dockerfile
FROM scratch
COPY target/wasm32-unknown-unknown/release/my-generator.wasm /my-generator.wasm
```

```bash
# Build and publish
docker build -t your-org/csilgen-my-generator:latest .
docker push your-org/csilgen-my-generator:latest

# Users can extract the WASM file
docker create your-org/csilgen-my-generator:latest
docker cp container_id:/my-generator.wasm ./
docker rm container_id
```

## Installation Methods

### 1. Manual Installation

Users manually download and place the WASM file:

```bash
# Create generators directory
mkdir -p ~/.csilgen/generators

# Download and install
curl -L https://github.com/you/your-generator/releases/latest/download/your-generator.wasm \
     -o ~/.csilgen/generators/your-generator.wasm

# Verify installation
csilgen list-generators | grep your-generator
```

### 2. Installation Script

Provide an installation script for convenience:

```bash
#!/bin/bash
# install.sh
set -e

GENERATOR_NAME="my-generator"
LATEST_URL="https://api.github.com/repos/you/my-generator/releases/latest"
INSTALL_DIR="${HOME}/.csilgen/generators"

echo "Installing ${GENERATOR_NAME}..."

# Create installation directory
mkdir -p "$INSTALL_DIR"

# Get latest release download URL
DOWNLOAD_URL=$(curl -s "$LATEST_URL" | grep "browser_download_url.*\.wasm" | cut -d '"' -f 4)

if [ -z "$DOWNLOAD_URL" ]; then
    echo "Error: Could not find WASM file in latest release"
    exit 1
fi

# Download and install
curl -L "$DOWNLOAD_URL" -o "$INSTALL_DIR/${GENERATOR_NAME}.wasm"

echo "✓ ${GENERATOR_NAME} installed successfully!"
echo ""
echo "Usage:"
echo "  csilgen generate --input api.csil --target ${GENERATOR_NAME} --output ./generated/"
```

Users can install with:
```bash
curl -sSL https://raw.githubusercontent.com/you/my-generator/main/install.sh | bash
```

### 3. Package Manager Integration

Integrate with existing package managers:

**Homebrew (macOS/Linux):**
```ruby
# my-generator.rb
class MyGenerator < Formula
  desc "CSIL generator for My Language"
  homepage "https://github.com/you/my-generator"
  url "https://github.com/you/my-generator/releases/latest/download/my-generator.wasm"
  sha256 "..." # Calculate SHA256 of the WASM file
  
  def install
    (lib/"csilgen"/"generators").install "my-generator.wasm"
  end
  
  def caveats
    <<~EOS
      The generator has been installed to #{lib}/csilgen/generators/
      
      You may need to copy it to your csilgen generators directory:
        mkdir -p ~/.csilgen/generators
        cp #{lib}/csilgen/generators/my-generator.wasm ~/.csilgen/generators/
    EOS
  end
end
```

## Versioning and Updates

### Semantic Versioning

Follow semantic versioning for your generator releases:

- **Major (1.0.0 → 2.0.0)**: Breaking changes to generated code or interface
- **Minor (1.0.0 → 1.1.0)**: New features, additional capabilities
- **Patch (1.0.0 → 1.0.1)**: Bug fixes, optimizations

### Update Notifications

Consider implementing update notification mechanisms:

1. **Version endpoint**: Provide a URL that returns the latest version
2. **Generator metadata**: Include version in generator metadata
3. **Update checking**: Let users check for updates

```bash
# Example update check
csilgen check-updates my-generator
# Output: my-generator: 1.0.0 → 1.2.0 (update available)
```

### Migration Guides

For breaking changes, provide migration guides:

```markdown
# Migration Guide: v1.x → v2.x

## Breaking Changes

1. **Output structure changed**: Generated files now use different directory structure
   - Before: `types.ext`, `services.ext`
   - After: `models/types.ext`, `api/services.ext`

2. **Configuration options renamed**:
   - `use_tabs` → `indent_style: tabs`
   - `max_line_length` → `line_length`

## Migration Steps

1. Update generator: `curl -L ... -o ~/.csilgen/generators/my-generator.wasm`
2. Update configuration in your build scripts
3. Update any scripts that depend on old output structure
```

## Documentation and Support

### 1. README Template

Create comprehensive documentation:

```markdown
# My CSIL Generator

Generate [Target Language] code from CSIL specifications.

## Features

- ✅ Full CDDL type support
- ✅ CSIL service definitions  
- ✅ Field metadata processing
- ✅ Validation constraints
- ✅ Custom annotations

## Installation

```bash
curl -L https://github.com/you/my-generator/releases/latest/download/my-generator.wasm \
     -o ~/.csilgen/generators/my-generator.wasm
```

## Usage

```bash
# Basic usage
csilgen generate --input api.csil --target my-generator --output ./generated/

# With options
csilgen generate --input api.csil --target my-generator --output ./gen/ \
  --option indent_size=4 \
  --option generate_docs=true
```

## Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `indent_size` | number | 2 | Number of spaces for indentation |
| `use_tabs` | boolean | false | Use tabs instead of spaces |
| `generate_docs` | boolean | true | Generate documentation comments |

## Examples

See [examples/](examples/) directory for complete examples.

## Support

- Issues: [GitHub Issues](https://github.com/you/my-generator/issues)
- Discussions: [GitHub Discussions](https://github.com/you/my-generator/discussions)
- Documentation: [Wiki](https://github.com/you/my-generator/wiki)
```

### 2. Examples and Tutorials

Provide working examples:

```
examples/
├── basic/
│   ├── api.csil
│   ├── generate.sh
│   └── expected-output/
├── advanced/
│   ├── complex-api.csil
│   ├── config.json
│   └── expected-output/
└── tutorials/
    ├── getting-started.md
    ├── advanced-features.md
    └── troubleshooting.md
```

### 3. Community and Support

- Create GitHub Discussions for Q&A
- Set up issue templates for bug reports
- Consider creating a Discord/Slack community
- Contribute to CSIL ecosystem documentation

## Quality Assurance

### 1. Automated Testing

Ensure comprehensive testing in CI:

```yaml
# Test matrix for different scenarios
strategy:
  matrix:
    test-case:
      - basic-types
      - complex-services
      - metadata-heavy
      - large-schema
      - edge-cases
```

### 2. Compatibility Testing

Test with different CSIL features:

- All CDDL basic types
- Complex nested structures
- Service definitions with all directions
- All field metadata types
- Large specifications (performance)
- Invalid/edge case inputs

### 3. Performance Benchmarks

Include performance metrics in releases:

```markdown
## Performance (v1.2.0)

| Metric | Value |
|--------|-------|
| Module size | 245 KB |
| Cold start time | 12ms |
| Generation speed | 1000 types/sec |
| Peak memory | 4.2 MB |
```

## Security Considerations

### 1. WASM Module Security

- Use safe Rust practices (no `unsafe` unless necessary)
- Validate all inputs at WASM boundary
- Implement proper memory management
- Limit resource usage (memory, time)

### 2. Generated Code Security

- Sanitize generated identifiers
- Prevent code injection through user input
- Generate secure code patterns
- Include security warnings in documentation

### 3. Distribution Security

- Sign WASM modules if possible
- Use HTTPS for all downloads
- Provide checksums for verification
- Use secure CI/CD practices

## Publication Checklist

Before publishing your generator:

- [ ] All tests pass
- [ ] Documentation is complete
- [ ] Examples work correctly
- [ ] WASM module is optimized
- [ ] Security review completed
- [ ] Version is properly tagged
- [ ] Release notes are written
- [ ] Installation instructions tested
- [ ] Backwards compatibility considered
- [ ] Migration guide provided (if breaking changes)

## Best Practices

1. **Keep modules small**: Optimize for size and load time
2. **Provide clear documentation**: Users should understand capabilities
3. **Include comprehensive examples**: Show real-world usage
4. **Test thoroughly**: Cover edge cases and error conditions
5. **Follow semantic versioning**: Communicate breaking changes clearly
6. **Support the community**: Respond to issues and questions
7. **Maintain quality**: Regular updates and bug fixes

## Future Considerations

- **Registry integration**: Consider official CSIL generator registry
- **Plugin discovery**: Automatic generator discovery mechanisms  
- **Dependency management**: Handling generator dependencies
- **Composition**: Combining multiple generators
- **Caching**: Improve performance with intelligent caching

Your generator is now ready for distribution! 🚀