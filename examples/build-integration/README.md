# Build System Integration Examples

This directory demonstrates how to integrate CSIL code generation into common build systems.

## Rust Integration (`rust-project/`)

Shows how to integrate CSIL generation into Cargo build process:

- **`build.rs`**: Build script that runs csilgen during compilation
- **`Cargo.toml`**: Build dependencies and configuration
- **`api.csil`**: CSIL specification for the project
- **`src/main.rs`**: Example usage of generated types

### Try it:
```bash
cd examples/build-integration/rust-project
cargo build  # Triggers CSIL generation via build.rs
cargo run    # Uses the generated types
```

## NPM Integration (`npm-project/`)

Shows how to integrate CSIL generation into npm/Node.js build process:

- **`package.json`**: npm scripts that run csilgen before build
- **`api.csil`**: CSIL specification for the project  
- **`src/index.js`**: Example usage of generated TypeScript types

### Try it:
```bash
cd examples/build-integration/npm-project
npm install
npm run build      # Runs generate-types then tsc
npm run dev        # Generates types and runs the example
```

## Integration Patterns

### 1. Build-Time Generation (Recommended)
- Types are generated during the build process
- Ensures generated code is always up-to-date with the CSIL spec
- Fails the build if CSIL spec is invalid

### 2. Pre-Commit Hooks
```bash
# Add to .git/hooks/pre-commit
csilgen generate --input api.csil --target rust --output src/generated/
git add src/generated/
```

### 3. CI/CD Pipeline
```yaml
# GitHub Actions example
- name: Generate CSIL Types
  run: |
    csilgen generate --input api.csil --target typescript --output src/generated/
    git diff --exit-code src/generated/ || (echo "Generated types are out of date" && exit 1)
```

### 4. Watch Mode (Development)
```bash
# Watch for changes and regenerate
find . -name "*.csil" | entr csilgen generate --input api.csil --target rust --output src/generated/
```

## Benefits

1. **Type Safety**: Generated types ensure compile-time validation
2. **Consistency**: Same CSIL spec generates consistent types across languages
3. **Automation**: No manual type maintenance required
4. **Validation**: Build fails if CSIL spec is invalid
5. **Documentation**: Generated types include documentation from CSIL metadata

These patterns ensure your generated types stay synchronized with your CSIL specifications across different build environments.