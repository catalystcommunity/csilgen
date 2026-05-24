# Custom Generator Example

This example demonstrates authoring a CSIL generator from scratch — emitting Go code, including how to handle services and field metadata. For the **canonical reference and the full interface spec**, see [`tools/generator-template/README.md`](../../tools/generator-template/README.md) and [`tools/generator-template/GENERATOR_INTERFACE.md`](../../tools/generator-template/GENERATOR_INTERFACE.md); this example is a working specimen, not the authoring docs.

## What This Generator Does

- **Input**: a CSIL specification (services + field metadata).
- **Output**: Go source files (structs, service interfaces, etc.).
- Demonstrates field-visibility handling (`@send-only` / `@receive-only` / `@bidirectional`), the per-direction service-operation emission model (handler + router + outbound encoders for `<->` / `<-`, not the gRPC-style send/recv streams), and idiomatic Go naming.

## Files

- `Cargo.toml` — wasm `cdylib` crate config; the package is named `csilgen-<target>-generator` so the build output filename derives the `--target` name.
- `src/lib.rs` — generator implementation with the four required WASM exports.
- `example-input.csil` — sample CSIL to exercise it.
- `build.sh` — convenience script to build the wasm module.

## Building & Using

```bash
cd examples/custom-generator
./build.sh
# Produces target/wasm32-unknown-unknown/release/csilgen_<target>_generator.wasm
```

To use it with the CLI, drop the built `.wasm` into a discovery directory. Discovery is **first-write-wins** across three paths:

1. `target/wasm32-unknown-unknown/release/` — local dev build.
2. `./.generators/` — project-local override.
3. `~/.csilgen/generators/` — per-user baseline.

```bash
# Per-project pin (overrides any homedir copy without touching it):
mkdir -p ../../.generators
cp target/wasm32-unknown-unknown/release/csilgen_<target>_generator.wasm ../../.generators/

# Now `--target <target>` resolves dynamically — no CLI change required:
csilgen generate \
    --input example-input.csil \
    --target <target> \
    --output ./generated/
```

`<target>` is whatever string sits between `csilgen_` and `_generator.wasm` in the filename. That's the entire registration mechanism — no `csilgen generator install` command exists, and none is needed.

## Interface (Summary)

A generator is a `cdylib` exporting four C-ABI functions:

```rust
#[unsafe(no_mangle)] pub extern "C" fn get_metadata() -> *const u8;
#[unsafe(no_mangle)] pub extern "C" fn allocate(size: usize) -> *mut u8;
#[unsafe(no_mangle)] pub extern "C" fn deallocate(ptr: *mut u8, size: usize);
#[unsafe(no_mangle)] pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8;
```

The host serializes a `WasmGeneratorInput` (your spec + config + generator metadata) into wasm memory and calls `generate`. You return a length-prefixed JSON-serialized `WasmGeneratorOutput` containing `files: Vec<GeneratedFile>` (each with `path` and `content`), warnings, and stats. See `GENERATOR_INTERFACE.md` for the full spec.

## Key Implementation Patterns

### Field metadata (visibility, descriptions, constraints)

```rust
for entry in &group.entries {
    for meta in &entry.metadata {
        match meta {
            CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly) => { /* … */ }
            CsilFieldMetadata::Visibility(CsilFieldVisibility::ReceiveOnly) => { /* … */ }
            CsilFieldMetadata::Description(text) => { /* doc comment */ }
            CsilFieldMetadata::Constraint(c) => { /* validation */ }
            _ => {}
        }
    }
}
```

### Service operations by direction

```rust
for op in &service.operations {
    match op.direction {
        CsilServiceDirection::Unidirectional => {
            // Emit: handler returns Output (server) / method calls transport (client).
        }
        CsilServiceDirection::Bidirectional => {
            // Emit: per-side inbound handler + outbound encoder + router entry.
            // Generators emit shapes + routing; the implementer owns the wire.
        }
        CsilServiceDirection::Reverse => {
            // Server pushes Output; client handles Output. No server inbound,
            // no client outbound.
        }
    }
}
```

See `csil-spec.md` "Operation Directions" for the contract every generator follows.

## Tips

1. **Start with type generation**; add services once your AST traversal is solid.
2. **Test against small CSIL fixtures** in `#[cfg(test)]` modules in `src/lib.rs` — you can call `process_generation` directly without going through wasmtime.
3. **Match the cross-generator wire convention**: PascalCase method names in router switches so frames are interoperable.
4. **Don't open the connection**: emit handler interfaces and `(method, bytes)` from encoders; let the implementer wire those to their WebSocket/TCP/etc.
