//! TypeScript code generator for CSIL specifications (WASM module).
//!
//! Dispatches on the requested target:
//! - `typescript-typesonly` -> `types.gen.ts`
//! - `typescript-client`     -> `types.gen.ts` + `client.gen.ts`
//! - `typescript-server`     -> `types.gen.ts` + `server.gen.ts`
//! - `typescript` (aggregate) -> all three

mod client;
mod codec;
mod common;
mod package;
mod server;
mod types;

use std::collections::HashSet;

#[cfg(test)]
mod tests;

use csilgen_common::{
    CsilRuleType, CsilSpecSerialized, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
};

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "typescript-code-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "TypeScript types, client, and server generator".to_string(),
        target: "typescript".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some(
            "https://github.com/catalystcommunity/csilgen/typescript-generator".to_string(),
        ),
    };

    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(_) => return std::ptr::null(),
    };

    let bytes = metadata_json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Memory deallocation (WASM export)
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Main generator function (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => return std::ptr::null_mut(),
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated_ptr.add(4), bytes.len());
            }

            allocated_ptr
        }
        Err(_code) => std::ptr::null_mut(),
    }
}

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let (files, warnings) = generate_output(&input).map_err(|_| error_codes::GENERATION_ERROR)?;

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: files.iter().map(|f| f.content.len()).sum(),
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings,
        stats,
    })
}

// A rule whose TypeScript type name equals one of these would shadow a global the
// generated code uses unqualified (`Map<K, V>`, `Promise`, ...) or collide with a
// name the generated runtime declares or imports itself, and break the package.
// Where tsc accepts the clash (`AsyncCodec`), `index.ts` re-exports only the first
// declaration of a name, so the generated one silently drops out of the package.
// Such a rule is renamed to `<Name>Type`; its wire name stays the declared name.
const RESERVED_TYPE_NAMES: &[&str] = &[
    "ApiClient",
    "AsyncApiClient",
    "AsyncCodec",
    "AsyncServiceTransport",
    "CborTag",
    "CborValue",
    "Codec",
    "CsilDecimal",
    "Cursor",
    "Date",
    "Map",
    "Promise",
    "Record",
    "RequestContext",
    "RpcReply",
    "ServerHandlers",
    "ServiceError",
    "ServiceTransport",
    "Uint8Array",
];

/// Whether the spec's own `ServiceError` record is exactly `{ code, message }`. The
/// router throws `{ code, message } satisfies ServiceError`, so that shape can keep
/// its name, and specs commonly declare it so. Any other shape is renamed.
fn service_error_fits_router(spec: &csilgen_common::CsilSpecSerialized) -> bool {
    spec.rules.iter().any(|rule| {
        rule.name == "ServiceError"
            && match &rule.rule_type {
                csilgen_common::CsilRuleType::GroupDef(group)
                | csilgen_common::CsilRuleType::TypeDef(
                    csilgen_common::CsilTypeExpression::Group(group),
                ) => {
                    let mut keys: Vec<&str> = group
                        .entries
                        .iter()
                        .filter_map(|entry| match &entry.key {
                            Some(csilgen_common::CsilGroupKey::Bare(key)) => Some(key.as_str()),
                            _ => None,
                        })
                        .collect();
                    keys.sort_unstable();
                    keys == ["code", "message"] && group.entries.len() == 2
                }
                _ => false,
            }
    })
}

/// Which files one run emits, and in which shape. Emission and the derived-name
/// reservation both read it, so a name is reserved exactly when its item is emitted.
#[derive(Debug, Clone)]
struct Surfaces {
    /// The aggregate client class name; empty when the option turns it off.
    aggregate: String,
    client: bool,
    server: bool,
    mode: common::BidiTransport,
    style: common::ClientStyle,
}

impl Surfaces {
    fn new(input: &WasmGeneratorInput) -> Result<Self, String> {
        let (client, server) = match input.config.target.as_str() {
            "typescript-typesonly" => (false, false),
            "typescript-client" => (true, false),
            "typescript-server" => (false, true),
            // "typescript" and any alias emit everything
            _ => (true, true),
        };
        // A service-less spec has nothing for `client.gen.ts`/`server.gen.ts` to
        // route to, and `types.gen.ts` only exports the synthetic `ServiceError`
        // they reference when the spec declares services, so both are skipped (as
        // Go and Python do).
        let services = common::has_services(&input.csil_spec);
        Ok(Self {
            aggregate: client::aggregate_name(input),
            client: client && services,
            server: server && services,
            mode: common::bidi_transport(input)?,
            style: common::client_style(input)?,
        })
    }

    /// Each client file to emit with its shape. `Both` (the default) ships the
    /// blocking client at `client.gen.ts` and an async twin at `client.async.gen.ts`
    /// whose symbols carry an `Async` marker so both coexist in one barrel; `Async`
    /// makes the async client a drop-in at `client.gen.ts`.
    fn client_files(&self) -> Vec<(&'static str, common::ClientShape)> {
        if !self.client {
            return Vec::new();
        }
        let sync = common::ClientShape {
            is_async: false,
            marker: "",
        };
        let async_drop_in = common::ClientShape {
            is_async: true,
            marker: "",
        };
        let async_twin = common::ClientShape {
            is_async: true,
            marker: "Async",
        };
        match self.style {
            common::ClientStyle::Sync => vec![("client.gen.ts", sync)],
            common::ClientStyle::Async => vec![("client.gen.ts", async_drop_in)],
            common::ClientStyle::Both => {
                vec![("client.gen.ts", sync), ("client.async.gen.ts", async_twin)]
            }
        }
    }
}

/// The type names the generator builds from other rules and emits for this run:
/// the per-service client, handler and wire-id names, `<ErrorArm>Refusal`, and
/// `<Enum>Numbers`. A rule that takes one of them would collide with the
/// declaration made for its owner, or hide it from the package `index.ts` (which
/// re-exports a name only once). A name whose item is not emitted is not reserved,
/// so a rule is never renamed for a clash that cannot happen.
fn derived_type_names(spec: &CsilSpecSerialized, surfaces: &Surfaces) -> HashSet<String> {
    let mut names = HashSet::new();
    let channels = surfaces.mode == common::BidiTransport::Connection;
    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::ServiceDef(def) => {
                let base = common::service_base(&rule.name);
                for (_, shape) in surfaces.client_files() {
                    if client::service_class_has_methods(def, surfaces.mode) {
                        names.insert(shape.class_name(&base));
                    }
                    if channels && common::service_has_channel_ops(def) {
                        names.insert(client::channel_handlers_name(&base, shape));
                    }
                }
                if surfaces.server {
                    names.insert(server::handlers_name(&rule.name));
                    if def.wire_id.is_some() {
                        names.insert(server::wire_ids_name(&rule.name));
                    }
                    if channels && common::service_has_channel_ops(def) {
                        names.insert(server::channel_handlers_name(&base));
                    }
                }
            }
            _ if csilgen_common::rule_enum_numbers(rule).is_some() => {
                names.insert(format!("{}Numbers", common::to_pascal(&rule.name)));
            }
            _ => {}
        }
    }
    for (_, _, arms) in spec.rpc_operations() {
        for arm in &arms[1..] {
            if let Some(name) = arm.rpc_variant_name() {
                names.insert(codec::refusal_name(name));
            }
        }
    }
    names
}

/// Give each later service whose `<Base>` (its PascalCase name without a trailing
/// `Service`) is taken a digit suffix until it is free, as the shared collision
/// pass does. That pass compares full PascalCase names, so it keeps `Foo` and
/// `FooService` apart, but both derive `FooClient` here.
fn rename_service_bases(spec: &CsilSpecSerialized, surfaces: &Surfaces) -> CsilSpecSerialized {
    let mut names: HashSet<String> = spec.rules.iter().map(|rule| rule.name.clone()).collect();
    // The aggregate client class (`ApiClient` by default, `AsyncApiClient` for the
    // twin) shares the client file with the per-service classes, so a service `Api`
    // would declare `ApiClient` twice.
    let mut bases: HashSet<String> = surfaces
        .client_files()
        .iter()
        .filter_map(|(_, shape)| {
            let aggregate = shape.aggregate_name(&surfaces.aggregate);
            aggregate
                .strip_suffix(&shape.class_name(""))
                .map(str::to_string)
        })
        .collect();
    let reserved = bases.clone();
    let mut renames = std::collections::HashMap::new();
    for rule in &spec.rules {
        if !matches!(rule.rule_type, CsilRuleType::ServiceDef(_)) {
            continue;
        }
        let base = common::service_base(&rule.name);
        if !reserved.contains(&base) && bases.insert(base) {
            continue;
        }
        let mut n = 2;
        let mut candidate = format!("{}{n}", rule.name);
        while names.contains(&candidate) || bases.contains(&common::service_base(&candidate)) {
            n += 1;
            candidate = format!("{}{n}", rule.name);
        }
        names.insert(candidate.clone());
        bases.insert(common::service_base(&candidate));
        renames.insert(rule.name.clone(), candidate);
    }
    csilgen_common::rename_rules(spec, &renames)
}

/// Give each operation input or output that is an inline record a named record
/// rule, `<Service><Op>Input` or `<Service><Op>Output` as the shared union pass
/// names its rules. The shared hoister leaves op boundaries alone, and a bare
/// inline group has no TypeScript codec: the client typed it `object` and handed
/// it to `encodeValue`, which does not encode a plain object. A named record encodes
/// as the same CBOR map, so the wire does not change.
fn hoist_record_op_boundaries(spec: &CsilSpecSerialized) -> CsilSpecSerialized {
    let canonical = |name: &str| name.replace(['-', '_'], "").to_lowercase();
    let mut used: HashSet<String> = spec.rules.iter().map(|r| canonical(&r.name)).collect();
    let mut synth = Vec::new();
    let mut rules = Vec::with_capacity(spec.rules.len());
    for rule in &spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            rules.push(rule.clone());
            continue;
        };
        let mut service = service.clone();
        for op in &mut service.operations {
            for (boundary, suffix) in [
                (&mut op.input_type, "Input"),
                (&mut op.output_type, "Output"),
            ] {
                if !matches!(boundary, csilgen_common::CsilTypeExpression::Group(_)) {
                    continue;
                }
                let base = format!(
                    "{}{}{suffix}",
                    common::to_pascal(&rule.name),
                    common::to_pascal(&op.name)
                );
                let mut name = base.clone();
                let mut n = 2;
                while used.contains(&canonical(&name)) {
                    name = format!("{base}{n}");
                    n += 1;
                }
                used.insert(canonical(&name));
                synth.push(csilgen_common::CsilRule {
                    name: name.clone(),
                    rule_type: CsilRuleType::TypeDef(boundary.clone()),
                    position: op.position.clone(),
                    doc_comments: Vec::new(),
                });
                *boundary = csilgen_common::CsilTypeExpression::Reference(name);
            }
        }
        rules.push(csilgen_common::CsilRule {
            rule_type: CsilRuleType::ServiceDef(service),
            ..rule.clone()
        });
    }
    rules.extend(synth);
    CsilSpecSerialized {
        rules,
        ..spec.clone()
    }
}

/// Rename every rule whose TypeScript type name is reserved, derived from another
/// rule, or equal to an earlier rule's type name. A rename changes the names
/// derived from the renamed rule (its `Refusal` class), so it repeats until no
/// rule moves.
fn rename_spec(
    spec: &CsilSpecSerialized,
    decimal: common::DecimalMapping,
    surfaces: &Surfaces,
) -> CsilSpecSerialized {
    // The library mapping imports `Decimal` from `decimal.js`, so a rule of that
    // name would clash with it.
    let library_decimal = matches!(decimal, common::DecimalMapping::Library);
    let keep_service_error = service_error_fits_router(spec);
    let mut spec = spec.clone();
    loop {
        let before = spec.declared_names.len();
        let derived = derived_type_names(&spec, surfaces);
        spec = csilgen_common::rename_reserved(&spec, |name| {
            let ident = common::to_pascal(name);
            if ident == "ServiceError" {
                return !keep_service_error;
            }
            (library_decimal && ident == "Decimal")
                || RESERVED_TYPE_NAMES.contains(&ident.as_str())
                || derived.contains(&ident)
        });
        spec = csilgen_common::rename_ident_collisions(&spec, common::to_pascal);
        spec = rename_service_bases(&spec, surfaces);
        if spec.declared_names.len() == before {
            return spec;
        }
    }
}

/// Produce the set of files appropriate for the requested target. The client
/// and server outputs always travel with the types file they import from.
///
/// Returns `Err(message)` on misconfiguration (e.g. an invalid
/// `ts_bidirectional_transport` value) so the wasm boundary surfaces a clean
/// generation failure instead of silently degrading.
pub fn generate_files(input: &WasmGeneratorInput) -> Result<Vec<GeneratedFile>, String> {
    generate_output(input).map(|(files, _)| files)
}

/// `generate_files`, plus one warning for each rule renamed on the way.
fn generate_output(
    input: &WasmGeneratorInput,
) -> Result<(Vec<GeneratedFile>, Vec<GeneratorWarning>), String> {
    // Validate options *before* deciding which files to emit so a bad option
    // fails the entire run, regardless of which target is requested.
    let surfaces = Surfaces::new(input)?;
    let decimal = common::decimal_mapping(input)?;
    let _import_extension = common::import_extension(input)?;

    // Inline (anonymous) composites become synthesized named rules so every emitter
    // sees ordinary references; a spec without them is reproduced unchanged. A
    // mixed op boundary becomes a named union first, so the client and server expose
    // every arm. An all-literal choice stays inline: the codec renders a bare-literal
    // enum for it in the field position itself. The renames run on the result, so a
    // synthesized name that clashes is renamed and warned like a declared one; the
    // synthesized rules come last, so they move before a user rule does.
    let hoisted = csilgen_common::hoist_inline_composites(
        &hoist_record_op_boundaries(&csilgen_common::hoist_op_union_boundaries(&input.csil_spec)),
        csilgen_common::HoistOptions {
            hoist_all_literal_choices: false,
        },
    );
    let renamed = rename_spec(&hoisted, decimal, &surfaces);
    let warnings = csilgen_common::rename_warnings(&renamed, &input.config.target);
    let input = &WasmGeneratorInput {
        csil_spec: renamed,
        ..input.clone()
    };
    let mut files = vec![GeneratedFile {
        path: "types.gen.ts".to_string(),
        content: types::generate(input),
    }];

    // The codec carries every record's wire (de)serializers. It rides alongside the
    // types for every surface (a typesonly consumer still gets usable codecs, and the
    // client imports its `to<T>Cbor`/`from<T>Cbor` directly).
    if let Some(content) = codec::generate(input) {
        files.push(GeneratedFile {
            path: "codec.gen.ts".to_string(),
            content,
        });
    }

    for (path, shape) in surfaces.client_files() {
        files.push(GeneratedFile {
            path: path.to_string(),
            content: client::generate(input, shape)?,
        });
    }
    if surfaces.server {
        files.push(GeneratedFile {
            path: "server.gen.ts".to_string(),
            content: server::generate(input)?,
        });
    }

    // When opted in, turn the output directory into a publishable npm package by
    // appending a barrel + package.json + tsconfig.json. Default output is unchanged.
    package::augment(input, &mut files);

    Ok((files, warnings))
}
