//! Swift code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target swift` from `csilgen_swift_generator.wasm`.
//! Emits idiomatic Swift: `struct` records, `enum` (associated values) for variant
//! choices, a `protocol` server seam, typed client structs, and verbose/compact
//! channel routers. Identifiers are camel/Pascal-cased for Swift while every wire
//! string (service / op / event / field key) stays verbatim, so a Swift peer agrees
//! byte-for-byte with the Rust/Go/Python/TypeScript clients.

use convert_case::{Case, Casing};
use csilgen_common::{
    ChoiceClass, CsilControlOperator, CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression,
    CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition,
    CsilServiceDirection, CsilServiceOperation, CsilSizeConstraint, CsilTypeExpression,
    CsilValidationConstraint, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, WasmGeneratorInput, WasmGeneratorOutput,
    choice_arm_literal, wasm_interface::*,
};

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "swift-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Swift code generator with service support".to_string(),
        target: "swift".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen".to_string()),
    };
    write_json(&metadata) as *const u8
}

#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    match process_generation(input_ptr, input_len) {
        Ok(output) => write_json(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn write_json<T: serde::Serialize>(value: &T) -> *mut u8 {
    let json = match serde_json::to_string(value) {
        Ok(j) => j,
        Err(_) => return std::ptr::null_mut(),
    };
    let bytes = json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::write(ptr as *mut u32, bytes.len() as u32);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }
    ptr
}

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = build_files(&input)?;
    let total_size: usize = files.iter().map(|f| f.content.len()).sum();
    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: total_size,
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };
    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::<GeneratorWarning>::new(),
        stats,
    })
}

/// Which service surface a (sub-)target asks for. The base `swift` target emits the
/// server handler protocol + routers; the explicit sub-targets narrow that.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

/// Which client surface(s) to emit. Only the transport seam (the byte carrier that
/// performs the network round-trip) and the per-method signatures turn `async`; the
/// generated codec never does I/O and stays synchronous. `Both` is the default so every
/// consumer keeps their blocking client and gains an `async` twin for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStyle {
    /// Blocking-only client at `Client.swift`. The host owns its own I/O loop.
    Sync,
    /// `async` client, a drop-in at `Client.swift` with the canonical symbol names
    /// (just `async`). For hosts whose carrier suspends.
    Async,
    /// Emit both — the blocking client at `Client.swift` plus an `async` twin at
    /// `ClientAsync.swift` whose symbols carry an `Async` marker so the two coexist in
    /// one module without name collisions. Default.
    Both,
}

/// Read & validate `client_style` from the generation options. Any value other than
/// `sync`/`async`/`both` is rejected so misconfiguration fails at generation time
/// instead of silently degrading; absent defaults to `Both`. Returns a message that
/// names the offending option, mirroring how a typed-enum option is validated.
fn client_style(
    options: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<ClientStyle, String> {
    match options.get("client_style") {
        None => Ok(ClientStyle::Both),
        Some(v) => match v.as_str() {
            Some("sync") => Ok(ClientStyle::Sync),
            Some("async") => Ok(ClientStyle::Async),
            Some("both") => Ok(ClientStyle::Both),
            Some(other) => Err(format!(
                "client_style must be \"sync\", \"async\", or \"both\", got {other:?}"
            )),
            None => Err(format!("client_style must be a string, got {v:?}")),
        },
    }
}

/// The shape of one emitted client file: whether its methods/seam are `async` and the
/// symbol marker that keeps an `async` twin distinct from the blocking client when both
/// land in the same module. `marker` is empty for a stand-alone client (sync, or
/// async-as-drop-in) and `"Async"` for the twin in `Both` mode.
#[derive(Debug, Clone, Copy)]
struct ClientShape {
    is_async: bool,
    marker: &'static str,
}

impl ClientShape {
    /// The effect clause for a method/seam signature: `async throws` when async, else
    /// `throws` — matching the blocking client's existing error idiom.
    fn effects(&self) -> &'static str {
        if self.is_async {
            "async throws"
        } else {
            "throws"
        }
    }

    /// `await ` keyword (trailing space) for the seam call site, else empty.
    fn await_kw(&self) -> &'static str {
        if self.is_async { "await " } else { "" }
    }

    /// The byte-transport protocol name (`CsilTransport`, or `AsyncCsilTransport` for the
    /// twin), per the `Async{TransportName}` marker convention.
    fn transport_name(&self) -> String {
        format!("{}CsilTransport", self.marker)
    }

    /// A per-service client struct name (`FooClient`, or `FooAsyncClient` for the twin).
    fn client_name(&self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }
}

fn build_files(input: &WasmGeneratorInput) -> Result<Vec<GeneratedFile>, i32> {
    let surface = match input.config.target.as_str() {
        "swift" | "swift-server" => Surface::Server,
        "swift-client" => Surface::Client,
        "swift-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    // Validate `client_style` early so a bad value fails the whole run regardless of the
    // requested surface, mirroring the TypeScript generator's option validation.
    let style = client_style(&input.config.options).map_err(|_| error_codes::GENERATION_ERROR)?;

    // Rewrite inline (anonymous) composite field types to synthesized named rules so
    // every emitter below sees them as ordinary references and routes them through the
    // named-rule machinery (types, codec, client, services all read from this one
    // hoisted view). `hoist_all_literal_choices: true` because this generator's own
    // pre-shared-classifier hoister (`inline_choice`) hoisted EVERY inline choice, all-
    // literal or not — Swift has no other route for an inline choice in a field
    // position, so leaving one inline was never an option here (unlike TypeScript,
    // which renders a bare-literal enum in place). See
    // `csilgen_common::hoist_inline_composites`.
    let hoisted = {
        let mut cloned = input.clone();
        cloned.csil_spec = csilgen_common::hoist_inline_composites(
            &input.csil_spec,
            csilgen_common::HoistOptions {
                hoist_all_literal_choices: true,
            },
        );
        cloned
    };
    let input = &hoisted;

    let mut files = Vec::new();

    if let Some(types) = generate_types(input) {
        files.push(GeneratedFile {
            path: "Types.swift".to_string(),
            content: types,
        });
    }

    // Per-type CBOR (de)serializers make the records usable over the wire without a
    // hand-written codec; the typed client calls them.
    if let Some(codec) = generate_codec(input) {
        files.push(GeneratedFile {
            path: "Codec.swift".to_string(),
            content: codec,
        });
    }

    if input.csil_spec.service_count > 0 {
        // A package's `genquickstart.md` demonstrates the calling side (the CSIL-RPC and
        // CSIL-Datagrams sections, over the typed client) AND the handling side (the
        // CSIL-Events section, over the channel router + handler protocol), so a package
        // must carry BOTH surfaces for its own quickstart to compile — regardless of which
        // (sub-)target was requested. A flat (non-package) build stays byte-identical: it
        // emits only the requested surface. Mirrors the OCaml generator.
        let pkg_mode = emit_packages_includes(&input.config.options, "swift");
        let want_client = matches!(surface, Surface::Client)
            || (pkg_mode && !matches!(surface, Surface::TypesOnly));
        let want_server = matches!(surface, Surface::Server)
            || (pkg_mode && !matches!(surface, Surface::TypesOnly));

        if want_client {
            // `Both` (the default) ships the blocking client at `Client.swift` and an
            // `async` twin at `ClientAsync.swift`; `Async` makes the `async` client a
            // drop-in at `Client.swift` (canonical names); `Sync` is today's output.
            let sync = ClientShape {
                is_async: false,
                marker: "",
            };
            let async_drop_in = ClientShape {
                is_async: true,
                marker: "",
            };
            let async_twin = ClientShape {
                is_async: true,
                marker: "Async",
            };
            match style {
                ClientStyle::Sync => {
                    if let Some(client) = generate_client(input, sync) {
                        files.push(GeneratedFile {
                            path: "Client.swift".to_string(),
                            content: client,
                        });
                    }
                }
                ClientStyle::Async => {
                    if let Some(client) = generate_client(input, async_drop_in) {
                        files.push(GeneratedFile {
                            path: "Client.swift".to_string(),
                            content: client,
                        });
                    }
                }
                ClientStyle::Both => {
                    if let Some(client) = generate_client(input, sync) {
                        files.push(GeneratedFile {
                            path: "Client.swift".to_string(),
                            content: client,
                        });
                    }
                    if let Some(client) = generate_client(input, async_twin) {
                        files.push(GeneratedFile {
                            path: "ClientAsync.swift".to_string(),
                            content: client,
                        });
                    }
                }
            }
        }

        if want_server && let Some(services) = generate_services(input) {
            files.push(GeneratedFile {
                path: "Services.swift".to_string(),
                content: services,
            });
        }
    }

    // A self-contained Swift Package Manager package is opt-in: only when
    // `emit_packages` names "swift" do we relocate sources into SwiftPM's required
    // `Sources/<Target>/` layout and add a manifest; otherwise output is unchanged.
    if let Some(pkg) = SwiftPackage::from_config(input) {
        files = wrap_as_package(files, &pkg);
        // The README sits at the package root (beside Package.swift), not under
        // Sources/, so it is not a compiled source. It is opt-out: an explicit
        // `emit_readme: false` suppresses it, while an absent, non-bool, or `true`
        // value keeps the default emission.
        if emit_readme_enabled(&input.config.options) {
            files.push(GeneratedFile {
                path: "genquickstart.md".to_string(),
                content: swift_readme(input, &pkg),
            });
        }
    }

    Ok(files)
}

/// The resolved coordinates of a self-contained SwiftPM package. Present only when
/// the consumer asked for one via `emit_packages` containing `"swift"`.
struct SwiftPackage {
    /// The SwiftPM package's `name:` (and library product name).
    name: String,
    /// The single target name; a valid Swift identifier, so PascalCased.
    target: String,
    /// Informational: SwiftPM publishes the version a git tag carries, so this only
    /// feeds the manifest comment rather than any build setting.
    version: String,
}

impl SwiftPackage {
    /// Resolve package coordinates from the generator config, or `None` when package
    /// mode was not requested. Parses `emit_packages` defensively: anything that is not
    /// a JSON array of strings containing `"swift"` leaves package mode off.
    fn from_config(input: &WasmGeneratorInput) -> Option<Self> {
        if !emit_packages_includes(&input.config.options, "swift") {
            return None;
        }
        // A path-style `package_name` is the cross-ecosystem source of truth; the Swift
        // package name wants only its tail. See `package_name_last_segment`.
        let name = option_str(&input.config.options, "package_name")
            .map(|name| csilgen_common::package_name_last_segment(name).to_string())
            .unwrap_or_else(|| "CsilgenClient".to_string());
        let target = swift_type_name(&name);
        let version = option_str(&input.config.options, "package_version")
            .map(str::to_string)
            .unwrap_or_else(|| "0.1.0".to_string());
        Some(SwiftPackage {
            name,
            target,
            version,
        })
    }
}

/// A non-empty string option, or `None` for a missing / non-string / empty value.
fn option_str<'a>(
    options: &'a std::collections::HashMap<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    options
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether `emit_packages` is a JSON array that contains the string `lang`. Defensive
/// by construction: a missing key, a non-array value, or non-string elements simply do
/// not match, leaving package mode off rather than erroring.
fn emit_packages_includes(
    options: &std::collections::HashMap<String, serde_json::Value>,
    lang: &str,
) -> bool {
    options
        .get("emit_packages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(lang)))
}

/// Whether the package README should be emitted. Only an explicit `emit_readme: false`
/// opts out; any other value (absent, non-bool, or `true`) keeps the README.
fn emit_readme_enabled(options: &std::collections::HashMap<String, serde_json::Value>) -> bool {
    options.get("emit_readme").and_then(|v| v.as_bool()) != Some(false)
}

/// Relocate the generated `.swift` sources under `Sources/<Target>/` (SwiftPM's
/// required layout) and prepend the `Package.swift` manifest. Files that are already
/// the manifest are left at the package root.
fn wrap_as_package(files: Vec<GeneratedFile>, pkg: &SwiftPackage) -> Vec<GeneratedFile> {
    let mut out = Vec::with_capacity(files.len() + 1);
    out.push(GeneratedFile {
        path: "Package.swift".to_string(),
        content: package_manifest(pkg),
    });
    for file in files {
        out.push(GeneratedFile {
            path: format!("Sources/{}/{}", pkg.target, file.path),
            content: file.content,
        });
    }
    out
}

/// The `Package.swift` manifest: a single library product and target named for the
/// package, no dependencies. Pinned to swift-tools-version 5.9.
fn package_manifest(pkg: &SwiftPackage) -> String {
    let SwiftPackage {
        name,
        target,
        version,
    } = pkg;
    let name_lit = swift_string_lit(name);
    let target_lit = swift_string_lit(target);
    format!(
        "// swift-tools-version:5.9\n\
// {name} {version} — generated by csilgen; DO NOT EDIT.\n\
// SwiftPM derives a published version from the package's git tag, so the version\n\
// above is informational only.\n\
import PackageDescription\n\
\n\
let package = Package(\n\
    name: {name_lit},\n\
    products: [\n\
        .library(name: {name_lit}, targets: [{target_lit}]),\n\
    ],\n\
    targets: [\n\
        .target(name: {target_lit}),\n\
    ]\n\
)\n"
    )
}

// ---------------------------------------------------------------------------
// Package README — 3-transport Quickstart (CSIL-RPC / Events / Datagrams)
// ---------------------------------------------------------------------------

/// Which of the three transport sections to render. The `genquickstart_transports`
/// option is a JSON array subset of `["rpc","events","datagrams"]`; unknown entries are
/// ignored, and an absent or all-unknown value means "all three" so the document always
/// renders something coherent.
fn wanted_transports(
    options: &std::collections::HashMap<String, serde_json::Value>,
) -> (bool, bool, bool) {
    let Some(items) = options
        .get("genquickstart_transports")
        .and_then(|v| v.as_array())
    else {
        return (true, true, true);
    };
    let names: std::collections::BTreeSet<&str> = items.iter().filter_map(|v| v.as_str()).collect();
    let any_known = ["rpc", "events", "datagrams"]
        .iter()
        .any(|t| names.contains(t));
    if any_known {
        (
            names.contains("rpc"),
            names.contains("events"),
            names.contains("datagrams"),
        )
    } else {
        (true, true, true)
    }
}

/// The package README: a transport-by-transport Quickstart over the official
/// `CsilgenTransport` library. The generated codec owns CBOR (de)serialization and the
/// library owns the envelope/framing/lifecycle; the consumer supplies only a *carrier*
/// that moves bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events over TLS,
/// CSIL-Datagrams over UDP) is a complete, copy-paste example built on the library.
fn swift_readme(input: &WasmGeneratorInput, pkg: &SwiftPackage) -> String {
    let module = &pkg.target;
    let name = &pkg.name;
    let mut out = format!(
        "# {name}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the `CsilgenTransport` library owns the envelope, framing,\n\
         and connection lifecycle. You supply only a *carrier* that moves bytes, so the\n\
         same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw UDP unchanged.\n\n\
         ## Consume\n\n\
         Add this package and the transport library to your `Package.swift`. The transport\n\
         lib is not yet published, so depend on it by local path for now:\n\n\
         ```swift\n\
         // TODO: point at the published git URL + version once tagged.\n\
         .package(path: \"./{name}\"),\n\
         .package(path: \"../csilgen/transports/swift\"),\n\
         ```\n\n\
         and list both products in your target's `dependencies`: `\"{module}\"` and\n\
         `.product(name: \"CsilgenTransport\", package: \"swift\")`.\n\n"
    );

    let (rpc, events, datagrams) = wanted_transports(&input.config.options);
    let unary = first_swift_unary_example(input);
    let channel = first_swift_channel_example(input);
    if rpc {
        out.push_str(&swift_rpc_section(module, unary.as_ref()));
    }
    if events {
        out.push_str(&swift_events_section(module, channel.as_ref()));
    }
    if datagrams {
        out.push_str(&swift_datagrams_section(module, unary.as_ref()));
    }
    out
}

/// The pieces a unary (`->`) example needs: the sync client struct to construct, the
/// method to call, a compiling sample request literal (`None` for a request-less op),
/// the request/response record type names (so the datagram section can name them), and
/// the op's datagram ordinal.
struct SwiftUnaryExample {
    client_struct: String,
    method: String,
    sample: Option<String>,
    req_type: Option<String>,
    res_type: String,
    op_ord: u64,
}

/// The first service (declared order) with a unary op whose success type — and, when
/// present, request type — is a record the generated codec covers, so the example can
/// call the clean typed sync client form. `None` for a serviceless / non-record-op spec.
fn first_swift_unary_example(input: &WasmGeneratorInput) -> Option<SwiftUnaryExample> {
    let records = swift_record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            if !is_record_ref(&success, &records) {
                continue;
            }
            let null_in = is_null_input(&op.input_type);
            if !null_in && !is_record_ref(&op.input_type, &records) {
                continue;
            }
            let (sample, req_type) = if null_in {
                (None, None)
            } else if let CsilTypeExpression::Reference(name) = &op.input_type {
                (
                    Some(swift_record_literal(
                        input,
                        name,
                        swift_find_record(input, name)?,
                    )),
                    Some(swift_type_name(name)),
                )
            } else {
                (None, None)
            };
            if !null_in && sample.is_none() {
                continue;
            }
            let CsilTypeExpression::Reference(res_name) = &success else {
                continue;
            };
            return Some(SwiftUnaryExample {
                // The blocking client is the unmarked `<Base>Client` (sync shape).
                client_struct: format!("{}Client", service_base(&rule.name)),
                method: swift_ident(&op.name),
                sample,
                req_type,
                res_type: swift_type_name(res_name),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.unwrap_or(1),
            });
        }
    }
    None
}

/// The pieces the Events session needs: the generated handler protocol + channel router +
/// outbound encoder names, the inbound (op input) and outbound (op success output) record
/// type names, the handler method name, the outbound sample literal, and the wire service.
struct SwiftChannelExample {
    handler_protocol: String,
    service_wire: String,
    route_fn: String,
    encode_fn: String,
    handler_method: String,
    inbound_type: String,
    outbound_type: String,
    outbound_sample: String,
}

/// The first service (declared order) with a `<->` op whose input and success output are
/// both records (so the generated router, encoder, and per-type codec helpers exist).
/// `None` when no service has a usable channel op — the Events section then shows the
/// handshake/heartbeat without dispatch wiring.
fn first_swift_channel_example(input: &WasmGeneratorInput) -> Option<SwiftChannelExample> {
    let records = swift_record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            if !is_record_ref(&success, &records) || !is_record_ref(&op.input_type, &records) {
                continue;
            }
            // The verbose router decodes the op INPUT and calls the handler; the encoder
            // produces the op success OUTPUT. Both must be named records for a sample.
            let (CsilTypeExpression::Reference(in_name), CsilTypeExpression::Reference(out_name)) =
                (&op.input_type, &success)
            else {
                continue;
            };
            let type_name = swift_type_name(&rule.name);
            let method_pascal = swift_type_name(&op.name);
            return Some(SwiftChannelExample {
                handler_protocol: type_name.clone(),
                service_wire: rule.name.clone(),
                route_fn: format!("route{type_name}Channel"),
                encode_fn: format!("encode{type_name}{method_pascal}"),
                handler_method: swift_ident(&op.name),
                inbound_type: swift_type_name(in_name),
                outbound_type: swift_type_name(out_name),
                outbound_sample: swift_record_literal(
                    input,
                    out_name,
                    swift_find_record(input, out_name)?,
                ),
            });
        }
    }
    None
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `CsilTransport` byte seam that
/// builds/parses the envelope with the library's `RpcRequest`/`RpcResponse` (never hand-
/// rolled) and POSTs it to `{baseURL}/csil/v1/rpc`. The typed client decodes the success
/// payload; a non-zero transport status and the `ServiceError` arm are surfaced distinctly.
fn swift_rpc_section(module: &str, ex: Option<&SwiftUnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`); you\n\
         bring a carrier that moves bytes. The `URLSession` carrier below is just one\n\
         example — it implements the generated `CsilTransport` byte seam, so any HTTP\n\
         client drops in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```swift\n");
    out.push_str("import Foundation\n");
    out.push_str(&format!("import {module}\n"));
    out.push_str("import CsilgenTransport\n\n");
    out.push_str(SWIFT_RPC_CARRIER);
    out.push('\n');
    out.push_str(&format!(
        "let client = {}(transport: HttpRpcCarrier(baseURL: \"http://localhost:5080\"))\n",
        ex.client_struct
    ));
    match &ex.sample {
        Some(literal) => out.push_str(&format!(
            "let result = try client.{}({literal})\n",
            ex.method
        )),
        None => out.push_str(&format!("let result = try client.{}()\n", ex.method)),
    }
    out.push_str("print(result)\n");
    out.push_str("```\n\n");
    out
}

/// CSIL-Events over TLS: a full session example. Wraps a TLS `ByteStream`
/// (Network.framework `NWConnection`) in the library's `StreamCarrier` (length-prefix
/// framing), performs the `$hello`/`$hello-ack` handshake, sends one outbound event via
/// the generated `encode<Service><Op>`, and runs a recv loop that decodes each frame to an
/// `Event`, answers `$ping` with `$pong`, and dispatches typed events to the generated
/// `route<Service>Channel`. With no channel op the dispatch wiring becomes a note.
fn swift_events_section(module: &str, ch: Option<&SwiftChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns\n\
         the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and length-\n\
         prefix framing (`StreamCarrier` over a `ByteStream`); the generated router\n\
         dispatches typed events. The TLS carrier below (Network.framework `NWConnection`)\n\
         is just one example — a WebSocket/QUIC `ByteStream` drops in unchanged.\n\n",
    );
    out.push_str("```swift\n");
    out.push_str("import Foundation\n");
    out.push_str("import Network\n");
    out.push_str(&format!("import {module}\n"));
    out.push_str("import CsilgenTransport\n\n");
    out.push_str(SWIFT_EVENTS_CARRIER);
    out.push('\n');
    match ch {
        Some(ch) => out.push_str(&swift_events_session(ch)),
        None => out.push_str(SWIFT_EVENTS_NO_CHANNEL_SESSION),
    }
    out.push_str("```\n\n");
    out
}

/// The channel session body for an Events connection that has a `<->` op: a `CsilCodec`
/// backed by the op's generated per-type helpers, the handshake, one outbound event via
/// the generated encoder, and the recv loop that heartbeats and dispatches into the
/// generated router.
fn swift_events_session(ch: &SwiftChannelExample) -> String {
    format!(
        r#"
// A CsilCodec backed by the generated per-type helpers (inbound {inbound}, outbound
// {outbound}). The generated router uses it to (de)serialize channel payloads.
struct ChannelCodec: CsilCodec {{
    func encode<T>(_ value: T) throws -> [UInt8] {{
        if let v = value as? {outbound} {{ return v.toCbor() }}
        throw CsilCborError.typeMismatch
    }}
    func decode<T>(_ data: [UInt8], as type: T.Type) throws -> T {{
        if type == {inbound}.self {{ return try {inbound}.fromCbor(data) as! T }}
        throw CsilCborError.typeMismatch
    }}
}}

// The generated handler seam; dispatch lands here. Implement every op of the service.
struct Handler: {handler} {{
    func {method}(_ msg: {inbound}) throws {{
        print("event {method}", msg)
    }}
    // ... implement the service's remaining operations.
}}

func session() throws {{
    // StreamCarrier wraps any ByteStream with the library's 4-byte length-prefix framing.
    // The max-frame guard is a carrier setting, not a generated constant: raise maxFrame
    // when a peer accepts payloads larger than the 16 MiB default (the envelope adds
    // framing and request metadata around the payload, so the limit must exceed the
    // largest payload), or lower it to harden an exposed listener. Valid limits are
    // 1...maxFrameLimit and are checked at construction.
    let carrier = try StreamCarrier(
        stream: TlsByteStream(host: "localhost", port: 7443), maxFrame: maxFrameDefault)
    let codec = ChannelCodec()
    let handler = Handler()

    // $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the wire
    // profile for the connection's lifetime.
    try carrier.sendFrame(
        Hello(versions: [csilVersion], profiles: ["verbose"], service: "{service}").encode())
    guard let ackFrame = try carrier.recvFrame() else {{
        throw TransportError.carrier("connection closed during handshake")
    }}
    let ack = try HelloAck.decode(ackFrame)
    guard let profile = Profile.parse(ack.profile) else {{
        throw TransportError.malformed("peer chose an unknown profile")
    }}

    // Send one outbound event via the generated encoder, framed as a verbose Event.
    let out = try {encode}(codec: codec, msg: {sample})
    try carrier.sendFrame(
        Event.verbose(service: "{service}", event: out.op, payload: out.data).encode(profile))

    // Recv loop: decode each frame to an Event, answer $ping with $pong, dispatch the rest
    // to the generated router.
    while let frame = try carrier.recvFrame() {{
        let ev = try Event.decode(frame, profile)
        if ev.event == Control.pingName {{
            let ping = try Heartbeat.decode(ev.payload)
            try carrier.sendFrame(
                Event.verbose(
                    service: nil, event: Control.pongName,
                    payload: Heartbeat(nonce: ping.nonce).encode()
                ).encode(profile))
            continue
        }}
        try {route}(handler, codec: codec, op: ev.event!, data: ev.payload)
    }}
}}

try session()
"#,
        inbound = ch.inbound_type,
        outbound = ch.outbound_type,
        handler = ch.handler_protocol,
        method = ch.handler_method,
        service = ch.service_wire,
        encode = ch.encode_fn,
        sample = ch.outbound_sample,
        route = ch.route_fn,
    )
}

/// The Events session body when the spec declares no `<->` op: the handshake and heartbeat
/// still apply, so they are shown, with a note where the dispatch would go.
const SWIFT_EVENTS_NO_CHANNEL_SESSION: &str = r#"
func session() throws {
    // StreamCarrier wraps any ByteStream with the library's 4-byte length-prefix framing.
    // The max-frame guard is a carrier setting, not a generated constant: raise maxFrame
    // when a peer accepts payloads larger than the 16 MiB default (the envelope adds
    // framing and request metadata around the payload, so the limit must exceed the
    // largest payload), or lower it to harden an exposed listener. Valid limits are
    // 1...maxFrameLimit and are checked at construction.
    let carrier = try StreamCarrier(
        stream: TlsByteStream(host: "localhost", port: 7443), maxFrame: maxFrameDefault)

    // $hello / $hello-ack handshake (control plane).
    try carrier.sendFrame(Hello(versions: [csilVersion], profiles: ["verbose"]).encode())
    guard let ackFrame = try carrier.recvFrame() else {
        throw TransportError.carrier("connection closed during handshake")
    }
    let ack = try HelloAck.decode(ackFrame)
    guard let profile = Profile.parse(ack.profile) else {
        throw TransportError.malformed("peer chose an unknown profile")
    }

    // Recv loop: answer $ping with $pong. This package declares no <->/<- operations, so
    // there is no generated channel router to dispatch typed events into.
    while let frame = try carrier.recvFrame() {
        let ev = try Event.decode(frame, profile)
        if ev.event == Control.pingName {
            let ping = try Heartbeat.decode(ev.payload)
            try carrier.sendFrame(
                Event.verbose(
                    service: nil, event: Control.pongName,
                    payload: Heartbeat(nonce: ping.nonce).encode()
                ).encode(profile))
        }
    }
}

try session()
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in the
/// library's `Datagram`, and `sendDatagram` it fire-and-forget. The recv path
/// `Datagram.decode`s an inbound datagram and decodes its payload with the generated codec
/// into the RESPONSE type — there is NO synchronous response.
fn swift_datagrams_section(module: &str, ex: Option<&SwiftUnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram`\n\
         envelope; you bring a datagram carrier. The UDP carrier below (Network.framework\n\
         `NWConnection`) is one example — QUIC datagrams or a WebRTC channel drop in\n\
         unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_type), Some(sample)) = (&ex.req_type, &ex.sample) else {
        out.push_str(
            "This package's `->` operations take no request, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    out.push_str("```swift\n");
    out.push_str("import Foundation\n");
    out.push_str("import Network\n");
    out.push_str(&format!("import {module}\n"));
    out.push_str("import CsilgenTransport\n\n");
    out.push_str(SWIFT_DATAGRAMS_CARRIER);
    out.push('\n');
    out.push_str(&format!(
        r#"// The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
let opOrd: UInt64 = {op_ord}

func main() throws {{
    let carrier = UdpDatagramCarrier(host: "localhost", port: 9000)

    // Fire-and-forget: encode the `->` request and send it. seq 0 marks an unsequenced
    // datagram.
    let req: {req_type} = {sample}
    try carrier.sendDatagram(Datagram(opOrd: opOrd, seq: 0, payload: req.toCbor()).encode())

    // Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is NO
    // synchronous response; the caller must tolerate loss and reordering and handle a reply
    // whenever (if ever) it shows up.
    if let inbound = try carrier.recvDatagram() {{
        let dg = try Datagram.decode(inbound)
        let resp = try {res_type}.fromCbor(dg.payload)
        print("late response", resp)
    }}
}}

try main()
"#,
        op_ord = ex.op_ord,
        req_type = req_type,
        sample = sample,
        res_type = ex.res_type,
    ));
    out.push_str("```\n\n");
    out
}

/// The record a type reference names, if any — both `Name = { .. }` (`TypeDef(Group)`)
/// and a bare group rule (`GroupDef`) are records.
fn swift_find_record<'a>(
    input: &'a WasmGeneratorInput,
    name: &str,
) -> Option<&'a CsilGroupExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter(|r| r.name == name)
        .find_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        })
}

/// `TypeName(label: <sample>, ...)` over a record's required (non-optional) named
/// fields, keyed by the camelCase init labels the generated struct uses (optionals
/// default to `nil` in the memberwise init, so they are omitted).
fn swift_record_literal(
    input: &WasmGeneratorInput,
    name: &str,
    group: &CsilGroupExpression,
) -> String {
    let type_name = swift_type_name(name);
    let args: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            let field = entry_field_name(e)?;
            Some(format!("{field}: {}", swift_sample(input, &e.value_type)))
        })
        .collect();
    format!("{type_name}({})", args.join(", "))
}

/// A compiling Swift literal for `ty`: real values for scalars/collections and nested
/// records (required fields only). A shape a generic sample cannot fabricate falls back
/// to an empty-collection / default-init best effort the consumer edits.
fn swift_sample(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "float" => "0.0".to_string(),
            "int" | "uint" => "0".to_string(),
            "bytes" | "bstr" => "[]".to_string(),
            "timestamp" => "\"1970-01-01T00:00:00Z\"".to_string(),
            "decimal" => "\"0\"".to_string(),
            _ => "[]".to_string(),
        },
        CsilTypeExpression::Array { .. } => "[]".to_string(),
        CsilTypeExpression::Map { .. } => "[:]".to_string(),
        CsilTypeExpression::Reference(name) => match swift_find_record(input, name) {
            Some(group) => swift_record_literal(input, name, group),
            None => format!("{}()", swift_type_name(name)),
        },
        _ => "[]".to_string(),
    }
}

/// The CSIL-RPC HTTP carrier — spec-independent, so a constant. It builds the request
/// envelope with the library's `RpcRequest`, POSTs it to `{baseURL}/csil/v1/rpc` with a
/// blocking `URLSession`, and lets `RpcResponse.decode` + `asTransportError()` raise on a
/// non-zero transport status; the typed `ServiceError` arm (a status-0 variant) is
/// surfaced separately.
const SWIFT_RPC_CARRIER: &str = r#"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (RpcRequest/RpcResponse); the carrier owns only the transport. Swap URLSession for any
// HTTP client — it implements the generated CsilTransport byte seam.
struct HttpRpcCarrier: CsilTransport {
    let baseURL: String

    func call(service: String, op: String, request: [UInt8]) throws -> [UInt8] {
        // The library builds the envelope (tag-24 payload, canonical CBOR); never hand-roll it.
        let envelope = RpcRequest(service: service, op: op, payload: request).encode()
        let mount = baseURL.hasSuffix("/") ? baseURL + "csil/v1/rpc" : baseURL + "/csil/v1/rpc"
        var http = URLRequest(url: URL(string: mount)!)
        http.httpMethod = "POST"
        http.setValue("application/cbor", forHTTPHeaderField: "Content-Type")
        http.setValue("application/cbor", forHTTPHeaderField: "Accept")
        http.httpBody = Data(envelope)

        // Block the calling thread on the async URLSession task — the seam is sync.
        let semaphore = DispatchSemaphore(value: 0)
        var outcome: Result<[UInt8], Error> =
            .failure(TransportError.carrier("csil-rpc: no response"))
        URLSession.shared.dataTask(with: http) { data, response, error in
            defer { semaphore.signal() }
            if let error { outcome = .failure(error); return }
            guard let status = (response as? HTTPURLResponse)?.statusCode, status == 200,
                  let data
            else {
                outcome = .failure(TransportError.carrier("csil-rpc: bad HTTP response"))
                return
            }
            outcome = .success([UInt8](data))
        }.resume()
        semaphore.wait()

        // The library decodes the response and raises a TransportError for any non-zero
        // transport status (distinct from a typed application error).
        let resp = try RpcResponse.decode(try outcome.get())
        if let err = resp.asTransportError() { throw err }
        // A typed application error rides as a status-0 `ServiceError` variant — surface it
        // so the typed client decodes success only.
        if resp.variant == "ServiceError" {
            throw TransportError.carrier("csil-rpc \(service)/\(op): ServiceError")
        }
        return resp.payload
    }
}
"#;

/// The CSIL-Events TLS carrier — spec-independent. A `ByteStream` over Network.framework's
/// `NWConnection` (TLS); `StreamCarrier` adds the library's length-prefix framing. The
/// event-driven `NWConnection` is bridged to the synchronous `ByteStream` seam with a
/// semaphore so the host owns a simple blocking I/O loop.
const SWIFT_EVENTS_CARRIER: &str = r#"// One example carrier: a TLS byte stream (Network.framework) the library frames with its
// 4-byte length prefix via StreamCarrier. NWConnection is event-driven; the host owns the
// I/O loop, so each read/write bridges to the synchronous ByteStream seam.
final class TlsByteStream: ByteStream {
    private let conn: NWConnection
    private var buffer: [UInt8] = []

    init(host: String, port: UInt16) {
        conn = NWConnection(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!,
            using: .tls)
        let ready = DispatchSemaphore(value: 0)
        conn.stateUpdateHandler = { if $0 == .ready { ready.signal() } }
        conn.start(queue: .global())
        ready.wait()
    }

    func write(_ bytes: [UInt8]) throws {
        let done = DispatchSemaphore(value: 0)
        var failure: Error?
        conn.send(
            content: Data(bytes),
            completion: .contentProcessed { failure = $0; done.signal() })
        done.wait()
        if let failure { throw TransportError.carrier("tls write: \(failure)") }
    }

    func readExactly(_ count: Int) throws -> [UInt8] {
        while buffer.count < count {
            let done = DispatchSemaphore(value: 0)
            var chunk: [UInt8] = []
            conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, _, _ in
                if let data { chunk = [UInt8](data) }
                done.signal()
            }
            done.wait()
            if chunk.isEmpty { break }  // a clean end of stream
            buffer.append(contentsOf: chunk)
        }
        let take = min(count, buffer.count)
        let out = Array(buffer.prefix(take))
        buffer.removeFirst(take)
        return out
    }
}
"#;

/// The CSIL-Datagrams UDP carrier — spec-independent. A `DatagramCarrier` over
/// Network.framework's `NWConnection` (UDP). `sendDatagram` writes one packet;
/// `recvDatagram` resolves the next inbound packet (or nil) — it never waits for or
/// correlates a reply.
const SWIFT_DATAGRAMS_CARRIER: &str = r#"// One example carrier: UDP via Network.framework. Datagrams are unreliable and unordered,
// so the carrier never waits for or correlates a reply.
final class UdpDatagramCarrier: DatagramCarrier {
    private let conn: NWConnection

    init(host: String, port: UInt16) {
        conn = NWConnection(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!,
            using: .udp)
        let ready = DispatchSemaphore(value: 0)
        conn.stateUpdateHandler = { if $0 == .ready { ready.signal() } }
        conn.start(queue: .global())
        ready.wait()
    }

    func sendDatagram(_ datagram: [UInt8]) throws {
        let done = DispatchSemaphore(value: 0)
        var failure: Error?
        conn.send(
            content: Data(datagram),
            completion: .contentProcessed { failure = $0; done.signal() })
        done.wait()
        if let failure { throw TransportError.carrier("udp send: \(failure)") }
    }

    func recvDatagram() throws -> [UInt8]? {
        let done = DispatchSemaphore(value: 0)
        var packet: [UInt8]? = nil
        conn.receiveMessage { data, _, _, _ in
            if let data, !data.isEmpty { packet = [UInt8](data) }
            done.signal()
        }
        done.wait()
        return packet
    }
}
"#;

// ---------------------------------------------------------------------------
// Identifier + literal helpers
// ---------------------------------------------------------------------------

/// Swift reserved words that, when they collide with a generated identifier, must be
/// wrapped in backticks to stay a valid identifier (Swift's standard escape).
const SWIFT_KEYWORDS: &[&str] = &[
    "associatedtype",
    "class",
    "deinit",
    "enum",
    "extension",
    "fileprivate",
    "func",
    "import",
    "init",
    "inout",
    "internal",
    "let",
    "open",
    "operator",
    "private",
    "precedencegroup",
    "protocol",
    "public",
    "rethrows",
    "static",
    "struct",
    "subscript",
    "typealias",
    "var",
    "break",
    "case",
    "catch",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "fallthrough",
    "for",
    "guard",
    "if",
    "in",
    "repeat",
    "return",
    "throw",
    "switch",
    "where",
    "while",
    "as",
    "false",
    "is",
    "nil",
    "self",
    "Self",
    "super",
    "throws",
    "true",
    "try",
    "any",
    "await",
    "actor",
    "async",
];

/// Backtick-escape an identifier when it collides with a Swift keyword.
fn escape_ident(name: &str) -> String {
    if SWIFT_KEYWORDS.contains(&name) {
        format!("`{name}`")
    } else {
        name.to_string()
    }
}

/// A Swift property/method identifier: lowerCamelCase, keyword-escaped. An identifier
/// that camel-cases to empty (degenerate input) falls back to a safe placeholder.
fn swift_ident(name: &str) -> String {
    let camel = name.to_case(Case::Camel);
    let camel = if camel.is_empty() {
        "field".to_string()
    } else {
        camel
    };
    escape_ident(&camel)
}

/// A Swift type identifier: UpperCamelCase (acronyms are not special-cased; csilgen
/// type names are already chosen by the author).
fn swift_type_name(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    if pascal.is_empty() {
        "AnonymousType".to_string()
    } else {
        pascal
    }
}

/// The wire key for a group entry: the CSIL field name **verbatim**. Case transforms
/// are for the Swift identifier only; they must never reach the CBOR map key.
fn wire_key(key: &CsilGroupKey) -> Option<String> {
    match key {
        CsilGroupKey::Bare(name) => Some(name.clone()),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => Some(name.clone()),
        _ => None,
    }
}

/// The Swift property name for a group entry, or `None` when no stable name exists
/// (a typed key); such entries are skipped uniformly by every emitter.
fn entry_field_name(entry: &CsilGroupEntry) -> Option<String> {
    let key = wire_key(entry.key.as_ref()?)?;
    Some(swift_ident(&key))
}

/// Strip a trailing `Service` suffix and Pascal-case the remainder so the generated
/// client/handler type reads `AttestationClient`, not `AttestationServiceClient`.
fn service_base(name: &str) -> String {
    let pascal = swift_type_name(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// A safely-escaped Swift double-quoted string literal for arbitrary text.
fn swift_string_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A push op (`-> Event`) carries a `null` input type: there is no request to send.
fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Map a CSIL type to its idiomatic Swift spelling. `optional` wraps the result in
/// `T?`. Wire/encoding concerns live in the transport lib; this only names the type.
fn map_type(type_expr: &CsilTypeExpression, optional: bool) -> String {
    let base = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "Int64".to_string(),
            "uint" => "UInt64".to_string(),
            "float" => "Double".to_string(),
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`.
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "[UInt8]".to_string(),
            "bool" => "Bool".to_string(),
            // RFC3339 UTC text on the wire (CBOR tag 0); kept as a String so the
            // generated types stay Foundation-free and portable.
            "timestamp" => "String".to_string(),
            // Exact decimal as canonical decimal text (CBOR tag 4); kept as a String
            // for the same Foundation-free reason.
            "decimal" => "String".to_string(),
            "any" => "AnyCsilValue".to_string(),
            other => swift_type_name(other),
        },
        CsilTypeExpression::Reference(name) => swift_type_name(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("[{}]", map_type(element_type, false))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            format!("[{}: {}]", map_type(key, false), map_type(value, false))
        }
        CsilTypeExpression::Tuple(group) => map_tuple(group),
        CsilTypeExpression::Literal(lit) => match lit {
            CsilLiteralValue::Integer(_) => "Int64".to_string(),
            CsilLiteralValue::Float(_) => "Double".to_string(),
            CsilLiteralValue::Text(_) => "String".to_string(),
            CsilLiteralValue::Bool(_) => "Bool".to_string(),
            CsilLiteralValue::Bytes(_) => "[UInt8]".to_string(),
            CsilLiteralValue::Null => "AnyCsilValue".to_string(),
            CsilLiteralValue::Array(_) => "AnyCsilValue".to_string(),
        },
        CsilTypeExpression::Constrained { base_type, .. } => {
            // Constraints (.size/.regex/.ge…) are validation rules, not Swift types.
            return map_type(base_type, optional);
        }
        // A stringy choice (open `text` and/or string literals) is just "some string";
        // an inline non-stringy choice has no name to bind an enum to, so the opaque
        // `AnyCsilValue` keeps it constructible without inventing a type.
        CsilTypeExpression::Choice(choices) => {
            if choice_is_stringy(choices) {
                "String".to_string()
            } else {
                "AnyCsilValue".to_string()
            }
        }
        _ => "AnyCsilValue".to_string(),
    };
    if optional { format!("{base}?") } else { base }
}

/// A CSIL tuple becomes a native Swift tuple type, labelled by key where present and
/// `field0`/`field1`/… otherwise, preserving each position's type.
fn map_tuple(group: &CsilGroupExpression) -> String {
    let parts: Vec<String> = group
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let ty = map_type(
                &entry.value_type,
                matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
            );
            match entry.key.as_ref().and_then(wire_key) {
                Some(name) => format!("{}: {ty}", swift_ident(&name)),
                None => format!("field{index}: {ty}"),
            }
        })
        .collect();
    // A single-element Swift tuple is just its element type.
    if parts.len() == 1 {
        map_type(
            &group.entries[0].value_type,
            matches!(group.entries[0].occurrence, Some(CsilOccurrence::Optional)),
        )
    } else {
        format!("({})", parts.join(", "))
    }
}

/// The Swift tuple-label for one tuple position: the keyed name where the entry has
/// one, else the positional `field<index>` `map_tuple` already uses — kept as its own
/// function so encode/decode (`swift_enc_value`/`swift_dec_value`) address exactly the
/// member the type declaration (`map_tuple`) named.
fn tuple_field_label(group: &CsilGroupExpression, index: usize) -> String {
    match group.entries[index].key.as_ref().and_then(wire_key) {
        Some(name) => swift_ident(&name),
        None => format!("field{index}"),
    }
}

fn literal_to_swift(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => swift_string_lit(s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "[]".to_string(),
        CsilLiteralValue::Array(elements) => {
            let parts: Vec<String> = elements.iter().map(literal_to_swift).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

fn swift_literal_cbor_expr(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) if *i >= 0 => format!(".uint(UInt64({i}))"),
        CsilLiteralValue::Integer(i) => format!(".int(Int64({i}))"),
        CsilLiteralValue::Float(f) => format!(".double({f})"),
        CsilLiteralValue::Text(s) => format!(".text({})", swift_string_lit(s)),
        CsilLiteralValue::Bool(b) => format!(".bool({b})"),
        CsilLiteralValue::Null => ".null".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(".bytes([{values}])")
        }
        CsilLiteralValue::Array(items) => {
            let values = items
                .iter()
                .map(swift_literal_cbor_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!(".array([{values}])")
        }
    }
}

fn swift_literal_value_expr(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Null => "AnyCsilValue.null".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{values}]")
        }
        other => literal_to_swift(other),
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn generate_types(input: &WasmGeneratorInput) -> Option<String> {
    let mut body = String::new();
    let mut any = false;
    let mut needs_validation = false;

    // `input.csil_spec` has already been through `csilgen_common::hoist_inline_composites`
    // (see `build_files`), so every inline choice a field/array-element/map-value/tuple-
    // element carried now shows up here as an ordinary synthesized `TypeDef(Choice(..))`
    // rule, appended after the source-declared rules — this same loop emits it exactly
    // like an author-declared choice rule, with no separate hoisted-types pass needed.
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                any = true;
                body.push_str(&emit_struct(&rule.name, group, &mut needs_validation));
            }
            CsilRuleType::TypeDef(type_expr) => match type_expr {
                CsilTypeExpression::Group(group) => {
                    any = true;
                    body.push_str(&emit_struct(&rule.name, group, &mut needs_validation));
                }
                CsilTypeExpression::Choice(choices) => {
                    any = true;
                    body.push_str(&emit_enum(&rule.name, choices));
                }
                other => {
                    any = true;
                    body.push_str(&format!(
                        "public typealias {} = {}\n\n",
                        swift_type_name(&rule.name),
                        map_type(other, false)
                    ));
                }
            },
            CsilRuleType::TypeChoice(choices) => {
                any = true;
                body.push_str(&emit_enum(&rule.name, choices));
            }
            _ => {}
        }
    }

    if !any {
        return None;
    }

    let mut content = header("Generated CSIL types.");
    if needs_validation {
        content.push_str(VALIDATION_ERROR_SWIFT);
        content.push('\n');
    }
    if body.contains("AnyCsilValue") {
        content.push_str(ANY_VALUE_SWIFT);
        content.push('\n');
    }
    content.push_str(&body);
    Some(content)
}

/// A `struct` record: camelCased `let` properties (wire keys kept verbatim in a doc
/// comment), a public memberwise init that pins `.default`s and defaults optionals to
/// `nil`, `Equatable`/`Sendable`, and a `validate()` when the spec carries checks.
fn emit_struct(name: &str, group: &CsilGroupExpression, needs_validation: &mut bool) -> String {
    let type_name = swift_type_name(name);
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is a generated CSIL record type.\n"
    ));
    out.push_str(&format!(
        "public struct {type_name}: Equatable, Sendable {{\n"
    ));

    // Stored properties.
    let mut fields: Vec<(String, String, &CsilGroupEntry)> = Vec::new();
    for entry in &group.entries {
        let Some(field) = entry_field_name(entry) else {
            out.push_str("    // group-spread entry skipped (no field name)\n");
            continue;
        };
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        // `entry.value_type` is already the post-hoist type: an inline choice this
        // field carried has been rewritten to a `Reference` to its synthesized named
        // type by `csilgen_common::hoist_inline_composites` (see `build_files`), so
        // `map_type` routes it through that type like any other named reference.
        let ty = map_type(&entry.value_type, optional);
        if let Some(desc) = field_description(&entry.metadata) {
            out.push_str(&format!("    /// {desc}\n"));
        }
        if let Some(wire) = entry.key.as_ref().and_then(wire_key) {
            let swift_form = swift_ident(&wire);
            if swift_form != wire {
                out.push_str(&format!("    /// wire key: {wire}\n"));
            }
        }
        out.push_str(&format!("    public let {field}: {ty}\n"));
        fields.push((field, ty, entry));
    }

    // Public memberwise init carrying defaults.
    out.push('\n');
    let params: Vec<String> = fields
        .iter()
        .map(|(field, ty, entry)| {
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            let default = entry_default(entry);
            let suffix = match (default, optional) {
                (Some(value), _) => format!(" = {}", literal_to_swift(value)),
                (None, true) => " = nil".to_string(),
                (None, false) => String::new(),
            };
            format!("{field}: {ty}{suffix}")
        })
        .collect();
    out.push_str(&format!("    public init({}) {{\n", params.join(", ")));
    for (field, _, _) in &fields {
        out.push_str(&format!("        self.{field} = {field}\n"));
    }
    out.push_str("    }\n");

    // Validation.
    let validate = emit_validate(&fields);
    if let Some(v) = validate {
        *needs_validation = true;
        out.push('\n');
        out.push_str(&v);
    }

    // Wire-key map: the verbatim CBOR map keys keyed by Swift property name, so a
    // hand-written codec or the transport seam can map identifiers to wire keys.
    let wire_pairs: Vec<String> = fields
        .iter()
        .filter_map(|(field, _, entry)| {
            let wire = entry.key.as_ref().and_then(wire_key)?;
            Some(format!(
                "        {}: {}",
                swift_string_lit(field),
                swift_string_lit(&wire)
            ))
        })
        .collect();
    if !wire_pairs.is_empty() {
        out.push('\n');
        out.push_str("    /// CBOR wire keys (verbatim) keyed by Swift property name.\n");
        out.push_str("    public static let wireKeys: [String: String] = [\n");
        out.push_str(&wire_pairs.join(",\n"));
        out.push_str("\n    ]\n");
    }

    out.push_str("}\n\n");
    out
}

/// Whether every arm of a choice is "some text": the open `text`/`tstr` builtin or a
/// string literal (including one wrapped by a trailing control operator, see
/// `choice_arm_literal`). Used only for an *inline* anonymous choice (a choice written
/// directly as a field's type, with no rule name to bind an enum to) as an
/// is-it-just-a-string fallback signal. A *named* choice/union rule always gets its
/// own tagged-enum codec via `emit_enum`/`swift_union_choices`, mixed or not — see
/// `all_text_literals` for the (narrower) test that actually gates that collapse.
fn choice_is_stringy(choices: &[CsilTypeExpression]) -> bool {
    !choices.is_empty()
        && choices.iter().all(|c| match c {
            CsilTypeExpression::Builtin(n) => n == "text" || n == "tstr",
            _ => matches!(choice_arm_literal(c), Some(CsilLiteralValue::Text(_))),
        })
}

/// The verbatim wire strings of a closed string-literal vocabulary (every literal a
/// text literal), or `None` when any literal is a different kind. Operates on the
/// already-confirmed-all-literal list `classify_choice`/`swift_choice_shape` hands it
/// (see below), not on raw choice arms — a bare `text`/`tstr` open arm, or any other
/// non-literal arm, never reaches this function at all: `ChoiceClass::Union` catches
/// those upstream.
fn all_text_literals(literals: &[&CsilLiteralValue]) -> Option<Vec<String>> {
    literals
        .iter()
        .map(|lit| match lit {
            CsilLiteralValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The literal-kind tag of a non-text scalar literal, for `all_scalar_literals`'s
/// same-kind check. Text is deliberately absent: a text-literal choice already has its
/// own dedicated route (`all_text_literals`/`emit_string_enum`), and keeping the two
/// kind spaces disjoint means a choice can never match both.
fn scalar_literal_kind(value: &CsilLiteralValue) -> Option<&'static str> {
    match value {
        CsilLiteralValue::Integer(_) => Some("int"),
        CsilLiteralValue::Float(_) => Some("float"),
        CsilLiteralValue::Bool(_) => Some("bool"),
        CsilLiteralValue::Bytes(_) => Some("bytes"),
        _ => None,
    }
}

/// The verbatim literals of a closed uniform-kind scalar vocabulary — every literal an
/// int/float/bool/bytes literal of the *same* kind — or `None` when the kinds mix or
/// any literal is text (which `all_text_literals` owns instead). This is the non-text
/// sibling of `all_text_literals`: same bare-literal wire contract (interop's
/// `Priority = 1 / 2 / 3` is bare `1`/`2`/`3` on the wire, not a `[index, value]`
/// tagged sum), just for the literal kinds that can't back a Swift `String`-raw-value
/// enum.
fn all_scalar_literals(literals: &[&CsilLiteralValue]) -> Option<Vec<CsilLiteralValue>> {
    let mut kind = None;
    for lit in literals {
        let this_kind = scalar_literal_kind(lit)?;
        match kind {
            None => kind = Some(this_kind),
            Some(k) if k == this_kind => {}
            _ => return None,
        }
    }
    Some(literals.iter().map(|lit| (*lit).clone()).collect())
}

/// The verbatim literals of a closed ALL-literal vocabulary whose kinds are NOT
/// uniform (e.g. `"a" / 1`) — the third bare-literal shape alongside
/// `all_text_literals` (pure text) and `all_scalar_literals` (one uniform non-text
/// scalar kind). Per the CSIL wire contract an ALL-literal choice always rides bare
/// regardless of whether its declared literal kinds happen to match — the wire value
/// is the literal itself, self-discriminating by its own CBOR major type + value — so
/// a mixed-kind literal set needs no tag any more than a uniform-kind one does. `None`
/// when a literal isn't a kind `all_text_literals`/`all_scalar_literals` would
/// otherwise recognize (bytes included, matching this generator's own existing
/// scalar-enum scope; a `null`/array literal isn't classifiable here — see
/// `swift_choice_shape`, which falls such a vocabulary back to `Union` since neither
/// `mixed_enum_case_name`/`mixed_case_pattern` can discriminate those kinds). Called
/// only from `classify_enum_literals`, after `all_text_literals`/`all_scalar_literals`
/// have both already failed, so this never needs to re-check uniformity itself.
/// Mirrors the PHP/Go generators' analogously broadened `choice_is_enum`/
/// `choice_all_literal` for the same Finding.
fn all_mixed_literals(literals: &[&CsilLiteralValue]) -> Option<Vec<CsilLiteralValue>> {
    for lit in literals {
        if !matches!(lit, CsilLiteralValue::Text(_)) && scalar_literal_kind(lit).is_none() {
            return None;
        }
    }
    Some(literals.iter().map(|lit| (*lit).clone()).collect())
}

/// Swift's classification of a CSIL choice: the shared `csilgen_common::classify_choice`
/// decides the normative ENUM-vs-UNION split (an ALL-literal vocabulary, any kind or
/// mix, is an `Enum`; at least one non-literal arm is a `Union`), and this generator's
/// own three literal-kind sub-shapes (`all_text_literals`/`all_scalar_literals`/
/// `all_mixed_literals`) further split a confirmed `Enum` into the bare-wire rendering
/// it gets. `Union` also covers the rare all-literal vocabulary containing a
/// `null`/array literal (see `all_mixed_literals`'s doc) — Swift has no codec for that
/// shape as a bare enum, so it falls back to the tagged-sum union exactly as it did
/// before this classifier existed.
enum SwiftChoiceShape {
    StringEnum(Vec<String>),
    ScalarEnum(Vec<CsilLiteralValue>),
    MixedEnum(Vec<CsilLiteralValue>),
    Union,
}

fn swift_choice_shape(choices: &[CsilTypeExpression]) -> SwiftChoiceShape {
    match csilgen_common::classify_choice(choices) {
        ChoiceClass::Enum(literals) => classify_enum_literals(&literals),
        ChoiceClass::Union(_) => SwiftChoiceShape::Union,
    }
}

/// Sub-classify a confirmed-all-literal vocabulary into the bare-wire enum shape Swift
/// renders it as, trying the narrowest shape first so the three stay mutually
/// exclusive: pure text, then uniform-kind scalar, then mixed kinds; a vocabulary none
/// of the three covers (`null`/array literals) falls back to `Union`.
fn classify_enum_literals(literals: &[&CsilLiteralValue]) -> SwiftChoiceShape {
    if let Some(labels) = all_text_literals(literals) {
        return SwiftChoiceShape::StringEnum(labels);
    }
    if let Some(lits) = all_scalar_literals(literals) {
        return SwiftChoiceShape::ScalarEnum(lits);
    }
    if let Some(lits) = all_mixed_literals(literals) {
        return SwiftChoiceShape::MixedEnum(lits);
    }
    SwiftChoiceShape::Union
}

/// A closed set of string literals as a `String`-backed Swift enum: the raw value is the
/// wire string verbatim (so case order/spelling never drifts onto the wire), the case
/// name is the camelCased label, and `CaseIterable` is free and conventional here.
fn emit_string_enum(type_name: &str, labels: &[String]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is a generated CSIL string enum (a closed set of wire values).\n"
    ));
    out.push_str(&format!(
        "public enum {type_name}: String, Equatable, Sendable, CaseIterable {{\n"
    ));
    for label in labels {
        out.push_str(&format!(
            "    case {} = {}\n",
            swift_ident(label),
            swift_string_lit(label)
        ));
    }
    out.push_str("}\n\n");
    out
}

/// The case name `emit_enum` and `emit_union_codec` both use for a choice arm, so the
/// type declaration and its codec never drift apart: a `Reference`/`Builtin` arm gets a
/// named case (its referenced/builtin name), everything else — including a `Literal`
/// arm — gets a positional `case<index>`.
fn enum_case_name(choice: &CsilTypeExpression, index: usize) -> String {
    match choice {
        CsilTypeExpression::Reference(arm) | CsilTypeExpression::Builtin(arm) => swift_ident(arm),
        _ => format!("case{index}"),
    }
}

/// A Swift case identifier for one scalar-literal enum member (see
/// `all_scalar_literals`). There's no wire-verbatim label to camelCase the way a text
/// literal's own value doubles as its case name, so the identifier is derived from the
/// literal itself: `v1`/`vNeg5` for an int, `vTrue`/`vFalse` for a bool, a
/// decimal-point-sanitized `v3_14` for a float. A bytes literal has no compact textual
/// form worth naming after, so it falls back to the positional form every other kind
/// also uses as its last resort.
fn scalar_enum_case_name(lit: &CsilLiteralValue, index: usize) -> String {
    match lit {
        CsilLiteralValue::Integer(n) if *n < 0 => format!("vNeg{}", n.unsigned_abs()),
        CsilLiteralValue::Integer(n) => format!("v{n}"),
        CsilLiteralValue::Bool(b) => format!("v{}", if *b { "True" } else { "False" }),
        CsilLiteralValue::Float(f) => {
            let text = f.to_string();
            let (sign, digits) = match text.strip_prefix('-') {
                Some(rest) => ("Neg", rest),
                None => ("", text.as_str()),
            };
            format!("v{sign}{}", digits.replace('.', "_"))
        }
        _ => format!("v{index}"),
    }
}

/// The unique case identifiers for a scalar-literal enum (see `all_scalar_literals`),
/// de-duplicating any two literals whose derived name collides (e.g. `1` and `-1` both
/// sanitizing to the same text in a hypothetical future literal kind) by falling back to
/// the positional form, which is always unique since it's the loop index.
fn scalar_enum_case_names(lits: &[CsilLiteralValue]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    lits.iter()
        .enumerate()
        .map(|(index, lit)| {
            let name = scalar_enum_case_name(lit, index);
            if seen.insert(name.clone()) {
                name
            } else {
                format!("v{index}")
            }
        })
        .collect()
}

/// A Swift case identifier for one member of a MIXED-kind all-literal enum (see
/// `all_mixed_literals`). A text literal gets the same wire-verbatim camelCased
/// label `emit_string_enum` uses for a pure-text enum (there's no reason to lose
/// that readability just because a sibling arm happens to be a different kind);
/// every other kind falls back to `scalar_enum_case_name`'s derived-from-the-
/// literal-itself naming.
fn mixed_enum_case_name(lit: &CsilLiteralValue, index: usize) -> String {
    match lit {
        CsilLiteralValue::Text(s) => swift_ident(s),
        other => scalar_enum_case_name(other, index),
    }
}

/// The unique case identifiers for a mixed-kind literal enum (see
/// `all_mixed_literals`), de-duplicating a name collision (e.g. a text literal `"1"`
/// and an integer literal `1` both naming their case `v1`/`v1`... actually `"1"`
/// camelCases to `_1` via `swift_ident`, but two literals of the same kind could
/// still collide) the same way `scalar_enum_case_names` does: fall back to the
/// positional form, which is always unique since it's the loop index.
fn mixed_enum_case_names(lits: &[CsilLiteralValue]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    lits.iter()
        .enumerate()
        .map(|(index, lit)| {
            let name = mixed_enum_case_name(lit, index);
            if seen.insert(name.clone()) {
                name
            } else {
                format!("v{index}")
            }
        })
        .collect()
}

/// The Swift type Swift's raw-value-enum syntax natively backs a scalar literal kind
/// with, or `None` when that kind can't be a `case x = <literal>` raw value at all.
/// Swift's raw-value enum sugar only accepts integer, floating-point, and string literal
/// raw values (`RawRepresentable` synthesis pattern-matches those literal AST kinds
/// specifically) — a bool or bytes literal can't be written as `case x = true` /
/// `case x = [1, 2]` there, so those two kinds fall back to a manual (no raw value)
/// enum with a hand-written `toCborValue()`/`init(cborValue:)` switch instead of
/// `emit_scalar_enum`'s native-raw-value path.
fn scalar_swift_raw_type(lit: &CsilLiteralValue) -> Option<&'static str> {
    match lit {
        CsilLiteralValue::Integer(_) => Some("Int64"),
        CsilLiteralValue::Float(_) => Some("Double"),
        _ => None,
    }
}

/// A closed uniform-kind scalar-literal choice (`all_scalar_literals`) as a Swift enum:
/// int/float back a native `RawRepresentable` enum (`enum X: Int64 { case v1 = 1 ... }`,
/// the same shape `emit_string_enum` already uses for `String`), bool/bytes — which
/// can't be raw-value literals in Swift (see `scalar_swift_raw_type`) — become a manual
/// enum with no raw value at all; `emit_scalar_enum_codec` supplies the matching
/// `toCborValue()`/`init(cborValue:)` for either shape.
fn emit_scalar_enum(type_name: &str, lits: &[CsilLiteralValue]) -> String {
    let names = scalar_enum_case_names(lits);
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is a generated CSIL scalar-literal enum (a closed set of wire values).\n"
    ));
    match lits.first().and_then(scalar_swift_raw_type) {
        Some(raw) => {
            out.push_str(&format!(
                "public enum {type_name}: {raw}, Equatable, Sendable, CaseIterable {{\n"
            ));
            for (case_name, lit) in names.iter().zip(lits) {
                out.push_str(&format!(
                    "    case {case_name} = {}\n",
                    literal_to_swift(lit)
                ));
            }
        }
        None => {
            out.push_str(&format!(
                "public enum {type_name}: Equatable, Sendable, CaseIterable {{\n"
            ));
            for case_name in &names {
                out.push_str(&format!("    case {case_name}\n"));
            }
        }
    }
    out.push_str("}\n\n");
    out
}

/// A closed MIXED-kind all-literal choice (`all_mixed_literals`, e.g. `"a" / 1`) as a
/// Swift enum: no raw value is possible — Swift's raw-value enum sugar requires ONE
/// uniform backing type (`RawRepresentable` synthesis), and this choice's arms don't
/// share one — so it's always the manual (no-raw-value) shape `emit_scalar_enum`
/// already uses for its own bool/bytes case, just with a case per literal regardless
/// of kind. `emit_mixed_enum_codec` supplies the matching `toCborValue()`/
/// `init(cborValue:)`.
fn emit_mixed_enum(type_name: &str, lits: &[CsilLiteralValue]) -> String {
    let names = mixed_enum_case_names(lits);
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is a generated CSIL mixed-literal enum (a closed set of wire values of differing kinds).\n"
    ));
    out.push_str(&format!(
        "public enum {type_name}: Equatable, Sendable, CaseIterable {{\n"
    ));
    for case_name in &names {
        out.push_str(&format!("    case {case_name}\n"));
    }
    out.push_str("}\n\n");
    out
}

/// A variant/sum type as a Swift `enum` with associated values, one case per declared
/// choice arm. A pure string-literal set becomes a `String`-backed enum
/// (`emit_string_enum`); a uniform-kind int/float/bool/bytes-literal set becomes a
/// scalar-backed enum (`emit_scalar_enum`); a MIXED-kind all-literal set (differing
/// literal kinds, e.g. `"a" / 1`) becomes a manual no-raw-value enum
/// (`emit_mixed_enum`) — all three keep the literal as the wire's own discriminant,
/// no `[index, payload]` wrapper. Every other shape — including one that mixes the
/// open `text` builtin with string literals, e.g. `text / "pending" / "confirmed" /
/// ...` (at least one non-literal arm) — becomes a tagged enum: reference/builtin
/// arms take the referenced struct or mapped Swift type under a named case,
/// everything else (literals, nested shapes) takes a positional case. The enum
/// case itself is the wire discriminant; see `emit_union_codec` for how that's
/// encoded/decoded as CBOR.
fn emit_enum(name: &str, choices: &[CsilTypeExpression]) -> String {
    let type_name = swift_type_name(name);
    match swift_choice_shape(choices) {
        SwiftChoiceShape::StringEnum(labels) => return emit_string_enum(&type_name, &labels),
        SwiftChoiceShape::ScalarEnum(lits) => return emit_scalar_enum(&type_name, &lits),
        SwiftChoiceShape::MixedEnum(lits) => return emit_mixed_enum(&type_name, &lits),
        SwiftChoiceShape::Union => {}
    }
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is a generated CSIL variant (sum) type.\n"
    ));
    out.push_str(&format!(
        "public enum {type_name}: Equatable, Sendable {{\n"
    ));
    for (index, choice) in choices.iter().enumerate() {
        let case = enum_case_name(choice, index);
        out.push_str(&format!("    case {case}({})\n", map_type(choice, false)));
    }
    out.push_str("}\n\n");
    out
}

/// The default literal for a field: the `.default(...)` control operator or the
/// `@default(...)` annotation. The annotation wins if somehow both are present.
fn entry_default(entry: &CsilGroupEntry) -> Option<&CsilLiteralValue> {
    for meta in &entry.metadata {
        if let CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, value }) =
            meta
            && name == "default"
        {
            return Some(value);
        }
    }
    if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
        for op in constraints {
            if let CsilControlOperator::Default(value) = op {
                return Some(value);
            }
        }
    }
    None
}

fn field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|m| match m {
        CsilFieldMetadata::Description(desc) => Some(desc.as_str()),
        _ => None,
    })
}

/// The base builtin name of a (possibly `.`-constrained) type, used to decide whether
/// a numeric comparison can be emitted as a Swift scalar compare.
fn base_builtin(type_expr: &CsilTypeExpression) -> Option<&str> {
    match type_expr {
        CsilTypeExpression::Builtin(name) => Some(name.as_str()),
        CsilTypeExpression::Constrained { base_type, .. } => base_builtin(base_type),
        _ => None,
    }
}

/// Whether the field's base type is a Swift numeric scalar (so `<`/`>` compares
/// compile). `decimal`/`timestamp` map to `String` here, so an ordered comparison on
/// them would be a lexical string compare — semantically wrong — and is skipped.
fn is_numeric(type_expr: &CsilTypeExpression) -> bool {
    matches!(base_builtin(type_expr), Some("int" | "uint" | "float"))
}

/// Emit `func validate() throws` when any field carries a runtime check. Length/size
/// checks use `.count`; numeric comparisons use Swift operators; `.regex` uses the
/// stdlib `Regex` (Foundation-free). Optional fields guard on unwrap first.
fn emit_validate(fields: &[(String, String, &CsilGroupEntry)]) -> Option<String> {
    let mut body = String::new();
    for (field, _, entry) in fields {
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        for meta in &entry.metadata {
            if let CsilFieldMetadata::Constraint(constraint) = meta {
                emit_annotation_check(&mut body, field, optional, &entry.value_type, constraint);
            }
        }
        if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
            for op in constraints {
                emit_control_check(&mut body, field, optional, &entry.value_type, op);
            }
        }
    }
    if body.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str(
        "    /// Validate field constraints, throwing CsilValidationError on the first failure.\n",
    );
    out.push_str("    public func validate() throws {\n");
    out.push_str(&body);
    out.push_str("    }\n");
    Some(out)
}

/// The expression that reads `field` inside a check: a non-optional reads
/// `self.field` directly; an optional binds to `v` via `if let` (see `emit_check`).
fn access(field: &str, optional: bool) -> String {
    if optional {
        "v".to_string()
    } else {
        format!("self.{field}")
    }
}

/// Emit one guard. An optional field combines its unwrap and the condition into a
/// single Swift `if let v = self.field, <cond>` so the deref never runs on `nil`; a
/// required field tests the condition directly. Either way a failure throws.
fn emit_check(body: &mut String, field: &str, optional: bool, cond: &str, message: &str) {
    if optional {
        body.push_str(&format!("        if let v = self.{field}, {cond} {{\n"));
    } else {
        body.push_str(&format!("        if {cond} {{\n"));
    }
    body.push_str(&format!(
        "            throw CsilValidationError({})\n",
        swift_string_lit(message)
    ));
    body.push_str("        }\n");
}

fn emit_len_check(body: &mut String, field: &str, optional: bool, op: &str, n: u64, message: &str) {
    // A `count < 0` test can never fire — Swift counts are non-negative — so a
    // minimum-of-zero bound is a dead branch; skip it rather than emit always-false code.
    if op == "<" && n == 0 {
        return;
    }
    let a = access(field, optional);
    emit_check(
        body,
        field,
        optional,
        &format!("{a}.count {op} {n}"),
        message,
    );
}

fn emit_numeric_check(
    body: &mut String,
    field: &str,
    optional: bool,
    op: &str,
    bound: &str,
    message: &str,
) {
    let a = access(field, optional);
    emit_check(body, field, optional, &format!("{a} {op} {bound}"), message);
}

fn emit_annotation_check(
    body: &mut String,
    field: &str,
    optional: bool,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
) {
    match constraint {
        CsilValidationConstraint::MinLength(n) => emit_len_check(
            body,
            field,
            optional,
            "<",
            *n,
            &format!("field '{field}' must have at least {n} characters"),
        ),
        CsilValidationConstraint::MaxLength(n) => emit_len_check(
            body,
            field,
            optional,
            ">",
            *n,
            &format!("field '{field}' must have at most {n} characters"),
        ),
        CsilValidationConstraint::MinItems(n) => emit_len_check(
            body,
            field,
            optional,
            "<",
            *n,
            &format!("field '{field}' must have at least {n} items"),
        ),
        CsilValidationConstraint::MaxItems(n) => emit_len_check(
            body,
            field,
            optional,
            ">",
            *n,
            &format!("field '{field}' must have at most {n} items"),
        ),
        CsilValidationConstraint::MinValue(value) if is_numeric(value_type) => {
            let bound = literal_to_swift(value);
            emit_numeric_check(
                body,
                field,
                optional,
                "<",
                &bound,
                &format!("field '{field}' must be at least {bound}"),
            );
        }
        CsilValidationConstraint::MaxValue(value) if is_numeric(value_type) => {
            let bound = literal_to_swift(value);
            emit_numeric_check(
                body,
                field,
                optional,
                ">",
                &bound,
                &format!("field '{field}' must be at most {bound}"),
            );
        }
        CsilValidationConstraint::Custom { name, value } if name == "regex" => {
            if let CsilLiteralValue::Text(pattern) = value {
                emit_regex_check(body, field, optional, pattern);
            }
        }
        // A non-numeric ordered bound (decimal/timestamp map to String here) or an
        // advisory custom constraint is left to the consumer; it surfaces as a note.
        CsilValidationConstraint::MinValue(_) | CsilValidationConstraint::MaxValue(_) => {
            body.push_str(&format!(
                "        // field '{field}': ordered bound on a non-scalar type left to the consumer\n"
            ));
        }
        CsilValidationConstraint::Custom { name, .. } => {
            body.push_str(&format!(
                "        // field '{field}': custom constraint '{name}' is advisory; enforce in application code\n"
            ));
        }
    }
}

fn emit_control_check(
    body: &mut String,
    field: &str,
    optional: bool,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
) {
    let numeric = is_numeric(value_type);
    let mut ordered = |swift_op: &str, value: &CsilLiteralValue, phrasing: &str| {
        if numeric {
            let bound = literal_to_swift(value);
            emit_numeric_check(
                body,
                field,
                optional,
                swift_op,
                &bound,
                &format!("field '{field}' must be {phrasing} {bound}"),
            );
        } else {
            body.push_str(&format!(
                "        // field '{field}': ordered constraint on a non-scalar type left to the consumer\n"
            ));
        }
    };
    match op {
        CsilControlOperator::GreaterEqual(v) => ordered("<", v, "at least"),
        CsilControlOperator::LessEqual(v) => ordered(">", v, "at most"),
        CsilControlOperator::GreaterThan(v) => ordered("<=", v, "greater than"),
        CsilControlOperator::LessThan(v) => ordered(">=", v, "less than"),
        CsilControlOperator::Equal(v) => ordered("!=", v, "equal to"),
        CsilControlOperator::NotEqual(v) => ordered("==", v, "not equal to"),
        CsilControlOperator::Size(size) => emit_size_check(body, field, optional, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(body, field, optional, pattern),
        CsilControlOperator::Default(_) => {}
        CsilControlOperator::Bits(_)
        | CsilControlOperator::And(_)
        | CsilControlOperator::Within(_)
        | CsilControlOperator::Json
        | CsilControlOperator::Cbor
        | CsilControlOperator::Cborseq => {
            body.push_str(&format!(
                "        // field '{field}': encoding/structural operator handled at (de)serialization, not validated\n"
            ));
        }
    }
}

fn emit_size_check(body: &mut String, field: &str, optional: bool, size: &CsilSizeConstraint) {
    let mut one = |op: &str, n: u64, word: &str| {
        emit_len_check(
            body,
            field,
            optional,
            op,
            n,
            &format!("field '{field}' must have {word} {n} elements"),
        );
    };
    match size {
        CsilSizeConstraint::Exact(n) => one("!=", *n, "exactly"),
        CsilSizeConstraint::Min(n) => one("<", *n, "at least"),
        CsilSizeConstraint::Max(n) => one(">", *n, "at most"),
        CsilSizeConstraint::Range { min, max } => {
            one("<", *min, "at least");
            one(">", *max, "at most");
        }
    }
}

/// Regex via the Swift-stdlib `Regex` (Foundation-free). `firstMatch(in:)` throws and
/// returns `nil` when the whole value does not contain a match.
fn emit_regex_check(body: &mut String, field: &str, optional: bool, pattern: &str) {
    let lit = swift_string_lit(pattern);
    let a = access(field, optional);
    let cond = format!("try Regex({lit}).firstMatch(in: {a}) == nil");
    emit_check(
        body,
        field,
        optional,
        &cond,
        &format!("field '{field}' must match pattern {pattern}"),
    );
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Codec (Codec.swift)
// ---------------------------------------------------------------------------

/// The Swift type names of the record (`struct`) rules — the types whose CBOR form
/// is a map and which the codec covers with `toCborValue`/`init(cborValue:)`.
fn swift_record_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(swift_type_name(&r.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(swift_type_name(&r.name)),
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference / tuple (NOT a record group or a choice,
/// which have their own handling). A field referencing one must encode/decode as the
/// underlying type rather than the `.null` / `asText` stub a bare non-record
/// reference would otherwise yield, which silently dropped the field's data.
fn swift_codec_aliases(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((rule.name.clone(), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Named choice/union rules (`TypeDef(Choice(..))` / `TypeChoice(..)`) that `emit_enum`
/// renders as a tagged `enum` rather than a `String`-/scalar-backed one — i.e. every
/// named choice except the pure text-literal set (`all_text_literals`), the uniform
/// scalar-literal set (`all_scalar_literals`), and the MIXED-kind all-literal set
/// (`all_mixed_literals`, e.g. `"a" / 1`), which are `emit_enum`'s only remaining
/// special cases now that the premature `choice_is_stringy` collapse is gone. This
/// deliberately does NOT also exclude on `choice_is_stringy`: an all-`text`-plus-
/// literals mix like `OrderStatus` (the motivating case for this whole codec) is
/// `choice_is_stringy` but is NOT `all_text_literals`, so excluding on it would silently
/// drop the exact shape this helper exists to cover. Keyed by the raw CSIL rule name
/// (matching how `swift_codec_aliases` keys transparent aliases), since that's what a
/// `CsilTypeExpression::Reference(name)` carries.
fn swift_union_choices(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, Vec<CsilTypeExpression>> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| {
            let choices = match &rule.rule_type {
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => choices,
                CsilRuleType::TypeChoice(choices) => choices,
                _ => return None,
            };
            match swift_choice_shape(choices) {
                SwiftChoiceShape::Union => Some((rule.name.clone(), choices.clone())),
                _ => None,
            }
        })
        .collect()
}

/// Named closed string-literal choice rules (`TypeDef(Choice(..))` / `TypeChoice(..)`
/// where every arm is a text literal, per `all_text_literals`) — one of the two
/// complementary sets to `swift_union_choices` (`swift_scalar_enum_choices` is the
/// other). `emit_enum` renders these as a `String`-backed enum, but that enum still
/// needs its own `toCborValue()`/`init(cborValue:)` (see `emit_string_enum_codec`):
/// before this exact map existed, a struct field referencing a rule like `Color = "red"
/// / "green" / "blue"` fell through `swift_enc_value`/`swift_dec_value`'s catch-all
/// (`.null` on encode, `asText` on decode — silently dropping the field's data and
/// producing a decode that doesn't even type-check against the `Color` field type).
/// Keyed by the raw CSIL rule name, matching `swift_union_choices`/`swift_codec_aliases`.
fn swift_string_enum_choices(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, Vec<String>> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| {
            let choices = match &rule.rule_type {
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => choices,
                CsilRuleType::TypeChoice(choices) => choices,
                _ => return None,
            };
            match swift_choice_shape(choices) {
                SwiftChoiceShape::StringEnum(labels) => Some((rule.name.clone(), labels)),
                _ => None,
            }
        })
        .collect()
}

/// Named closed uniform-kind scalar-literal choice rules (`TypeDef(Choice(..))` /
/// `TypeChoice(..)` where every arm is a same-kind int/float/bool/bytes literal, per
/// `all_scalar_literals`) — the non-text sibling of `swift_string_enum_choices`, same
/// role: routes a field referencing e.g. `Priority = 1 / 2 / 3` through
/// `emit_scalar_enum`'s codec (`emit_scalar_enum_codec`) instead of
/// `swift_enc_value`/`swift_dec_value`'s catch-all. Keyed by the raw CSIL rule name,
/// matching `swift_union_choices`/`swift_string_enum_choices`/`swift_codec_aliases`.
fn swift_scalar_enum_choices(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, Vec<CsilLiteralValue>> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| {
            let choices = match &rule.rule_type {
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => choices,
                CsilRuleType::TypeChoice(choices) => choices,
                _ => return None,
            };
            match swift_choice_shape(choices) {
                SwiftChoiceShape::ScalarEnum(lits) => Some((rule.name.clone(), lits)),
                _ => None,
            }
        })
        .collect()
}

/// Named closed MIXED-kind all-literal choice rules (`TypeDef(Choice(..))` /
/// `TypeChoice(..)` where every arm is a literal but the kinds DON'T all match, per
/// `all_mixed_literals`, e.g. `"a" / 1`) — the third sibling to
/// `swift_string_enum_choices`/`swift_scalar_enum_choices`, same role: routes a
/// field referencing such a rule through `emit_mixed_enum`'s codec
/// (`emit_mixed_enum_codec`) instead of either narrower enum path or
/// `swift_enc_value`/`swift_dec_value`'s tagged-union catch-all. Keyed by the raw
/// CSIL rule name, matching the other three classification maps.
fn swift_mixed_enum_choices(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, Vec<CsilLiteralValue>> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| {
            let choices = match &rule.rule_type {
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => choices,
                CsilRuleType::TypeChoice(choices) => choices,
                _ => return None,
            };
            match swift_choice_shape(choices) {
                SwiftChoiceShape::MixedEnum(lits) => Some((rule.name.clone(), lits)),
                _ => None,
            }
        })
        .collect()
}

/// The CBOR encoding of a text key; comparing these lexicographically is RFC 8949
/// §4.2.1 key ordering, computed at generation time for a canonical map.
fn cbor_text_key_bytes(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let n = bytes.len() as u64;
    let mt = 3u8 << 5;
    let mut head = Vec::new();
    if n < 24 {
        head.push(mt | n as u8);
    } else if n < 0x100 {
        head.push(mt | 24);
        head.push(n as u8);
    } else {
        head.push(mt | 25);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    }
    head.extend_from_slice(bytes);
    head
}

/// Strip `.constraint` wrappers (recursively — the parser only ever nests one deep in
/// practice, but this matches `choice_arm_literal`'s recursion for safety) to reach the
/// underlying type shape a constraint decorates.
fn unwrap_constrained(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => unwrap_constrained(base_type),
        other => other,
    }
}

/// A Swift expression building a `CsilCborValue` from `expr` (a typed value).
fn swift_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!(".int({expr})"),
            "uint" => format!(".uint({expr})"),
            "float" | "float64" | "double" => format!(".double({expr})"),
            "text" | "tstr" => format!(".text({expr})"),
            "bytes" | "bstr" => format!(".bytes({expr})"),
            "bool" => format!(".bool({expr})"),
            "timestamp" => format!(".tag(0, .text({expr}))"),
            // Swift carries `decimal` as canonical text; the tag-4 form is a follow-up.
            "decimal" => format!(".text({expr})"),
            "nil" | "null" => ".null".to_string(),
            _ => ".null".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&swift_type_name(name)) => {
            format!("{expr}.toCborValue()")
        }
        // A reference to a named choice rule — tagged union (`emit_union_codec`) or
        // closed string-literal enum (`emit_string_enum_codec`) alike, and likewise for
        // a choice hoisted out of an inline field position (`csilgen_common::hoist_inline_composites`) —
        // has its own `toCborValue()`/`init(cborValue:)`, exactly like a record's.
        CsilTypeExpression::Reference(name) if choice_codecs.contains(name) => {
            format!("{expr}.toCborValue()")
        }
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`,
        // `Tags = [* text]`, `Uuid = text`) has no codec of its own; the Swift
        // `typealias` makes the field value already a dictionary/array/scalar, so we
        // encode it as the underlying type rather than stubbing it to `.null`.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            swift_enc_value(&aliases[name], expr, records, aliases, choice_codecs)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = swift_enc_value(element_type, "$0", records, aliases, choice_codecs);
            format!("CsilCborValue.array({expr}.map {{ {inner} }})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let k = swift_enc_value(key, "$0.key", records, aliases, choice_codecs);
            let v = swift_enc_value(value, "$0.value", records, aliases, choice_codecs);
            format!("CsilCborValue.map({expr}.map {{ ({k}, {v}) }})")
        }
        // A tuple is a fixed-shape CBOR array, one element per position; `map_tuple`
        // collapses a single-element tuple to a bare Swift value (no field access), so
        // the encoder must match — `expr` is already that one element's own value.
        CsilTypeExpression::Tuple(group) if group.entries.len() == 1 => {
            let inner = swift_enc_value(
                &group.entries[0].value_type,
                expr,
                records,
                aliases,
                choice_codecs,
            );
            format!("CsilCborValue.array([{inner}])")
        }
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let accessor = format!("{expr}.{}", tuple_field_label(group, index));
                    swift_enc_value(
                        &entry.value_type,
                        &accessor,
                        records,
                        aliases,
                        choice_codecs,
                    )
                })
                .collect();
            format!("CsilCborValue.array([{}])", parts.join(", "))
        }
        CsilTypeExpression::Choice(choices) if choice_is_stringy(choices) => {
            format!(".text({expr})")
        }
        CsilTypeExpression::Literal(lit) => swift_literal_cbor_expr(lit),
        _ => ".null".to_string(),
    }
}

/// A Swift (throwing) expression decoding a typed value from `expr` (a `CsilCborValue`).
fn swift_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("try CsilCbor.asI64({expr})"),
            "uint" => format!("try CsilCbor.asU64({expr})"),
            "float" | "float64" | "double" => format!("try CsilCbor.asDouble({expr})"),
            "text" | "tstr" => format!("try CsilCbor.asText({expr})"),
            "bytes" | "bstr" => format!("try CsilCbor.asBytes({expr})"),
            "bool" => format!("try CsilCbor.asBool({expr})"),
            "timestamp" => format!("try CsilCbor.asTaggedText({expr}, 0)"),
            "decimal" => format!("try CsilCbor.asText({expr})"),
            _ => format!("try CsilCbor.asText({expr})"),
        },
        CsilTypeExpression::Reference(name) if records.contains(&swift_type_name(name)) => {
            format!("try {}(cborValue: {expr})", swift_type_name(name))
        }
        // A reference to a named choice rule (tagged union, string enum, or one
        // hoisted from an inline field position, per `csilgen_common::hoist_inline_composites`) decodes via its own
        // `init(cborValue:)` — see `emit_union_codec`/`emit_string_enum_codec`.
        CsilTypeExpression::Reference(name) if choice_codecs.contains(name) => {
            format!("try {}(cborValue: {expr})", swift_type_name(name))
        }
        // A reference to a transparent alias decodes as its underlying type; the
        // dictionary/array/scalar the underlying decoder yields is exactly the
        // `typealias`-named field's type, so it assigns through without a cast.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            swift_dec_value(&aliases[name], expr, records, aliases, choice_codecs)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = swift_dec_value(element_type, "$0", records, aliases, choice_codecs);
            format!("try CsilCbor.asArray({expr}).map {{ {inner} }}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kt = map_type(key, false);
            let vt = map_type(value, false);
            let k = swift_dec_value(key, "$1.0", records, aliases, choice_codecs);
            let v = swift_dec_value(value, "$1.1", records, aliases, choice_codecs);
            format!("try CsilCbor.asMap({expr}).reduce(into: [{kt}: {vt}]()) {{ $0[{k}] = {v} }}")
        }
        // Mirrors the encoder: a fixed-shape CBOR array, positionally decoded; a
        // single-element tuple decodes straight to its one element (no field access),
        // matching `map_tuple`'s Swift-side collapse.
        CsilTypeExpression::Tuple(group) if group.entries.len() == 1 => {
            let elem_access = format!("(try CsilCbor.asArray({expr})[0])");
            swift_dec_value(
                &group.entries[0].value_type,
                &elem_access,
                records,
                aliases,
                choice_codecs,
            )
        }
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let elem_access = format!("(try CsilCbor.asArray({expr})[{index}])");
                    let dec = swift_dec_value(
                        &entry.value_type,
                        &elem_access,
                        records,
                        aliases,
                        choice_codecs,
                    );
                    format!("{}: {dec}", tuple_field_label(group, index))
                })
                .collect();
            format!("({})", parts.join(", "))
        }
        CsilTypeExpression::Choice(choices) if choice_is_stringy(choices) => {
            format!("try CsilCbor.asText({expr})")
        }
        CsilTypeExpression::Literal(lit) => {
            let expected = swift_literal_cbor_expr(lit);
            let value = swift_literal_value_expr(lit);
            format!("try CsilCbor.expectLiteral({expr}, {expected}, {value})")
        }
        _ => format!("try CsilCbor.asText({expr})"),
    }
}

/// Emit the `extension <Type>` carrying the record's codec.
fn emit_struct_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> String {
    let type_name = swift_type_name(name);
    // (member, wire, entry, effective type) in declaration order, and a
    // canonical-key-order copy for the encoder so the wire map is deterministic.
    // `e.value_type` is already the post-hoist type (see `build_files`): an inline
    // choice this field carried has been rewritten to a `Reference` to its
    // synthesized named choice by `csilgen_common::hoist_inline_composites`.
    let named: Vec<(String, String, &CsilGroupEntry, CsilTypeExpression)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let member = entry_field_name(e)?;
            let wire = e.key.as_ref().and_then(wire_key)?;
            Some((member, wire, e, e.value_type.clone()))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry, CsilTypeExpression)> =
        named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this record (deep, canonical key order).\n");
    out.push_str("    func toCborValue() -> CsilCborValue {\n");
    out.push_str("        var csilEntries: [(CsilCborValue, CsilCborValue)] = []\n");
    for (member, wire, entry, ty) in &canonical {
        let wire_lit = swift_string_lit(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let enc = swift_enc_value(ty, "csilV", records, aliases, choice_codecs);
            out.push_str(&format!(
                "        if let csilV = self.{member} {{ csilEntries.append(({wire_lit}, {enc})) }}\n"
            ));
        } else {
            let enc = swift_enc_value(
                ty,
                &format!("self.{member}"),
                records,
                aliases,
                choice_codecs,
            );
            out.push_str(&format!(
                "        csilEntries.append(({wire_lit}, {enc}))\n"
            ));
        }
    }
    out.push_str("        return .map(csilEntries)\n    }\n\n");

    out.push_str("    /// Reconstruct this record from a decoded CBOR value tree.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    for (member, wire, entry, ty) in &named {
        let wire_lit = swift_string_lit(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let opt_ty = map_type(ty, true);
            let dec = swift_dec_value(ty, "csilV", records, aliases, choice_codecs);
            out.push_str(&format!(
                "        let {member}: {opt_ty} = if let csilV = CsilCbor.mapGet(cborValue, {wire_lit}) {{ {dec} }} else {{ nil }}\n"
            ));
        } else {
            let dec = swift_dec_value(
                ty,
                &format!("(try CsilCbor.require(cborValue, {wire_lit}))"),
                records,
                aliases,
                choice_codecs,
            );
            out.push_str(&format!("        let {member} = {dec}\n"));
        }
    }
    let init_args: Vec<String> = named
        .iter()
        .map(|(member, _, _, _)| format!("{member}: {member}"))
        .collect();
    out.push_str(&format!("        self.init({})\n", init_args.join(", ")));
    out.push_str("    }\n\n");

    out.push_str("    /// Encode this record to canonical CSIL CBOR bytes.\n");
    out.push_str("    func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }\n\n");
    out.push_str("    /// Decode a CSIL CBOR byte payload into this record.\n");
    out.push_str(&format!(
        "    static func fromCbor(_ bytes: [UInt8]) throws -> {type_name} {{ try {type_name}(cborValue: CsilCbor.decode(bytes)) }}\n"
    ));
    out.push_str("}\n\n");
    out
}

/// Emit the `extension <Type>` carrying a named union rule's codec: a `[index, payload]`
/// tagged sum, 0-based declaration order, matching `emit_enum`'s case naming
/// (`enum_case_name`) so the type and its codec never drift. The Swift `enum` case
/// itself is already the discriminant (same design as the Dart/Zig generators' tagged
/// sum/sealed-class-per-arm types), so encode has no value-equality ambiguity to
/// resolve the way Go's `interface{}`-backed union does: a literal-typed arm's payload
/// is its own declared literal (`swift_enc_value` routes `Literal` through
/// `swift_literal_cbor_expr`, which ignores the wrapped associated value), and decode
/// validates that same literal via `CsilCbor.expectLiteral` on the way back in.
fn emit_union_codec(
    name: &str,
    choices: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> String {
    let type_name = swift_type_name(name);
    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this variant: a `[index, payload]` tagged sum\n");
    out.push_str("    /// (0-based declaration order is the wire contract).\n");
    out.push_str("    func toCborValue() -> CsilCborValue {\n");
    out.push_str("        switch self {\n");
    for (index, choice) in choices.iter().enumerate() {
        let case = enum_case_name(choice, index);
        let enc = swift_enc_value(choice, "csilV", records, aliases, choice_codecs);
        out.push_str(&format!(
            "        case .{case}(let csilV):\n            return .array([.uint({index}), {enc}])\n"
        ));
    }
    out.push_str("        }\n    }\n\n");

    out.push_str("    /// Reconstruct this variant from a decoded CBOR value tree.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    out.push_str("        let csilElems = try CsilCbor.asArray(cborValue)\n");
    out.push_str("        guard csilElems.count == 2 else { throw CsilCborError.malformed }\n");
    out.push_str("        let csilIndex = try CsilCbor.asU64(csilElems[0])\n");
    out.push_str("        let csilPayload = csilElems[1]\n");
    out.push_str("        switch csilIndex {\n");
    for (index, choice) in choices.iter().enumerate() {
        let case = enum_case_name(choice, index);
        let dec = swift_dec_value(choice, "csilPayload", records, aliases, choice_codecs);
        out.push_str(&format!("        case {index}: self = .{case}({dec})\n"));
    }
    out.push_str("        default: throw CsilCborError.typeMismatch\n");
    out.push_str("        }\n    }\n\n");

    out.push_str("    /// Encode this variant to canonical CSIL CBOR bytes.\n");
    out.push_str("    func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }\n\n");
    out.push_str("    /// Decode a CSIL CBOR byte payload into this variant.\n");
    out.push_str(&format!(
        "    static func fromCbor(_ bytes: [UInt8]) throws -> {type_name} {{ try {type_name}(cborValue: CsilCbor.decode(bytes)) }}\n"
    ));
    out.push_str("}\n\n");
    out
}

/// Emit the `extension <Type>` carrying a closed string-literal enum's codec (a named
/// rule where `all_text_literals` holds, or a choice hoisted from an inline field
/// position with the same shape — see `csilgen_common::hoist_inline_composites`). The bare CBOR text is
/// the enum's own wire discriminant (there's no other arm to distinguish it from), so
/// encode is just the `rawValue`; decode goes through the failable `init(rawValue:)`
/// so an unrecognized wire string throws instead of being silently accepted — the
/// "decode-time membership validation" half of the literal-enum wire contract.
fn emit_string_enum_codec(type_name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this enum: its wire string verbatim.\n");
    out.push_str("    func toCborValue() -> CsilCborValue { .text(self.rawValue) }\n\n");
    out.push_str(
        "    /// Reconstruct this enum from a decoded CBOR value tree, rejecting a wire\n",
    );
    out.push_str("    /// string outside the declared closed set.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    out.push_str("        let csilS = try CsilCbor.asText(cborValue)\n");
    out.push_str(&format!(
        "        guard let csilV = {type_name}(rawValue: csilS) else {{ throw CsilCborError.typeMismatch }}\n"
    ));
    out.push_str("        self = csilV\n");
    out.push_str("    }\n\n");
    out.push_str("    /// Encode this enum to canonical CSIL CBOR bytes.\n");
    out.push_str("    func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }\n\n");
    out.push_str("    /// Decode a CSIL CBOR byte payload into this enum.\n");
    out.push_str(&format!(
        "    static func fromCbor(_ bytes: [UInt8]) throws -> {type_name} {{ try {type_name}(cborValue: CsilCbor.decode(bytes)) }}\n"
    ));
    out.push_str("}\n\n");
    out
}

/// A Swift array-literal expression for a byte string, used only by
/// `emit_scalar_enum_codec_manual`'s bytes-literal switch patterns — `literal_to_swift`
/// stubs `Bytes` to `"[]"` (it's only ever called for a *default value* expression
/// elsewhere, never a bytes literal), so the actual byte values need their own literal
/// builder here.
fn swift_bytes_array_literal(bytes: &[u8]) -> String {
    let values = bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{values}]")
}

/// The literal pattern text for one arm of `emit_scalar_enum_codec_manual`'s decode
/// switch: a plain `true`/`false` for a bool (Swift's built-in equality-based
/// `~=` on `Bool` matches it directly), or a `[UInt8]` array literal for bytes (matched
/// the same way via the stdlib's `~=` overload for any `Equatable`).
fn scalar_case_pattern(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Bytes(bytes) => swift_bytes_array_literal(bytes),
        _ => literal_to_swift(lit),
    }
}

/// Emit the `extension <Type>` carrying a scalar-literal enum's codec (`emit_scalar_enum`'s
/// int/float raw-value-backed shape). Same bare-literal wire contract as
/// `emit_string_enum_codec`, just numeric: encode returns the member's own `rawValue` as
/// its canonical CBOR scalar (an int splits `.uint`/`.int` by sign the same way a
/// standalone int literal field already does, per `swift_literal_cbor_expr`), decode
/// reads that raw scalar and maps it back through the failable `init(rawValue:)`,
/// throwing `CsilCborError.typeMismatch` — the same error the string-enum and
/// tagged-union codecs already throw for an out-of-set value — on anything outside the
/// declared closed set.
fn emit_scalar_enum_codec_raw(type_name: &str, is_int: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this enum: its wire literal verbatim.\n");
    if is_int {
        out.push_str("    func toCborValue() -> CsilCborValue {\n");
        out.push_str("        let csilV = self.rawValue\n");
        out.push_str("        return csilV >= 0 ? .uint(UInt64(csilV)) : .int(csilV)\n");
        out.push_str("    }\n\n");
    } else {
        out.push_str("    func toCborValue() -> CsilCborValue { .double(self.rawValue) }\n\n");
    }
    out.push_str(
        "    /// Reconstruct this enum from a decoded CBOR value tree, rejecting a scalar\n",
    );
    out.push_str("    /// outside the declared closed set.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    let as_fn = if is_int { "asI64" } else { "asDouble" };
    out.push_str(&format!(
        "        let csilS = try CsilCbor.{as_fn}(cborValue)\n"
    ));
    out.push_str(&format!(
        "        guard let csilV = {type_name}(rawValue: csilS) else {{ throw CsilCborError.typeMismatch }}\n"
    ));
    out.push_str("        self = csilV\n");
    out.push_str("    }\n\n");
    out.push_str("    /// Encode this enum to canonical CSIL CBOR bytes.\n");
    out.push_str("    func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }\n\n");
    out.push_str("    /// Decode a CSIL CBOR byte payload into this enum.\n");
    out.push_str(&format!(
        "    static func fromCbor(_ bytes: [UInt8]) throws -> {type_name} {{ try {type_name}(cborValue: CsilCbor.decode(bytes)) }}\n"
    ));
    out.push_str("}\n\n");
    out
}

/// Emit the `extension <Type>` carrying a scalar-literal enum's codec for the two kinds
/// that can't be native raw-value enums (`emit_scalar_enum`'s manual/no-raw-value shape
/// for bool/bytes — see `scalar_swift_raw_type`). Same bare-literal wire contract as
/// `emit_scalar_enum_codec_raw`, just hand-switched instead of routed through
/// `RawRepresentable`: encode switches on the case to its own literal, decode reads the
/// raw scalar and switches it back to a case, throwing `CsilCborError.typeMismatch` on
/// anything outside the declared closed set.
fn emit_scalar_enum_codec_manual(type_name: &str, lits: &[CsilLiteralValue]) -> String {
    let names = scalar_enum_case_names(lits);
    let is_bool = matches!(lits.first(), Some(CsilLiteralValue::Bool(_)));
    let as_fn = if is_bool { "asBool" } else { "asBytes" };
    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this enum: its wire literal verbatim.\n");
    out.push_str("    func toCborValue() -> CsilCborValue {\n");
    out.push_str("        switch self {\n");
    for (case_name, lit) in names.iter().zip(lits) {
        out.push_str(&format!(
            "        case .{case_name}: return {}\n",
            swift_literal_cbor_expr(lit)
        ));
    }
    out.push_str("        }\n    }\n\n");
    out.push_str(
        "    /// Reconstruct this enum from a decoded CBOR value tree, rejecting a scalar\n",
    );
    out.push_str("    /// outside the declared closed set.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    out.push_str(&format!(
        "        let csilS = try CsilCbor.{as_fn}(cborValue)\n"
    ));
    out.push_str("        switch csilS {\n");
    for (case_name, lit) in names.iter().zip(lits) {
        out.push_str(&format!(
            "        case {}: self = .{case_name}\n",
            scalar_case_pattern(lit)
        ));
    }
    out.push_str("        default: throw CsilCborError.typeMismatch\n");
    out.push_str("        }\n    }\n\n");
    out.push_str("    /// Encode this enum to canonical CSIL CBOR bytes.\n");
    out.push_str("    func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }\n\n");
    out.push_str("    /// Decode a CSIL CBOR byte payload into this enum.\n");
    out.push_str(&format!(
        "    static func fromCbor(_ bytes: [UInt8]) throws -> {type_name} {{ try {type_name}(cborValue: CsilCbor.decode(bytes)) }}\n"
    ));
    out.push_str("}\n\n");
    out
}

/// Emit the `extension <Type>` carrying a closed scalar-literal choice's codec (a named
/// rule, or a choice hoisted from an inline field position, where every arm is a
/// same-kind int/float/bool/bytes literal — see `all_scalar_literals`) — the non-text
/// sibling of `emit_string_enum_codec`, dispatching to the native raw-value shape for
/// int/float or the manual shape for bool/bytes per `scalar_swift_raw_type`.
fn emit_scalar_enum_codec(type_name: &str, lits: &[CsilLiteralValue]) -> String {
    match lits.first() {
        Some(CsilLiteralValue::Integer(_)) => emit_scalar_enum_codec_raw(type_name, true),
        Some(CsilLiteralValue::Float(_)) => emit_scalar_enum_codec_raw(type_name, false),
        _ => emit_scalar_enum_codec_manual(type_name, lits),
    }
}

/// The `CsilCborValue` pattern matching ONE arm of a MIXED-kind literal enum's
/// decoded wire value directly — unlike `emit_scalar_enum_codec_manual`'s
/// `scalar_case_pattern` (which switches on an already-kind-unwrapped scalar via a
/// single shared `CsilCbor.as*` accessor), a mixed-kind enum has no single accessor
/// that covers every arm, so decode switches on the `CsilCborValue` tree node's own
/// case instead (see `emit_mixed_enum_codec`). Deliberately NOT `swift_literal_cbor_
/// expr` (which wraps an int in an explicit `UInt64(...)`/`Int64(...)` conversion
/// call for use as an EXPRESSION) — a Swift `case` pattern needs a bare literal
/// (`case .uint(1):`), not a call expression, so the associated-value payload here
/// is always a plain literal token.
fn mixed_case_pattern(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Integer(i) if *i >= 0 => format!(".uint({i})"),
        CsilLiteralValue::Integer(i) => format!(".int({i})"),
        CsilLiteralValue::Float(f) => format!(".double({f})"),
        CsilLiteralValue::Text(s) => format!(".text({})", swift_string_lit(s)),
        CsilLiteralValue::Bool(b) => format!(".bool({b})"),
        CsilLiteralValue::Bytes(bytes) => format!(".bytes({})", swift_bytes_array_literal(bytes)),
        // `all_mixed_literals` only ever collects text/int/float/bool/bytes arms
        // (the same kinds `all_text_literals`/`all_scalar_literals` recognize), so
        // a null/array literal never reaches here; kept for match exhaustiveness.
        _ => ".null".to_string(),
    }
}

/// Emit the `extension <Type>` carrying a MIXED-kind literal enum's codec (see
/// `all_mixed_literals`/`emit_mixed_enum`) — the third bare-literal wire shape
/// alongside `emit_string_enum_codec`/`emit_scalar_enum_codec`: encode switches on
/// the Swift case to its own literal's `CsilCborValue` (`swift_literal_cbor_expr`,
/// same as every other literal-carrying codec path in this generator); decode
/// switches on the DECODED `CsilCborValue` tree node's own case directly (no single
/// `CsilCbor.as*` accessor spans every arm's kind the way it does for a uniform-kind
/// scalar enum), throwing `CsilCborError.typeMismatch` — the same error every other
/// enum/union codec in this generator throws — on anything outside the declared
/// closed set.
fn emit_mixed_enum_codec(type_name: &str, lits: &[CsilLiteralValue]) -> String {
    let names = mixed_enum_case_names(lits);
    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this enum: its wire literal verbatim.\n");
    out.push_str("    func toCborValue() -> CsilCborValue {\n");
    out.push_str("        switch self {\n");
    for (case_name, lit) in names.iter().zip(lits) {
        out.push_str(&format!(
            "        case .{case_name}: return {}\n",
            swift_literal_cbor_expr(lit)
        ));
    }
    out.push_str("        }\n    }\n\n");
    out.push_str(
        "    /// Reconstruct this enum from a decoded CBOR value tree, rejecting a value\n",
    );
    out.push_str("    /// outside the declared closed set. Mixed literal kinds mean no single\n");
    out.push_str("    /// `CsilCbor.as*` accessor covers every arm, so this switches on the\n");
    out.push_str("    /// value tree's own case directly instead.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    out.push_str("        switch cborValue {\n");
    for (case_name, lit) in names.iter().zip(lits) {
        out.push_str(&format!(
            "        case {}: self = .{case_name}\n",
            mixed_case_pattern(lit)
        ));
    }
    out.push_str("        default: throw CsilCborError.typeMismatch\n");
    out.push_str("        }\n    }\n\n");
    out.push_str("    /// Encode this enum to canonical CSIL CBOR bytes.\n");
    out.push_str("    func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }\n\n");
    out.push_str("    /// Decode a CSIL CBOR byte payload into this enum.\n");
    out.push_str(&format!(
        "    static func fromCbor(_ bytes: [UInt8]) throws -> {type_name} {{ try {type_name}(cborValue: CsilCbor.decode(bytes)) }}\n"
    ));
    out.push_str("}\n\n");
    out
}

/// Whether `swift_enc_value`/`swift_dec_value` model an op-boundary type faithfully, so
/// a per-op codec helper round-trips it rather than silently stubbing to `.null`. Records,
/// named choice rules (tagged unions and string enums alike), builtins, transparent
/// aliases, arrays, maps, and stringy choices all reach a real builder. An inline
/// non-stringy choice has no wire discriminator here, a tuple has no field-builder of
/// its own (unlike the Go generator), and an unmodeled reference has no codec — those
/// keep the client's skip-with-note fallback.
fn swift_op_boundary_expressible(
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> bool {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(_) => true,
        CsilTypeExpression::Reference(name) => {
            records.contains(&swift_type_name(name))
                || aliases.contains_key(name)
                || choice_codecs.contains(name)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            swift_op_boundary_expressible(element_type, records, aliases, choice_codecs)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            swift_op_boundary_expressible(key, records, aliases, choice_codecs)
                && swift_op_boundary_expressible(value, records, aliases, choice_codecs)
        }
        CsilTypeExpression::Choice(choices) => choice_is_stringy(choices),
        _ => false,
    }
}

/// The `<Base><Op>` stem shared by an op's per-op codec helpers and the client method
/// that calls them, so the two never drift (`Member` + `GetMember` → `MemberGetMember`).
fn op_codec_stem(service_name: &str, op: &CsilServiceOperation) -> String {
    format!(
        "{}{}",
        service_base(service_name),
        swift_type_name(&op.name)
    )
}

/// Per-op CBOR helpers for non-record op boundaries (scalar-id requests, `[T]`/map/scalar
/// responses), so the client owns one byte seam for every op and a consumer-side server can
/// compose `decode(request)`/`encode(response)` for shapes records never covered. Records
/// keep their `toCbor`/`fromCbor` methods; this only adds the op-keyed free functions the
/// non-record path needs, so a record-only spec's codec stays byte-identical.
fn emit_op_codecs(
    input: &WasmGeneratorInput,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> String {
    let mut out = String::new();
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let null_input = is_null_input(&op.input_type);
            let req_ok = null_input
                || swift_op_boundary_expressible(&op.input_type, records, aliases, choice_codecs);
            if !req_ok || !swift_op_boundary_expressible(&success, records, aliases, choice_codecs)
            {
                continue;
            }
            let stem = op_codec_stem(&rule.name, op);
            // A null input carries no request body, and a record boundary already has
            // `toCbor`/`fromCbor`; only the non-record halves need a per-op helper.
            if !null_input && !is_record_ref(&op.input_type, records) {
                out.push_str(&emit_op_codec_pair(
                    &format!("{stem}Request"),
                    &op.input_type,
                    records,
                    aliases,
                    choice_codecs,
                ));
            }
            if !is_record_ref(&success, records) {
                out.push_str(&emit_op_codec_pair(
                    &format!("{stem}Response"),
                    &success,
                    records,
                    aliases,
                    choice_codecs,
                ));
            }
        }
    }
    out
}

/// One `encode<Name>`/`decode<Name>` free-function pair over the same value builders the
/// record codec uses, giving an arbitrary op-boundary shape the byte seam a record has.
fn emit_op_codec_pair(
    helper: &str,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
) -> String {
    let swift_type = map_type(ty, false);
    let enc = swift_enc_value(ty, "value", records, aliases, choice_codecs);
    let dec = swift_dec_value(ty, "csilRoot", records, aliases, choice_codecs);
    format!(
        "/// Encode the {helper} payload to canonical CSIL CBOR bytes.\n\
         public func encode{helper}(_ value: {swift_type}) -> [UInt8] {{\n\
         {indent}CsilCbor.encode({enc})\n}}\n\n\
         /// Decode canonical CSIL CBOR bytes into the {helper} payload.\n\
         public func decode{helper}(_ bytes: [UInt8]) throws -> {swift_type} {{\n\
         {indent}let csilRoot = try CsilCbor.decode(bytes)\n\
         {indent}return {dec}\n}}\n\n",
        indent = "    "
    )
}

/// Build `Codec.swift`: the self-contained CBOR runtime plus a codec extension per
/// record and per named union. `None` when the spec declares neither.
fn generate_codec(input: &WasmGeneratorInput) -> Option<String> {
    let records = swift_record_names(input);
    let unions = swift_union_choices(input);
    let string_enums = swift_string_enum_choices(input);
    let scalar_enums = swift_scalar_enum_choices(input);
    let mixed_enums = swift_mixed_enum_choices(input);
    if records.is_empty()
        && unions.is_empty()
        && string_enums.is_empty()
        && scalar_enums.is_empty()
        && mixed_enums.is_empty()
    {
        return None;
    }
    let aliases = swift_codec_aliases(input);
    // Every named choice rule with its own `toCborValue()`/`init(cborValue:)` — tagged
    // union, closed string enum, closed scalar-literal enum, and closed mixed-literal
    // enum alike — so `swift_enc_value`/`swift_dec_value` route a `Reference` to one
    // through that codec instead of the generic fallback. `input.csil_spec` has already
    // been through `csilgen_common::hoist_inline_composites` (see `build_files`), so a
    // choice hoisted out of an inline field position is just another rule these four
    // maps pick up the same way they pick up an author-declared choice rule — no
    // separate hoisted-types set to chain in.
    let choice_codecs: std::collections::HashSet<String> = unions
        .keys()
        .chain(string_enums.keys())
        .chain(scalar_enums.keys())
        .chain(mixed_enums.keys())
        .cloned()
        .collect();
    let mut body = String::new();
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            body.push_str(&emit_struct_codec(
                &rule.name,
                group,
                &records,
                &aliases,
                &choice_codecs,
            ));
        }
        // Declaration-order lookup into the (unordered) maps, so the emitted codec
        // order matches the spec rather than HashMap iteration order.
        if let Some(choices) = unions.get(&rule.name) {
            body.push_str(&emit_union_codec(
                &rule.name,
                choices,
                &records,
                &aliases,
                &choice_codecs,
            ));
        } else if string_enums.contains_key(&rule.name) {
            body.push_str(&emit_string_enum_codec(&swift_type_name(&rule.name)));
        } else if let Some(lits) = scalar_enums.get(&rule.name) {
            body.push_str(&emit_scalar_enum_codec(&swift_type_name(&rule.name), lits));
        } else if let Some(lits) = mixed_enums.get(&rule.name) {
            body.push_str(&emit_mixed_enum_codec(&swift_type_name(&rule.name), lits));
        }
    }
    // Per-op byte helpers for non-record/non-union op boundaries, so the client and a
    // consumer-side server share one codec surface for every op, not just record↔record.
    body.push_str(&emit_op_codecs(input, &records, &aliases, &choice_codecs));
    let mut content = header("Generated CBOR (de)serializers for the CSIL value types.");
    content.push_str(CODEC_RUNTIME_SWIFT);
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

/// Whether an operation's success type is a record the codec can (de)serialize, so
/// the typed client method can call `toCbor`/`fromCbor` directly.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(n) if records.contains(&swift_type_name(n)))
}

/// The caller-supplied byte carrier the generated client delegates to. The seam's `call`
/// is `async throws` for the async shape (it owns the network round-trip); the blocking
/// shape keeps the plain `throws`. The protocol name is marked (`AsyncCsilTransport`)
/// only for the twin so it coexists with the blocking `CsilTransport` in one module.
fn transport_protocol_swift(shape: ClientShape) -> String {
    let name = shape.transport_name();
    let effects = shape.effects();
    // The carrier note tracks the seam's concurrency model so a reader of the generated
    // source knows whether to supply a blocking or suspending implementation.
    let carrier_note = if shape.is_async {
        "Asynchronous — the seam suspends on the I/O round-trip."
    } else {
        "Synchronous and blocking — the host owns the I/O loop."
    };
    format!(
        "/// The caller-supplied byte carrier: it performs the call named by (service, op)\n/// with the already-encoded request bytes and returns the response bytes, or\n/// throws. {carrier_note} The generated\n/// client owns (de)serialization; the carrier only moves bytes.\npublic protocol {name} {{\n    func call(service: String, op: String, request: [UInt8]) {effects} -> [UInt8]\n}}\n"
    )
}

/// Client scaffolding emitted once at the top of the client file: the shared error type
/// (primary file only) and the caller-supplied transport seam for this shape.
fn client_prelude_swift(shape: ClientShape) -> String {
    let mut out = String::new();
    if shape.marker.is_empty() {
        out.push_str(CLIENT_ERROR_SWIFT);
        out.push('\n');
    }
    out.push_str(&transport_protocol_swift(shape));
    out
}

fn generate_client(input: &WasmGeneratorInput, shape: ClientShape) -> Option<String> {
    let records = swift_record_names(input);
    let aliases = swift_codec_aliases(input);
    let unions = swift_union_choices(input);
    let string_enums = swift_string_enum_choices(input);
    let scalar_enums = swift_scalar_enum_choices(input);
    let mixed_enums = swift_mixed_enum_choices(input);
    let choice_codecs: std::collections::HashSet<String> = unions
        .keys()
        .chain(string_enums.keys())
        .chain(scalar_enums.keys())
        .chain(mixed_enums.keys())
        .cloned()
        .collect();
    let mut body = String::new();
    let mut any = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_client_struct(
                &rule.name,
                service,
                &records,
                &aliases,
                &choice_codecs,
                shape,
            ));
            // Wire-id ordinals are a module-level `enum` shared across both client shapes;
            // only the primary file emits them so the twin never redeclares the enum.
            if shape.marker.is_empty() {
                body.push_str(&emit_wire_ids(&rule.name, service));
            }
            any = true;
        }
    }
    if !any {
        return None;
    }
    let mut content = header("Generated CSIL service clients.");
    content.push_str(&client_prelude_swift(shape));
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

fn emit_client_struct(
    name: &str,
    service: &CsilServiceDefinition,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choice_codecs: &std::collections::HashSet<String>,
    shape: ClientShape,
) -> String {
    let base = service_base(name);
    let client = shape.client_name(&base);
    let transport = shape.transport_name();
    let effects = shape.effects();
    let await_kw = shape.await_kw();
    // Canonical wire strings: the verbatim CSIL service and op names
    // (csil-rpc-transport.md §1.1).
    let wire_service = name;
    let mut out = String::new();
    out.push_str(&format!(
        "/// {client} is a typed client for the {name} service. The client owns\n/// (de)serialization; the carrier only moves bytes.\n"
    ));
    out.push_str(&format!("public struct {client} {{\n"));
    out.push_str(&format!("    public let transport: {transport}\n"));
    out.push_str(&format!("    public init(transport: {transport}) {{\n"));
    out.push_str("        self.transport = transport\n");
    out.push_str("    }\n\n");

    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            out.push_str(&format!(
                "    // channel operation '{}' is not part of the RPC client\n",
                op.name
            ));
            continue;
        }
        let success = success_type(&op.output_type);
        let null_input = is_null_input(&op.input_type);
        let req_ok = null_input
            || swift_op_boundary_expressible(&op.input_type, records, aliases, choice_codecs);
        // Only a genuinely inexpressible boundary (an inline non-stringy choice with no wire
        // discriminator, a tuple the field builders don't model, or an unmodeled reference)
        // is skipped now; scalar/array/map/union shapes ride the per-op codec helpers, so
        // every other op gets a method.
        if !req_ok || !swift_op_boundary_expressible(&success, records, aliases, choice_codecs) {
            out.push_str(&format!(
                "    // operation '{}' has a payload csilgen can't (de)serialize; handle it manually\n",
                op.name
            ));
            continue;
        }
        let method = swift_ident(&op.name);
        let output = map_type(&success, false);
        let wire_op = &op.name;
        let stem = op_codec_stem(name, op);
        // A record success reuses its `fromCbor`; any other shape uses the op's per-op decoder.
        let decode_resp = if is_record_ref(&success, records) {
            format!("{output}.fromCbor(csilResp)")
        } else {
            format!("decode{stem}Response(csilResp)")
        };
        if null_input {
            out.push_str(&format!(
                "    public func {method}() {effects} -> {output} {{\n"
            ));
            out.push_str(&format!(
                "        let csilResp = try {await_kw}transport.call(service: {}, op: {}, request: [])\n",
                swift_string_lit(wire_service),
                swift_string_lit(wire_op),
            ));
        } else {
            let input = map_type(&op.input_type, false);
            // A record request reuses its `toCbor`; any other shape uses the op's per-op encoder.
            let req_bytes = if is_record_ref(&op.input_type, records) {
                "request.toCbor()".to_string()
            } else {
                format!("encode{stem}Request(request)")
            };
            out.push_str(&format!(
                "    public func {method}(_ request: {input}) {effects} -> {output} {{\n"
            ));
            out.push_str(&format!(
                "        let csilResp = try {await_kw}transport.call(service: {}, op: {}, request: {req_bytes})\n",
                swift_string_lit(wire_service),
                swift_string_lit(wire_op),
            ));
        }
        out.push_str(&format!("        return try {decode_resp}\n"));
        out.push_str("    }\n\n");
    }
    out.push_str("}\n\n");
    out
}

// ---------------------------------------------------------------------------
// Services (server)
// ---------------------------------------------------------------------------

fn generate_services(input: &WasmGeneratorInput) -> Option<String> {
    let mut body = String::new();
    let mut any = false;
    let has_channel =
        input.csil_spec.rules.iter().any(
            |r| matches!(&r.rule_type, CsilRuleType::ServiceDef(s) if service_has_channel_ops(s)),
        );
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_service_protocol(&rule.name, service));
            body.push_str(&emit_wire_ids(&rule.name, service));
            if service_has_channel_ops(service) {
                body.push_str(&emit_channel_router(&rule.name, service));
                body.push_str(&emit_channel_router_compact(&rule.name, service));
                body.push_str(&emit_channel_encoders(&rule.name, service));
            }
            any = true;
        }
    }
    if !any {
        return None;
    }
    let mut content = header("Generated CSIL service handler protocols and routers.");
    content.push_str(SERVER_PRELUDE_SWIFT);
    if has_channel {
        content.push('\n');
        content.push_str(CODEC_PRELUDE_SWIFT);
    }
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

fn service_has_channel_ops(service: &CsilServiceDefinition) -> bool {
    service
        .operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

fn emit_service_protocol(name: &str, service: &CsilServiceDefinition) -> String {
    let type_name = swift_type_name(name);
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is the server-side handler seam.\n"
    ));
    out.push_str(&format!("public protocol {type_name} {{\n"));
    for op in &service.operations {
        let method = swift_ident(&op.name);
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_type(&success_type(&op.output_type), false);
                if is_null_input(&op.input_type) {
                    out.push_str(&format!("    func {method}() throws -> {output}\n"));
                } else {
                    let input = map_type(&op.input_type, false);
                    out.push_str(&format!(
                        "    func {method}(_ request: {input}) throws -> {output}\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let input = map_type(&op.input_type, false);
                out.push_str(&format!("    func {method}(_ msg: {input}) throws\n"));
            }
            // Server pushes only; no inbound handler method.
            CsilServiceDirection::Reverse => {}
        }
    }
    out.push_str("}\n\n");
    out
}

/// `static let` wire-id ordinals exposing `@wire-id(N)` values. Emits nothing for a
/// wire-id-free service so its output stays byte-identical.
fn emit_wire_ids(name: &str, service: &CsilServiceDefinition) -> String {
    let Some(service_id) = service.wire_id else {
        return String::new();
    };
    let type_name = swift_type_name(name);
    let mut out = String::new();
    out.push_str(&format!(
        "/// Wire-id ordinals for the {name} service (compact transport profile).\n"
    ));
    out.push_str(&format!("public enum {type_name}WireID {{\n"));
    out.push_str(&format!(
        "    public static let service: UInt64 = {service_id}\n"
    ));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            let member = swift_ident(&op.name);
            out.push_str(&format!(
                "    public static let {member}: UInt64 = {op_id}\n"
            ));
        }
    }
    out.push_str("}\n\n");
    out
}

/// Verbose-profile channel router: dispatches one inbound frame by its wire operation
/// name (kept verbatim) to the matching handler method, decoding the body via the
/// injected codec.
fn emit_channel_router(name: &str, service: &CsilServiceDefinition) -> String {
    let type_name = swift_type_name(name);
    let mut out = String::new();
    out.push_str(&format!(
        "/// route{type_name}Channel decodes one inbound channel frame and dispatches\n"
    ));
    out.push_str("/// to the matching handler method (verbose profile, keyed by op name).\n");
    out.push_str(&format!(
        "public func route{type_name}Channel(_ handler: {type_name}, codec: CsilCodec, op: String, data: [UInt8]) throws {{\n"
    ));
    out.push_str("    switch op {\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = swift_ident(&op.name);
        let input = map_type(&op.input_type, false);
        out.push_str(&format!("    case {}:\n", swift_string_lit(&op.name)));
        out.push_str(&format!(
            "        let msg = try codec.decode(data, as: {input}.self)\n"
        ));
        out.push_str(&format!("        try handler.{method}(msg)\n"));
    }
    out.push_str("    default:\n");
    out.push_str("        throw CsilTransportError.unknownOperation(op)\n");
    out.push_str("    }\n}\n\n");
    out
}

/// Compact-profile twin: dispatches by `@wire-id` ordinal instead of op name. Emitted
/// only for wire-id-bearing services, keeping wire-id-free output byte-identical.
fn emit_channel_router_compact(name: &str, service: &CsilServiceDefinition) -> String {
    if service.wire_id.is_none() {
        return String::new();
    }
    let type_name = swift_type_name(name);
    let mut out = String::new();
    out.push_str(&format!(
        "/// route{type_name}ChannelCompact dispatches one inbound channel frame by its\n"
    ));
    out.push_str(
        "/// @wire-id ordinal (compact profile). The verbose twin is the name-keyed router.\n",
    );
    out.push_str(&format!(
        "public func route{type_name}ChannelCompact(_ handler: {type_name}, codec: CsilCodec, op: UInt64, data: [UInt8]) throws {{\n"
    ));
    out.push_str("    switch op {\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let Some(op_id) = op.wire_id else { continue };
        let method = swift_ident(&op.name);
        let input = map_type(&op.input_type, false);
        out.push_str(&format!("    case {op_id}:\n"));
        out.push_str(&format!(
            "        let msg = try codec.decode(data, as: {input}.self)\n"
        ));
        out.push_str(&format!("        try handler.{method}(msg)\n"));
    }
    out.push_str("    default:\n");
    out.push_str("        throw CsilTransportError.unknownOrdinal(op)\n");
    out.push_str("    }\n}\n\n");
    out
}

/// Outbound encoders for server-pushed (`<-` reverse, or bidirectional) messages: the
/// host frames the returned (op, bytes) onto its connection.
fn emit_channel_encoders(name: &str, service: &CsilServiceDefinition) -> String {
    let type_name = swift_type_name(name);
    let mut out = String::new();
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method = swift_type_name(&op.name);
        // The pushed message is the success arm; the error half is surfaced as a
        // transport status, not encoded into the outbound frame.
        let output = map_type(&success_type(&op.output_type), false);
        out.push_str(&format!(
            "/// encode{type_name}{method} encodes a '{}' message the server pushes to a peer.\n",
            op.name
        ));
        out.push_str(&format!(
            "public func encode{type_name}{method}(codec: CsilCodec, msg: {output}) throws -> (op: String, data: [UInt8]) {{\n"
        ));
        out.push_str(&format!(
            "    (op: {}, data: try codec.encode(msg))\n",
            swift_string_lit(&op.name)
        ));
        out.push_str("}\n\n");
    }
    out
}

/// Reduce an operation output to its success type by dropping its error arm(s) — any
/// `*Error`-named reference (`ServiceError`, `UserError`, `APIError`, …). In Swift the
/// error half is *thrown* by the transport, not returned, so the typed method returns
/// just the success value rather than an unnameable inline union (which would otherwise
/// degrade to opaque `AnyCsilValue`).
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(
                |c| !matches!(c, CsilTypeExpression::Reference(name) if name.ends_with("Error")),
            )
            .cloned()
            .collect();
        match kept.len() {
            1 => kept.into_iter().next().unwrap(),
            0 => type_expr.clone(),
            _ => CsilTypeExpression::Choice(kept),
        }
    } else {
        type_expr.clone()
    }
}

// ---------------------------------------------------------------------------
// Static preludes
// ---------------------------------------------------------------------------

fn header(summary: &str) -> String {
    format!("// {summary}\n// Code generated by csilgen; DO NOT EDIT.\n\n")
}

const VALIDATION_ERROR_SWIFT: &str = "\
/// Thrown by a generated type's validate() when a field constraint is violated.
public struct CsilValidationError: Error, Equatable {
    public let message: String
    public init(_ message: String) { self.message = message }
}
";

/// Defined independently of validation: a type can reference `AnyCsilValue` (the `any`
/// core type or a non-stringy inline choice) without carrying any runtime constraint, so
/// coupling this to the validation prelude would leave it undefined and fail to compile.
const ANY_VALUE_SWIFT: &str = "\
/// An opaque CSIL value used where a generated type cannot be named precisely
/// (a non-stringy inline choice or the `any` core type). The transport carries opaque
/// payload bytes, so a consumer can refine this as needed.
public typealias AnyCsilValue = [UInt8]
";

/// The `CsilClientError` type, shared by the blocking client and its `async` twin. Only
/// the primary file (the sync/drop-in client) declares it; the twin rides in the same
/// module and reuses it, so it is never redeclared.
const CLIENT_ERROR_SWIFT: &str = "\
/// A structured error from a generated client call: a service-returned error
/// (code/message), or a transport-level failure.
public struct CsilClientError: Error, Equatable {
    public let code: Int64
    public let message: String
    public init(code: Int64, message: String) {
        self.code = code
        self.message = message
    }
}
";

const SERVER_PRELUDE_SWIFT: &str = "\
/// Transport-level failures a router can raise (distinct from application errors,
/// which ride inside the payload as a declared `/ ErrorType` arm).
public enum CsilTransportError: Error, Equatable {
    case unknownOperation(String)
    case unknownOrdinal(UInt64)
}
";

const CODEC_PRELUDE_SWIFT: &str = "\
/// The consumer-supplied (de)serialization seam for channel messages. The generator
/// is codec-agnostic; the implementer wires this to canonical CBOR (the transport
/// lib), JSON, or anything else. Synchronous and throwing — no async.
public protocol CsilCodec {
    func encode<T>(_ value: T) throws -> [UInt8]
    func decode<T>(_ data: [UInt8], as type: T.Type) throws -> T
}
";

/// The self-contained canonical-CBOR runtime the generated codecs build on,
/// Foundation-free (a `[UInt8]` byte string for `bytes`, so the wire form is a CBOR
/// byte string — major type 2 — by construction rather than relying on a Codable
/// encoder's behavior).
const CODEC_RUNTIME_SWIFT: &str = r#"/// A minimal canonical-CBOR (RFC 8949 subset) value model and codec.
public indirect enum CsilCborValue {
    case uint(UInt64)
    case int(Int64)
    case bool(Bool)
    case double(Double)
    case null
    case text(String)
    case bytes([UInt8])
    case array([CsilCborValue])
    case map([(CsilCborValue, CsilCborValue)])
    case tag(UInt64, CsilCborValue)
}

public enum CsilCborError: Error {
    case malformed
    case trailingBytes
    case missingField(String)
    case typeMismatch
}

public enum CsilCbor {
    public static func encode(_ value: CsilCborValue) -> [UInt8] {
        var out: [UInt8] = []
        enc(value, &out)
        return out
    }

    static func head(_ major: UInt8, _ n: UInt64, _ out: inout [UInt8]) {
        let mt = major << 5
        if n < 24 {
            out.append(mt | UInt8(n))
        } else if n < 0x100 {
            out.append(mt | 24)
            out.append(UInt8(n))
        } else if n < 0x1_0000 {
            out.append(mt | 25)
            out.append(UInt8(n >> 8))
            out.append(UInt8(n & 0xff))
        } else if n < 0x1_0000_0000 {
            out.append(mt | 26)
            var i = 24
            while i >= 0 { out.append(UInt8((n >> UInt64(i)) & 0xff)); i -= 8 }
        } else {
            out.append(mt | 27)
            var i = 56
            while i >= 0 { out.append(UInt8((n >> UInt64(i)) & 0xff)); i -= 8 }
        }
    }

    static func enc(_ v: CsilCborValue, _ out: inout [UInt8]) {
        switch v {
        case .uint(let n): head(0, n, &out)
        case .int(let n):
            if n >= 0 { head(0, UInt64(n), &out) } else { head(1, UInt64(-(n + 1)), &out) }
        case .bool(let b): out.append(b ? 0xf5 : 0xf4)
        case .null: out.append(0xf6)
        case .double(let d):
            out.append(0xfb)
            let bits = d.bitPattern
            var i = 56
            while i >= 0 { out.append(UInt8((bits >> UInt64(i)) & 0xff)); i -= 8 }
        case .text(let s):
            let u = Array(s.utf8)
            head(3, UInt64(u.count), &out)
            out.append(contentsOf: u)
        case .bytes(let b):
            head(2, UInt64(b.count), &out)
            out.append(contentsOf: b)
        case .array(let xs):
            head(4, UInt64(xs.count), &out)
            for x in xs { enc(x, &out) }
        case .map(let kvs):
            head(5, UInt64(kvs.count), &out)
            for (k, val) in kvs { enc(k, &out); enc(val, &out) }
        case .tag(let t, let inner):
            head(6, t, &out)
            enc(inner, &out)
        }
    }

    public static func decode(_ b: [UInt8]) throws -> CsilCborValue {
        var pos = 0
        let v = try dec(b, &pos)
        if pos != b.count { throw CsilCborError.trailingBytes }
        return v
    }

    static func readArg(_ b: [UInt8], _ pos: inout Int, _ low: UInt8) throws -> UInt64 {
        if low < 24 { pos += 1; return UInt64(low) }
        switch low {
        case 24:
            let v = UInt64(b[pos + 1]); pos += 2; return v
        case 25:
            let v = (UInt64(b[pos + 1]) << 8) | UInt64(b[pos + 2]); pos += 3; return v
        case 26:
            var v: UInt64 = 0
            for i in 1...4 { v = (v << 8) | UInt64(b[pos + i]) }
            pos += 5; return v
        case 27:
            var v: UInt64 = 0
            for i in 1...8 { v = (v << 8) | UInt64(b[pos + i]) }
            pos += 9; return v
        default:
            throw CsilCborError.malformed
        }
    }

    static func dec(_ b: [UInt8], _ pos: inout Int) throws -> CsilCborValue {
        let ib = b[pos]
        let major = ib >> 5
        let low = ib & 0x1f
        if major == 7 {
            switch low {
            case 20: pos += 1; return .bool(false)
            case 21: pos += 1; return .bool(true)
            case 22, 23: pos += 1; return .null
            case 26:
                let bits = try readArg(b, &pos, low)
                return .double(Double(Float(bitPattern: UInt32(truncatingIfNeeded: bits))))
            case 27:
                let bits = try readArg(b, &pos, low)
                return .double(Double(bitPattern: bits))
            default:
                throw CsilCborError.malformed
            }
        }
        let arg = try readArg(b, &pos, low)
        switch major {
        case 0:
            return .uint(arg)
        case 1:
            if arg > UInt64(Int64.max) { throw CsilCborError.malformed }
            return .int(-1 - Int64(arg))
        case 2:
            let n = Int(arg)
            let slice = Array(b[pos..<pos + n]); pos += n
            return .bytes(slice)
        case 3:
            let n = Int(arg)
            let s = String(decoding: b[pos..<pos + n], as: UTF8.self); pos += n
            return .text(s)
        case 4:
            let n = Int(arg)
            var items: [CsilCborValue] = []
            for _ in 0..<n { items.append(try dec(b, &pos)) }
            return .array(items)
        case 5:
            let n = Int(arg)
            var kvs: [(CsilCborValue, CsilCborValue)] = []
            for _ in 0..<n {
                let k = try dec(b, &pos)
                let v = try dec(b, &pos)
                kvs.append((k, v))
            }
            return .map(kvs)
        case 6:
            let inner = try dec(b, &pos)
            return .tag(arg, inner)
        default:
            throw CsilCborError.malformed
        }
    }

    public static func mapGet(_ v: CsilCborValue, _ key: String) -> CsilCborValue? {
        if case .map(let kvs) = v {
            for (k, val) in kvs {
                if case .text(let s) = k, s == key { return val }
            }
        }
        return nil
    }

    public static func require(_ v: CsilCborValue, _ key: String) throws -> CsilCborValue {
        guard let x = mapGet(v, key) else { throw CsilCborError.missingField(key) }
        return x
    }

    public static func expectLiteral<T>(_ actual: CsilCborValue, _ expected: CsilCborValue, _ value: T) throws -> T {
        guard valueEquals(actual, expected) else { throw CsilCborError.typeMismatch }
        return value
    }

    static func valueEquals(_ a: CsilCborValue, _ b: CsilCborValue) -> Bool {
        switch (a, b) {
        case (.uint(let x), .uint(let y)): return x == y
        case (.int(let x), .int(let y)): return x == y
        case (.bool(let x), .bool(let y)): return x == y
        case (.float(let x), .float(let y)): return x == y
        case (.null, .null): return true
        case (.text(let x), .text(let y)): return x == y
        case (.bytes(let x), .bytes(let y)): return x == y
        case (.array(let x), .array(let y)):
            guard x.count == y.count else { return false }
            return zip(x, y).allSatisfy { valueEquals($0, $1) }
        default:
            return false
        }
    }

    public static func asI64(_ v: CsilCborValue) throws -> Int64 {
        switch v {
        case .uint(let n):
            if n > UInt64(Int64.max) { throw CsilCborError.typeMismatch }
            return Int64(n)
        case .int(let n): return n
        default: throw CsilCborError.typeMismatch
        }
    }

    public static func asU64(_ v: CsilCborValue) throws -> UInt64 {
        switch v {
        case .uint(let n): return n
        case .int(let n) where n >= 0: return UInt64(n)
        default: throw CsilCborError.typeMismatch
        }
    }

    public static func asDouble(_ v: CsilCborValue) throws -> Double {
        switch v {
        case .double(let d): return d
        case .uint(let n): return Double(n)
        case .int(let n): return Double(n)
        default: throw CsilCborError.typeMismatch
        }
    }

    public static func asBool(_ v: CsilCborValue) throws -> Bool {
        if case .bool(let b) = v { return b }
        throw CsilCborError.typeMismatch
    }

    public static func asText(_ v: CsilCborValue) throws -> String {
        if case .text(let s) = v { return s }
        throw CsilCborError.typeMismatch
    }

    public static func asBytes(_ v: CsilCborValue) throws -> [UInt8] {
        if case .bytes(let b) = v { return b }
        throw CsilCborError.typeMismatch
    }

    public static func asArray(_ v: CsilCborValue) throws -> [CsilCborValue] {
        if case .array(let xs) = v { return xs }
        throw CsilCborError.typeMismatch
    }

    public static func asMap(_ v: CsilCborValue) throws -> [(CsilCborValue, CsilCborValue)] {
        if case .map(let kvs) = v { return kvs }
        throw CsilCborError.typeMismatch
    }

    public static func asTaggedText(_ v: CsilCborValue, _ num: UInt64) throws -> String {
        if case .tag(let t, let inner) = v, t == num { return try asText(inner) }
        throw CsilCborError.typeMismatch
    }
}
"#;

#[cfg(test)]
mod tests;
