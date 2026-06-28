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
    CsilControlOperator, CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilSizeConstraint, CsilTypeExpression, CsilValidationConstraint, GeneratedFile,
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
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
        match surface {
            Surface::Client => {
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
            Surface::Server => {
                if let Some(services) = generate_services(input) {
                    files.push(GeneratedFile {
                        path: "Services.swift".to_string(),
                        content: services,
                    });
                }
            }
            Surface::TypesOnly => {}
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
                path: "README.md".to_string(),
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
        let name = option_str(&input.config.options, "package_name")
            .map(str::to_string)
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
// Package README + CSIL-RPC Quickstart
// ---------------------------------------------------------------------------

/// The package README, with a copy-paste **Quickstart**. For a client package the
/// Quickstart is a complete CSIL-RPC carrier (it reuses this package's own generated
/// `CsilCbor`/`CsilCborValue` for the envelope, so it adds no third-party CBOR
/// dependency; HTTP is Foundation's blocking `URLSession`), the typed sync client built
/// over it, and one example call — a user changes only the base-URL string. A spec with
/// no usable unary operation gets the import-the-types section without a carrier.
fn swift_readme(input: &WasmGeneratorInput, pkg: &SwiftPackage) -> String {
    let module = &pkg.target;
    let name = &pkg.name;
    let mut out = format!(
        "# {name}\n\n\
         Generated by csilgen. A typed, transport-agnostic CSIL-RPC client: the\n\
         generated codec owns CBOR (de)serialization; you supply a *carrier* that only\n\
         moves bytes.\n\n\
         ## Consume\n\n\
         Add this package to your `Package.swift` dependencies and list the `{name}`\n\
         library product in your target's `dependencies`:\n\n\
         ```swift\n\
         // TODO: point at the published git URL + version once tagged.\n\
         .package(path: \"./{name}\"),\n\
         ```\n\n"
    );
    match first_swift_example(input) {
        Some(example) => out.push_str(&swift_quickstart(module, &example)),
        None => out.push_str(&format!(
            "## Quickstart\n\n\
             This package has no unary service operations — import the module and use\n\
             its generated types and codec directly:\n\n\
             ```swift\n\
             import {module}\n\
             // Foo.fromCbor(bytes) decodes; value.toCbor() encodes.\n\
             ```\n"
        )),
    }
    out
}

/// The pieces the Quickstart's example call needs: the sync client struct to construct,
/// the method to call, and a compiling sample request literal (`None` for a request-less
/// push-style op).
struct SwiftExample {
    client_struct: String,
    method: String,
    sample: Option<String>,
}

/// The first service (declared order) with a unary op whose success type — and, when
/// present, request type — is a record the generated codec covers, so the example can
/// call the clean typed sync client form. `None` for a serviceless / non-record-op spec.
fn first_swift_example(input: &WasmGeneratorInput) -> Option<SwiftExample> {
    let records = swift_record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            if !is_record_ref(&success_type(&op.output_type), &records) {
                continue;
            }
            let null_in = is_null_input(&op.input_type);
            if !null_in && !is_record_ref(&op.input_type, &records) {
                continue;
            }
            let sample = if null_in {
                None
            } else if let CsilTypeExpression::Reference(name) = &op.input_type {
                Some(swift_record_literal(
                    input,
                    name,
                    swift_find_record(input, name)?,
                ))
            } else {
                None
            };
            if !null_in && sample.is_none() {
                continue;
            }
            return Some(SwiftExample {
                // The blocking client is the unmarked `<Base>Client` (sync shape).
                client_struct: format!("{}Client", service_base(&rule.name)),
                method: swift_ident(&op.name),
                sample,
            });
        }
    }
    None
}

fn swift_quickstart(module: &str, ex: &SwiftExample) -> String {
    let mut out = String::from("## Quickstart\n\n");
    out.push_str(
        "A complete CSIL-RPC carrier (no third-party deps — it reuses this package's\n\
         generated CBOR codec for the envelope) plus the typed client over Foundation's\n\
         blocking `URLSession`. Change the one base-URL string.\n\n",
    );
    out.push_str("```swift\n");
    out.push_str("import Foundation\n");
    out.push_str(&format!("import {module}\n\n"));
    out.push_str(SWIFT_CARRIER);
    out.push('\n');
    out.push_str(&format!(
        "let client = {}(transport: CsilRpcTransport(baseURL: \"http://localhost:5080\"))\n",
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
    out.push_str("```\n");
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

/// The carrier body — identical for every spec, so it is a constant. It wraps the
/// already-encoded request in a `CsilRpcRequest` envelope (tag-24 payload) using this
/// package's generated `CsilCbor`, POSTs it to `{baseURL}/csil/v1/rpc` with a blocking
/// `URLSession`, and unwraps the `CsilRpcResponse` — surfacing a non-zero transport
/// `status` or a typed `ServiceError` arm as a thrown `CsilClientError`.
const SWIFT_CARRIER: &str = r#"// The carrier owns only the CSIL-RPC envelope + HTTP; it never touches your types.
// Hybrid posture, path 1: the envelope reuses this package's generated CsilCbor /
// CsilCborValue, so it adds no third-party CBOR dependency. HTTP is Foundation's
// URLSession driven synchronously to satisfy the blocking `CsilTransport` seam.
struct CsilRpcTransport: CsilTransport {
    let baseURL: String

    func call(service: String, op: String, request: [UInt8]) throws -> [UInt8] {
        // CsilRpcRequest = { v, service, op, payload: #6.24(bstr) }; the payload is the
        // encoded request wrapped in CBOR tag 24 (embedded CBOR).
        let envelope = CsilCborValue.map([
            (.text("v"), .uint(1)),
            (.text("op"), .text(op)),
            (.text("service"), .text(service)),
            (.text("payload"), .tag(24, .bytes(request))),
        ])
        let mount = baseURL.hasSuffix("/") ? baseURL + "csil/v1/rpc" : baseURL + "/csil/v1/rpc"
        var http = URLRequest(url: URL(string: mount)!)
        http.httpMethod = "POST"
        http.setValue("application/cbor", forHTTPHeaderField: "Content-Type")
        http.setValue("application/cbor", forHTTPHeaderField: "Accept")
        http.httpBody = Data(CsilCbor.encode(envelope))

        // Block the calling thread on the async URLSession task — the seam is sync.
        let semaphore = DispatchSemaphore(value: 0)
        var outcome: Result<[UInt8], Error> =
            .failure(CsilClientError(code: -1, message: "csil-rpc: no response"))
        URLSession.shared.dataTask(with: http) { data, response, error in
            defer { semaphore.signal() }
            if let error { outcome = .failure(error); return }
            guard let status = (response as? HTTPURLResponse)?.statusCode, status == 200,
                  let data
            else {
                outcome = .failure(
                    CsilClientError(code: -1, message: "csil-rpc: bad HTTP response"))
                return
            }
            outcome = .success([UInt8](data))
        }.resume()
        semaphore.wait()
        let responseBytes = try outcome.get()

        let env = try CsilCbor.decode(responseBytes)
        let status = try CsilCbor.asI64(CsilCbor.require(env, "status"))
        // status != 0 is a transport failure: no typed payload.
        if status != 0 {
            throw CsilClientError(code: status, message: "csil-rpc: transport status \(status)")
        }
        let payloadValue = try CsilCbor.require(env, "payload")
        guard case .tag(24, let innerValue) = payloadValue, case .bytes(let inner) = innerValue
        else {
            throw CsilClientError(
                code: -1, message: "csil-rpc: response payload is not a tag-24 byte string")
        }
        // A typed ServiceError arm is an application error, not a transport one.
        if case .text(let variant)? = CsilCbor.mapGet(env, "variant"), variant == "ServiceError" {
            let e = try CsilCbor.decode(inner)
            let code = try CsilCbor.asI64(CsilCbor.require(e, "code"))
            let message = try CsilCbor.asText(CsilCbor.require(e, "message"))
            throw CsilClientError(code: code, message: message)
        }
        return inner
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

/// The wire `service` string: the service base, **lowercased**, per
/// `docs/cbor-wire-contract.md` (`CorndogsService` → `"corndogs"`) — distinct from
/// the Swift type name, so a Swift client reaches the same endpoint as its peers.
fn wire_service_string(name: &str) -> String {
    service_base(name).to_lowercase()
}

/// The wire `op` string: the operation name PascalCased with the simple rule
/// (capitalize after `_`/`-`, leave the rest), matching the other generators
/// (`submit-task` → `"SubmitTask"`).
fn wire_op_string(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(['_', '-']) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
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

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn generate_types(input: &WasmGeneratorInput) -> Option<String> {
    let mut body = String::new();
    let mut any = false;
    let mut needs_validation = false;

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
/// string literal. Such a choice carries no more information than `String` on the wire.
fn choice_is_stringy(choices: &[CsilTypeExpression]) -> bool {
    !choices.is_empty()
        && choices.iter().all(|c| match c {
            CsilTypeExpression::Builtin(n) => n == "text" || n == "tstr",
            CsilTypeExpression::Literal(CsilLiteralValue::Text(_)) => true,
            _ => false,
        })
}

/// The verbatim wire strings of a closed string-literal choice (every arm a text
/// literal), or `None` when the choice is anything else.
fn all_text_literals(choices: &[CsilTypeExpression]) -> Option<Vec<String>> {
    if choices.is_empty() {
        return None;
    }
    let mut labels = Vec::with_capacity(choices.len());
    for choice in choices {
        match choice {
            CsilTypeExpression::Literal(CsilLiteralValue::Text(s)) => labels.push(s.clone()),
            _ => return None,
        }
    }
    Some(labels)
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

/// A variant/sum type as a Swift `enum` with associated values, one case per declared
/// choice arm. A pure string-literal set becomes a `String`-backed enum; a choice that
/// only mixes open `text` with literals collapses to `String`; otherwise reference arms
/// take the referenced struct and builtin arms take the mapped Swift type.
fn emit_enum(name: &str, choices: &[CsilTypeExpression]) -> String {
    let type_name = swift_type_name(name);
    if let Some(labels) = all_text_literals(choices) {
        return emit_string_enum(&type_name, &labels);
    }
    if choice_is_stringy(choices) {
        return format!(
            "/// {type_name} is any CSIL text value (an open string choice).\npublic typealias {type_name} = String\n\n"
        );
    }
    let mut out = String::new();
    out.push_str(&format!(
        "/// {type_name} is a generated CSIL variant (sum) type.\n"
    ));
    out.push_str(&format!(
        "public enum {type_name}: Equatable, Sendable {{\n"
    ));
    for (index, choice) in choices.iter().enumerate() {
        match choice {
            CsilTypeExpression::Reference(arm) | CsilTypeExpression::Builtin(arm) => {
                let case = swift_ident(arm);
                out.push_str(&format!("    case {}({})\n", case, map_type(choice, false)));
            }
            other => {
                out.push_str(&format!(
                    "    case case{index}({})\n",
                    map_type(other, false)
                ));
            }
        }
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

fn unwrap_constrained(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => base_type,
        other => other,
    }
}

/// A Swift expression building a `CsilCborValue` from `expr` (a typed value).
fn swift_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
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
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`,
        // `Tags = [* text]`, `Uuid = text`) has no codec of its own; the Swift
        // `typealias` makes the field value already a dictionary/array/scalar, so we
        // encode it as the underlying type rather than stubbing it to `.null`.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            swift_enc_value(&aliases[name], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = swift_enc_value(element_type, "$0", records, aliases);
            format!("CsilCborValue.array({expr}.map {{ {inner} }})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let k = swift_enc_value(key, "$0.key", records, aliases);
            let v = swift_enc_value(value, "$0.value", records, aliases);
            format!("CsilCborValue.map({expr}.map {{ ({k}, {v}) }})")
        }
        CsilTypeExpression::Choice(choices) if choice_is_stringy(choices) => {
            format!(".text({expr})")
        }
        _ => ".null".to_string(),
    }
}

/// A Swift (throwing) expression decoding a typed value from `expr` (a `CsilCborValue`).
fn swift_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
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
        // A reference to a transparent alias decodes as its underlying type; the
        // dictionary/array/scalar the underlying decoder yields is exactly the
        // `typealias`-named field's type, so it assigns through without a cast.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            swift_dec_value(&aliases[name], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = swift_dec_value(element_type, "$0", records, aliases);
            format!("try CsilCbor.asArray({expr}).map {{ {inner} }}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kt = map_type(key, false);
            let vt = map_type(value, false);
            let k = swift_dec_value(key, "$1.0", records, aliases);
            let v = swift_dec_value(value, "$1.1", records, aliases);
            format!("try CsilCbor.asMap({expr}).reduce(into: [{kt}: {vt}]()) {{ $0[{k}] = {v} }}")
        }
        CsilTypeExpression::Choice(choices) if choice_is_stringy(choices) => {
            format!("try CsilCbor.asText({expr})")
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
) -> String {
    let type_name = swift_type_name(name);
    // (member, wire, entry) in declaration order, and a canonical-key-order copy for
    // the encoder so the wire map is deterministic.
    let named: Vec<(String, String, &CsilGroupEntry)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let member = entry_field_name(e)?;
            let wire = e.key.as_ref().and_then(wire_key)?;
            Some((member, wire, e))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    let mut out = String::new();
    out.push_str(&format!("public extension {type_name} {{\n"));
    out.push_str("    /// The CBOR value tree for this record (deep, canonical key order).\n");
    out.push_str("    func toCborValue() -> CsilCborValue {\n");
    out.push_str("        var csilEntries: [(CsilCborValue, CsilCborValue)] = []\n");
    for (member, wire, entry) in &canonical {
        let wire_lit = swift_string_lit(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let enc = swift_enc_value(&entry.value_type, "csilV", records, aliases);
            out.push_str(&format!(
                "        if let csilV = self.{member} {{ csilEntries.append(({wire_lit}, {enc})) }}\n"
            ));
        } else {
            let enc = swift_enc_value(
                &entry.value_type,
                &format!("self.{member}"),
                records,
                aliases,
            );
            out.push_str(&format!(
                "        csilEntries.append(({wire_lit}, {enc}))\n"
            ));
        }
    }
    out.push_str("        return .map(csilEntries)\n    }\n\n");

    out.push_str("    /// Reconstruct this record from a decoded CBOR value tree.\n");
    out.push_str("    init(cborValue: CsilCborValue) throws {\n");
    for (member, wire, entry) in &named {
        let wire_lit = swift_string_lit(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let opt_ty = map_type(&entry.value_type, true);
            let dec = swift_dec_value(&entry.value_type, "csilV", records, aliases);
            out.push_str(&format!(
                "        let {member}: {opt_ty} = if let csilV = CsilCbor.mapGet(cborValue, {wire_lit}) {{ {dec} }} else {{ nil }}\n"
            ));
        } else {
            let dec = swift_dec_value(
                &entry.value_type,
                &format!("(try CsilCbor.require(cborValue, {wire_lit}))"),
                records,
                aliases,
            );
            out.push_str(&format!("        let {member} = {dec}\n"));
        }
    }
    let init_args: Vec<String> = named
        .iter()
        .map(|(member, _, _)| format!("{member}: {member}"))
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

/// Build `Codec.swift`: the self-contained CBOR runtime plus a codec extension per
/// record. `None` when the spec declares no record types.
fn generate_codec(input: &WasmGeneratorInput) -> Option<String> {
    let records = swift_record_names(input);
    if records.is_empty() {
        return None;
    }
    let aliases = swift_codec_aliases(input);
    let mut body = String::new();
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            body.push_str(&emit_struct_codec(&rule.name, group, &records, &aliases));
        }
    }
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
    let mut body = String::new();
    let mut any = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_client_struct(&rule.name, service, &records, shape));
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
    shape: ClientShape,
) -> String {
    let base = service_base(name);
    let client = shape.client_name(&base);
    let transport = shape.transport_name();
    let effects = shape.effects();
    let await_kw = shape.await_kw();
    // Canonical wire strings (the wire contract): service lowercased, op PascalCased.
    let wire_service = wire_service_string(name);
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
        // The typed-codec path needs a record success type (and a record or null
        // request) so the method can call the generated to/from-CBOR. Anything else
        // is skipped with a note rather than emitting an uncompilable call.
        let resp_record = is_record_ref(&success, records);
        let req_ok = is_null_input(&op.input_type) || is_record_ref(&op.input_type, records);
        if !resp_record || !req_ok {
            out.push_str(&format!(
                "    // operation '{}' has a non-record payload; (de)serialize it manually\n",
                op.name
            ));
            continue;
        }
        let method = swift_ident(&op.name);
        let output = map_type(&success, false);
        let wire_op = wire_op_string(&op.name);
        if is_null_input(&op.input_type) {
            out.push_str(&format!(
                "    public func {method}() {effects} -> {output} {{\n"
            ));
            out.push_str(&format!(
                "        let csilResp = try {await_kw}transport.call(service: {}, op: {}, request: [])\n",
                swift_string_lit(&wire_service),
                swift_string_lit(&wire_op),
            ));
        } else {
            let input = map_type(&op.input_type, false);
            out.push_str(&format!(
                "    public func {method}(_ request: {input}) {effects} -> {output} {{\n"
            ));
            out.push_str(&format!(
                "        let csilResp = try {await_kw}transport.call(service: {}, op: {}, request: request.toCbor())\n",
                swift_string_lit(&wire_service),
                swift_string_lit(&wire_op),
            ));
        }
        out.push_str(&format!("        return try {output}.fromCbor(csilResp)\n"));
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
        out.push_str(&format!(
            "    case {}:\n",
            swift_string_lit(&wire_op_string(&op.name))
        ));
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
            swift_string_lit(&wire_op_string(&op.name))
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
