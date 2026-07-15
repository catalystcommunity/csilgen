# CSIL generator plugin contract

This document specifies the **plugin interface** every csilgen generator implements:
the WASM ABI, the JSON shapes crossing that boundary, how a generator is discovered
and invoked, and what "conformant" means at each tier. It is language-agnostic — a
generator can be authored in any language that compiles to a WASM `cdylib` (or
equivalent); nothing here is Rust-specific.

This is a **different, narrower** contract than
[`cbor-wire-contract.md`](cbor-wire-contract.md): that document governs the CBOR
bytes a *codec-emitting* generator produces on the wire between two runtime
processes. This document governs the boundary between the `csilgen` host and a
generator process at *generation time* — the thing every generator implements,
whether or not it ever touches CBOR. See "Conformance tiers" below for how the two
relate.

Everything here is derived from and verified against the actual implementation:
`wasm/csilgen-wasm-generators/src/lib.rs` (the host/runtime), `crates/csilgen-common/src/types.rs`
(the boundary types), and `wasm/csilgen-noop-generator/` (the minimal reference
generator). Where the implementation and other in-repo documentation disagree, that
is called out explicitly rather than papered over — see "Known discrepancies" at the
end.

## 1. Discovery & naming

A generator registers itself purely by filename: **`csilgen_<target>_generator.wasm`**.
The `<target>` substring — whatever sits between the `csilgen_` prefix and the
`_generator.wasm` suffix — is the name users pass to `--target`. Any file that
doesn't match this pattern (wrong extension, missing prefix/suffix, or an empty
target, e.g. `csilgen__generator.wasm`) is silently ignored by discovery; a search
directory can hold unrelated files without affecting anything
(`GeneratorRegistry::derive_target`).

The runtime scans three directories, **first-write-wins**, in this priority order:

1. `<current-working-directory>/target/wasm32-unknown-unknown/release` — local dev
   build output; only meaningful when `csilgen` is invoked from inside the csilgen
   workspace itself.
2. `./.generators` — a project-local pin/override, relative to the CLI's working
   directory.
3. `~/.csilgen/generators` — the user's installed baseline.

"First-write-wins" means the first directory in that list that supplies a given
generator id keeps it; the same id appearing in a lower-priority directory is
ignored, including when the higher-priority copy fails to load (a broken duplicate
in a lower-priority path does not fall back to mask a working higher-priority one).
This lets a project pin or replace a specific generator build in `.generators/`
without touching the user's homedir install. This was verified directly for this
document: copying a working generator into a scratch project's `./.generators/` and
a corrupted same-named file into a scratch `~/.csilgen/generators` resolved to the
working copy; removing the project copy then correctly fell through to the
homedir copy and surfaced its (expected) compile failure.

### Sub-target resolution

`--target <name>` resolves against the set of discovered generators' advertised
`target` (itself filename-derived) by, in order: exact match, else the longest
advertised target `t` such that `name` starts with `t + "-"`
(`resolve_generator_for_target` in `crates/csilgen-cli/src/lib.rs`). This is how one
`csilgen_typescript_generator.wasm` serves `typescript`, `typescript-client`,
`typescript-server`, `typescript-typesonly`, etc. — a single generator binary
advertises the base target `typescript`, and every dashed extension of that string
resolves to it.

The **full, unmodified `--target` string** — not the resolved generator id, not the
generator's advertised base target — is what the generator receives, in
`WasmGeneratorInput.config.target` (§4). A generator that wants to serve sub-targets
is entirely responsible for inspecting `config.target` itself and deciding what
subset of its normal output to emit; the host does no sub-target-specific dispatch
beyond finding the right `.wasm` file.

### What `get_metadata()` does *not* do

Every generator is required to export `get_metadata` (§2), but the shipped runtime
never calls it during discovery. `GeneratorMetadata` — including the `target` and
`version` fields — is synthesized entirely from the filename and a constant
capability list (`GeneratorRegistry::probe_generator_metadata`); the wasm module is
not even instantiated at discovery time. Consequently the version-compatibility and
capability-support checks in `GeneratorRegistry::check_compatibility` currently
always pass for any conforming filename, because they check the synthesized
metadata, not anything the module reports about itself. Implement `get_metadata`
correctly anyway: it is part of the exported ABI surface (§2), other tooling may
call it directly, and a future host version may start using it for real
version/capability negotiation.

## 2. The WASM ABI

A generator is a `cdylib` that exports:

```
memory                                                  ; standard wasm linear memory
get_metadata() -> *const u8
allocate(size: usize) -> *mut u8
deallocate(ptr: *mut u8, size: usize)
generate(input_ptr: *const u8, input_len: usize) -> *mut u8
```

`wasm/csilgen-noop-generator/src/lib.rs` is the minimal working implementation of
all four; treat it as the reference, not the illustrative code fragments in older
docs (see "Known discrepancies").

### Calling convention

**Input.** The host does **not** call the generator's own `allocate` to hand over
input. It writes the raw, UTF-8, JSON-serialized `WasmGeneratorInput` bytes directly
into the module's exported `memory` at a host-chosen offset, with **no length
prefix on the wire** — the length travels as the separate `input_len` argument —
and then calls `generate(input_ptr, input_len)`. A generator's `generate` reads
those bytes with something equivalent to `slice::from_raw_parts(input_ptr,
input_len)` and does not need to call its own `allocate` to read the input; it only
needs `allocate` to produce the *output* buffer, below. (Current host behavior
writes at a monotonically increasing offset starting at byte 1024 in the module's
memory and never calls the module's `deallocate` for this write — see "Known
discrepancies" for what this means in practice.)

**Output.** `generate` returns a pointer into its own memory. The buffer at that
pointer is **length-prefixed**: 4 bytes, a little-endian `u32` byte count, followed
by that many bytes of JSON-serialized `WasmGeneratorOutput` (§4). The generator
MUST obtain this buffer through its own `allocate` export (the host does not
pre-allocate an output buffer for the generator). A **null pointer return means
failure**; the host surfaces this as "Generator returned null pointer" with no
further detail, so prefer populating `WasmGeneratorOutput.warnings` for
non-fatal problems and reserve a null return for cases you can't produce any output
at all. `get_metadata()` uses the identical length-prefix convention for
`GeneratorMetadata` JSON.

**Deallocation.** `deallocate(ptr, size)` mirrors `allocate` and must correctly free
what `allocate` produced (reconstruct and drop the same allocation, not just
`free()` a raw pointer) — the reference implementation reconstructs a `Vec` and
lets it drop. The current host never calls a generator's `deallocate`; each
generation runs in its own fresh `wasmtime::Store`, and that store's entire linear
memory — anything the generator allocated with `allocate`, `Vec::forget`ed or not
— is reclaimed when the store is dropped at the end of the call. Implement
`deallocate` correctly regardless: it is exported ABI surface, not a private
implementation detail, and other embedders of a generator module may call it.

### Error signaling

There is no structured error channel from generator to host today, despite
`csilgen_common::WasmGeneratorError` existing as a type — nothing in the runtime
currently produces or consumes it; treat it as reserved for future use, not
something the host inspects. What the host actually does:

- A generator **panic/trap** during `generate` surfaces as `"Generator execution
  failed: {wasmtime error}"`.
- A **null return** from `generate` surfaces as `"Generator returned null
  pointer"`.
- Output that fails to deserialize as `WasmGeneratorOutput` surfaces the
  `serde_json` error verbatim.
- **Fuel exhaustion** (below) is detected by string-matching `"fuel"` in the
  wasmtime error's cause chain and surfaced as a dedicated "exceeded its
  instruction budget" message.

`csilgen_common::wasm_interface::error_codes` (`SUCCESS`, `INVALID_INPUT`,
`SERIALIZATION_ERROR`, `GENERATION_ERROR`, `OUT_OF_MEMORY`,
`UNSUPPORTED_FEATURE`) are `i32` constants a generator can use internally (the
noop generator does, to decide whether to return null), but they are not read by
the host from anywhere — there is no out-parameter or side channel for them. A
generator that wants to report *why* it failed, short of a hard trap, should do so
as a `WarningLevel::Warning` entry in a still-successful `WasmGeneratorOutput`
rather than by returning null with an internal code the host will never see.

### Execution limits

The wasmtime `Engine` is configured with `consume_fuel(true)`. Each call to
`generate` gets a fuel budget of:

```
fuel_budget = 1_000_000_000 + 50_000 * input_json.len()
```

i.e. a 1B-instruction floor plus 50,000 instructions per byte of the serialized
`WasmGeneratorInput`. This is sized from the input rather than a fixed ceiling
deliberately: codegen work scales with spec size, and an earlier fixed 100M-fuel
cap trapped mid-run on large real-world specs, surfacing as an opaque backtrace
inside whatever function happened to be running at exhaustion (historically
`convert_case`) rather than as a clear "ran out of budget" error. A synchronous
wasmtime call cannot be resumed once it traps on fuel exhaustion — it is terminal
for that `generate` call, not a refuel-and-continue point — and the host reports it
plainly as exceeding the instruction budget.

Two other configured limits are **not actually enforced** by the host today:

- `WasmLimits::max_execution_time` (default 30s) is checked **after**
  `generate` has already returned, by comparing elapsed wall-clock time — it is a
  post-hoc report, not a preemptive timeout. Fuel exhaustion is the only thing
  that can actually interrupt a runaway `generate` call mid-flight.
- `WasmLimits::max_memory_bytes` (default 64MB) is a struct field with no
  corresponding wasmtime `Store` limiter wired up; nothing currently caps a
  generator's memory growth beyond what fuel indirectly limits by making
  further work expensive.

`csilgen_common::wasm_interface::MAX_INPUT_SIZE` (64MB) and `MAX_OUTPUT_SIZE`
(256MB) are contract constants, not host-enforced limits — the host never checks
serialized input or output length against them. They exist for generators to
self-validate against, which is exactly what the reference `noop` generator does
(`process_generation` rejects `input_len > MAX_INPUT_SIZE` before parsing).

## 3. The boundary JSON shapes

`crates/csilgen-common/src/types.rs` is the **reference schema** — every type in
this section is a Rust struct/enum there, `#[derive(Serialize, Deserialize)]`ed
with `serde_json`'s default (untagged-by-variant-name, no field renaming) encoding.
JSON is the wire encoding of those Rust types, not an independently specified
schema; if the Rust types change, the JSON shape changes with them, in whatever way
serde's default derive produces. **There is no schema version field anywhere in
this envelope.** The schema is implicitly pinned to the csilgen release (in
practice, the `csilgen-common` crate version) that both the host and the generator
were built against; there is currently no mechanism for a generator to declare
"I speak schema version N" or for the host to detect a mismatch beyond outright
deserialization failure.

### `WasmGeneratorInput` (host → generator)

```rust
pub struct WasmGeneratorInput {
    pub csil_spec: CsilSpecSerialized,
    pub config: GeneratorConfig,
    pub generator_metadata: GeneratorMetadata,
}
```

- **`csil_spec`** — the parsed, validated CSIL specification. See below.
- **`config`** —
  - `target: String` — the **full, unmodified `--target` value** the user passed
    (§1), e.g. `"typescript-client"`, not the generator's own base target.
  - `output_dir: String` — the `--output` directory as given on the CLI. This is
    informational to the generator; the CLI itself resolves each returned file's
    path relative to it (see `GeneratedFile` below), so a generator has no reason
    to inspect this unless it wants to log or embed the path in generated
    comments.
  - `options: HashMap<String, serde_json::Value>` — generator-specific options.
    These come **only** from the CSIL source file's `options { … }` block (a flat
    `key: literal` map of strings/numbers/bools/nulls/arrays-of-literals), never
    from CLI flags — the CLI does not accept `--option` flags. A generator that
    wants to be configured must document which keys it reads from this block.
- **`generator_metadata`** — a `GeneratorMetadata` value built by the host for this
  call (name = the generator id derived from the resolved filename, `target` =
  `config.target`), **not** whatever the generator's own `get_metadata()` would
  return. Provided for the generator's convenience/logging; do not rely on it for
  anything the generator doesn't already know from `config.target`.

### `CsilSpecSerialized`

```rust
pub struct CsilSpecSerialized {
    pub rules: Vec<CsilRule>,
    pub source_content: Option<String>,     // currently always None from the CLI path
    pub service_count: usize,
    pub fields_with_metadata_count: usize,
}

pub struct CsilRule {
    pub name: String,
    pub rule_type: CsilRuleType,
    pub position: CsilPosition,              // { line, column, offset }, 1-based line/column
    #[serde(default)] pub doc_comments: Vec<String>,  // preceding `;;;` doc comments
}

pub enum CsilRuleType {
    TypeDef(CsilTypeExpression),
    GroupDef(CsilGroupExpression),
    TypeChoice(Vec<CsilTypeExpression>),
    GroupChoice(Vec<CsilGroupExpression>),
    ServiceDef(CsilServiceDefinition),
}
```

`CsilTypeExpression` covers every CSIL/CDDL-derived type shape a generator will
see: `Builtin(String)`, `Reference(String)`, `Array{element_type,occurrence}`,
`Map{key,value,occurrence}`, `Group(CsilGroupExpression)`,
`Tuple(CsilGroupExpression)` (fixed-shape/keyed arrays), `Choice(Vec<..>)`,
`Range{start,end,inclusive}`, `Socket(String)`, `Plug(String)`,
`Literal(CsilLiteralValue)`, and `Constrained{base_type, constraints:
Vec<CsilControlOperator>}` (the RFC 8610 control operators plus CSIL's
`@`-annotation-derived ones: size, regex, default, comparisons, bits, `.and`,
`.within`, `.json`/`.cbor`/`.cborseq`).

`CsilGroupEntry` (a field within a group/tuple) carries the rich, CSIL-specific
metadata that distinguishes this from plain CDDL:

```rust
pub struct CsilGroupEntry {
    pub key: Option<CsilGroupKey>,           // Bare(name) | Type(expr) | Literal(value)
    pub value_type: CsilTypeExpression,
    pub occurrence: Option<CsilOccurrence>,  // Optional | ZeroOrMore | OneOrMore | Exact(n) | Range{min,max}
    pub metadata: Vec<CsilFieldMetadata>,
    #[serde(default)] pub doc_comments: Vec<String>,
}

pub enum CsilFieldMetadata {
    Visibility(CsilFieldVisibility),          // SendOnly | ReceiveOnly | Bidirectional
    DependsOn { field: String, value: Option<CsilLiteralValue> },
    DependsOnExpr(CsilDependsCondition),      // boolean Compare/All/Any tree
    Constraint(CsilValidationConstraint),     // MinLength/MaxLength/MinItems/MaxItems/MinValue/MaxValue/Custom
    Description(String),
    Custom { name: String, parameters: Vec<CsilMetadataParameter> },  // @<name>(params...) hints
}
```

### Services

```rust
pub struct CsilServiceDefinition {
    pub operations: Vec<CsilServiceOperation>,
    #[serde(default)] pub wire_id: Option<u64>,   // @wire-id(N) service ordinal, compact-profile transports
}

pub struct CsilServiceOperation {
    pub name: String,
    pub input_type: CsilTypeExpression,
    pub output_type: CsilTypeExpression,
    pub direction: CsilServiceDirection,     // Unidirectional (->) | Bidirectional (<->) | Reverse (<-)
    pub position: CsilPosition,
    #[serde(default)] pub doc_comments: Vec<String>,
    #[serde(default)] pub wire_id: Option<u64>,   // @wire-id(N) operation ordinal
}
```

### `WasmGeneratorOutput` (generator → host)

```rust
pub struct WasmGeneratorOutput {
    pub files: Vec<GeneratedFile>,      // { path: String, content: String }
    pub warnings: Vec<GeneratorWarning>, // { level, message, location: Option<SourceLocation>, suggestion: Option<String> }
    pub stats: GenerationStats,          // files_generated, total_size_bytes, services_count,
                                          // fields_with_metadata_count, generation_time_ms, peak_memory_bytes
}
```

`GeneratedFile.path` is resolved against the CLI's `--output` directory by
`write_generated_files` in `crates/csilgen-cli/src/lib.rs`, which now
**sanitizes it host-side** before joining (`sanitize_output_path` in the same
file): it walks `Path::components()` and hard-errors, naming both the resolved
generator id and the offending path, on any `..` (`ParentDir`) component, any
`RootDir`/`Prefix` component (covers both a leading `/` and a Windows drive
letter), or a path with no real components at all (empty, or only `.`/`CurDir`
components). Because the target file does not exist yet at write time,
`canonicalize` isn't an option (it requires an existing path), so this is a
component-wise check rather than a resolve-and-compare one — but since only
plain `Normal` components ever reach the final joined path, nothing can land
outside `output_dir`. Nested relative subdirectories (e.g. `csilgen/api/Types.java`,
which the java generator emits) continue to work fine. A generator MUST still
emit only relative, traversal-safe paths (no leading `/`, no `..` components)
per this section — the host-side check is belt-and-braces defense against a
buggy or malicious generator, not a reason to be sloppy on the generator side.

`stats` and `warnings` are purely advisory — nothing downstream currently
validates them against the actual `files` returned; they exist for the CLI to
report to the user and are a reasonable place to put anything a generator wants
to tell the user short of a hard failure (§2, "Error signaling").

## 4. Conformance tiers

**Tier 0 — the plugin contract (this document).** Every generator implements
only what's above: discovery by filename, the four WASM exports, the calling
convention, and the `WasmGeneratorInput`/`WasmGeneratorOutput` JSON shapes. What a
generator actually *emits* is otherwise completely unconstrained — language
bindings, API documentation, an OpenAPI/JSON Schema document, project scaffolding,
a lint report, anything that can be expressed as a set of `(path, content)` pairs.
The in-repo `json` and `openapi` generators (`wasm/csilgen-json-generator`,
`wasm/csilgen-openapi-generator`) are the precedent for this: they satisfy Tier 0
in full and emit no runtime codec at all — they describe shapes, they don't encode
bytes.

**Tier 1 — runtime codecs and CSIL interoperability.** A generator that emits a
*runtime codec* — code that claims to encode/decode CSIL types as CBOR bytes
interoperable with other CSIL implementations — MUST additionally conform to
[`docs/cbor-wire-contract.md`](cbor-wire-contract.md): map keying by verbatim CSIL
field name, the scalar/tag encodings (including `timestamp`/tag 0 and
`decimal`/tag 4), enum-vs-union choice classification, hoisted-name conventions for
synthesized types, and decoder strictness. A generator that also emits clients
and/or servers over that codec MUST further conform to whichever transport
doc(s) apply to what it emits — `csil-rpc-transport.md`,
`csil-events-transport.md`, `csil-datagrams-transport.md`, built on
`csil-transport-conventions.md`. A Tier 1 generator can and should validate itself
against `tests/interop`, the N×N cross-language interoperability suite
(`cargo run -p xtask interop`) — that is the practical acceptance test for "does
this codec actually round-trip against everyone else's."

**Convenience machinery is not part of the contract.** The in-repo Rust generators
share classification/hoisting logic from `csilgen-common`
(`csilgen_common::choice::classify_choice` for enum-vs-union classification,
`csilgen_common::hoist` for literal-choice hoisting) so that every Rust-authored
generator in this repo applies the wire contract identically instead of
re-deriving it. That is a convenience implementation available to Rust generators
built against `csilgen-common`, not a requirement of either tier above. A
generator authored in any other language, or a Rust generator that doesn't import
`csilgen-common`'s helpers, satisfies Tier 1 the same way: by producing
byte-identical output to what the wire contract specifies, by whatever means.

## 5. Minimal walkthrough

Two starting points, both verified to build and run against the actual host:

- **`wasm/csilgen-noop-generator/`** — the smallest complete, correct
  implementation of all four exports and the length-prefix protocol. Read
  `src/lib.rs` top to bottom; it is short enough to hold in your head, and its
  `#[cfg(test)]` module shows how to unit-test `process_generation` directly,
  without going through wasmtime at all.
- **`examples/custom-generator/`** — a real, buildable, discoverable generator
  (target `mdsummary`) that walks the AST to produce a Markdown summary, aimed
  at someone authoring a generator from scratch, including the field-metadata
  and service-direction patterns noop deliberately skips. Its `README.md` is
  the narrative, including a verified end-to-end transcript.

## Known discrepancies (resolved)

The following were found while writing this document by reading the runtime
rather than trusting prior descriptions of it, and have since been fixed —
recorded here rather than deleted outright, per this repo's practice of
surfacing rather than silently papering over drift:

- The doc comment on `csilgen_common::types` (`crates/csilgen-common/src/types.rs`,
  around `WasmGeneratorError`) used to show an illustrative `generate` signature
  of `generate(input_ptr, input_len, output_ptr: *mut *mut u8, output_len: *mut
  usize) -> i32` — a four-argument, out-parameter calling convention no
  generator in this repo ever implemented. It now shows the real two-argument,
  single-pointer-return signature from §2 above.
- `tools/generator-template/GENERATOR_INTERFACE.md`'s "Memory Allocation Flow"
  section used to describe the handshake as "host allocates input memory:
  calls `allocate(input_size)`" and "host deallocates memories: calls
  `deallocate()` for both buffers." Neither ever happened: the host writes
  input directly into the module's `memory` without calling the module's
  `allocate`, and never calls the module's `deallocate` at all (§2). That
  section, and the analogous claims in `tools/generator-template/README.md`'s
  "Memory Management" section, now match the verified call-by-call flow above.
- `examples/custom-generator/src/lib.rs`, `Cargo.toml`, and `build.sh` used to
  not implement this contract at all — they used `wasm-bindgen`, a completely
  different set of exported functions and types, and a `wasm-pack --target
  web` build that produced `custom_csil_generator.wasm` (a filename that
  would not even be discovered per §1). The crate is now a real `cdylib`
  implementing the four exports directly, building to
  `csilgen_mdsummary_generator.wasm`, with a `README.md` transcript verified
  against the actual CLI.
- Several docs under `tools/generator-template/` (its `README.md`, and the
  `go-generator`/`simple-docs` sub-examples' `README.md`s) showed
  `csilgen generate … --option key=value` on the command line. That flag has
  never existed — options come only from the CSIL source file's `options { … }`
  block (§3). Those examples now show the block instead.
