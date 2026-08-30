//! C# code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target csharp` from `csilgen_csharp_generator.wasm`.
//! Emits idiomatic modern C# (net8.0 / C# 12): file-scoped `namespace Csilgen.Transport;`,
//! `sealed record` types with `required`/nullable `init` properties, closed
//! discriminated-union emulation for variants, a primary-constructor client, and a
//! server interface + verbose/compact channel routers — never the wire bytes.

use csilgen_common::{
    ChoiceClass, CsilControlOperator, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilSizeConstraint, CsilTypeExpression, CsilValidationConstraint, GeneratedFile,
    GenerationStats, GeneratorCapability, GeneratorMetadata, WasmGeneratorInput,
    WasmGeneratorOutput, choice_arm_literal, classify_choice, wasm_interface::*,
};
use csilgen_common::{CsilFieldMetadata, GeneratorWarning};
use std::collections::HashMap;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "csharp-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "C# (.NET 8 / C# 12) code generator".to_string(),
        target: "csharp".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
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
    render(input)
}

/// In-memory C# type chosen for the CSIL `decimal` core type. The wire form is CBOR
/// tag 4 either way; only the emitted property type differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` record (no NuGet dependency).
    Csil,
    /// Use the BCL `decimal` (System.Decimal).
    Library,
}

struct CsharpConfig {
    namespace: String,
    decimal_mapping: DecimalMapping,
}

/// Derive a valid C# namespace from a `package_name`: its last path segment, with each
/// dot-delimited part sanitized to a C# identifier (alphanumerics/underscore, never a
/// leading digit). This keeps the in-code namespace aligned with the package id and the
/// csproj `RootNamespace`.
fn csharp_namespace_from_package(package_name: &str) -> String {
    let tail = csilgen_common::package_name_last_segment(package_name);
    tail.split('.')
        .map(|segment| {
            let mut ident: String = segment
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if ident.is_empty() {
                ident.push('_');
            }
            if ident.starts_with(|c: char| c.is_ascii_digit()) {
                ident.insert(0, '_');
            }
            ident
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl CsharpConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
        // An explicit namespace wins; absent that, a configured `package_name` drives the
        // namespace so generated code lives under its own package name (aligned with the
        // csproj RootNamespace) rather than squatting in the transport library's
        // `Csilgen.Transport` namespace — where its `Cbor`/`CborValue` would collide with
        // the library's own when a consumer references both.
        let namespace = options
            .get("csharp_namespace")
            .or_else(|| options.get("namespace"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                options
                    .get("package_name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(csharp_namespace_from_package)
            })
            .unwrap_or_else(|| "Csilgen.Transport".to_string());

        // A typo in decimal_mapping is a hard error so misconfiguration surfaces at
        // generation time rather than silently degrading to the default.
        let decimal_mapping = match options.get("decimal_mapping") {
            None => DecimalMapping::Csil,
            Some(v) => match v.as_str() {
                Some("csil") => DecimalMapping::Csil,
                Some("library") => DecimalMapping::Library,
                _ => return Err(error_codes::GENERATION_ERROR),
            },
        };

        Ok(Self {
            namespace,
            decimal_mapping,
        })
    }
}

/// Which client variant(s) to emit. `Both` is the default so every consumer keeps the
/// blocking client they had AND gets an async twin for free; a consumer can opt down to a
/// single shape explicitly. Only the transport seam and the per-method return types turn
/// async — the generated codec never does I/O and stays synchronous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStyle {
    /// Blocking-only client at `Client.gen.cs` (today's output, unchanged).
    Sync,
    /// `Task`-returning client, a drop-in at `Client.gen.cs` with the canonical symbol
    /// names — a consumer swapping sync for async changes nothing but the `await`.
    Async,
    /// Emit both — the sync client at `Client.gen.cs` plus an async twin at
    /// `ClientAsync.gen.cs` whose symbols carry an `Async` marker so the two coexist in
    /// one namespace without collisions. Default.
    Both,
}

/// Read & validate `client_style` from the options block. Mirrors `decimal_mapping`: any
/// value other than `sync`/`async`/`both` is rejected at generation time rather than
/// silently degrading. Absent -> `Both`, so the blocking client is preserved and the
/// async twin comes for free.
fn client_style(options: &HashMap<String, serde_json::Value>) -> Result<ClientStyle, i32> {
    match options.get("client_style") {
        None => Ok(ClientStyle::Both),
        Some(v) => match v.as_str() {
            Some("sync") => Ok(ClientStyle::Sync),
            Some("async") => Ok(ClientStyle::Async),
            Some("both") => Ok(ClientStyle::Both),
            _ => Err(error_codes::GENERATION_ERROR),
        },
    }
}

/// The shape of one emitted client file: whether its methods are async and the symbol
/// marker that keeps an async twin distinct from the sync client when both land in one
/// namespace. `marker` is empty for a stand-alone client (sync, or async-as-drop-in) and
/// `"Async"` for the twin in `Both` mode. Per .NET convention the marker is ALSO the
/// method-name suffix, so a twin method is `SubmitTaskAsync` while a drop-in keeps the
/// canonical `SubmitTask` (true drop-in: same names, just awaited).
#[derive(Debug, Clone, Copy)]
struct ClientShape {
    is_async: bool,
    marker: &'static str,
}

impl ClientShape {
    /// The byte-transport interface name: `ICsilTransport`, or `ICsilAsyncTransport` for
    /// the twin (the `Async` marker is inserted after the `I` so the C# `I`-prefix idiom
    /// is preserved rather than producing `AsyncICsilTransport`).
    fn transport_name(&self) -> String {
        format!("ICsil{}Transport", self.marker)
    }

    /// A per-service client class name: `FooClient`, or `FooAsyncClient` for the twin.
    fn class_name(&self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }
}

/// The single typed entry point used by both the WASM `generate` export and the
/// integration tests. Kept `pub` so the `rlib` crate-type lets tests drive real
/// generation (a `cdylib`-only crate cannot be linked by integration tests).
pub fn render(mut input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    // A field/element/value/tuple-slot whose type is an inline (anonymous) choice or group
    // has no named rule behind it, so `map_csil_type`/`csharp_enc_value`/`csharp_dec_value`
    // have no codec route and used to collapse it to `object` + CBOR null (value dropped).
    // Hoisting each such shape to a synthesized named rule up front makes every downstream
    // pass treat it exactly like a hand-written named choice/group — one code path, no
    // bespoke inline handling anywhere. Shared machinery (`csilgen_common::hoist`) replaces
    // this crate's former local hoist pass verbatim.
    //
    // `hoist_all_literal_choices: true` — UNLIKE TypeScript/Java/OCaml's `false`, C#'s field
    // codec (`map_csil_type_inner`/`csharp_enc_value`/`csharp_dec_value`) has NO case at all
    // for an un-hoisted `CsilTypeExpression::Choice`, literal or not: it falls through to the
    // `object`/`new CborValue.Null()` blind-passthrough fallback (the exact bug this module
    // exists to fix). So C# has always relied on hoisting EVERY inline choice, closed-literal
    // ones included, to a named rule with its own codec — confirmed by the pre-migration
    // output for `tests/interop/interop.csil`'s all-literal `Scalars.size` field, which
    // hoists to a synthesized `ScalarsSize` enum today. Passing `false` here would silently
    // regress that field (and every field like it) back to an untyped `object` with a
    // null-emitting codec.
    input.csil_spec = csilgen_common::hoist_inline_composites(
        &input.csil_spec,
        csilgen_common::HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
    let config = CsharpConfig::from_options(&input.config.options)?;
    // Validate client_style up front (before emitting any file) so a bad value fails the
    // whole run regardless of which target is requested.
    let style = client_style(&input.config.options)?;
    let warnings: Vec<GeneratorWarning> = Vec::new();
    let mut files = Vec::new();

    // The base `csharp` (and explicit `csharp-server`) target emits the server
    // surface; `csharp-client` emits the typed client. An unrecognized sub-target is
    // an error, not a silent fall-through.
    enum Surface {
        Server,
        Client,
    }
    let surface = match input.config.target.as_str() {
        "csharp" | "csharp-server" => Surface::Server,
        "csharp-client" => Surface::Client,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    if let Some(types) = generate_types(&input, &config) {
        files.push(GeneratedFile {
            path: "Types.gen.cs".to_string(),
            content: types,
        });
    }

    // The self-contained CsilDecimal record is only worth emitting under the default
    // mapping and only when the spec actually uses `decimal`; the library mapping
    // pulls the BCL `decimal` instead, so no helper is generated.
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(&input, "decimal") {
        files.push(GeneratedFile {
            path: "CsilDecimal.gen.cs".to_string(),
            content: csil_decimal_file(&config),
        });
    }

    // The self-contained per-record CBOR codec is emitted for either surface whenever
    // the spec declares records: the client encodes/decodes through it, and a server
    // host can reach for the same canonical bytes. Nothing else can serialize a value
    // here — C# reflection is the path we are deliberately dropping for the byte seam.
    if let Some(codec) = generate_codec(&input, &config) {
        files.push(GeneratedFile {
            path: "Codec.gen.cs".to_string(),
            content: codec,
        });
    }

    // A package's `genquickstart.md` demonstrates both the calling side (the RPC and
    // Datagrams sections, over the client) and the handling side (the Events section, over
    // the generated router in `Services.gen.cs`), so a package must carry BOTH surfaces for
    // its own quickstart to compile — regardless of which surface the requested target
    // names. A flat (non-package) build stays byte-identical: it emits only the requested
    // surface. This mirrors the OCaml generator's all-surfaces-in-package-mode rule.
    let pkg_mode = package_requested(&input.config.options, "csharp");
    let want_client = matches!(surface, Surface::Client) || pkg_mode;
    let want_server = matches!(surface, Surface::Server) || pkg_mode;

    if input.csil_spec.service_count > 0 {
        if want_client {
            // The sync client and the async drop-in share the canonical filename and
            // symbol names; the async twin (Both mode) takes a separate file and the
            // `Async` marker so the two coexist in one namespace.
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
                    if let Some(client) = generate_client(&input, &config, sync) {
                        files.push(GeneratedFile {
                            path: "Client.gen.cs".to_string(),
                            content: client,
                        });
                    }
                }
                ClientStyle::Async => {
                    if let Some(client) = generate_client(&input, &config, async_drop_in) {
                        files.push(GeneratedFile {
                            path: "Client.gen.cs".to_string(),
                            content: client,
                        });
                    }
                }
                ClientStyle::Both => {
                    if let Some(client) = generate_client(&input, &config, sync) {
                        files.push(GeneratedFile {
                            path: "Client.gen.cs".to_string(),
                            content: client,
                        });
                    }
                    if let Some(client) = generate_client(&input, &config, async_twin) {
                        files.push(GeneratedFile {
                            path: "ClientAsync.gen.cs".to_string(),
                            content: client,
                        });
                    }
                }
            }
        }
        if want_server && let Some(services) = generate_services(&input, &config) {
            files.push(GeneratedFile {
                path: "Services.gen.cs".to_string(),
                content: services,
            });
        }
    }

    // Self-contained publishable-package mode: when `emit_packages` opts the `csharp`
    // target in, the output directory additionally carries an SDK-style `.csproj` so the
    // directory itself is a valid, NuGet-packable .NET project. SDK-style projects glob
    // `**/*.cs` by default, so the flat generated files compile in with no extra wiring.
    // The default (non-package) output is otherwise byte-identical.
    if pkg_mode {
        let coords = PackageCoords::from_options(&config, &input.config.options);
        files.push(GeneratedFile {
            path: format!("{}.csproj", coords.package_id),
            content: csproj_file(&coords),
        });
        // The README is opt-out: only an explicit `emit_readme: false` suppresses it.
        // Absent / non-bool / `true` all keep the prior behavior so existing consumers
        // see no change.
        let emit_readme = input
            .config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false);
        if emit_readme {
            files.push(GeneratedFile {
                path: "genquickstart.md".to_string(),
                content: readme_file(&input, &config, &coords),
            });
        }
    }

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
        warnings,
        stats,
    })
}

const FILE_HEADER: &str = "// <auto-generated>\n// Code generated by csilgen; DO NOT EDIT.\n// </auto-generated>\n#nullable enable\n";

// ---------------------------------------------------------------------------
// Self-contained publishable package (.csproj)
// ---------------------------------------------------------------------------

/// Whether the `emit_packages` option opts a given target into self-contained package
/// emission. The option is a JSON array of target ids; parsed defensively so a missing
/// option, a non-array value, or non-string elements all simply mean "no package".
fn package_requested(options: &HashMap<String, serde_json::Value>, target: &str) -> bool {
    options
        .get("emit_packages")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|e| e.as_str() == Some(target)))
}

/// The NuGet/MSBuild coordinates for the emitted `.csproj`. `package_id` doubles as the
/// `RootNamespace` per the package contract.
struct PackageCoords {
    package_id: String,
    version: String,
    generate_on_build: bool,
}

impl PackageCoords {
    fn from_options(config: &CsharpConfig, options: &HashMap<String, serde_json::Value>) -> Self {
        // `package_name` is the explicit override; absent that, the project is named after
        // the namespace the generated `.cs` files already declare, so the package id and
        // the in-code namespace stay aligned.
        let package_id = options
            .get("package_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            // A path-style `package_name` is the cross-ecosystem source of truth; the
            // NuGet id wants only its tail. See `package_name_last_segment`.
            .map(csilgen_common::package_name_last_segment)
            .unwrap_or(&config.namespace)
            .to_string();
        let version = options
            .get("package_version")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("0.1.0")
            .to_string();
        // GeneratePackageOnBuild is opt-in: a plain `dotnet build` then stays a compile
        // (no pack step), while consumers that want a `.nupkg` on every build can ask for
        // it. The project is `dotnet pack`-able either way.
        let generate_on_build = options
            .get("package_generate_on_build")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            package_id,
            version,
            generate_on_build,
        }
    }
}

/// Escape a string for safe inclusion as XML element text in the emitted `.csproj`, so a
/// `&`/`<`/`>` in a caller-supplied package name or version can never break the XML.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Build an SDK-style `.csproj` with no third-party dependencies (BCL only). The
/// generated `.cs` files fully qualify their `System.*` types, so `ImplicitUsings` is
/// disabled deliberately rather than relied upon.
fn csproj_file(coords: &PackageCoords) -> String {
    let id = xml_escape(&coords.package_id);
    let version = xml_escape(&coords.version);
    let generate = if coords.generate_on_build {
        "    <GeneratePackageOnBuild>true</GeneratePackageOnBuild>\n"
    } else {
        ""
    };
    format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <LangVersion>12.0</LangVersion>\n    <Nullable>enable</Nullable>\n    <ImplicitUsings>disable</ImplicitUsings>\n    <Deterministic>true</Deterministic>\n    <PackageId>{id}</PackageId>\n    <Version>{version}</Version>\n    <RootNamespace>{id}</RootNamespace>\n{generate}  </PropertyGroup>\n</Project>\n"
    )
}

// ---------------------------------------------------------------------------
// Package README (a 3-transport Quickstart over the Csilgen.Transport library)
// ---------------------------------------------------------------------------

/// Which transport sections a consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of `["rpc","events","datagrams"]`;
/// unknown entries are ignored, and an absent or empty value means "all three". The three
/// sections always render in a fixed order so the document reads the same regardless of how
/// the subset was written.
fn wanted_transports(options: &HashMap<String, serde_json::Value>) -> (bool, bool, bool) {
    let listed = match options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
            let any_known = ["rpc", "events", "datagrams"]
                .iter()
                .any(|t| names.contains(t));
            any_known.then(|| {
                (
                    names.contains("rpc"),
                    names.contains("events"),
                    names.contains("datagrams"),
                )
            })
        }
        _ => None,
    };
    listed.unwrap_or((true, true, true))
}

/// The package README: a transport-by-transport Quickstart over the official
/// `Csilgen.Transport` library. The generated codec owns CBOR (de)serialization and the
/// library owns the envelope/framing/lifecycle; the consumer supplies only a *carrier* that
/// moves bytes, so the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw UDP
/// unchanged. Each requested section (CSIL-RPC over HTTP, CSIL-Events over TLS, CSIL-Datagrams
/// over UDP) is a complete, copy-paste example on the library.
fn readme_file(
    input: &WasmGeneratorInput,
    config: &CsharpConfig,
    coords: &PackageCoords,
) -> String {
    let title = &coords.package_id;
    let mut out = format!(
        "# {title}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the `Csilgen.Transport` library owns the envelope, framing,\n\
         and connection lifecycle. You supply only a *carrier* that moves bytes, so the same\n\
         typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw UDP unchanged.\n\n\
         ## Install\n\n\
         ```sh\n\
         # The Csilgen.Transport library is not yet published to NuGet; reference its project\n\
         # (or a vendored copy) until it ships. Reference the generated project directly:\n\
         dotnet add reference path/to/{title}.csproj\n\
         dotnet add reference path/to/Csilgen.Transport.csproj\n\
         ```\n\n\
         > Generate this package under a namespace **other than** `Csilgen.Transport` (set\n\
         > the `namespace` option) so the generated codec types don't collide with the library.\n\n"
    );

    let records = record_names(input);
    let (rpc, events, datagrams) = wanted_transports(&input.config.options);
    let unary = first_readme_example(input, &records, config);
    let channel = first_channel_example(input, &records, config);
    if rpc {
        out.push_str(&rpc_section(config, unary.as_ref()));
    }
    if events {
        out.push_str(&events_section(config, channel.as_ref()));
    }
    if datagrams {
        out.push_str(&datagrams_section(config, unary.as_ref()));
    }
    out
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `ICsilTransport` byte seam that
/// encodes the request with the library's `RpcRequest` and decodes its `RpcResponse` (never
/// hand-rolled), POSTing to `{baseUrl}/csil/v1/rpc` with the stdlib `HttpClient`. A non-zero
/// transport status (via `AsTransportError`) and the typed `ServiceError` arm are surfaced
/// distinctly. Then the typed client calls the first `->` op.
fn rpc_section(config: &CsharpConfig, ex: Option<&ReadmeExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`); you\n\
         bring a carrier that moves bytes. The HTTP carrier below is just one example — swap\n\
         `HttpClient` for any client (it implements the generated `ICsilTransport` seam).\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    let carrier = RPC_CARRIER_CSHARP.replace("__NAMESPACE__", &config.namespace);
    let call = match &ex.sample {
        Some(sample) => format!("client.{}({sample})", ex.method),
        None => format!("client.{}()", ex.method),
    };
    let example = format!(
        "// Construct the client over the carrier and make one call. Change the URL only.\n\
         public static class CsilRpcExample\n{{\n    \
         public static void Run()\n    {{\n        \
         var client = new {client}(new HttpRpcCarrier(\"http://localhost:5080\"));\n        \
         var response = {call};\n        \
         System.Console.WriteLine(response);\n    }}\n}}\n",
        client = ex.client_class,
    );
    out.push_str(&format!("```csharp\n{carrier}\n{example}```\n\n"));
    out
}

/// The HTTP carrier body — spec-independent, so a constant with a `__NAMESPACE__` placeholder.
/// It builds the request envelope with the library's `RpcRequest`, POSTs it to
/// `{baseUrl}/csil/v1/rpc`, and returns the success payload bytes the typed client decodes.
/// `RpcResponse.AsTransportError()` surfaces any non-zero transport status; the typed
/// `ServiceError` arm (a status-0 variant) is surfaced separately.
const RPC_CARRIER_CSHARP: &str = r#"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (RpcRequest/RpcResponse); the carrier owns only the transport. Swap HttpClient for any client.
using System;
using System.Net.Http;
using System.Net.Http.Headers;
using Csilgen.Transport;
using __NAMESPACE__;

public sealed class HttpRpcCarrier : ICsilTransport, IDisposable
{
    private readonly string _url;
    private readonly HttpClient _http;

    public HttpRpcCarrier(string baseUrl) : this(baseUrl, new HttpClientHandler()) { }

    // The handler seam keeps the carrier testable: inject a stub HttpMessageHandler to
    // exercise it in-process with no sockets.
    public HttpRpcCarrier(string baseUrl, HttpMessageHandler handler)
    {
        _url = baseUrl.TrimEnd('/') + "/csil/v1/rpc";
        _http = new HttpClient(handler);
    }

    public byte[] Call(string service, string op, byte[] request)
    {
        // The library owns the envelope; hand it the already-encoded request bytes.
        byte[] envelope = new RpcRequest(service, op, request).Encode();
        using var msg = new HttpRequestMessage(HttpMethod.Post, _url)
        {
            Content = new ByteArrayContent(envelope),
        };
        msg.Content.Headers.ContentType = new MediaTypeHeaderValue("application/cbor");
        msg.Headers.Accept.ParseAdd("application/cbor");

        using HttpResponseMessage http = _http.Send(msg);
        byte[] body = http.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult();
        if (!http.IsSuccessStatusCode)
            throw new System.Exception($"csil-rpc {service}/{op}: http {(int)http.StatusCode}");

        // AsTransportError() surfaces any non-zero transport status as a StatusException.
        RpcResponse resp = RpcResponse.Decode(body);
        if (resp.AsTransportError() is StatusException ex)
            throw ex;
        // A typed application error rides as a status-0 "ServiceError" variant — distinct
        // from a transport failure. Surface it so the typed client decodes success only.
        if (resp.Variant == "ServiceError")
            throw new System.Exception($"csil-rpc {service}/{op}: ServiceError");
        return resp.Payload;
    }

    public void Dispose() => _http.Dispose();
}
"#;

/// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as the
/// library's `StreamCarrier` (CSIL length-prefix framing), performs the `$hello`/`$hello-ack`
/// handshake, sends one outbound event via the generated `<Base>Router.Encode<Op>`, and runs a
/// recv loop that decodes each frame to an `Event`, answers `$ping` with `$pong`, and dispatches
/// typed events to the generated `<Base>Router.RouteChannel`. When the spec has no channel ops the
/// dispatch wiring is replaced with a note (the handshake + heartbeat still apply).
fn events_section(config: &CsharpConfig, ch: Option<&ChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns the\n\
         `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and framing; the\n\
         generated router dispatches typed events. The TLS carrier below is just one example —\n\
         a WebSocket/WebTransport/QUIC carrier drops in unchanged.\n\n",
    );
    let block = match ch {
        Some(ch) => EVENTS_SESSION_CSHARP
            .replace("__NAMESPACE__", &config.namespace)
            .replace("__ROUTER__", &ch.router)
            .replace("__IFACE__", &ch.iface)
            .replace("__HANDLER_BODY__", &ch.handler_body)
            .replace("__ENCODE__", &ch.encode_method)
            .replace("__OUTBOUND_SAMPLE__", &ch.outbound_sample)
            .replace("__OUTBOUND__", &ch.outbound_type)
            .replace("__INBOUND__", &ch.inbound_type)
            .replace("__SERVICE__", &ch.service_wire),
        None => EVENTS_NO_CHANNEL_CSHARP.to_string(),
    };
    out.push_str(&format!("```csharp\n{block}```\n\n"));
    out
}

/// The full Events session for a connection with a `<->` op: a TLS `StreamCarrier`, the
/// `ICsilCodec` bridge to the generated static `Codec`, the handshake, one outbound event via
/// the generated encoder, and the recv loop that heartbeats and dispatches into the generated
/// router. Placeholders (router/iface/encoder names, the inbound/outbound types, the handler
/// stubs, the wire service, the namespace) are filled per-spec.
const EVENTS_SESSION_CSHARP: &str = r#"// One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
using System;
using System.Net.Security;
using System.Net.Sockets;
using Csilgen.Transport;
using __NAMESPACE__;

public static class CsilEventsExample
{
    // The max-frame guard is a carrier setting, not a generated constant: raise it when a
    // peer accepts payloads larger than the 16 MiB default (the envelope adds framing and
    // request metadata around the payload, so the limit must exceed the largest payload),
    // or lower it to harden an exposed listener. Valid limits are 1..=Conventions.MaxFrameLimit
    // and are checked at construction.
    const int MaxFrame = Conventions.MaxFrameDefault;

    static IFrameCarrier OpenTlsCarrier(string host, int port)
    {
        var tcp = new TcpClient(host, port);
        var tls = new SslStream(tcp.GetStream());
        tls.AuthenticateAsClient(host);
        // StreamCarrier owns the length-prefix framing over any duplex stream.
        return new StreamCarrier(tls, MaxFrame);
    }

    // Bridges the library's byte seam to the generated static Codec. Codec.Decode is
    // generic-static, so the router-supplied runtime type is bound by reflection.
    private sealed class GenCodec : ICsilCodec
    {
        public byte[] Encode(object value) => Codec.Encode(value);

        public object Decode(byte[] data, System.Type targetType) =>
            typeof(Codec).GetMethod("Decode")!.MakeGenericMethod(targetType).Invoke(null, new object[] { data })!;
    }

    // A handler implementing __IFACE__'s methods; __ROUTER__.RouteChannel dispatches each
    // decoded inbound channel message (inbound __INBOUND__) here.
    private sealed class ChannelHandlers : __IFACE__
    {
__HANDLER_BODY__
    }

    public static void Run()
    {
        IFrameCarrier carrier = OpenTlsCarrier("localhost", 7443);

        // $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the
        // wire profile for the connection's lifetime.
        carrier.SendFrame(new Hello(new ulong[] { Conventions.Version }, new[] { "verbose" }) { Service = "__SERVICE__" }.Encode());
        byte[]? ack = carrier.RecvFrame();
        if (ack is null) throw new System.Exception("connection closed during handshake");
        ProfileExtensions.TryParse(HelloAck.Decode(ack).Profile, out Profile profile);

        var codec = new GenCodec();
        var handlers = new ChannelHandlers();

        // Send one outbound event via the generated encoder (outbound __OUTBOUND__).
        var (method, body) = __ROUTER__.__ENCODE__(codec, __OUTBOUND_SAMPLE__);
        carrier.SendFrame(Event.Verbose("__SERVICE__", method, body).Encode(profile));

        // Recv loop: decode each frame to an Event, answer $ping with $pong, dispatch the
        // rest to the generated router.
        for (byte[]? frame = carrier.RecvFrame(); frame is not null; frame = carrier.RecvFrame())
        {
            Event ev = Event.Decode(frame, profile);
            if (ev.EventName == Control.PingName)
            {
                Heartbeat ping = Heartbeat.Decode(ev.Payload);
                carrier.SendFrame(Event.Verbose("__SERVICE__", Control.PongName, new Heartbeat(ping.Nonce).Encode()).Encode(profile));
                continue;
            }
            __ROUTER__.RouteChannel(handlers, codec, ev.EventName!, ev.Payload);
        }
    }
}
"#;

/// The Events session when the spec declares no channel ops: the handshake and heartbeat still
/// apply, so they are shown, with a note where the dispatch would go. References only library
/// types (no generated router), so it needs no package `using`.
const EVENTS_NO_CHANNEL_CSHARP: &str = r#"// One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
using System;
using System.Net.Security;
using System.Net.Sockets;
using Csilgen.Transport;

public static class CsilEventsExample
{
    // The max-frame guard is a carrier setting an operator can raise or lower; valid limits
    // are 1..=Conventions.MaxFrameLimit and are checked at construction.
    const int MaxFrame = Conventions.MaxFrameDefault;

    static IFrameCarrier OpenTlsCarrier(string host, int port)
    {
        var tcp = new TcpClient(host, port);
        var tls = new SslStream(tcp.GetStream());
        tls.AuthenticateAsClient(host);
        return new StreamCarrier(tls, MaxFrame);
    }

    public static void Run()
    {
        IFrameCarrier carrier = OpenTlsCarrier("localhost", 7443);

        // $hello / $hello-ack handshake (control plane).
        carrier.SendFrame(new Hello(new ulong[] { Conventions.Version }, new[] { "verbose" }).Encode());
        byte[]? ack = carrier.RecvFrame();
        if (ack is null) throw new System.Exception("connection closed during handshake");
        ProfileExtensions.TryParse(HelloAck.Decode(ack).Profile, out Profile profile);

        // Recv loop: answer $ping with $pong. This package declares no <->/<- operations,
        // so there is no generated channel router to dispatch typed events into.
        for (byte[]? frame = carrier.RecvFrame(); frame is not null; frame = carrier.RecvFrame())
        {
            Event ev = Event.Decode(frame, profile);
            if (ev.EventName == Control.PingName)
            {
                Heartbeat ping = Heartbeat.Decode(ev.Payload);
                carrier.SendFrame(Event.Verbose(null, Control.PongName, new Heartbeat(ping.Nonce).Encode()).Encode(profile));
            }
        }
    }
}
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in the
/// library's `Datagram`, and `SendDatagram` it fire-and-forget. The recv path `Datagram.Decode`s
/// an inbound datagram and decodes its payload with the generated codec — there is NO synchronous
/// response.
fn datagrams_section(config: &CsharpConfig, ex: Option<&ReadmeExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram` envelope; you\n\
         bring a datagram carrier. The UDP carrier below is one example — a WebRTC unreliable\n\
         DataChannel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no record `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_sample), Some(res_type)) = (&ex.sample, &ex.res_type) else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    let block = DATAGRAMS_EXAMPLE_CSHARP
        .replace("__NAMESPACE__", &config.namespace)
        .replace("__OP_ORD__", &ex.op_ord.to_string())
        .replace("__REQ_SAMPLE__", req_sample)
        .replace("__RES_TYPE__", res_type);
    out.push_str(&format!("```csharp\n{block}```\n\n"));
    out
}

/// The UDP datagram example body — spec-independent but for the op ordinal, request literal, and
/// response type. It connects a UDP `Socket`, wraps it in the library's `UdpDatagramCarrier`,
/// sends one `Datagram`-framed request, and decodes a (possibly never-arriving) response datagram.
const DATAGRAMS_EXAMPLE_CSHARP: &str = r#"// One example carrier: UDP via a connected stdlib Socket. Datagrams are unreliable and
// unordered, so the carrier never waits for or correlates a reply.
using System;
using System.Net.Sockets;
using Csilgen.Transport;
using __NAMESPACE__;

public static class CsilDatagramsExample
{
    // The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
    private const ulong OpOrd = __OP_ORD__;

    static IDatagramCarrier OpenUdpCarrier(string host, int port)
    {
        var sock = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        sock.Connect(host, port);
        return new UdpDatagramCarrier(sock);
    }

    public static void Run()
    {
        IDatagramCarrier carrier = OpenUdpCarrier("localhost", 9000);

        // Fire-and-forget: encode the `->` request via the generated codec and send it.
        // seq 0 marks an unsequenced datagram.
        var req = __REQ_SAMPLE__;
        carrier.SendDatagram(new Datagram(OpOrd, 0, Codec.Encode(req)).Encode());

        // Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is
        // NO synchronous response; the caller must tolerate loss and reordering and handle a
        // reply whenever (if ever) it shows up.
        byte[]? inbound = carrier.RecvDatagram();
        if (inbound is not null)
        {
            Datagram dg = Datagram.Decode(inbound);
            __RES_TYPE__ resp = Codec.Decode<__RES_TYPE__>(dg.Payload);
            System.Console.WriteLine($"late response {resp}");
        }
    }
}
"#;

/// The pieces the RPC + datagram examples need: the client class to construct, the method to
/// call, a compiling C# request literal (`None` when the op takes no input), the response record
/// class name (for the datagram codec), and the op's datagram ordinal.
struct ReadmeExample {
    client_class: String,
    method: String,
    sample: Option<String>,
    res_type: Option<String>,
    op_ord: u32,
}

/// The first service (in rule order, matching the generated client) with a unary op the
/// client emits a typed method for: a record (or null) request and a record success type.
/// `None` for a serviceless package.
fn first_readme_example(
    input: &WasmGeneratorInput,
    records: &std::collections::HashSet<String>,
    config: &CsharpConfig,
) -> Option<ReadmeExample> {
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let req_null = op_input_is_null(&op.input_type);
            if !is_record_ref(&success, records)
                || !(req_null || is_record_ref(&op.input_type, records))
            {
                continue;
            }
            return Some(ReadmeExample {
                client_class: format!("{}Client", service_base(&rule.name)),
                method: pascal_ident(&op.name),
                sample: if req_null {
                    None
                } else {
                    Some(csharp_request_literal(
                        input,
                        &op.input_type,
                        records,
                        config,
                    ))
                },
                res_type: record_ref_pascal(&success),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.map(|id| id as u32).unwrap_or(1),
            });
        }
    }
    None
}

/// The pieces the Events session needs: the generated router class + handler interface +
/// outbound encoder method names, the inbound (router-decoded input) and outbound (encoder
/// output) record class names, a constructible outbound literal, the wire service, and the
/// handler-stub body that implements the full service interface.
struct ChannelExample {
    service_wire: String,
    router: String,
    iface: String,
    encode_method: String,
    inbound_type: String,
    outbound_type: String,
    outbound_sample: String,
    handler_body: String,
}

/// The first service (in rule order) with a `<->` op whose input and output are both records,
/// so the generated router, handler interface, and outbound encoder all exist with codec-backed
/// (de)serialization. `None` when no service has a usable channel op — the Events section then
/// shows the handshake/heartbeat without dispatch wiring.
fn first_channel_example(
    input: &WasmGeneratorInput,
    records: &std::collections::HashSet<String>,
    config: &CsharpConfig,
) -> Option<ChannelExample> {
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        let chan = service
            .operations
            .iter()
            .find(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))?;
        if !is_record_ref(&chan.input_type, records) || !is_record_ref(&chan.output_type, records) {
            continue;
        }
        let (Some(inbound_type), Some(outbound_type)) = (
            record_ref_pascal(&chan.input_type),
            record_ref_pascal(&chan.output_type),
        ) else {
            continue;
        };
        let base = service_base(&rule.name);
        return Some(ChannelExample {
            service_wire: rule.name.clone(),
            router: format!("{base}Router"),
            iface: service_interface_name(&rule.name),
            encode_method: format!("Encode{}", pascal_ident(&chan.name)),
            inbound_type,
            outbound_type,
            outbound_sample: csharp_request_literal(input, &chan.output_type, records, config),
            handler_body: csharp_handler_stubs(service, config),
        });
    }
    None
}

/// The `ChannelHandlers` body implementing every method of the service interface: unary ops
/// throw `NotImplementedException` (the Events example only drives the channel path), and each
/// bidirectional channel op prints what it received. Reverse ops contribute no inbound method.
fn csharp_handler_stubs(service: &CsilServiceDefinition, config: &CsharpConfig) -> String {
    let mut out = String::new();
    for op in &service.operations {
        let method = pascal_ident(&op.name);
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_csil_type(&success_type(&op.output_type), config);
                if op_input_is_null(&op.input_type) {
                    out.push_str(&format!(
                        "        public {output} {method}() => throw new System.NotImplementedException();\n"
                    ));
                } else {
                    let input = map_csil_type(&op.input_type, config);
                    out.push_str(&format!(
                        "        public {output} {method}({input} value) => throw new System.NotImplementedException();\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let input = map_csil_type(&op.input_type, config);
                out.push_str(&format!(
                    "        public void {method}({input} message) => System.Console.WriteLine($\"event {method}\");\n"
                ));
            }
            CsilServiceDirection::Reverse => {}
        }
    }
    // Trim the trailing newline so the placeholder substitutes cleanly inside the class body.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The Pascal-case class name a record reference names, if it is a reference. Records emit a
/// generated type by this name, so the datagram codec and channel example refer to it directly.
fn record_ref_pascal(ty: &CsilTypeExpression) -> Option<String> {
    match ty {
        CsilTypeExpression::Reference(name) => Some(pascal_ident(name)),
        _ => None,
    }
}

/// A compiling `new Record { ... }` literal for a record-typed request. Always wraps the
/// reference in a `new` so the call site is unambiguous.
fn csharp_request_literal(
    input: &WasmGeneratorInput,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    config: &CsharpConfig,
) -> String {
    let mut visited = std::collections::HashSet::new();
    csharp_sample(input, ty, records, config, &mut visited)
}

/// A compiling C# value for `ty`: real values for scalars, a `new Record { required... }`
/// for a record reference (recursively), and `default!` for any shape a generic sample
/// can't safely fabricate (maps, arrays, choices, decimal, timestamp, aliases) — `default!`
/// type-checks against any target type, so the snippet always compiles even where the user
/// must fill a value in. `visited` breaks self-referential record cycles.
fn csharp_sample(
    input: &WasmGeneratorInput,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    config: &CsharpConfig,
    visited: &mut std::collections::HashSet<String>,
) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" | "string" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "System.Array.Empty<byte>()".to_string(),
            "int" | "uint" | "integer" | "number" | "float" | "float16" | "float32" | "float64"
            | "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
            | "nint" | "double" => "0".to_string(),
            _ => "default!".to_string(),
        },
        CsilTypeExpression::Reference(name) => {
            let pascal = pascal_ident(name);
            match find_record_group(input, name) {
                Some(group) if !visited.contains(&pascal) => {
                    visited.insert(pascal.clone());
                    let literal = record_literal(input, &pascal, group, records, config, visited);
                    visited.remove(&pascal);
                    literal
                }
                // A non-record reference (scalar/collection alias) or a recursive cycle:
                // `default!` is the always-compiling escape.
                _ => "default!".to_string(),
            }
        }
        _ => "default!".to_string(),
    }
}

/// `new Pascal { Prop = <sample>, ... }` over a record's required fields (optional fields
/// are nullable `init` properties and may be omitted).
fn record_literal(
    input: &WasmGeneratorInput,
    pascal: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    config: &CsharpConfig,
    visited: &mut std::collections::HashSet<String>,
) -> String {
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            e.key.as_ref().map(|key| {
                format!(
                    "{} = {}",
                    member_ident(pascal, &wire_key(key)),
                    csharp_sample(input, &e.value_type, records, config, visited)
                )
            })
        })
        .collect();
    if fields.is_empty() {
        format!("new {pascal} {{ }}")
    } else {
        format!("new {pascal} {{ {} }}", fields.join(", "))
    }
}

/// The record group a type name refers to, if it names a record (`Name = { ... }` parses as
/// `TypeDef(Group)`; a bare group rule is `GroupDef`).
fn find_record_group<'a>(
    input: &'a WasmGeneratorInput,
    name: &str,
) -> Option<&'a CsilGroupExpression> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        }
    })
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn generate_types(input: &WasmGeneratorInput, config: &CsharpConfig) -> Option<String> {
    let mut body = String::new();
    let mut aliases: Vec<String> = Vec::new();
    let mut has_types = false;

    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                has_types = true;
                emit_record(&mut body, &rule.name, group, config);
            }
            CsilRuleType::TypeDef(type_expr) => {
                has_types = true;
                if let CsilTypeExpression::Group(group) = type_expr {
                    emit_record(&mut body, &rule.name, group, config);
                } else if let CsilTypeExpression::Choice(choices) = type_expr {
                    // A named type-choice (`Color = "a" / "b"`, `IdOrName = uint / text`) is a
                    // real enum or tagged union with its own codec, not a transparent alias.
                    emit_type_choice(&mut body, &rule.name, choices, config);
                } else {
                    // A scalar/reference/collection alias. `global using` (not a plain
                    // file-scoped `using`) so the named type is visible from the service
                    // and client files too; the target is namespace-qualified because a
                    // global-using alias resolves its right side in the global namespace.
                    let target = map_csil_type_qualified(type_expr, config);
                    aliases.push(format!(
                        "global using {} = {target};",
                        pascal_ident(&rule.name)
                    ));
                }
            }
            CsilRuleType::TypeChoice(choices) => {
                has_types = true;
                emit_type_choice(&mut body, &rule.name, choices, config);
            }
            CsilRuleType::GroupChoice(choices) => {
                has_types = true;
                emit_group_choice(&mut body, &rule.name, choices, config);
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    if !has_types {
        return None;
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    // Using-alias directives must precede the file-scoped namespace.
    if !aliases.is_empty() {
        for alias in &aliases {
            content.push_str(alias);
            content.push('\n');
        }
        content.push('\n');
    }
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(&body);
    Some(content)
}

/// Emit a CSIL struct as a `public sealed record` with `required`/nullable `init`
/// properties. The CSIL field name is preserved verbatim as the CBOR wire key in a
/// comment above each property; the PascalCase property name is generator-side only.
fn emit_record(body: &mut String, name: &str, group: &CsilGroupExpression, config: &CsharpConfig) {
    let record = pascal_ident(name);
    body.push_str(&format!("public sealed record {record}\n{{\n"));

    for entry in &group.entries {
        if let Some(key) = &entry.key {
            let wire = wire_key(key);
            let prop = member_ident(&record, &wire);
            let base = map_csil_type(&entry.value_type, config);
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));

            body.push_str(&format!("    // CBOR key: {wire}\n"));
            if optional {
                let nullable = csharp_nullable(&base);
                body.push_str(&format!("    public {nullable} {prop} {{ get; init; }}\n"));
            } else {
                body.push_str(&format!(
                    "    public required {base} {prop} {{ get; init; }}\n"
                ));
            }
        }
    }

    if group.entries.iter().any(entry_has_check) {
        body.push('\n');
        body.push_str(
            "    /// <summary>Throws System.ArgumentException when a field violates a CSIL constraint.</summary>\n",
        );
        body.push_str("    public void Validate()\n    {\n");
        for entry in &group.entries {
            if let Some(key) = &entry.key {
                let field = FieldRef::new(
                    member_ident(&record, &wire_key(key)),
                    matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
                );
                // Every check on this field is collected unguarded first, then — for
                // an optional field — nested inside one shared null-narrowing `if` so
                // the guard variable is declared exactly once per field (see
                // `FieldRef` doc comment for why per-check guards collide as CS0128).
                let mut checks = String::new();
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Constraint(constraint) = metadata {
                        emit_metadata_constraint(
                            &mut checks,
                            &field,
                            &entry.value_type,
                            constraint,
                            config,
                        );
                    }
                }
                if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                    for op in constraints {
                        emit_control_op_check(&mut checks, &field, &entry.value_type, op, config);
                    }
                }
                if checks.is_empty() {
                    continue;
                }
                if field.optional {
                    body.push_str(&format!(
                        "        if ({} is {{ }} {})\n        {{\n",
                        field.prop, field.local
                    ));
                    body.push_str(&indent_block(&checks));
                    body.push_str("        }\n");
                } else {
                    body.push_str(&checks);
                }
            }
        }
        body.push_str("    }\n");
    }

    body.push_str("}\n\n");
}

/// A type-choice is either a closed enum (every arm a literal) or a tagged union of
/// reference/shape arms emulated as a `sealed abstract record` base plus one
/// `sealed record` per arm.
fn emit_type_choice(
    body: &mut String,
    name: &str,
    choices: &[CsilTypeExpression],
    config: &CsharpConfig,
) {
    // `classify_choice` is THE normative enum/union split (csilgen_common::choice): every
    // arm a literal, of any kind or a mix of kinds, is an enum; this call site must agree
    // with `emit_choice_codec`'s or the declaration and the codec disagree on the shape.
    if matches!(classify_choice(choices), ChoiceClass::Enum(_)) {
        emit_enum(body, name, choices);
        return;
    }

    let base = pascal_ident(name);
    body.push_str(
        "// Closed discriminated union; consume with an exhaustive `switch` expression.\n",
    );
    body.push_str(&format!("public abstract record {base};\n"));
    for (index, choice) in choices.iter().enumerate() {
        let arm = union_arm_name(&base, index, choice);
        let inner = map_csil_type(choice, config);
        if let CsilTypeExpression::Reference(reference) = choice {
            // The CSIL variant wire name is the reference verbatim so a decoder can map the
            // tag back to this arm.
            body.push_str(&format!("// variant {} '{reference}'\n", index));
        } else {
            body.push_str(&format!("// variant {index}\n"));
        }
        body.push_str(&format!(
            "public sealed record {arm}({inner} Value) : {base};\n"
        ));
    }
    body.push('\n');
}

/// The concrete arm record name for variant `index` of a union named `base`. A reference
/// arm is named after the referenced type (`IdOrNameTask`); any other shape is positional
/// (`IdOrNameVariant1`, 1-based for readability). Shared by the type emitter and the codec
/// so the two never drift on the arm spelling.
fn union_arm_name(base: &str, index: usize, choice: &CsilTypeExpression) -> String {
    match choice {
        CsilTypeExpression::Reference(reference) => format!("{base}{}", pascal_ident(reference)),
        _ => format!("{base}Variant{}", index + 1),
    }
}

/// A group-choice is a tagged union whose arms are anonymous records; each arm
/// becomes a `sealed record` carrying that arm's fields.
fn emit_group_choice(
    body: &mut String,
    name: &str,
    choices: &[CsilGroupExpression],
    config: &CsharpConfig,
) {
    let base = pascal_ident(name);
    body.push_str(
        "// Closed discriminated union; consume with an exhaustive `switch` expression.\n",
    );
    body.push_str(&format!("public abstract record {base};\n\n"));
    for (index, choice) in choices.iter().enumerate() {
        let arm = format!("{base}Variant{}", index + 1);
        body.push_str(&format!("// variant {} of {base}\n", index + 1));
        body.push_str(&format!("public sealed record {arm} : {base}\n{{\n"));
        for entry in &choice.entries {
            if let Some(key) = &entry.key {
                let wire = wire_key(key);
                let prop = member_ident(&arm, &wire);
                let base_type = map_csil_type(&entry.value_type, config);
                let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
                body.push_str(&format!("    // CBOR key: {wire}\n"));
                if optional {
                    let nullable = csharp_nullable(&base_type);
                    body.push_str(&format!("    public {nullable} {prop} {{ get; init; }}\n"));
                } else {
                    body.push_str(&format!(
                        "    public required {base_type} {prop} {{ get; init; }}\n"
                    ));
                }
            }
        }
        body.push_str("}\n\n");
    }
}

/// The C# enum member identifier for one literal-choice arm. `Text`/`Integer` keep their
/// original spelling (the text verbatim, or `Value{n}` for an int) so an all-`Text`- or
/// all-`Integer`-arm enum — the only kinds `classify_choice` recognized before this
/// migration — generates a byte-identical member list. The other literal kinds are only
/// reachable now that `classify_choice` folds every literal kind (not just Text/Integer)
/// into `Enum`; `index` disambiguates a kind with no natural short textual form
/// (`Bytes`/`Array`) since two distinct values of that kind would otherwise collide on the
/// same generic member name.
fn enum_member_ident(enum_name: &str, literal: &CsilLiteralValue, index: usize) -> String {
    match literal {
        CsilLiteralValue::Text(text) => member_ident(enum_name, text),
        CsilLiteralValue::Integer(value) => member_ident(enum_name, &format!("Value{value}")),
        CsilLiteralValue::Bool(b) => member_ident(enum_name, if *b { "True" } else { "False" }),
        CsilLiteralValue::Null => member_ident(enum_name, "Null"),
        CsilLiteralValue::Float(_) => member_ident(enum_name, &format!("Value{index}")),
        CsilLiteralValue::Bytes(_) => member_ident(enum_name, &format!("Bytes{index}")),
        CsilLiteralValue::Array(_) => member_ident(enum_name, &format!("Array{index}")),
    }
}

fn emit_enum(body: &mut String, name: &str, choices: &[CsilTypeExpression]) {
    let enum_name = pascal_ident(name);
    body.push_str(&format!("public enum {enum_name}\n{{\n"));
    for (index, choice) in choices.iter().enumerate() {
        // `choice_arm_literal` sees through a `.default`-style control-operator wrapper so a
        // trailing-`.default` literal arm still contributes its enum member. `classify_choice`
        // guarantees every arm here carries a literal (that is what makes this an `Enum` in
        // the first place), so `None` cannot occur.
        let Some(literal) = choice_arm_literal(choice) else {
            continue;
        };
        let member = enum_member_ident(&enum_name, literal, index);
        match literal {
            CsilLiteralValue::Text(text) => {
                // The literal text is the wire value verbatim; the member name is a
                // generator-side PascalCase mapping of it.
                body.push_str(&format!("    // wire value: {text}\n"));
                body.push_str(&format!("    {member},\n"));
            }
            CsilLiteralValue::Integer(value) => {
                body.push_str(&format!("    {member} = {value},\n"));
            }
            _ => {
                body.push_str(&format!("    {member},\n"));
            }
        }
    }
    body.push_str("}\n\n");
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// The synchronous byte transport seam. Kept byte-identical to its original form so a
/// `sync` (or default-`both`) client's output never changes.
const CLIENT_TRANSPORT_SYNC: &str = "\
/// <summary>The caller-supplied dumb byte transport seam. The generated client owns
/// (de)serialization (canonical CBOR via the generated codec); the transport only
/// moves bytes — it performs the call named by (service, op) with the already-encoded
/// request and returns the response bytes. Synchronous and blocking — no Task/async.</summary>
public interface ICsilTransport
{
    byte[] Call(string service, string op, byte[] request);
}
";

/// The shared client exception. Emitted only by the canonical (unmarked) prelude so that
/// when an async twin shares the namespace with the sync client it does not redefine it.
const CLIENT_EXCEPTION: &str = "\
/// <summary>Raised by a generated client when the transport reports a failure.</summary>
public sealed class CsilClientException : System.Exception
{
    public long Code { get; }

    public CsilClientException(long code, string message) : base(message)
    {
        Code = code;
    }
}
";

/// The async byte transport seam, parameterized by interface name so the drop-in can reuse
/// the canonical `ICsilTransport` while the twin takes `ICsilAsyncTransport`. The seam owns
/// the I/O round-trip and so returns a `Task<byte[]>`; the generated codec stays synchronous.
fn client_transport_async(transport: &str) -> String {
    format!(
        "/// <summary>The caller-supplied dumb byte transport seam (async variant). The generated\n/// client owns (de)serialization (canonical CBOR via the generated codec); the transport\n/// only moves bytes — it performs the call named by (service, op) with the already-encoded\n/// request and returns a Task of the response bytes. The generated codec stays synchronous.</summary>\npublic interface {transport}\n{{\n    System.Threading.Tasks.Task<byte[]> Call(string service, string op, byte[] request);\n}}\n"
    )
}

/// The prelude (transport interface + shared exception) for one client file. The exception
/// rides only with the canonical (unmarked) prelude; the marked twin reuses the sync file's
/// copy since both compile into one namespace.
fn client_prelude(shape: &ClientShape) -> String {
    let mut prelude = String::new();
    if shape.is_async {
        prelude.push_str(&client_transport_async(&shape.transport_name()));
    } else {
        prelude.push_str(CLIENT_TRANSPORT_SYNC);
    }
    if shape.marker.is_empty() {
        prelude.push('\n');
        prelude.push_str(CLIENT_EXCEPTION);
    }
    prelude
}

fn generate_client(
    input: &WasmGeneratorInput,
    config: &CsharpConfig,
    shape: ClientShape,
) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    let records = record_names(input);
    // Aliases and choices let the client tell an expressible non-record boundary (which
    // rides a per-op codec helper) from an inexpressible one (which it still skips).
    let aliases = codec_aliases(input);
    let choices = choice_names(input);

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_class(
                &mut body, &rule.name, service, config, &records, &aliases, &choices, shape,
            );
            emitted = true;
        }
    }

    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(&client_prelude(&shape));
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

#[allow(clippy::too_many_arguments)]
fn emit_client_class(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
    shape: ClientShape,
) {
    let base = service_base(name);
    let client = shape.class_name(&base);
    let transport = shape.transport_name();

    // A primary-constructor parameter C# never reads inside the class body is a
    // compile warning (CS9113 "parameter is unread"): a channel-only service (every
    // op `<->`/`->>`, none `Unidirectional`), or one whose every unary op has a
    // payload `op_boundary_expressible` can't model, would emit a `transport` param
    // no method ever calls. Rather than suppress the warning, skip the class outright
    // — mirroring the typescript generator's `has_services` gating and go/python,
    // which likewise never emit a client with no callable method.
    let has_usable_op = service.operations.iter().any(|operation| {
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            return false;
        }
        let success = success_type(&operation.output_type);
        let null_input = op_input_is_null(&operation.input_type);
        let req_ok =
            null_input || op_boundary_expressible(&operation.input_type, records, aliases, choices);
        req_ok && op_boundary_expressible(&success, records, aliases, choices)
    });
    if !has_usable_op {
        body.push_str(&format!(
            "// {name} has no unary operation csilgen can put on an RPC client (channel-only,\n\
             // or every payload is inexpressible); no {client} is emitted so `transport`\n\
             // is never left an unread constructor parameter.\n\n"
        ));
        return;
    }

    body.push_str(&format!(
        "/// <summary>Typed RPC client for the {name} service. The client owns\n/// (de)serialization via the generated codec; the transport only moves bytes.</summary>\n"
    ));
    body.push_str(&format!(
        "public sealed class {client}({transport} transport)\n{{\n"
    ));

    for operation in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops ride
        // the router/encoder surface emitted by the server target.
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            body.push_str(&format!(
                "    // channel operation {} is not part of the RPC client\n",
                operation.name
            ));
            continue;
        }
        let success = success_type(&operation.output_type);
        let null_input = op_input_is_null(&operation.input_type);
        let req_ok =
            null_input || op_boundary_expressible(&operation.input_type, records, aliases, choices);
        // Only a genuinely inexpressible boundary (an inline multi-variant choice with no
        // wire discriminator, or an unmodeled reference) is skipped now; scalar/array/map
        // shapes ride the per-op codec helpers, so every other op gets a method.
        if !req_ok || !op_boundary_expressible(&success, records, aliases, choices) {
            body.push_str(&format!(
                "    // operation '{}' has a payload csilgen can't (de)serialize; handle it manually\n",
                operation.name
            ));
            continue;
        }
        // The `Async` suffix rides the marker so the twin's `SubmitTaskAsync` coexists with
        // the sync `SubmitTask`, while the drop-in keeps the canonical name.
        let method = format!("{}{}", pascal_ident(&operation.name), shape.marker);
        let output = map_csil_type(&success, config);
        let stem = op_codec_stem(name, &operation.name);
        // Only the seam round-trip and the return type turn async; `System.Threading.Tasks`
        // is fully qualified so a record literally named `Task` never shadows the future.
        let (async_kw, await_kw, ret) = if shape.is_async {
            (
                "async ",
                "await ",
                format!("System.Threading.Tasks.Task<{output}>"),
            )
        } else {
            ("", "", output.clone())
        };
        // A null request carries an empty body; a record reuses `Codec.Encode<T>`; any
        // other shape uses the op's per-op request encoder.
        let (params_sig, req_bytes) = match op_param(&operation.input_type) {
            None => (String::new(), "System.Array.Empty<byte>()".to_string()),
            Some(param) => {
                let input = map_csil_type(&operation.input_type, config);
                let enc = if is_record_ref(&operation.input_type, records) {
                    format!("Codec.Encode({param})")
                } else {
                    format!("Codec.Encode{stem}Request({param})")
                };
                (format!("{input} {param}"), enc)
            }
        };
        // Wire strings are the verbatim CSIL service and operation names
        // (csil-rpc-transport.md §1.1/§1.3), distinct from the C# identifiers. They
        // never change with the client shape — the async twin rides the same wire.
        let call = format!(
            "{await_kw}transport.Call(\"{name}\", \"{wire_op}\", {req_bytes})",
            wire_op = operation.name
        );
        // A record success reuses the generic `Codec.Decode<T>`; any other shape uses the
        // op's per-op response decoder.
        let decode = if is_record_ref(&success, records) {
            format!("Codec.Decode<{output}>({call})")
        } else {
            format!("Codec.Decode{stem}Response({call})")
        };
        body.push_str(&format!(
            "    public {async_kw}{ret} {method}({params_sig}) =>\n        {decode};\n"
        ));
    }

    body.push_str("}\n\n");
}

/// Whether a type is a reference to a record the codec can (de)serialize, so a typed
/// client method can round-trip it through the generated `Codec`.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(&pascal_ident(name)))
}

/// Whether `csharp_enc_value`/`csharp_dec_value` model an op-boundary type faithfully, so
/// a per-op codec helper round-trips it rather than silently stubbing it to null. Records,
/// scalars, transparent aliases, named enums/unions (choices), arrays, maps, and tuples all
/// resolve to real codec expressions. An inline multi-variant choice has no wire
/// discriminator and an unmodeled reference has no codec, so those two keep the
/// skip-with-note path the client falls back to.
fn op_boundary_expressible(
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> bool {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(_) => true,
        CsilTypeExpression::Reference(name) => {
            let pascal = pascal_ident(name);
            records.contains(&pascal) || aliases.contains_key(&pascal) || choices.contains(&pascal)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            op_boundary_expressible(element_type, records, aliases, choices)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            op_boundary_expressible(key, records, aliases, choices)
                && op_boundary_expressible(value, records, aliases, choices)
        }
        CsilTypeExpression::Tuple(_) => true,
        _ => false,
    }
}

/// The `<Base><Method>` stem shared by an op's per-op codec helpers and the client method
/// that calls them, so the two never drift (`MemberService.get-member` → `MemberGetMember`).
fn op_codec_stem(service_name: &str, op_name: &str) -> String {
    format!("{}{}", service_base(service_name), pascal_ident(op_name))
}

/// Per-op CBOR helpers on the generated `Codec` for every NON-record op boundary, so the
/// typed client (and a server in another assembly) shares one byte seam for scalar-id
/// requests, `[]T`/map responses, and the like — not just record↔record. Record boundaries
/// keep the generic `Codec.Encode<T>`/`Codec.Decode<T>`, so a record-only spec stays
/// byte-identical and only specs with non-record ops grow these methods.
fn emit_op_codecs(
    input: &WasmGeneratorInput,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
    config: &CsharpConfig,
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
            let null_input = op_input_is_null(&op.input_type);
            let req_ok =
                null_input || op_boundary_expressible(&op.input_type, records, aliases, choices);
            if !req_ok || !op_boundary_expressible(&success, records, aliases, choices) {
                continue;
            }
            let stem = op_codec_stem(&rule.name, &op.name);
            // A null request carries an empty body and a record reuses `Codec.Encode<T>`,
            // so only a non-null, non-record request needs its own per-op helper.
            if !null_input && !is_record_ref(&op.input_type, records) {
                out.push_str(&emit_op_codec_pair(
                    &format!("{stem}Request"),
                    &op.input_type,
                    records,
                    aliases,
                    choices,
                    config,
                ));
            }
            if !is_record_ref(&success, records) {
                out.push_str(&emit_op_codec_pair(
                    &format!("{stem}Response"),
                    &success,
                    records,
                    aliases,
                    choices,
                    config,
                ));
            }
        }
    }
    out
}

/// One `Encode<Name>`/`Decode<Name>` pair over the same value builders the record codec
/// uses, so an arbitrary op-boundary shape gets the byte seam a record type has.
fn emit_op_codec_pair(
    helper: &str,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
    config: &CsharpConfig,
) -> String {
    let cs_type = map_csil_type(ty, config);
    let enc = csharp_enc_value(ty, "value", records, aliases, choices);
    let dec = csharp_dec_value(ty, "Cbor.Decode(data)", records, aliases, choices);
    format!(
        "    /// <summary>Encode the {helper} op-boundary payload to canonical CSIL CBOR bytes.</summary>\n    \
         public static byte[] Encode{helper}({cs_type} value) => Cbor.Encode({enc});\n\n    \
         /// <summary>Decode canonical CSIL CBOR bytes into the {helper} op-boundary payload.</summary>\n    \
         public static {cs_type} Decode{helper}(byte[] data) => {dec};\n\n"
    )
}

// ---------------------------------------------------------------------------
// Codec (Codec.gen.cs)
// ---------------------------------------------------------------------------

/// The C# record type names (the types whose CBOR form is a map and which the codec
/// covers with a `<T>ToCborValue`/`<T>FromCborValue` pair).
fn record_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(_) => Some(pascal_ident(&rule.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(pascal_ident(&rule.name)),
            _ => None,
        })
        .collect()
}

/// Whether the spec declares any record type (and so wants a `Codec.gen.cs`).
fn spec_has_records(input: &WasmGeneratorInput) -> bool {
    input.csil_spec.rules.iter().any(|rule| {
        matches!(
            &rule.rule_type,
            CsilRuleType::GroupDef(_) | CsilRuleType::TypeDef(CsilTypeExpression::Group(_))
        )
    })
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target is
/// a map / array / scalar / reference / tuple (NOT a record `Group` or a `Choice`, which
/// have their own handling). C# spells such an alias as a `global using` synonym, so a
/// field typed `StringInt64Map` *is* `Dictionary<string, long>` at the use site; without
/// resolving the reference here the codec would stub the field to null and drop its data.
/// Keyed by the same `pascal_ident` spelling the `records` set uses so a `Reference` is
/// looked up identically in both sets.
fn codec_aliases(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((pascal_ident(&rule.name), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Pascal names of every named type-choice rule (enum or tagged union). A field that
/// references one routes through that choice's generated `…ToCborValue`/`…FromCborValue`
/// helper, rather than being stubbed to null the way a plain reference once was.
fn choice_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(_)) | CsilRuleType::TypeChoice(_) => {
                Some(pascal_ident(&rule.name))
            }
            _ => None,
        })
        .collect()
}

/// The CBOR encoding of a text key. Comparing these byte slices lexicographically is
/// exactly RFC 8949 §4.2.1 canonical key ordering, computed here at generation time so
/// the emitted encoder lays a record's map keys down in canonical order.
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

fn codec_unwrap_constrained(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => base_type,
        other => other,
    }
}

/// A C# expression building a `CborValue` from `expr` (a typed value of the field's
/// mapped type). Composites compose via LINQ so nesting stays a single expression.
fn csharp_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("new CborValue.Int({expr})"),
            "uint" => format!("new CborValue.Uint({expr})"),
            "float" | "float64" | "double" => format!("new CborValue.Float({expr})"),
            "text" | "tstr" => format!("new CborValue.Text({expr})"),
            "bytes" | "bstr" => format!("new CborValue.Bytes({expr})"),
            "bool" => format!("new CborValue.Bool({expr})"),
            "timestamp" => format!("Cbor.EncTimestamp({expr})"),
            "decimal" => format!("Cbor.EncDecimal({expr})"),
            // `any` is already a `CborValue`; carry it through verbatim.
            "any" => expr.to_string(),
            "nil" | "null" => "new CborValue.Null()".to_string(),
            _ => "new CborValue.Null()".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&pascal_ident(name)) => {
            format!("{}ToCborValue({expr})", pascal_ident(name))
        }
        // A reference to a named enum/union routes through that choice's generated codec.
        CsilTypeExpression::Reference(name) if choices.contains(&pascal_ident(name)) => {
            format!("{}ToCborValue({expr})", pascal_ident(name))
        }
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`) carries
        // no codec of its own; encode it as its underlying map/array/scalar. The C# alias
        // is a `global using` synonym, so `expr` is already the underlying type the
        // resolved encoder expects.
        CsilTypeExpression::Reference(name) if aliases.contains_key(&pascal_ident(name)) => {
            csharp_enc_value(
                &aliases[&pascal_ident(name)],
                expr,
                records,
                aliases,
                choices,
            )
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = csharp_enc_value(element_type, "csilElem", records, aliases, choices);
            format!("new CborValue.Array({expr}.Select(csilElem => (CborValue){inner}).ToList())")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kenc = csharp_enc_value(key, "csilKv.Key", records, aliases, choices);
            let venc = csharp_enc_value(value, "csilKv.Value", records, aliases, choices);
            format!(
                "new CborValue.Map({expr}.Select(csilKv => ((CborValue){kenc}, (CborValue){venc})).ToList())"
            )
        }
        // A tuple is a fixed-length positional CBOR array.
        CsilTypeExpression::Tuple(group) => {
            csharp_tuple_enc(&group.entries, expr, records, aliases, choices)
        }
        CsilTypeExpression::Literal(lit) => csharp_literal_cbor_expr(lit),
        // A shape the codec cannot model precisely is carried as null rather than emitting
        // uncompilable code.
        _ => "new CborValue.Null()".to_string(),
    }
}

/// Encode a tuple as `new CborValue.Array(new CborValue[] { e0, e1, ... })` — one element
/// per position. An optional element is null-in-place: its CBOR null holds the slot so the
/// array stays fixed-length and positional.
fn csharp_tuple_enc(
    entries: &[CsilGroupEntry],
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    let elems: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let field = match &entry.key {
                Some(key) => pascal_ident(&wire_key(key)),
                None => format!("Field{index}"),
            };
            let access = format!("{expr}.{field}");
            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                let bind = format!("csilTup{index}");
                let non_null = map_csil_type(&entry.value_type, &codec_config());
                let enc = csharp_enc_value(&entry.value_type, &bind, records, aliases, choices);
                format!("{access} is {non_null} {bind} ? (CborValue){enc} : new CborValue.Null()")
            } else {
                csharp_enc_value(&entry.value_type, &access, records, aliases, choices)
            }
        })
        .collect();
    format!(
        "new CborValue.Array(new CborValue[] {{ {} }})",
        elems.join(", ")
    )
}

/// A C# expression decoding a typed value from `expr` (a `CborValue`).
fn csharp_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("Cbor.AsI64({expr})"),
            "uint" => format!("Cbor.AsU64({expr})"),
            "float" | "float64" | "double" => format!("Cbor.AsDouble({expr})"),
            "text" | "tstr" => format!("Cbor.AsText({expr})"),
            "bytes" | "bstr" => format!("Cbor.AsBytes({expr})"),
            "bool" => format!("Cbor.AsBool({expr})"),
            "timestamp" => format!("Cbor.AsTimestamp({expr})"),
            "decimal" => format!("Cbor.AsDecimal({expr})"),
            // `any` is carried through as the decoded `CborValue` itself.
            "any" => expr.to_string(),
            _ => format!("Cbor.AsText({expr})"),
        },
        CsilTypeExpression::Reference(name) if records.contains(&pascal_ident(name)) => {
            format!("{}FromCborValue({expr})", pascal_ident(name))
        }
        CsilTypeExpression::Reference(name) if choices.contains(&pascal_ident(name)) => {
            format!("{}FromCborValue({expr})", pascal_ident(name))
        }
        // A reference to a transparent alias decodes as its underlying map/array/scalar;
        // the value the resolved decoder returns is assignable to the alias-typed field
        // because the C# alias is a `global using` synonym for that same type.
        CsilTypeExpression::Reference(name) if aliases.contains_key(&pascal_ident(name)) => {
            csharp_dec_value(
                &aliases[&pascal_ident(name)],
                expr,
                records,
                aliases,
                choices,
            )
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = csharp_dec_value(element_type, "csilElem", records, aliases, choices);
            format!("Cbor.AsArray({expr}).Select(csilElem => {inner}).ToList()")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kdec = csharp_dec_value(key, "csilKv.Key", records, aliases, choices);
            let vdec = csharp_dec_value(value, "csilKv.Value", records, aliases, choices);
            format!("Cbor.AsMap({expr}).ToDictionary(csilKv => {kdec}, csilKv => {vdec})")
        }
        CsilTypeExpression::Tuple(group) => {
            csharp_tuple_dec(ty, &group.entries, expr, records, aliases, choices)
        }
        CsilTypeExpression::Literal(lit) => {
            let expected = csharp_literal_cbor_expr(lit);
            let value = csharp_literal_value_expr(lit);
            format!("Cbor.ExpectLiteral({expr}, {expected}, {value})")
        }
        _ => format!("Cbor.AsText({expr})"),
    }
}

/// Decode a fixed-length positional CBOR array back into a C# value tuple. The array is
/// bound once inside an immediately-invoked lambda so each position is read exactly once;
/// an optional element reads a CBOR null in its slot back to a null value.
fn csharp_tuple_dec(
    tuple_ty: &CsilTypeExpression,
    entries: &[CsilGroupEntry],
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    let tuple_type = map_csil_type(tuple_ty, &codec_config());
    let parts: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let slot = format!("csilTupArr[{index}]");
            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                let non_null = map_csil_type(&entry.value_type, &codec_config());
                let dec = csharp_dec_value(&entry.value_type, &slot, records, aliases, choices);
                format!("{slot} is CborValue.Null ? ({non_null}?)null : {dec}")
            } else {
                csharp_dec_value(&entry.value_type, &slot, records, aliases, choices)
            }
        })
        .collect();
    format!(
        "((System.Func<{tuple_type}>)(() => {{ var csilTupArr = Cbor.AsArray({expr}); return ({}); }}))()",
        parts.join(", ")
    )
}

/// Emit the per-record `ToCborValue`/`FromCborValue` pair. The encoder lays keys in
/// canonical RFC 8949 order; the decoder reads by key in declaration order (order is
/// irrelevant on decode). Both methods are members of the generated `Codec` class.
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    let type_name = pascal_ident(name);
    // (prop, wire, entry) in declaration order, plus a canonical-key-order copy for
    // the encoder so the emitted map is deterministic across languages.
    let named: Vec<(String, String, &CsilGroupEntry)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let key = e.key.as_ref()?;
            let wire = wire_key(key);
            Some((member_ident(&type_name, &wire), wire, e))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    let mut out = String::new();
    out.push_str(&format!(
        "    /// <summary>The canonical CBOR value tree for a {type_name}.</summary>\n"
    ));
    out.push_str(&format!(
        "    public static CborValue {type_name}ToCborValue({type_name} value)\n    {{\n"
    ));
    out.push_str("        var csilEntries = new System.Collections.Generic.List<(CborValue, CborValue)>();\n");
    for (index, (prop, wire, entry)) in canonical.iter().enumerate() {
        let wire_lit = csharp_escape(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // An absent optional is omitted from the map entirely (wire contract). The
            // unwrapped non-null binding is named per-field: a C# `is { }` pattern variable
            // leaks into the enclosing method scope, so a shared name would both collide
            // (CS0128) and force every optional through the first field's type (CS1503).
            let bind = format!("csilV{index}");
            let enc = csharp_enc_value(&entry.value_type, &bind, records, aliases, choices);
            out.push_str(&format!(
                "        if (value.{prop} is {{ }} {bind})\n        {{\n            csilEntries.Add((new CborValue.Text(\"{wire_lit}\"), {enc}));\n        }}\n"
            ));
        } else {
            let enc = csharp_enc_value(
                &entry.value_type,
                &format!("value.{prop}"),
                records,
                aliases,
                choices,
            );
            out.push_str(&format!(
                "        csilEntries.Add((new CborValue.Text(\"{wire_lit}\"), {enc}));\n"
            ));
        }
    }
    out.push_str("        return new CborValue.Map(csilEntries);\n    }\n\n");

    out.push_str(&format!(
        "    /// <summary>Reconstruct a {type_name} from a decoded CBOR value tree.</summary>\n"
    ));
    out.push_str(&format!(
        "    public static {type_name} {type_name}FromCborValue(CborValue value)\n    {{\n"
    ));
    for (index, (_, wire, entry)) in named.iter().enumerate() {
        let wire_lit = csharp_escape(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let nullable = csharp_nullable(&map_csil_type(&entry.value_type, &codec_config()));
            let dec = csharp_dec_value(
                &entry.value_type,
                &format!("csilRaw{index}"),
                records,
                aliases,
                choices,
            );
            out.push_str(&format!(
                "        {nullable} csilField{index} = Cbor.MapGet(value, \"{wire_lit}\") is {{ }} csilRaw{index} ? {dec} : null;\n"
            ));
        } else {
            let dec = csharp_dec_value(
                &entry.value_type,
                &format!("Cbor.Require(value, \"{wire_lit}\")"),
                records,
                aliases,
                choices,
            );
            out.push_str(&format!("        var csilField{index} = {dec};\n"));
        }
    }
    out.push_str(&format!("        return new {type_name}\n        {{\n"));
    for (index, (prop, _, _)) in named.iter().enumerate() {
        out.push_str(&format!("            {prop} = csilField{index},\n"));
    }
    out.push_str("        };\n    }\n\n");
    out
}

/// Emit the codec pair for a named type-choice. `classify_choice` (THE normative
/// enum/union split, `csilgen_common::choice`) decides the shape: an all-literal choice —
/// of any kind, even a mix of kinds — is a closed enum whose wire form is the bare literal;
/// any other choice is a tagged union encoded as the locked `[variant_index, value]`
/// 2-element array (0-based index in declaration order, recursive value codec per arm).
/// This call site must classify identically to `emit_type_choice`'s (the declaration side)
/// or the declared C# shape and its codec disagree.
fn emit_choice_codec(
    name: &str,
    cases: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    let type_name = pascal_ident(name);
    match classify_choice(cases) {
        ChoiceClass::Enum(_) => emit_enum_codec(&type_name, cases),
        ChoiceClass::Union(_) => emit_union_codec(&type_name, cases, records, aliases, choices),
    }
}

/// A boolean C# expression asserting the `CborValue` bound to `expr` structurally equals
/// literal `lit`, via a nested type + property pattern (`is Type { Value: X }`) so the
/// check composes recursively for `Array` without a bespoke runtime helper — the generated
/// `Codec` class cannot reach `Cbor.ValueEquals` (private, used only by `Cbor`'s own
/// `ExpectLiteral`). The `Integer` split mirrors `csharp_literal_cbor_expr`/`Cbor.Enc`:
/// canonical CBOR always encodes a non-negative integer as major type 0, so `Cbor.Decode`
/// hands back `CborValue.Uint` for one, never `CborValue.Int`, regardless of which C#
/// record built it.
fn csharp_literal_equals_expr(expr: &str, lit: &CsilLiteralValue) -> String {
    match lit {
        // A non-negative literal must match either wire representation: a value that came
        // straight from `ToCborValue` (this same codec's mixed-enum encode arm always emits
        // `CborValue.Int`, even for a non-negative member — see emit_enum_codec's comment on
        // why encode keeps the pre-migration rendering) as well as a value that came back
        // through actual CBOR byte encode/decode (canonical CBOR has one wire form for a
        // non-negative integer, major type 0, which this codec's byte-level decoder always
        // reconstructs as `CborValue.Uint`). Guarding on `Uint` alone made `FromCborValue`
        // reject the exact value `ToCborValue` had just produced for any in-memory (no byte
        // round-trip) caller — the defect this comment exists to prevent regressing.
        CsilLiteralValue::Integer(i) if *i >= 0 => {
            format!(
                "{expr} is CborValue.Uint {{ Value: {i}UL }} or CborValue.Int {{ Value: {i}L }}"
            )
        }
        CsilLiteralValue::Integer(i) => format!("{expr} is CborValue.Int {{ Value: {i}L }}"),
        CsilLiteralValue::Float(f) => format!("{expr} is CborValue.Float {{ Value: {f} }}"),
        CsilLiteralValue::Text(s) => {
            format!(
                "{expr} is CborValue.Text {{ Value: \"{}\" }}",
                csharp_escape(s)
            )
        }
        CsilLiteralValue::Bool(b) => format!(
            "{expr} is CborValue.Bool {{ Value: {} }}",
            if *b { "true" } else { "false" }
        ),
        CsilLiteralValue::Null => format!("{expr} is CborValue.Null"),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "({expr} is CborValue.Bytes csilLitBytes{expr_id} && csilLitBytes{expr_id}.Value.SequenceEqual(new byte[] {{ {values} }}))",
                expr_id = ident_suffix(expr)
            )
        }
        CsilLiteralValue::Array(items) => {
            let binding = format!("csilLitArr{}", ident_suffix(expr));
            let checks: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    csharp_literal_equals_expr(&format!("{binding}.Items[{index}]"), item)
                })
                .collect();
            let joined = if checks.is_empty() {
                "true".to_string()
            } else {
                checks.join(" && ")
            };
            format!(
                "({expr} is CborValue.Array {binding} && {binding}.Items.Count == {} && {joined})",
                items.len()
            )
        }
    }
}

/// A short, unique-enough identifier fragment derived from `expr` so a nested
/// `csharp_literal_equals_expr` binding (`csilLitArr...`/`csilLitBytes...`) doesn't shadow
/// an outer one when `Array` literals nest.
fn ident_suffix(expr: &str) -> String {
    expr.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Bare-literal enum codec: each member maps to its verbatim wire literal, mirroring
/// `emit_enum`'s member spelling exactly (`enum_member_ident`, shared by both).
fn emit_enum_codec(type_name: &str, cases: &[CsilTypeExpression]) -> String {
    // `choice_arm_literal` strips a `.default`-style wrapper so a trailing-`.default`
    // literal arm is still classified and encoded as its own literal value.
    let int_only = cases
        .iter()
        .all(|c| matches!(choice_arm_literal(c), Some(CsilLiteralValue::Integer(_))));
    let text_only = cases
        .iter()
        .all(|c| matches!(choice_arm_literal(c), Some(CsilLiteralValue::Text(_))));

    let mut to_arms = String::new();
    for (index, case) in cases.iter().enumerate() {
        let Some(literal) = choice_arm_literal(case) else {
            continue;
        };
        let member = enum_member_ident(type_name, literal, index);
        match literal {
            // Encode is unaffected by the mixed-kind decode bug below: each arm's `value
            // switch` produces its own `CborValue` per enum MEMBER, not per wire kind — kept
            // byte-identical to the pre-migration rendering for these two kinds.
            CsilLiteralValue::Text(text) => {
                let lit = csharp_escape(text);
                to_arms.push_str(&format!(
                    "        {type_name}.{member} => new CborValue.Text(\"{lit}\"),\n"
                ));
            }
            CsilLiteralValue::Integer(value) => {
                to_arms.push_str(&format!(
                    "        {type_name}.{member} => new CborValue.Int({value}),\n"
                ));
            }
            other => {
                let cbor_expr = csharp_literal_cbor_expr(other);
                to_arms.push_str(&format!("        {type_name}.{member} => {cbor_expr},\n"));
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "    /// <summary>The bare-literal CBOR value for a {type_name}.</summary>\n"
    ));
    out.push_str(&format!(
        "    public static CborValue {type_name}ToCborValue({type_name} value) => value switch\n    {{\n{to_arms}        _ => throw new CborException(\"invalid {type_name}\"),\n    }};\n\n"
    ));
    out.push_str(&format!(
        "    /// <summary>Reconstruct a {type_name} from its bare-literal CBOR value.</summary>\n"
    ));

    // A uniform (all-Text or all-Integer) vocabulary keeps its pre-migration decode
    // rendering byte-for-byte: a single `Cbor.AsI64`/`Cbor.AsText` extraction, then a switch
    // over the bare scalar. This is every enum any pinned interop/example spec declares
    // today. Only a choice this shortcut cannot honestly serve — a genuinely mixed-kind
    // vocabulary (the confirmed defect: `Cbor.AsText` can't hand back an `int` for a bare
    // integer pattern, CS8121), or a uniform vocabulary of some OTHER single literal kind
    // (Bool/Null/Float/Bytes/Array-only, reachable now that `classify_choice` folds every
    // literal kind into `Enum`, not just Text/Integer) — takes the general path: each arm
    // matched by ITS OWN kind-appropriate pattern against the raw `CborValue`, so no single
    // scalar extractor has to assume every member shares one wire kind.
    if int_only {
        let mut from_arms = String::new();
        for (index, case) in cases.iter().enumerate() {
            if let Some(literal @ CsilLiteralValue::Integer(value)) = choice_arm_literal(case) {
                let member = enum_member_ident(type_name, literal, index);
                from_arms.push_str(&format!("        {value} => {type_name}.{member},\n"));
            }
        }
        out.push_str(&format!(
            "    public static {type_name} {type_name}FromCborValue(CborValue value) => Cbor.AsI64(value) switch\n    {{\n{from_arms}        _ => throw new CborException(\"invalid {type_name} value\"),\n    }};\n\n"
        ));
    } else if text_only {
        let mut from_arms = String::new();
        for (index, case) in cases.iter().enumerate() {
            if let Some(literal @ CsilLiteralValue::Text(text)) = choice_arm_literal(case) {
                let member = enum_member_ident(type_name, literal, index);
                let lit = csharp_escape(text);
                from_arms.push_str(&format!("        \"{lit}\" => {type_name}.{member},\n"));
            }
        }
        out.push_str(&format!(
            "    public static {type_name} {type_name}FromCborValue(CborValue value) => Cbor.AsText(value) switch\n    {{\n{from_arms}        _ => throw new CborException(\"invalid {type_name} value\"),\n    }};\n\n"
        ));
    } else {
        let mut from_arms = String::new();
        for (index, case) in cases.iter().enumerate() {
            let Some(literal) = choice_arm_literal(case) else {
                continue;
            };
            let member = enum_member_ident(type_name, literal, index);
            let guard = csharp_literal_equals_expr("csilLitValue", literal);
            from_arms.push_str(&format!(
                "        CborValue csilLitValue when {guard} => {type_name}.{member},\n"
            ));
        }
        out.push_str(&format!(
            "    public static {type_name} {type_name}FromCborValue(CborValue value) => value switch\n    {{\n{from_arms}        _ => throw new CborException(\"invalid {type_name} value\"),\n    }};\n\n"
        ));
    }
    out
}

/// Tagged-union codec: encode as `[variant_index, value]` (0-based index in declaration
/// order), decode by reading the index then dispatching to that arm's value codec.
fn emit_union_codec(
    type_name: &str,
    cases: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashSet<String>,
) -> String {
    let mut to_arms = String::new();
    let mut from_arms = String::new();
    for (index, case) in cases.iter().enumerate() {
        let arm = union_arm_name(type_name, index, case);
        let enc = csharp_enc_value(case, "csilArm.Value", records, aliases, choices);
        let dec = csharp_dec_value(case, "csilArr[1]", records, aliases, choices);
        to_arms.push_str(&format!(
            "        {arm} csilArm => new CborValue.Array(new CborValue[] {{ new CborValue.Uint({index}), (CborValue){enc} }}),\n"
        ));
        from_arms.push_str(&format!("        {index} => new {arm}({dec}),\n"));
    }
    let mut out = String::new();
    out.push_str(&format!(
        "    /// <summary>The tagged-sum CBOR value for a {type_name}: [variant_index, value].</summary>\n"
    ));
    out.push_str(&format!(
        "    public static CborValue {type_name}ToCborValue({type_name} value) => value switch\n    {{\n{to_arms}        _ => throw new CborException(\"invalid {type_name}\"),\n    }};\n\n"
    ));
    out.push_str(&format!(
        "    /// <summary>Reconstruct a {type_name} from its tagged-sum CBOR value.</summary>\n"
    ));
    out.push_str(&format!(
        "    public static {type_name} {type_name}FromCborValue(CborValue value)\n    {{\n        var csilArr = Cbor.AsArray(value);\n        return Cbor.AsU64(csilArr[0]) switch\n        {{\n{from_arms}            _ => throw new CborException(\"invalid {type_name} variant\"),\n        }};\n    }}\n\n"
    ));
    out
}

/// The codec emits with a default decimal mapping resolver: only the in-memory type
/// name of the optional-field local matters here (the wire form is identical), so the
/// `CsilDecimal` default suffices for that spelling.
fn codec_config() -> CsharpConfig {
    CsharpConfig {
        namespace: String::new(),
        decimal_mapping: DecimalMapping::Csil,
    }
}

/// Build `Codec.gen.cs`: the self-contained canonical-CBOR runtime, the per-record
/// codec pairs, and the generic `Encode<T>`/`Decode<T>` byte surface. `None` when the
/// spec declares no record types.
fn generate_codec(input: &WasmGeneratorInput, config: &CsharpConfig) -> Option<String> {
    let records = record_names(input);
    let aliases = codec_aliases(input);
    let choices = choice_names(input);
    // Per-op byte helpers for non-record op boundaries; their presence also means a
    // record-free spec with scalar/array/map ops still needs the codec runtime + class.
    let op_codecs = emit_op_codecs(input, &records, &aliases, &choices, config);
    if !spec_has_records(input) && op_codecs.is_empty() {
        return None;
    }
    let uses_timestamp = spec_uses_builtin(input, "timestamp");
    let uses_decimal = spec_uses_builtin(input, "decimal");

    let mut methods = String::new();
    let mut to_arms = String::new();
    let mut from_arms = String::new();
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group)
            | CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                let pascal = pascal_ident(&rule.name);
                methods.push_str(&emit_record_codec(
                    &rule.name, group, &records, &aliases, &choices,
                ));
                to_arms.push_str(&format!(
                    "        {pascal} csilTyped => {pascal}ToCborValue(csilTyped),\n"
                ));
                from_arms.push_str(&format!(
                    "        if (csilType == typeof({pascal})) return {pascal}FromCborValue(value);\n"
                ));
            }
            // A named enum/union gets its own codec helper pair, reached from the record
            // fields that reference it (and from the generic byte surface).
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(cases))
            | CsilRuleType::TypeChoice(cases) => {
                let pascal = pascal_ident(&rule.name);
                methods.push_str(&emit_choice_codec(
                    &rule.name, cases, &records, &aliases, &choices,
                ));
                to_arms.push_str(&format!(
                    "        {pascal} csilTyped => {pascal}ToCborValue(csilTyped),\n"
                ));
                from_arms.push_str(&format!(
                    "        if (csilType == typeof({pascal})) return {pascal}FromCborValue(value);\n"
                ));
            }
            _ => {}
        }
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    // LINQ powers the array/map (de)serializers; the directive must precede the
    // file-scoped namespace.
    content.push_str("using System.Linq;\n\n");
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(CODEC_RUNTIME_CSHARP);
    if uses_timestamp {
        content.push('\n');
        content.push_str(CODEC_TIMESTAMP_CSHARP);
    }
    if uses_decimal {
        content.push('\n');
        content.push_str(CODEC_BIGINT_CSHARP);
        content.push('\n');
        content.push_str(match config.decimal_mapping {
            DecimalMapping::Csil => CODEC_DECIMAL_CSIL_CSHARP,
            DecimalMapping::Library => CODEC_DECIMAL_LIBRARY_CSHARP,
        });
    }
    content.push('\n');

    content.push_str(
        "/// <summary>The typed CBOR codec: encodes/decodes a generated record to and\n/// from canonical CSIL CBOR bytes. Static type dispatch, never reflection.</summary>\n",
    );
    content.push_str("public static class Codec\n{\n");
    content.push_str(
        "    public static byte[] Encode<T>(T value) => Cbor.Encode(ToCborValue(value!));\n\n",
    );
    content
        .push_str("    public static T Decode<T>(byte[] data) => (T)FromCborValue(typeof(T), Cbor.Decode(data));\n\n");
    content.push_str("    static CborValue ToCborValue(object value) => value switch\n    {\n");
    content.push_str(&to_arms);
    content.push_str(
        "        _ => throw new System.ArgumentException(\"csilgen: no CSIL codec for the requested type\"),\n    };\n\n",
    );
    content.push_str(
        "    static object FromCborValue(System.Type csilType, CborValue value)\n    {\n",
    );
    content.push_str(&from_arms);
    content.push_str(
        "        throw new System.ArgumentException(\"csilgen: no CSIL codec for the requested type\");\n    }\n\n",
    );
    content.push_str(&methods);
    content.push_str(&op_codecs);
    // Drop the trailing blank line the last method leaves so the class closes cleanly.
    while content.ends_with("\n\n") {
        content.pop();
    }
    content.push_str("}\n");
    Some(content)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

const CODEC_PRELUDE: &str = "\
/// <summary>The consumer-supplied (de)serialization layer for channel messages. The
/// generator is codec-agnostic; the implementer wires this to CBOR, JSON, or anything
/// else its protocol expects.</summary>
public interface ICsilCodec
{
    byte[] Encode(object value);
    object Decode(byte[] data, System.Type targetType);
}
";

fn generate_services(input: &WasmGeneratorInput, config: &CsharpConfig) -> Option<String> {
    let mut body = String::new();
    let needs_codec = spec_has_channel_ops(input);
    let mut emitted = false;

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_service_interface(&mut body, &rule.name, service, config);
            emit_wire_ids(&mut body, &rule.name, service);
            if service_has_channel_ops(service) {
                emit_channel_router(&mut body, &rule.name, service, config);
            }
            emitted = true;
        }
    }

    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    if needs_codec {
        content.push_str(CODEC_PRELUDE);
        content.push('\n');
    }
    content.push_str(&body);
    Some(content)
}

fn emit_service_interface(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
) {
    let iface = service_interface_name(name);
    body.push_str(&format!(
        "/// <summary>Server handler interface for the {name} service.</summary>\n"
    ));
    body.push_str(&format!("public interface {iface}\n{{\n"));

    for operation in &service.operations {
        let method = pascal_ident(&operation.name);
        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_csil_type(&success_type(&operation.output_type), config);
                match op_param(&operation.input_type) {
                    None => body.push_str(&format!("    {output} {method}();\n")),
                    Some(param) => {
                        let input = map_csil_type(&operation.input_type, config);
                        body.push_str(&format!("    {output} {method}({input} {param});\n"));
                    }
                }
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the host's plumbing pulls a frame and hands
                // it to the channel router, which decodes and dispatches here.
                let input = map_csil_type(&operation.input_type, config);
                let param =
                    op_param(&operation.input_type).unwrap_or_else(|| "message".to_string());
                body.push_str(&format!("    void {method}({input} {param});\n"));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound handler method.
            }
        }
    }

    body.push_str("}\n\n");
}

/// Emit wire-id ordinal constants exposing `@wire-id(N)` so a host references them
/// instead of hardcoding. Purely additive: emits nothing unless the service carries a
/// wire-id, keeping wire-id-free output byte-identical.
fn emit_wire_ids(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let base = service_base(name);
    body.push_str(&format!(
        "/// <summary>Wire-id ordinals for the {name} service (transport compact profiles).</summary>\n"
    ));
    body.push_str(&format!("public static class {base}WireIds\n{{\n"));
    body.push_str(&format!("    public const ulong Service = {service_id};\n"));
    for operation in &service.operations {
        if let Some(op_id) = operation.wire_id {
            let method = pascal_ident(&operation.name);
            body.push_str(&format!("    public const ulong {method} = {op_id};\n"));
        }
    }
    body.push_str("}\n\n");
}

/// Emit the channel router(s). The verbose router dispatches on the wire method name;
/// the compact twin dispatches on the `@wire-id` ordinal and is emitted ONLY when the
/// service carries wire-ids, so wire-id-free specs stay byte-identical. Outbound
/// encoders are emitted for every server-pushed (bidirectional/reverse) op.
fn emit_channel_router(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
) {
    let base = service_base(name);
    let iface = service_interface_name(name);
    body.push_str(&format!(
        "/// <summary>Channel routers + outbound encoders for the {name} service.</summary>\n"
    ));
    body.push_str(&format!("public static class {base}Router\n{{\n"));

    // Verbose router: switch on the wire method name string.
    body.push_str(&format!(
        "    public static void RouteChannel({iface} handlers, ICsilCodec codec, string method, byte[] data)\n    {{\n"
    ));
    body.push_str("        switch (method)\n        {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = pascal_ident(&operation.name);
        let input = map_csil_type(&operation.input_type, config);
        // The case key is the verbatim CSIL operation name (csil-rpc-transport.md
        // §1.3), matching what the peer's op encoder frames.
        body.push_str(&format!(
            "            case \"{}\":\n            {{\n",
            operation.name
        ));
        body.push_str(&format!(
            "                var message = ({input})codec.Decode(data, typeof({input}));\n"
        ));
        body.push_str(&format!("                handlers.{method}(message);\n"));
        body.push_str("                return;\n            }\n");
    }
    body.push_str(
        "            default:\n                throw new System.ArgumentException($\"unknown channel method '{method}'\");\n",
    );
    body.push_str("        }\n    }\n\n");

    // Compact router: emitted only for wire-id-bearing services.
    if service.wire_id.is_some() {
        body.push_str(&format!(
            "    public static void RouteChannelCompact({iface} handlers, ICsilCodec codec, ulong op, byte[] data)\n    {{\n"
        ));
        body.push_str("        switch (op)\n        {\n");
        for operation in &service.operations {
            if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let Some(op_id) = operation.wire_id else {
                continue;
            };
            let method = pascal_ident(&operation.name);
            let input = map_csil_type(&operation.input_type, config);
            body.push_str(&format!("            case {op_id}:\n            {{\n"));
            body.push_str(&format!(
                "                var message = ({input})codec.Decode(data, typeof({input}));\n"
            ));
            body.push_str(&format!("                handlers.{method}(message);\n"));
            body.push_str("                return;\n            }\n");
        }
        body.push_str(
            "            default:\n                throw new System.ArgumentException($\"unknown channel ordinal {op}\");\n",
        );
        body.push_str("        }\n    }\n\n");
    }

    // Outbound encoders for every server-pushed op (bidirectional + reverse).
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method = pascal_ident(&operation.name);
        let output = map_csil_type(&operation.output_type, config);
        body.push_str(&format!(
            "    public static (string Method, byte[] Data) Encode{method}(ICsilCodec codec, {output} message)\n    {{\n"
        ));
        body.push_str("        var data = codec.Encode(message);\n");
        // The framed wire string is the verbatim CSIL operation name; only the
        // `Encode<Op>` method identifier is PascalCased.
        body.push_str(&format!("        return (\"{}\", data);\n", operation.name));
        body.push_str("    }\n\n");
    }

    // Each member trails a blank line as a separator; drop the last one so the class
    // closes without a stray blank line before its `}` (what dotnet format expects).
    if body.ends_with("\n\n") {
        body.pop();
    }
    body.push_str("}\n\n");
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A property's C# name plus whether it is optional (nullable). Threaded through the
/// check emitters so each can guard a null optional with `if (X is { } localGuard)`.
///
/// `local` is the pattern-variable name a null-narrowing guard binds to. C# scopes an
/// `is` pattern variable to the whole enclosing block, not just its `if` statement, so
/// two guards sharing a name collide as CS0128 even when they sit in separate sibling
/// `if`s (e.g. one per constraint on the same field, or one per optional field in the
/// same `Validate()`). A fixed name like `value` reused across every guard in a method
/// is exactly that collision; deriving `local` from `prop` keeps it unique, since C#
/// already requires `prop` to be unique among a record's members.
struct FieldRef {
    prop: String,
    optional: bool,
    local: String,
}

impl FieldRef {
    fn new(prop: String, optional: bool) -> Self {
        let local = guard_local_name(&prop);
        Self {
            prop,
            optional,
            local,
        }
    }

    /// The expression a check reads. An optional field is unwrapped to its bound
    /// non-null guard variable inside the narrowing block; a required field reads the
    /// property directly.
    fn access(&self) -> &str {
        if self.optional {
            &self.local
        } else {
            &self.prop
        }
    }

    /// One unguarded check: `if (cond) { throw ...; }` at the method's base indent.
    /// All of a field's checks are collected at this same indent, then — for an
    /// optional field — wrapped together in a *single* null-narrowing `if` by the
    /// caller, so one guard variable covers every check on that field instead of one
    /// guard per check.
    fn check(&self, cond: &str, message: &str) -> String {
        format!(
            "        if ({cond})\n        {{\n            throw new System.ArgumentException(\"{message}\");\n        }}\n"
        )
    }
}

/// Derive an optional field's null-narrowing guard-variable name from its (unique)
/// property name: lowercase the leading letter (stripping a keyword-escape `@` first,
/// since `@` may only prefix a whole identifier) and suffix `Value`. Kept distinct from
/// `access()`'s non-optional branch (the bare property name) so a guard can never
/// shadow the property it narrows.
fn guard_local_name(prop: &str) -> String {
    let mut chars = prop.trim_start_matches('@').chars();
    match chars.next() {
        None => "value".to_string(),
        Some(first) => format!("{}{}Value", first.to_lowercase(), chars.as_str()),
    }
}

/// Indent every line of a block of already-emitted checks by one more level, for
/// nesting inside a field's null-narrowing guard `if`.
fn indent_block(checks: &str) -> String {
    checks.lines().map(|line| format!("    {line}\n")).collect()
}

fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let meta = entry.metadata.iter().any(|m| match m {
        CsilFieldMetadata::Constraint(c) => constraint_is_check(c),
        _ => false,
    });
    let op = match &entry.value_type {
        CsilTypeExpression::Constrained { constraints, .. } => {
            constraints.iter().any(control_op_is_check)
        }
        _ => false,
    };
    meta || op
}

fn constraint_is_check(constraint: &CsilValidationConstraint) -> bool {
    // `@default` is a construction concern, not a Validate() check; `regex` is the
    // only Custom that yields one.
    match constraint {
        CsilValidationConstraint::Custom { name, .. } => name == "regex",
        _ => true,
    }
}

fn control_op_is_check(op: &CsilControlOperator) -> bool {
    matches!(
        op,
        CsilControlOperator::Size(_)
            | CsilControlOperator::Regex(_)
            | CsilControlOperator::GreaterEqual(_)
            | CsilControlOperator::LessEqual(_)
            | CsilControlOperator::GreaterThan(_)
            | CsilControlOperator::LessThan(_)
            | CsilControlOperator::Equal(_)
            | CsilControlOperator::NotEqual(_)
    )
}

fn emit_metadata_constraint(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
    config: &CsharpConfig,
) {
    match constraint {
        CsilValidationConstraint::MinLength(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                "<",
                *n,
                &format!("at least {n} characters"),
            );
        }
        CsilValidationConstraint::MaxLength(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                ">",
                *n,
                &format!("at most {n} characters"),
            );
        }
        CsilValidationConstraint::MinItems(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                "<",
                *n,
                &format!("at least {n} items"),
            );
        }
        CsilValidationConstraint::MaxItems(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                ">",
                *n,
                &format!("at most {n} items"),
            );
        }
        CsilValidationConstraint::MinValue(v) => {
            emit_ordered_check(body, field, value_type, ("<", "at least"), v, config);
        }
        CsilValidationConstraint::MaxValue(v) => {
            emit_ordered_check(body, field, value_type, (">", "at most"), v, config);
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                emit_regex_check(body, field, pattern);
            }
        }
    }
}

fn emit_control_op_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
    config: &CsharpConfig,
) {
    match op {
        CsilControlOperator::GreaterEqual(v) => {
            emit_ordered_check(body, field, value_type, ("<", "at least"), v, config)
        }
        CsilControlOperator::LessEqual(v) => {
            emit_ordered_check(body, field, value_type, (">", "at most"), v, config)
        }
        CsilControlOperator::GreaterThan(v) => {
            emit_ordered_check(body, field, value_type, ("<=", "greater than"), v, config)
        }
        CsilControlOperator::LessThan(v) => {
            emit_ordered_check(body, field, value_type, (">=", "less than"), v, config)
        }
        CsilControlOperator::Equal(v) => {
            emit_ordered_check(body, field, value_type, ("!=", "equal to"), v, config)
        }
        CsilControlOperator::NotEqual(v) => {
            emit_ordered_check(body, field, value_type, ("==", "not equal to"), v, config)
        }
        CsilControlOperator::Size(size) => emit_size_check(body, field, value_type, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(body, field, pattern),
        // Applied at construction / (de)serialization, not validated here.
        CsilControlOperator::Default(_)
        | CsilControlOperator::Bits(_)
        | CsilControlOperator::And(_)
        | CsilControlOperator::Within(_)
        | CsilControlOperator::Json
        | CsilControlOperator::Cbor
        | CsilControlOperator::Cborseq => {}
    }
}

fn emit_len_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    op: &str,
    n: u64,
    tail: &str,
) {
    let accessor = len_accessor(value_type);
    let cond = format!("{}.{accessor} {op} {n}", field.access());
    let message = csharp_escape(&format!("field '{}' must have {tail}", field.prop));
    body.push_str(&field.check(&cond, &message));
}

fn emit_size_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    size: &CsilSizeConstraint,
) {
    match size {
        CsilSizeConstraint::Exact(n) => emit_len_check(
            body,
            field,
            value_type,
            "!=",
            *n,
            &format!("exactly {n} elements"),
        ),
        CsilSizeConstraint::Min(n) => emit_len_check(
            body,
            field,
            value_type,
            "<",
            *n,
            &format!("at least {n} elements"),
        ),
        CsilSizeConstraint::Max(n) => emit_len_check(
            body,
            field,
            value_type,
            ">",
            *n,
            &format!("at most {n} elements"),
        ),
        CsilSizeConstraint::Range { min, max } => {
            emit_len_check(
                body,
                field,
                value_type,
                "<",
                *min,
                &format!("at least {min} elements"),
            );
            emit_len_check(
                body,
                field,
                value_type,
                ">",
                *max,
                &format!("at most {max} elements"),
            );
        }
    }
}

fn emit_regex_check(body: &mut String, field: &FieldRef, pattern: &str) {
    let cond = format!(
        "!System.Text.RegularExpressions.Regex.IsMatch({}, \"{}\")",
        field.access(),
        csharp_escape(pattern)
    );
    let message = csharp_escape(&format!(
        "field '{}' must match pattern '{}'",
        field.prop, pattern
    ));
    body.push_str(&field.check(&cond, &message));
}

/// One ordered comparison honoring the field's type. `vop` is the C# operator whose
/// truth means the constraint is violated; numeric/timestamp fields compare with
/// operators, a CsilDecimal compares through `CompareTo` so the emitted C# is valid.
fn emit_ordered_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
    config: &CsharpConfig,
) {
    let (vop, desc) = op;
    let access = field.access();
    match ordered_kind(value_type, config) {
        OrderedKind::Numeric => {
            let Some(rendered) = literal_as_number(value) else {
                return;
            };
            let cond = format!("{access} {vop} {rendered}");
            let message =
                csharp_escape(&format!("field '{}' must be {desc} {rendered}", field.prop));
            body.push_str(&field.check(&cond, &message));
        }
        OrderedKind::LibraryDecimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let bound = format!(
                "decimal.Parse(\"{}\", System.Globalization.CultureInfo.InvariantCulture)",
                csharp_escape(&text)
            );
            let cond = format!("{access} {vop} {bound}");
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.check(&cond, &message));
        }
        OrderedKind::CsilDecimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let cond = format!(
                "{access}.CompareTo(CsilDecimal.Parse(\"{}\")) {vop} 0",
                csharp_escape(&text)
            );
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.check(&cond, &message));
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            let bound = format!("System.DateTimeOffset.Parse(\"{}\")", csharp_escape(&text));
            let cond = format!("{access} {vop} {bound}");
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.check(&cond, &message));
        }
    }
}

enum OrderedKind {
    Numeric,
    CsilDecimal,
    LibraryDecimal,
    Timestamp,
}

fn ordered_kind(value_type: &CsilTypeExpression, config: &CsharpConfig) -> OrderedKind {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    if let CsilTypeExpression::Builtin(name) = base {
        match name.as_str() {
            "decimal" => match config.decimal_mapping {
                DecimalMapping::Csil => OrderedKind::CsilDecimal,
                DecimalMapping::Library => OrderedKind::LibraryDecimal,
            },
            "timestamp" => OrderedKind::Timestamp,
            _ => OrderedKind::Numeric,
        }
    } else {
        OrderedKind::Numeric
    }
}

/// `.Length` for strings/byte arrays, `.Count` for collections; defaults to `.Length`.
fn len_accessor(value_type: &CsilTypeExpression) -> &'static str {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    match base {
        CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. } => "Count",
        CsilTypeExpression::Builtin(name) if name == "bytes" || name == "bstr" => "Length",
        _ => "Length",
    }
}

fn literal_as_number(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn literal_as_decimal_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn literal_as_timestamp_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Type mapping & helpers
// ---------------------------------------------------------------------------

/// Map a CSIL type expression to a non-nullable C# type string. Optionality is
/// applied by the caller (a record property appends `?`), so this never embeds `?`.
fn map_csil_type(type_expr: &CsilTypeExpression, config: &CsharpConfig) -> String {
    map_csil_type_inner(type_expr, config, false)
}

/// Same mapping, but generator-emitted type names (records, the `CsilDecimal` helper)
/// are prefixed with the configured namespace. A `global using` alias resolves its
/// right-hand side in the *global* namespace, so the target must be fully qualified or
/// the alias fails to find a type that lives inside `namespace Csilgen.Transport;`.
fn map_csil_type_qualified(type_expr: &CsilTypeExpression, config: &CsharpConfig) -> String {
    map_csil_type_inner(type_expr, config, true)
}

fn map_csil_type_inner(
    type_expr: &CsilTypeExpression,
    config: &CsharpConfig,
    qualify: bool,
) -> String {
    let prefix = if qualify {
        format!("{}.", config.namespace)
    } else {
        String::new()
    };
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "long".to_string(),
            "uint" => "ulong".to_string(),
            // `nint` is a CBOR negative integer; it still lands in a signed 64-bit C# `long`.
            "nint" => "long".to_string(),
            "float" | "float64" | "double" => "double".to_string(),
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`.
            "text" | "tstr" => "string".to_string(),
            "bytes" | "bstr" => "byte[]".to_string(),
            "bool" => "bool".to_string(),
            // CBOR tag 0, RFC3339, always UTC per the wire contract.
            "timestamp" => "System.DateTimeOffset".to_string(),
            // CBOR tag 4 exact decimal; concrete C# type depends on decimal_mapping. Only
            // the generated `CsilDecimal` lives in our namespace and needs qualifying.
            "decimal" => match config.decimal_mapping {
                DecimalMapping::Csil => format!("{prefix}CsilDecimal"),
                DecimalMapping::Library => "decimal".to_string(),
            },
            // CDDL's open `any` is the untyped CBOR item; it is carried verbatim as the
            // generated codec's own `CborValue` value tree so it round-trips losslessly.
            "any" => format!("{prefix}CborValue"),
            // `nil`/`null` are the CBOR null item — `object?` in C#.
            "nil" | "null" => "object?".to_string(),
            other => format!("{prefix}{}", pascal_ident(other)),
        },
        CsilTypeExpression::Reference(name) => format!("{prefix}{}", pascal_ident(name)),
        CsilTypeExpression::Array { element_type, .. } => {
            format!(
                "System.Collections.Generic.List<{}>",
                map_csil_type_inner(element_type, config, qualify)
            )
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "System.Collections.Generic.Dictionary<{}, {}>",
            map_csil_type_inner(key, config, qualify),
            map_csil_type_inner(value, config, qualify)
        ),
        // C# value tuple preserves per-position types where Go would use a struct.
        CsilTypeExpression::Tuple(group) => csharp_tuple(&group.entries, config, qualify),
        CsilTypeExpression::Literal(lit) => match lit {
            CsilLiteralValue::Integer(_) => "long".to_string(),
            CsilLiteralValue::Float(_) => "double".to_string(),
            CsilLiteralValue::Text(_) => "string".to_string(),
            CsilLiteralValue::Bool(_) => "bool".to_string(),
            CsilLiteralValue::Bytes(_) => "byte[]".to_string(),
            CsilLiteralValue::Null => "object?".to_string(),
            CsilLiteralValue::Array(_) => "object".to_string(),
        },
        CsilTypeExpression::Constrained { base_type, .. } => {
            map_csil_type_inner(base_type, config, qualify)
        }
        _ => "object".to_string(),
    }
}

/// Append C#'s nullable marker without doubling it on an already-nullable type.
fn csharp_nullable(base: &str) -> String {
    if base.ends_with('?') {
        base.to_string()
    } else {
        format!("{base}?")
    }
}

fn csharp_tuple(entries: &[CsilGroupEntry], config: &CsharpConfig, qualify: bool) -> String {
    let fields: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut field_type = map_csil_type_inner(&entry.value_type, config, qualify);
            // An optional tuple element is held as null-in-place (the wire keeps a fixed-length
            // positional array with a CBOR null where the value is absent), so its C# type is
            // nullable.
            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                field_type = csharp_nullable(&field_type);
            }
            let field_name = match &entry.key {
                Some(key) => pascal_ident(&wire_key(key)),
                None => format!("Field{index}"),
            };
            format!("{field_type} {field_name}")
        })
        .collect();
    format!("({})", fields.join(", "))
}

/// The wire (CBOR map key) string for a group key — the CSIL name verbatim.
fn wire_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` arm of a `Res / ServiceError` union — that error half surfaces as a
/// thrown exception, not part of the typed response.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !matches!(c, CsilTypeExpression::Reference(name) if name == "ServiceError"))
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

/// A push op (`-> Event`) carries a `null` input type: on a unary RPC there is no
/// request to send, so the request parameter is dropped.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// The camelCase parameter name for an operation's request, or `None` when the op
/// takes no input. A reference input names the parameter after its type (camelCased,
/// keyword-escaped — e.g. input `Event` yields `@event`); anything else is `request`.
fn op_param(type_expr: &CsilTypeExpression) -> Option<String> {
    if op_input_is_null(type_expr) {
        return None;
    }
    match type_expr {
        CsilTypeExpression::Reference(name) => Some(camel_ident(name)),
        _ => Some("request".to_string()),
    }
}

fn spec_has_channel_ops(input: &WasmGeneratorInput) -> bool {
    input.csil_spec.rules.iter().any(|r| match &r.rule_type {
        CsilRuleType::ServiceDef(def) => service_has_channel_ops(def),
        _ => false,
    })
}

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

fn spec_uses_builtin(input: &WasmGeneratorInput, builtin: &str) -> bool {
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group) => group
                .entries
                .iter()
                .any(|e| type_uses_builtin(&e.value_type, builtin)),
            CsilRuleType::TypeDef(type_expr) => type_uses_builtin(type_expr, builtin),
            CsilRuleType::TypeChoice(choices) => {
                choices.iter().any(|c| type_uses_builtin(c, builtin))
            }
            CsilRuleType::GroupChoice(choices) => choices.iter().any(|g| {
                g.entries
                    .iter()
                    .any(|e| type_uses_builtin(&e.value_type, builtin))
            }),
            CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
                type_uses_builtin(&op.input_type, builtin)
                    || type_uses_builtin(&op.output_type, builtin)
            }),
        })
}

fn type_uses_builtin(type_expr: &CsilTypeExpression, builtin: &str) -> bool {
    match type_expr {
        CsilTypeExpression::Builtin(name) => name == builtin,
        CsilTypeExpression::Array { element_type, .. } => type_uses_builtin(element_type, builtin),
        CsilTypeExpression::Map { key, value, .. } => {
            type_uses_builtin(key, builtin) || type_uses_builtin(value, builtin)
        }
        CsilTypeExpression::Choice(choices) => {
            choices.iter().any(|c| type_uses_builtin(c, builtin))
        }
        CsilTypeExpression::Constrained { base_type, .. } => type_uses_builtin(base_type, builtin),
        CsilTypeExpression::Group(group) | CsilTypeExpression::Tuple(group) => group
            .entries
            .iter()
            .any(|e| type_uses_builtin(&e.value_type, builtin)),
        _ => false,
    }
}

/// `FooService` keeps its `Service` suffix on the interface (`IFooService`); a bare
/// `Attestation` gains one (`IAttestationService`).
fn service_interface_name(name: &str) -> String {
    format!("I{}Service", service_base(name))
}

/// The service base used for the client class and wire-id prefix: the PascalCased
/// name with any trailing `Service` removed. C# identifiers only — wire strings
/// carry the verbatim CSIL service name instead (csil-rpc-transport.md §1.1).
fn service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

// ---------------------------------------------------------------------------
// Identifier casing & keyword escaping
// ---------------------------------------------------------------------------

/// PascalCase the identifier, then `@`-escape it if it collides with a C# keyword.
fn pascal_ident(s: &str) -> String {
    escape_keyword(&pascal_case(s))
}

/// camelCase the identifier, then `@`-escape it. This is where CSIL `event` (or a
/// reference type `Event` used as a parameter) becomes the escaped `@event`, since
/// C# keywords are lowercase and only surface a collision in camelCase contexts.
fn camel_ident(s: &str) -> String {
    escape_keyword(&camel_case(s))
}

/// `pascal_ident` for a member (property or enum case) of the type named `enclosing`
/// (already PascalCased). C# forbids a member spelled like its containing type (CS0542),
/// so a CSIL field such as `relation` inside record `Relation` gains a trailing
/// underscore. That escape is collision-free because `pascal_case` strips underscores,
/// so no other field's mapping can produce the escaped spelling. Only the C# member
/// name changes — the CBOR wire key stays the verbatim CSIL name.
fn member_ident(enclosing: &str, s: &str) -> String {
    let ident = pascal_ident(s);
    if ident == enclosing {
        format!("{ident}_")
    } else {
        ident
    }
}

fn pascal_case(s: &str) -> String {
    s.split(['_', '-'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

fn camel_case(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
    }
}

/// `@`-escape an identifier that collides with a C# reserved keyword so it stays a
/// legal identifier (e.g. `event` -> `@event`).
fn escape_keyword(ident: &str) -> String {
    if is_csharp_keyword(ident) {
        format!("@{ident}")
    } else {
        ident.to_string()
    }
}

fn is_csharp_keyword(ident: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "abstract",
        "as",
        "base",
        "bool",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "checked",
        "class",
        "const",
        "continue",
        "decimal",
        "default",
        "delegate",
        "do",
        "double",
        "else",
        "enum",
        "event",
        "explicit",
        "extern",
        "false",
        "finally",
        "fixed",
        "float",
        "for",
        "foreach",
        "goto",
        "if",
        "implicit",
        "in",
        "int",
        "interface",
        "internal",
        "is",
        "lock",
        "long",
        "namespace",
        "new",
        "null",
        "object",
        "operator",
        "out",
        "override",
        "params",
        "private",
        "protected",
        "public",
        "readonly",
        "ref",
        "return",
        "sbyte",
        "sealed",
        "short",
        "sizeof",
        "stackalloc",
        "static",
        "string",
        "struct",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "uint",
        "ulong",
        "unchecked",
        "unsafe",
        "ushort",
        "using",
        "virtual",
        "void",
        "volatile",
        "while",
    ];
    KEYWORDS.contains(&ident)
}

/// Escape a string for safe inclusion inside a C# double-quoted (non-verbatim)
/// literal so an embedded quote/backslash/newline can never break the literal.
fn csharp_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn csharp_literal_cbor_expr(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Integer(i) if *i >= 0 => format!("new CborValue.Uint({i}UL)"),
        CsilLiteralValue::Integer(i) => format!("new CborValue.Int({i}L)"),
        CsilLiteralValue::Float(f) => format!("new CborValue.Float({f})"),
        CsilLiteralValue::Text(s) => format!("new CborValue.Text(\"{}\")", csharp_escape(s)),
        CsilLiteralValue::Bool(b) => {
            format!("new CborValue.Bool({})", if *b { "true" } else { "false" })
        }
        CsilLiteralValue::Null => "new CborValue.Null()".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("new CborValue.Bytes(new byte[] {{ {values} }})")
        }
        CsilLiteralValue::Array(items) => {
            let values = items
                .iter()
                .map(csharp_literal_cbor_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("new CborValue.Array(new CborValue[] {{ {values} }})")
        }
    }
}

fn csharp_literal_value_expr(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Integer(i) => format!("{i}L"),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{}\"", csharp_escape(s)),
        CsilLiteralValue::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("new byte[] {{ {values} }}")
        }
        CsilLiteralValue::Array(items) => {
            let values = items
                .iter()
                .map(csharp_literal_value_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("new object?[] {{ {values} }}")
        }
    }
}

// ---------------------------------------------------------------------------
// CsilDecimal helper file
// ---------------------------------------------------------------------------

const CSIL_DECIMAL_BODY: &str = r#"/// <summary>The exact, base-10 `decimal` core type. On the wire it is CBOR tag 4
/// (decimal fraction): a two-element array [exponent, mantissa] whose value is
/// Mantissa * 10^Exponent. The value is kept as exact integers, never a float, so no
/// precision is lost. The BCL `System.Formats.Cbor` package is deliberately not taken
/// (it is an out-of-band NuGet dependency); the transport library hand-rolls the codec.</summary>
public sealed record CsilDecimal(long Exponent, System.Numerics.BigInteger Mantissa)
    : System.IComparable<CsilDecimal>
{
    /// <summary>Parse canonical decimal text (what ToString emits) into an exact value.</summary>
    public static CsilDecimal Parse(string text)
    {
        text = text.Trim();
        bool negative = false;
        if (text.StartsWith('-'))
        {
            negative = true;
            text = text[1..];
        }
        else if (text.StartsWith('+'))
        {
            text = text[1..];
        }

        string intPart = text;
        string fracPart = "";
        int dot = text.IndexOf('.');
        if (dot >= 0)
        {
            intPart = text[..dot];
            fracPart = text[(dot + 1)..];
        }

        string digits = intPart + fracPart;
        if (digits.Length == 0)
        {
            digits = "0";
        }

        var mantissa = System.Numerics.BigInteger.Parse(digits);
        if (negative)
        {
            mantissa = -mantissa;
        }
        return new CsilDecimal(-fracPart.Length, mantissa);
    }

    /// <summary>Exact ordering: both values are scaled to a common exponent and their
    /// integer mantissas compared, so no float rounding can flip the result.</summary>
    public int CompareTo(CsilDecimal? other)
    {
        if (other is null)
        {
            return 1;
        }

        System.Numerics.BigInteger left = Mantissa;
        System.Numerics.BigInteger right = other.Mantissa;
        if (Exponent > other.Exponent)
        {
            left *= System.Numerics.BigInteger.Pow(10, (int)(Exponent - other.Exponent));
        }
        else if (other.Exponent > Exponent)
        {
            right *= System.Numerics.BigInteger.Pow(10, (int)(other.Exponent - Exponent));
        }
        return left.CompareTo(right);
    }
}
"#;

fn csil_decimal_file(config: &CsharpConfig) -> String {
    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(CSIL_DECIMAL_BODY);
    content
}

// ---------------------------------------------------------------------------
// Codec runtime (static C# the generated per-record codecs build on)
// ---------------------------------------------------------------------------

/// The self-contained canonical-CBOR (RFC 8949 subset) value model, encoder, decoder,
/// and accessors. `bytes` is a `byte[]` carried as a CBOR byte string (major type 2)
/// by construction, never an array of integers. The BCL `System.Formats.Cbor` package
/// is deliberately not taken (it is an out-of-band NuGet dependency); this is the one
/// place a csilgen generator emits payload-wire code, because nothing else can.
const CODEC_RUNTIME_CSHARP: &str = r#"/// <summary>A minimal canonical-CBOR value tree: the closed set of variants the
/// generated codec builds and walks.</summary>
public abstract record CborValue
{
    public sealed record Uint(ulong Value) : CborValue;
    public sealed record Int(long Value) : CborValue;
    public sealed record Bool(bool Value) : CborValue;
    public sealed record Float(double Value) : CborValue;
    public sealed record Null : CborValue;
    public sealed record Text(string Value) : CborValue;
    public sealed record Bytes(byte[] Value) : CborValue;
    public sealed record Array(System.Collections.Generic.IReadOnlyList<CborValue> Items) : CborValue;
    public sealed record Map(System.Collections.Generic.IReadOnlyList<(CborValue Key, CborValue Value)> Entries) : CborValue;
    public sealed record Tag(ulong Number, CborValue Inner) : CborValue;
}

/// <summary>Raised when a CBOR payload is malformed or a value has an unexpected type.</summary>
public sealed class CborException : System.Exception
{
    public CborException(string message) : base(message) { }
}

/// <summary>The canonical-CBOR encoder, decoder, and typed accessors.</summary>
public static partial class Cbor
{
    public static byte[] Encode(CborValue value)
    {
        var csilOut = new System.Collections.Generic.List<byte>();
        Enc(value, csilOut);
        return csilOut.ToArray();
    }

    static void Head(byte major, ulong n, System.Collections.Generic.List<byte> csilOut)
    {
        var mt = (byte)(major << 5);
        if (n < 24)
        {
            csilOut.Add((byte)(mt | (byte)n));
        }
        else if (n < 0x100)
        {
            csilOut.Add((byte)(mt | 24));
            csilOut.Add((byte)n);
        }
        else if (n < 0x1_0000)
        {
            csilOut.Add((byte)(mt | 25));
            csilOut.Add((byte)(n >> 8));
            csilOut.Add((byte)(n & 0xff));
        }
        else if (n < 0x1_0000_0000)
        {
            csilOut.Add((byte)(mt | 26));
            for (int csilI = 24; csilI >= 0; csilI -= 8) { csilOut.Add((byte)((n >> csilI) & 0xff)); }
        }
        else
        {
            csilOut.Add((byte)(mt | 27));
            for (int csilI = 56; csilI >= 0; csilI -= 8) { csilOut.Add((byte)((n >> csilI) & 0xff)); }
        }
    }

    static void Enc(CborValue v, System.Collections.Generic.List<byte> csilOut)
    {
        switch (v)
        {
            case CborValue.Uint x:
                Head(0, x.Value, csilOut);
                break;
            case CborValue.Int x:
                if (x.Value >= 0) { Head(0, (ulong)x.Value, csilOut); }
                else { Head(1, (ulong)(-(x.Value + 1)), csilOut); }
                break;
            case CborValue.Bool x:
                csilOut.Add(x.Value ? (byte)0xf5 : (byte)0xf4);
                break;
            case CborValue.Null:
                csilOut.Add(0xf6);
                break;
            case CborValue.Float x:
                csilOut.Add(0xfb);
                var csilBits = System.BitConverter.DoubleToUInt64Bits(x.Value);
                for (int csilI = 56; csilI >= 0; csilI -= 8) { csilOut.Add((byte)((csilBits >> csilI) & 0xff)); }
                break;
            case CborValue.Text x:
                var csilU = System.Text.Encoding.UTF8.GetBytes(x.Value);
                Head(3, (ulong)csilU.Length, csilOut);
                csilOut.AddRange(csilU);
                break;
            case CborValue.Bytes x:
                Head(2, (ulong)x.Value.Length, csilOut);
                csilOut.AddRange(x.Value);
                break;
            case CborValue.Array x:
                Head(4, (ulong)x.Items.Count, csilOut);
                foreach (var csilItem in x.Items) { Enc(csilItem, csilOut); }
                break;
            case CborValue.Map x:
                Head(5, (ulong)x.Entries.Count, csilOut);
                foreach (var csilEntry in x.Entries) { Enc(csilEntry.Key, csilOut); Enc(csilEntry.Value, csilOut); }
                break;
            case CborValue.Tag x:
                Head(6, x.Number, csilOut);
                Enc(x.Inner, csilOut);
                break;
            default:
                throw new CborException("unknown CBOR value");
        }
    }

    public static CborValue Decode(byte[] b)
    {
        int csilPos = 0;
        var v = Dec(b, ref csilPos, 0);
        if (csilPos != b.Length) { throw new CborException("trailing bytes"); }
        return v;
    }

    static ulong ReadArg(byte[] b, ref int csilPos, byte low)
    {
        if (low < 24) { csilPos += 1; return low; }
        int csilWidth = low == 24 ? 1 : low == 25 ? 2 : low == 26 ? 4 : low == 27 ? 8 : 0;
        if (csilWidth == 0 || csilPos >= b.Length || b.Length - csilPos - 1 < csilWidth)
        {
            throw new CborException("truncated argument");
        }
        switch (low)
        {
            case 24:
            {
                ulong v = b[csilPos + 1];
                csilPos += 2;
                return v;
            }
            case 25:
            {
                ulong v = ((ulong)b[csilPos + 1] << 8) | b[csilPos + 2];
                csilPos += 3;
                return v;
            }
            case 26:
            {
                ulong v = 0;
                for (int csilI = 1; csilI <= 4; csilI++) { v = (v << 8) | b[csilPos + csilI]; }
                csilPos += 5;
                return v;
            }
            case 27:
            {
                ulong v = 0;
                for (int csilI = 1; csilI <= 8; csilI++) { v = (v << 8) | b[csilPos + csilI]; }
                csilPos += 9;
                return v;
            }
            default:
                throw new CborException("malformed");
        }
    }

    static CborValue Dec(byte[] b, ref int csilPos, int csilDepth)
    {
        if (csilDepth > 64) { throw new CborException("nesting limit exceeded"); }
        if (csilPos >= b.Length) { throw new CborException("unexpected end of input"); }
        var ib = b[csilPos];
        var major = (byte)(ib >> 5);
        var low = (byte)(ib & 0x1f);
        if (major == 7)
        {
            switch (low)
            {
                case 20: csilPos += 1; return new CborValue.Bool(false);
                case 21: csilPos += 1; return new CborValue.Bool(true);
                case 22:
                case 23: csilPos += 1; return new CborValue.Null();
                case 26:
                {
                    var bits = ReadArg(b, ref csilPos, low);
                    return new CborValue.Float(System.BitConverter.UInt32BitsToSingle((uint)bits));
                }
                case 27:
                {
                    var bits = ReadArg(b, ref csilPos, low);
                    return new CborValue.Float(System.BitConverter.UInt64BitsToDouble(bits));
                }
                default:
                    throw new CborException("malformed");
            }
        }
        var arg = ReadArg(b, ref csilPos, low);
        switch (major)
        {
            case 0:
                return new CborValue.Uint(arg);
            case 1:
                if (arg > long.MaxValue) { throw new CborException("malformed"); }
                return new CborValue.Int(-1 - (long)arg);
            case 2:
            {
                if (arg > (ulong)(b.Length - csilPos)) { throw new CborException("truncated byte string"); }
                var n = (int)arg;
                var slice = new byte[n];
                System.Array.Copy(b, csilPos, slice, 0, n);
                csilPos += n;
                return new CborValue.Bytes(slice);
            }
            case 3:
            {
                if (arg > (ulong)(b.Length - csilPos)) { throw new CborException("truncated text string"); }
                var n = (int)arg;
                string s;
                try { s = new System.Text.UTF8Encoding(false, true).GetString(b, csilPos, n); }
                catch (System.Text.DecoderFallbackException) { throw new CborException("invalid utf-8"); }
                csilPos += n;
                return new CborValue.Text(s);
            }
            case 4:
            {
                if (arg > (ulong)(b.Length - csilPos)) { throw new CborException("array length exceeds remaining input"); }
                var n = (int)arg;
                var items = new System.Collections.Generic.List<CborValue>(n);
                for (int csilI = 0; csilI < n; csilI++) { items.Add(Dec(b, ref csilPos, csilDepth + 1)); }
                return new CborValue.Array(items);
            }
            case 5:
            {
                if (arg > (ulong)(b.Length - csilPos)) { throw new CborException("map length exceeds remaining input"); }
                var n = (int)arg;
                var kvs = new System.Collections.Generic.List<(CborValue, CborValue)>(n);
                for (int csilI = 0; csilI < n; csilI++)
                {
                    var k = Dec(b, ref csilPos, csilDepth + 1);
                    var val = Dec(b, ref csilPos, csilDepth + 1);
                    kvs.Add((k, val));
                }
                return new CborValue.Map(kvs);
            }
            case 6:
            {
                var inner = Dec(b, ref csilPos, csilDepth + 1);
                return new CborValue.Tag(arg, inner);
            }
            default:
                throw new CborException("malformed");
        }
    }

    public static CborValue? MapGet(CborValue v, string key)
    {
        if (v is CborValue.Map m)
        {
            foreach (var e in m.Entries)
            {
                if (e.Key is CborValue.Text t && t.Value == key) { return e.Value; }
            }
        }
        return null;
    }

    public static CborValue Require(CborValue v, string key) =>
        MapGet(v, key) ?? throw new CborException($"missing field '{key}'");

    public static T ExpectLiteral<T>(CborValue actual, CborValue expected, T value)
    {
        if (!ValueEquals(actual, expected))
            throw new CborException("literal mismatch");
        return value;
    }

    static bool ValueEquals(CborValue actual, CborValue expected) => (actual, expected) switch
    {
        (CborValue.Uint a, CborValue.Uint b) => a.Value == b.Value,
        (CborValue.Int a, CborValue.Int b) => a.Value == b.Value,
        (CborValue.Bool a, CborValue.Bool b) => a.Value == b.Value,
        (CborValue.Float a, CborValue.Float b) => a.Value == b.Value,
        (CborValue.Null, CborValue.Null) => true,
        (CborValue.Text a, CborValue.Text b) => a.Value == b.Value,
        (CborValue.Bytes a, CborValue.Bytes b) => a.Value.SequenceEqual(b.Value),
        (CborValue.Array a, CborValue.Array b) => a.Items.Count == b.Items.Count && a.Items.Zip(b.Items).All(p => ValueEquals(p.First, p.Second)),
        _ => false,
    };

    public static long AsI64(CborValue v) => v switch
    {
        CborValue.Uint x when x.Value <= long.MaxValue => (long)x.Value,
        CborValue.Int x => x.Value,
        _ => throw new CborException("expected integer"),
    };

    public static ulong AsU64(CborValue v) => v switch
    {
        CborValue.Uint x => x.Value,
        CborValue.Int x when x.Value >= 0 => (ulong)x.Value,
        _ => throw new CborException("expected unsigned integer"),
    };

    public static double AsDouble(CborValue v) => v switch
    {
        CborValue.Float x => x.Value,
        CborValue.Uint x => x.Value,
        CborValue.Int x => x.Value,
        _ => throw new CborException("expected float"),
    };

    public static bool AsBool(CborValue v) =>
        v is CborValue.Bool x ? x.Value : throw new CborException("expected bool");

    public static string AsText(CborValue v) =>
        v is CborValue.Text x ? x.Value : throw new CborException("expected text");

    public static byte[] AsBytes(CborValue v) =>
        v is CborValue.Bytes x ? x.Value : throw new CborException("expected bytes");

    public static System.Collections.Generic.IReadOnlyList<CborValue> AsArray(CborValue v) =>
        v is CborValue.Array x ? x.Items : throw new CborException("expected array");

    public static System.Collections.Generic.IReadOnlyList<(CborValue Key, CborValue Value)> AsMap(CborValue v) =>
        v is CborValue.Map x ? x.Entries : throw new CborException("expected map");

    public static string AsTaggedText(CborValue v, ulong num) =>
        v is CborValue.Tag x && x.Number == num ? AsText(x.Inner) : throw new CborException("expected tagged text");
}
"#;

/// `timestamp` support, emitted only when the spec uses it: CBOR tag 0 RFC3339 text,
/// always serialized in UTC with a `Z` offset per the wire contract.
const CODEC_TIMESTAMP_CSHARP: &str = r#"public static partial class Cbor
{
    public static CborValue EncTimestamp(System.DateTimeOffset value) =>
        new CborValue.Tag(0, new CborValue.Text(
            value.ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.FFFFFFF'Z'", System.Globalization.CultureInfo.InvariantCulture)));

    public static System.DateTimeOffset AsTimestamp(CborValue v) =>
        System.DateTimeOffset.Parse(
            AsTaggedText(v, 0),
            System.Globalization.CultureInfo.InvariantCulture,
            System.Globalization.DateTimeStyles.AdjustToUniversal | System.Globalization.DateTimeStyles.AssumeUniversal);
}
"#;

/// Exact-integer mantissa helpers shared by both decimal mappings: a CBOR integer when
/// the value fits 64 bits, otherwise a bignum byte string (tag 2 non-negative, tag 3
/// negative) so an arbitrarily large mantissa stays exact.
const CODEC_BIGINT_CSHARP: &str = r#"public static partial class Cbor
{
    public static CborValue EncBigInteger(System.Numerics.BigInteger m)
    {
        if (m >= 0 && m <= ulong.MaxValue) { return new CborValue.Uint((ulong)m); }
        if (m < 0 && m >= long.MinValue) { return new CborValue.Int((long)m); }
        var nonneg = m >= 0;
        var mag = nonneg ? m : -(m + 1);
        var be = mag.ToByteArray(isUnsigned: true, isBigEndian: true);
        return new CborValue.Tag(nonneg ? 2u : 3u, new CborValue.Bytes(be));
    }

    public static System.Numerics.BigInteger AsBigInteger(CborValue v) => v switch
    {
        CborValue.Uint x => new System.Numerics.BigInteger(x.Value),
        CborValue.Int x => new System.Numerics.BigInteger(x.Value),
        CborValue.Tag t when t.Number == 2 && t.Inner is CborValue.Bytes b =>
            new System.Numerics.BigInteger(b.Value, isUnsigned: true, isBigEndian: true),
        CborValue.Tag t when t.Number == 3 && t.Inner is CborValue.Bytes b =>
            -1 - new System.Numerics.BigInteger(b.Value, isUnsigned: true, isBigEndian: true),
        _ => throw new CborException("expected integer mantissa"),
    };
}
"#;

/// `decimal` support for the default (csil) mapping: the generated `CsilDecimal` holds
/// the exact value as (exponent, mantissa) and encodes CBOR tag 4 `[exponent, mantissa]`.
const CODEC_DECIMAL_CSIL_CSHARP: &str = r#"public static partial class Cbor
{
    public static CborValue EncDecimal(CsilDecimal value) =>
        new CborValue.Tag(4, new CborValue.Array(new CborValue[]
        {
            EncBigInteger(value.Exponent),
            EncBigInteger(value.Mantissa),
        }));

    public static CsilDecimal AsDecimal(CborValue v)
    {
        if (v is not CborValue.Tag tag || tag.Number != 4 || tag.Inner is not CborValue.Array arr || arr.Items.Count != 2)
        {
            throw new CborException("expected decimal");
        }
        var exp = (long)AsBigInteger(arr.Items[0]);
        var mant = AsBigInteger(arr.Items[1]);
        return new CsilDecimal(exp, mant);
    }
}
"#;

/// `decimal` support for the library mapping: the BCL `System.Decimal` is decomposed to
/// an exact (exponent, mantissa) and encoded CBOR tag 4, then reconstructed with exact
/// integer scaling (never a float) so no precision is lost on the round-trip.
const CODEC_DECIMAL_LIBRARY_CSHARP: &str = r#"public static partial class Cbor
{
    public static CborValue EncDecimal(decimal value)
    {
        var bits = decimal.GetBits(value);
        var scale = (bits[3] >> 16) & 0x7f;
        var negative = bits[3] < 0;
        var mant = (new System.Numerics.BigInteger((uint)bits[2]) << 64)
            | (new System.Numerics.BigInteger((uint)bits[1]) << 32)
            | new System.Numerics.BigInteger((uint)bits[0]);
        if (negative) { mant = -mant; }
        return new CborValue.Tag(4, new CborValue.Array(new CborValue[]
        {
            new CborValue.Int(-scale),
            EncBigInteger(mant),
        }));
    }

    public static decimal AsDecimal(CborValue v)
    {
        if (v is not CborValue.Tag tag || tag.Number != 4 || tag.Inner is not CborValue.Array arr || arr.Items.Count != 2)
        {
            throw new CborException("expected decimal");
        }
        var exp = (long)AsBigInteger(arr.Items[0]);
        var result = (decimal)AsBigInteger(arr.Items[1]);
        var pow = 1m;
        for (long csilI = 0; csilI < System.Math.Abs(exp); csilI++) { pow *= 10m; }
        return exp >= 0 ? result * pow : result / pow;
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_checks_declared_lengths_before_conversion_or_allocation() {
        for guard in [
            "if (arg > (ulong)(b.Length - csilPos)) { throw new CborException(\"truncated byte string\"); }",
            "if (arg > (ulong)(b.Length - csilPos)) { throw new CborException(\"truncated text string\"); }",
            "if (arg > (ulong)(b.Length - csilPos)) { throw new CborException(\"array length exceeds remaining input\"); }",
            "if (arg > (ulong)(b.Length - csilPos)) { throw new CborException(\"map length exceeds remaining input\"); }",
        ] {
            assert!(
                CODEC_RUNTIME_CSHARP.contains(guard),
                "missing guard: {guard}"
            );
        }
        assert!(CODEC_RUNTIME_CSHARP.contains("if (csilDepth > 64)"));
        assert!(CODEC_RUNTIME_CSHARP.contains("new System.Text.UTF8Encoding(false, true)"));
        assert!(CODEC_RUNTIME_CSHARP.contains("b.Length - csilPos - 1 < csilWidth"));
    }

    fn config() -> CsharpConfig {
        CsharpConfig {
            namespace: "Csilgen.Transport".to_string(),
            decimal_mapping: DecimalMapping::Csil,
        }
    }

    #[test]
    fn pascal_and_camel_casing() {
        assert_eq!(pascal_case("subject_id"), "SubjectId");
        assert_eq!(pascal_case("deposit-claim"), "DepositClaim");
        assert_eq!(camel_case("DepositRequest"), "depositRequest");
        assert_eq!(camel_case("Event"), "event");
    }

    #[test]
    fn keyword_escaping() {
        // camelCase is where lowercase C# keywords surface a collision.
        assert_eq!(camel_ident("Event"), "@event");
        assert_eq!(camel_ident("Int"), "@int");
        assert_eq!(escape_keyword("class"), "@class");
        assert_eq!(escape_keyword("object"), "@object");
        assert_eq!(escape_keyword("params"), "@params");
        // PascalCase rarely collides; `Event` is not itself a keyword.
        assert_eq!(pascal_ident("event"), "Event");
        assert_eq!(pascal_ident("subject_id"), "SubjectId");
    }

    #[test]
    fn type_mapping_core() {
        let c = config();
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("int".to_string()), &c),
            "long"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("text".to_string()), &c),
            "string"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("tstr".to_string()), &c),
            "string"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("bytes".to_string()), &c),
            "byte[]"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("timestamp".to_string()), &c),
            "System.DateTimeOffset"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("decimal".to_string()), &c),
            "CsilDecimal"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Reference("User".to_string()), &c),
            "User"
        );
    }

    #[test]
    fn any_maps_to_cbor_value() {
        // CDDL `any` is the open CBOR item; it is carried verbatim as the generated codec's
        // own `CborValue` value tree so it round-trips losslessly (a `global using` qualifies
        // the type to the configured namespace).
        let c = config();
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("any".to_string()), &c),
            "CborValue"
        );
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Builtin("any".to_string()), &c),
            "Csilgen.Transport.CborValue"
        );
    }

    #[test]
    fn alias_targets_are_namespace_qualified() {
        // A `global using` resolves its right side in the global namespace, so generated
        // types (records, the CsilDecimal helper) must carry the namespace prefix.
        let c = config();
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Builtin("decimal".to_string()), &c),
            "Csilgen.Transport.CsilDecimal"
        );
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Reference("User".to_string()), &c),
            "Csilgen.Transport.User"
        );
        // Predefined/BCL targets stay unqualified even when qualifying.
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Builtin("int".to_string()), &c),
            "long"
        );
    }

    #[test]
    fn service_naming() {
        assert_eq!(service_interface_name("FooService"), "IFooService");
        assert_eq!(service_interface_name("Attestation"), "IAttestationService");
        assert_eq!(service_base("CorndogsService"), "Corndogs");
        assert_eq!(service_base("Attestation"), "Attestation");
    }

    use csilgen_common::{
        CsilPosition, CsilRule, CsilServiceOperation, CsilSpecSerialized, GeneratorConfig,
    };

    fn bare_entry(name: &str, ty: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: None,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    /// A corndogs-shaped spec: a `Task` record (text/bytes/optional-int/map/list),
    /// `SubmitTaskRequest`, a `ServiceError`, and `CorndogsService` with one unary
    /// `submit-task: SubmitTaskRequest -> Task / ServiceError`.
    fn corndogs_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let group_rule = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let optional_int = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("priority".to_string())),
            value_type: CsilTypeExpression::Builtin("int".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let task = group_rule(
            "Task",
            vec![
                bare_entry("uuid", text()),
                bare_entry("current_state", text()),
                bare_entry("payload", CsilTypeExpression::Builtin("bytes".to_string())),
                optional_int,
                bare_entry(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(text()),
                        value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                        occurrence: None,
                    },
                ),
                bare_entry(
                    "tags",
                    CsilTypeExpression::Array {
                        element_type: Box::new(text()),
                        occurrence: None,
                    },
                ),
            ],
        );
        let type_rule = |name: &str, ty: CsilTypeExpression| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::TypeDef(ty),
            position: pos(),
            doc_comments: Vec::new(),
        };
        // A named map alias (transparent: a `global using` to `Dictionary<string, long>`)
        // and a map-of-record alias. A field referencing either must round-trip through
        // the underlying map codec, not the null stub a bare non-record reference yields.
        let string_int64_map = type_rule(
            "StringInt64Map",
            CsilTypeExpression::Map {
                key: Box::new(text()),
                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            },
        );
        let task_map = type_rule(
            "TaskMap",
            CsilTypeExpression::Map {
                key: Box::new(text()),
                value: Box::new(CsilTypeExpression::Reference("Task".to_string())),
                occurrence: None,
            },
        );
        let req = group_rule(
            "SubmitTaskRequest",
            vec![
                bare_entry("task", CsilTypeExpression::Reference("Task".to_string())),
                bare_entry("queue", text()),
                bare_entry(
                    "counts",
                    CsilTypeExpression::Reference("StringInt64Map".to_string()),
                ),
                bare_entry(
                    "tasks_by_id",
                    CsilTypeExpression::Reference("TaskMap".to_string()),
                ),
            ],
        );
        let err = group_rule(
            "ServiceError",
            vec![
                bare_entry("code", CsilTypeExpression::Builtin("int".to_string())),
                bare_entry("message", text()),
            ],
        );
        let svc = CsilRule {
            name: "CorndogsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "submit-task".to_string(),
                    input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                    output_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Reference("Task".to_string()),
                        CsilTypeExpression::Reference("ServiceError".to_string()),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![task, string_int64_map, task_map, req, err, svc],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    fn file_content<'a>(output: &'a WasmGeneratorOutput, path: &str) -> &'a str {
        output
            .files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.content.as_str())
            .unwrap_or_else(|| panic!("expected file {path}"))
    }

    /// A minimal ping/pong spec: `PingRequest`/`PongResponse` share a single `message`
    /// field (so an echo of the request bytes decodes cleanly as the response), a
    /// `ServiceError`, and `PingService.ping: PingRequest -> PongResponse / ServiceError`.
    /// Used to verify the README carrier hermetically.
    fn pingpong_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let group_rule = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let req = group_rule("PingRequest", vec![bare_entry("message", text())]);
        let resp = group_rule("PongResponse", vec![bare_entry("message", text())]);
        let err = group_rule(
            "ServiceError",
            vec![
                bare_entry("code", CsilTypeExpression::Builtin("int".to_string())),
                bare_entry("message", text()),
            ],
        );
        let svc = CsilRule {
            name: "PingService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "ping".to_string(),
                    input_type: CsilTypeExpression::Reference("PingRequest".to_string()),
                    output_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Reference("PongResponse".to_string()),
                        CsilTypeExpression::Reference("ServiceError".to_string()),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![req, resp, err, svc],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    /// A service with ONLY a `<->` (bidirectional/channel) operation — no `Unidirectional`
    /// op at all, so the RPC client would have no method to put on it. Regression fixture for
    /// the CS9113 ("parameter is unread") client-skip fix: `emit_client_class` used to emit
    /// `public sealed class FooClient(ICsilTransport transport)` unconditionally, with a body
    /// of nothing but a `// channel operation ... is not part of the RPC client` comment, so
    /// `transport` was a primary-constructor parameter no method ever read.
    fn channel_only_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let msg = CsilRule {
            name: "ChatMessage".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry("body", text())],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let svc = CsilRule {
            name: "ChatService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "chat".to_string(),
                    input_type: CsilTypeExpression::Reference("ChatMessage".to_string()),
                    output_type: CsilTypeExpression::Reference("ChatMessage".to_string()),
                    direction: CsilServiceDirection::Bidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![msg, svc],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    #[test]
    fn channel_only_service_emits_no_client_class() {
        let output = render(channel_only_input("csharp-client")).expect("generation ok");
        let client = file_content(&output, "Client.gen.cs");
        assert!(
            !client.contains("class ChatClient"),
            "a channel-only service must not emit a client class with an unread \
             `transport` primary-constructor parameter: {client}"
        );
        assert!(
            client.contains("no ChatClient is emitted"),
            "the skip must still leave an explanatory note: {client}"
        );
    }

    /// The real CS9113 proof: a channel-only service's client (still emitted as an empty
    /// file with just an explanatory comment) must build with zero warnings. Skips cleanly
    /// when no dotnet toolchain is on PATH.
    #[test]
    fn channel_only_service_client_builds_without_unread_parameter_warning() {
        if !have_dotnet() {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }
        let mut input = channel_only_input("csharp-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        input.config.options.insert(
            "package_name".to_string(),
            serde_json::json!("Csilgen.ChannelOnlyTest"),
        );
        let output = render(input).expect("generation ok");
        assert!(csproj(&output).is_some(), "package mode emits a .csproj");

        let dir =
            std::env::temp_dir().join(format!("csilgen-cs-channelonly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        let run = run_dotnet(&dir, "build");
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet build failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !stdout.contains("CS9113") && !stdout.contains("is unread"),
            "no unread-parameter warning should be possible once the useless client is \
             skipped:\nstdout:\n{stdout}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A record referencing two named all-literal choices — `Color = "red" / "green" /
    /// "blue"` and `Level = 1 / 2 / 3` — for the enum-decode membership audit: unlike
    /// dart (whose closed choice is a transparent `String`/`int` alias with no codec of
    /// its own) csharp lowers a bare-literal choice to a real `enum` with its own
    /// `<Type>FromCborValue`, generated by `emit_enum_codec`.
    fn enum_audit_input(target: &str) -> WasmGeneratorInput {
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let lit_text = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));
        let lit_int = |n: i64| CsilTypeExpression::Literal(CsilLiteralValue::Integer(n));
        let color = CsilRule {
            name: "Color".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                lit_text("red"),
                lit_text("green"),
                lit_text("blue"),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let level = CsilRule {
            name: "Level".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![lit_int(1), lit_int(2), lit_int(3)]),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let item = CsilRule {
            name: "Item".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    bare_entry("color", CsilTypeExpression::Reference("Color".to_string())),
                    bare_entry("level", CsilTypeExpression::Reference("Level".to_string())),
                ],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![color, level, item],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    /// Empirical enum-decode membership audit (csharp): decode an out-of-set string
    /// (`"purple"` against `Color`) and an out-of-set int (`99` against `Level`) and
    /// confirm both raise the codec's standard `CborException`, while a valid member
    /// still round-trips byte-identical — the same contract python/ocaml/php/ruby/elixir
    /// already enforce. `emit_enum_codec`'s generated `switch` already has a `_ => throw
    /// new CborException(...)` fallback arm for both the text- and int-based enum decode,
    /// so this is confirmation, not a fix. Skips cleanly when no dotnet toolchain is on PATH.
    #[test]
    fn enum_decode_rejects_out_of_set_value_through_dotnet() {
        if !have_dotnet() {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }
        let output = render(enum_audit_input("csharp")).expect("generation ok");

        let dir =
            std::env::temp_dir().join(format!("csilgen-cs-enum-decode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(dir.join("Program.cs"), CSHARP_ENUM_DECODE_DRIVER).unwrap();
        std::fs::write(dir.join("roundtrip.csproj"), CSHARP_CSPROJ).unwrap();

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg("run")
            .arg("--project")
            .arg(&dir)
            .current_dir(&dir)
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        let run = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const CSHARP_ENUM_DECODE_DRIVER: &str = r#"using Csilgen.Transport;
using System.Collections.Generic;

var valid = new CborValue.Map(new List<(CborValue, CborValue)>
{
    (new CborValue.Text("color"), new CborValue.Text("red")),
    (new CborValue.Text("level"), new CborValue.Int(2)),
});
var back = Codec.ItemFromCborValue(valid);
if (back.Color != Color.Red || back.Level != Level.Value2)
{
    throw new System.Exception("valid round-trip mismatch");
}

var purple = new CborValue.Map(new List<(CborValue, CborValue)>
{
    (new CborValue.Text("color"), new CborValue.Text("purple")),
    (new CborValue.Text("level"), new CborValue.Int(2)),
});
try
{
    Codec.ItemFromCborValue(purple);
    throw new System.Exception("out-of-set color was accepted");
}
catch (CborException e)
{
    if (!e.Message.Contains("invalid Color"))
    {
        throw new System.Exception($"wrong error for out-of-set color: {e.Message}");
    }
}

var ninetynine = new CborValue.Map(new List<(CborValue, CborValue)>
{
    (new CborValue.Text("color"), new CborValue.Text("red")),
    (new CborValue.Text("level"), new CborValue.Int(99)),
});
try
{
    Codec.ItemFromCborValue(ninetynine);
    throw new System.Exception("out-of-set level was accepted");
}
catch (CborException e)
{
    if (!e.Message.Contains("invalid Level"))
    {
        throw new System.Exception($"wrong error for out-of-set level: {e.Message}");
    }
}

System.Console.WriteLine("ok");
"#;

    /// Regression spec for the confirmed mixed-kind-literal-choice decode defect: `Status`
    /// mixes a text literal, an integer literal, and a second text literal in one choice
    /// (`"pending" / 1 / "shipped"`). `classify_choice` (`csilgen_common::choice`) says this
    /// is ALL literal — an `Enum` — regardless of the kind mix. Before the fix,
    /// `emit_enum_codec`'s `int_based` gate required EVERY arm to be an `Integer`, so a
    /// mixed vocabulary fell to the `Cbor.AsText(value) switch` branch — and its bare
    /// `1 => Value1` arm (an `int` pattern matched against a `switch` typed `string`) is
    /// CS8121, a hard C# compile failure, not just a wrong-answer bug.
    fn mixed_literal_choice_input(target: &str) -> WasmGeneratorInput {
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let lit_text = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));
        let lit_int = |n: i64| CsilTypeExpression::Literal(CsilLiteralValue::Integer(n));
        let status = CsilRule {
            name: "Status".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                lit_text("pending"),
                lit_int(1),
                lit_text("shipped"),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let item = CsilRule {
            name: "Item".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry(
                    "status",
                    CsilTypeExpression::Reference("Status".to_string()),
                )],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![status, item],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    /// Drives the mixed-kind `Status` codec: every declared member (of either literal kind)
    /// round-trips through its own kind-appropriate wire form, and an out-of-set value of
    /// EITHER kind is rejected — not just one, since a wrong fix could easily satisfy one
    /// kind's membership check while silently accepting anything of the other kind's type.
    const CSHARP_MIXED_ENUM_DRIVER: &str = r#"using Csilgen.Transport;

if (Codec.StatusFromCborValue(Codec.StatusToCborValue(Status.Pending)) != Status.Pending)
{
    throw new System.Exception("Pending round-trip mismatch");
}
if (Codec.StatusFromCborValue(Codec.StatusToCborValue(Status.Value1)) != Status.Value1)
{
    throw new System.Exception("Value1 round-trip mismatch");
}
if (Codec.StatusFromCborValue(Codec.StatusToCborValue(Status.Shipped)) != Status.Shipped)
{
    throw new System.Exception("Shipped round-trip mismatch");
}

// An out-of-set TEXT value is rejected even though the vocabulary also contains an int.
try
{
    Codec.StatusFromCborValue(new CborValue.Text("unknown"));
    throw new System.Exception("out-of-set text value was accepted");
}
catch (CborException e)
{
    if (!e.Message.Contains("invalid Status"))
    {
        throw new System.Exception($"wrong error for out-of-set text: {e.Message}");
    }
}

// An out-of-set INTEGER value is rejected even though the vocabulary also contains text.
try
{
    Codec.StatusFromCborValue(new CborValue.Uint(42));
    throw new System.Exception("out-of-set integer value was accepted");
}
catch (CborException e)
{
    if (!e.Message.Contains("invalid Status"))
    {
        throw new System.Exception($"wrong error for out-of-set integer: {e.Message}");
    }
}

System.Console.WriteLine("ok");
"#;

    /// Proves the `is_literal_choice` -> `classify_choice` migration's headline fix: a
    /// mixed-kind all-literal choice generates C# that actually COMPILES (the pre-fix
    /// output was CS8121, so this test would fail to build, not just fail an assertion) and
    /// round-trips/rejects correctly for both literal kinds. Skips cleanly when no dotnet
    /// toolchain is on PATH.
    #[test]
    fn mixed_kind_literal_choice_round_trips_and_rejects_out_of_set_through_dotnet() {
        if !have_dotnet() {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }
        let output = render(mixed_literal_choice_input("csharp")).expect("generation ok");

        let dir =
            std::env::temp_dir().join(format!("csilgen-cs-mixed-enum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(dir.join("Program.cs"), CSHARP_MIXED_ENUM_DRIVER).unwrap();
        std::fs::write(dir.join("roundtrip.csproj"), CSHARP_CSPROJ).unwrap();

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg("run")
            .arg("--project")
            .arg(&dir)
            .current_dir(&dir)
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        let run = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The 3-transport verification spec: an `Echo` service with a `->` op (`ping`) and a
    /// record-typed `<->` op (`pulse`), both over `Ping`/`Pong` records, generated under a
    /// distinct namespace so the codec types don't collide with the transport library.
    fn transports_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let group_rule = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let ping = group_rule("Ping", vec![bare_entry("msg", text())]);
        let pong = group_rule("Pong", vec![bare_entry("msg", text())]);
        let op = |name: &str, dir: CsilServiceDirection| CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference("Ping".to_string()),
            output_type: CsilTypeExpression::Reference("Pong".to_string()),
            direction: dir,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        };
        let svc = CsilRule {
            name: "Echo".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    op("ping", CsilServiceDirection::Unidirectional),
                    op("pulse", CsilServiceDirection::Bidirectional),
                ],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let mut options = HashMap::new();
        options.insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        options.insert("namespace".to_string(), serde_json::json!("EchoSdk"));
        options.insert("package_name".to_string(), serde_json::json!("EchoSdk"));
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![ping, pong, svc],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options,
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    /// The slice of `md` from `heading` up to the next `## ` heading (or end).
    fn section<'a>(md: &'a str, heading: &str) -> &'a str {
        let start = md.find(heading).expect("section heading present");
        let rest = &md[start..];
        match rest[heading.len()..].find("\n## ") {
            Some(off) => &rest[..heading.len() + off],
            None => rest,
        }
    }

    /// The `csharp` block under `heading` (the section's first fenced block).
    fn section_block(md: &str, heading: &str) -> String {
        let sec = section(md, heading);
        let start =
            sec.find("```csharp\n").expect("section has a csharp block") + "```csharp\n".len();
        let rest = &sec[start..];
        let end = rest.find("\n```").expect("csharp block is closed");
        rest[..end].to_string()
    }

    #[test]
    fn package_readme_has_all_three_sections() {
        let output = render(transports_input("csharp-client")).expect("generation ok");
        let readme = file_content(&output, "genquickstart.md");

        assert!(readme.starts_with("# EchoSdk\n"));
        assert!(readme.contains("dotnet add reference"));
        for heading in [
            "## CSIL-RPC (HTTP)",
            "## CSIL-Events (TLS)",
            "## CSIL-Datagrams (UDP)",
        ] {
            assert!(readme.contains(heading), "README must contain {heading}");
        }

        let rpc = section(readme, "## CSIL-RPC (HTTP)");
        // RPC: the library envelope types + the canonical HTTP mount, no hand-rolled map.
        assert!(rpc.contains("using Csilgen.Transport;"));
        assert!(rpc.contains("public sealed class HttpRpcCarrier : ICsilTransport"));
        assert!(rpc.contains("\"/csil/v1/rpc\""));
        assert!(rpc.contains("new RpcRequest(service, op, request).Encode()"));
        assert!(rpc.contains("RpcResponse.Decode(body)"));
        assert!(rpc.contains("resp.AsTransportError() is StatusException"));
        assert!(rpc.contains("resp.Variant == \"ServiceError\""));
        assert!(rpc.contains("new EchoClient(new HttpRpcCarrier("));
        assert!(rpc.contains("client.Ping(new Ping { Msg = \"example\" })"));

        let events = section(readme, "## CSIL-Events (TLS)");
        // Events: the lib's handshake/framing/heartbeat surface + the generated router.
        // The carrier is built with an explicit max-frame limit so an operator can see and
        // change the guard without editing generated source (conventions doc §5).
        assert!(events.contains("new StreamCarrier(tls, MaxFrame)"));
        assert!(events.contains("const int MaxFrame = Conventions.MaxFrameDefault;"));
        assert!(events.contains("new Hello("));
        // The handshake and the outbound send name the service by its verbatim CSIL name.
        assert!(events.contains("{ Service = \"Echo\" }.Encode()"));
        assert!(events.contains("Event.Verbose(\"Echo\", method, body).Encode(profile)"));
        assert!(events.contains("HelloAck.Decode(ack)"));
        assert!(events.contains("ChannelHandlers : IEchoService"));
        assert!(events.contains("EchoRouter.EncodePulse(codec, new Pong { Msg = \"example\" })"));
        assert!(
            events.contains("EchoRouter.RouteChannel(handlers, codec, ev.EventName!, ev.Payload)")
        );
        assert!(events.contains("Control.PingName"));

        let datagrams = section(readme, "## CSIL-Datagrams (UDP)");
        // Datagrams: the lib's Datagram + UDP carrier seam, and the no-sync-response warning.
        assert!(datagrams.contains("new UdpDatagramCarrier(sock)"));
        assert!(datagrams.contains("new Datagram(OpOrd, 0, Codec.Encode(req)).Encode()"));
        assert!(datagrams.contains("Codec.Decode<Pong>(dg.Payload)"));
        assert!(datagrams.contains("NO synchronous response"));
    }

    #[test]
    fn genquickstart_transports_subset_emits_only_listed_sections() {
        let mut input = transports_input("csharp-client");
        input.config.options.insert(
            "genquickstart_transports".to_string(),
            serde_json::json!(["rpc"]),
        );
        let output = render(input).expect("generation ok");
        let readme = file_content(&output, "genquickstart.md");
        assert!(readme.contains("## CSIL-RPC (HTTP)"));
        assert!(!readme.contains("## CSIL-Events (TLS)"));
        assert!(!readme.contains("## CSIL-Datagrams (UDP)"));
    }

    #[test]
    fn genquickstart_transports_unknown_or_empty_falls_back_to_all() {
        for opt in [serde_json::json!([]), serde_json::json!(["bogus"])] {
            let mut input = transports_input("csharp-client");
            input
                .config
                .options
                .insert("genquickstart_transports".to_string(), opt.clone());
            let output = render(input).expect("generation ok");
            let readme = file_content(&output, "genquickstart.md");
            assert!(
                readme.contains("## CSIL-RPC (HTTP)")
                    && readme.contains("## CSIL-Events (TLS)")
                    && readme.contains("## CSIL-Datagrams (UDP)"),
                "{opt} must fall back to all three sections"
            );
        }
    }

    #[test]
    fn events_section_without_channel_ops_emits_a_note() {
        // pingpong has only a `->` op, so the Events section keeps the handshake but replaces
        // the dispatch wiring with a note (no generated router reference).
        let mut input = pingpong_input("csharp-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        let output = render(input).expect("generation ok");
        let readme = file_content(&output, "genquickstart.md");
        let events = section(readme, "## CSIL-Events (TLS)");
        assert!(events.contains("new Hello("));
        assert!(events.contains("no <->/<- operations"));
        assert!(!events.contains("RouteChannel"));
    }

    #[test]
    fn readme_emitted_only_in_package_mode() {
        let plain = render(corndogs_input("csharp-client")).expect("generation ok");
        assert!(!plain.files.iter().any(|f| f.path == "genquickstart.md"));
        let packaged = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!(["csharp"]))],
        ))
        .expect("generation ok");
        assert!(packaged.files.iter().any(|f| f.path == "genquickstart.md"));
    }

    #[test]
    fn emit_readme_false_suppresses_only_the_readme() {
        // Default package mode: the README is present.
        let with_readme = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!(["csharp"]))],
        ))
        .expect("generation ok");
        assert!(
            with_readme
                .files
                .iter()
                .any(|f| f.path == "genquickstart.md")
        );

        // Only an explicit `emit_readme: false` drops it; the `.csproj` stays.
        let without_readme = render(input_with_options(
            "csharp-client",
            &[
                ("emit_packages", serde_json::json!(["csharp"])),
                ("emit_readme", serde_json::json!(false)),
            ],
        ))
        .expect("generation ok");
        assert!(
            !without_readme
                .files
                .iter()
                .any(|f| f.path == "genquickstart.md")
        );
        assert!(
            without_readme
                .files
                .iter()
                .any(|f| f.path.ends_with(".csproj"))
        );
    }

    #[test]
    fn server_package_readme_has_all_three_sections() {
        // The README is surface-independent: a server package still documents all three
        // transports (the RPC client + Events router live on their respective surfaces, but
        // the genquickstart is one doc). The Events section names the generated server router.
        let output = render(transports_input("csharp-server")).expect("generation ok");
        let readme = file_content(&output, "genquickstart.md");
        assert!(readme.contains("## CSIL-RPC (HTTP)"));
        assert!(readme.contains("## CSIL-Events (TLS)"));
        assert!(readme.contains("## CSIL-Datagrams (UDP)"));
        assert!(readme.contains("EchoRouter.RouteChannel"));
    }

    #[test]
    fn client_uses_dumb_byte_seam() {
        let output = render(corndogs_input("csharp-client")).expect("generation ok");
        let prelude_or_client = output
            .files
            .iter()
            .map(|f| f.content.as_str())
            .collect::<String>();
        // The transport seam is now bytes-in/bytes-out — no generic reflection payload.
        assert!(
            prelude_or_client.contains("byte[] Call(string service, string op, byte[] request);")
        );
        assert!(!prelude_or_client.contains("Call<TRequest, TResponse>"));

        let client = file_content(&output, "Client.gen.cs");
        // Wire strings are the verbatim CSIL service and op names; round-trip through
        // the generated codec rather than a host-supplied serializer.
        assert!(client.contains(
            "public Task SubmitTask(SubmitTaskRequest submitTaskRequest) =>\n        Codec.Decode<Task>(transport.Call(\"CorndogsService\", \"submit-task\", Codec.Encode(submitTaskRequest)));"
        ));
    }

    #[test]
    fn channel_router_and_encoder_use_verbatim_wire_op() {
        let output = render(transports_input("csharp")).expect("generation ok");
        let services = file_content(&output, "Services.gen.cs");
        // The verbose router keys on the verbatim (kebab-case-as-written) CSIL op name;
        // the C# method identifier stays PascalCased.
        assert!(services.contains("case \"pulse\":"));
        assert!(services.contains("handlers.Pulse(message);"));
        // The outbound encoder keeps its PascalCased name but frames the verbatim op.
        assert!(services.contains("EncodePulse(ICsilCodec codec"));
        assert!(services.contains("return (\"pulse\", data);"));
        assert!(!services.contains("case \"Pulse\":"));
        assert!(!services.contains("return (\"Pulse\", data);"));
    }

    /// A spec whose ops exercise the boundary shapes the old record-only filter dropped:
    /// `MemberID` (scalar alias) requests, a `[*Member]` (bare-array) response, a `bool`
    /// (scalar) response, and a `{text => text}` (map) response — alongside the one
    /// record↔record op the filter kept.
    fn nonrecord_ops_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let r#ref = |n: &str| CsilTypeExpression::Reference(n.to_string());
        let alias = |name: &str, ty: CsilTypeExpression| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::TypeDef(ty),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let group_rule = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let member = group_rule(
            "Member",
            vec![
                bare_entry("id", r#ref("MemberID")),
                bare_entry("name", text()),
            ],
        );
        let limit = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("limit".to_string())),
            value_type: CsilTypeExpression::Builtin("uint".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let list_req = group_rule("ListMembersRequest", vec![limit]);
        let op = |name: &str, input: CsilTypeExpression, output: CsilTypeExpression| {
            CsilServiceOperation {
                name: name.to_string(),
                input_type: input,
                output_type: output,
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: Vec::new(),
                wire_id: None,
            }
        };
        let arr = |elem: CsilTypeExpression| CsilTypeExpression::Array {
            element_type: Box::new(elem),
            occurrence: None,
        };
        let map = |k: CsilTypeExpression, v: CsilTypeExpression| CsilTypeExpression::Map {
            key: Box::new(k),
            value: Box::new(v),
            occurrence: None,
        };
        let svc = CsilRule {
            name: "MemberService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    op("create-member", r#ref("Member"), r#ref("Member")),
                    op("get-member", r#ref("MemberID"), r#ref("Member")),
                    op(
                        "list-members",
                        r#ref("ListMembersRequest"),
                        arr(r#ref("Member")),
                    ),
                    op(
                        "delete-task",
                        r#ref("TaskID"),
                        CsilTypeExpression::Builtin("bool".to_string()),
                    ),
                    op(
                        "member-names",
                        r#ref("ListMembersRequest"),
                        map(text(), text()),
                    ),
                ],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![
                    alias("MemberID", text()),
                    alias("TaskID", text()),
                    member,
                    list_req,
                    svc,
                ],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    #[test]
    fn non_record_op_boundaries_get_client_methods_and_per_op_codecs() {
        let output = render(nonrecord_ops_input("csharp-client")).expect("generation ok");
        let client = file_content(&output, "Client.gen.cs");

        // Every op gets a method now — scalar-id request, bare-array, scalar, and map
        // responses included; none is dropped with a note.
        assert!(client.contains("public Member GetMember(MemberID memberID) =>"));
        assert!(client.contains(
            "public System.Collections.Generic.List<Member> ListMembers(ListMembersRequest"
        ));
        assert!(client.contains("public bool DeleteTask(TaskID taskID) =>"));
        assert!(client.contains(
            "public System.Collections.Generic.Dictionary<string, string> MemberNames(ListMembersRequest"
        ));
        assert!(!client.contains("handle it manually"));
        assert!(!client.contains("non-record payload"));

        // The record boundary keeps the generic codec; non-record boundaries ride per-op
        // helpers, so the client and a consumer share one byte seam for every op.
        assert!(client.contains(
            "public Member CreateMember(Member member) =>\n        Codec.Decode<Member>(transport.Call(\"MemberService\", \"create-member\", Codec.Encode(member)));"
        ));
        assert!(client.contains("Codec.EncodeMemberGetMemberRequest(memberID)"));
        assert!(client.contains("Codec.DecodeMemberListMembersResponse(transport.Call("));
        assert!(client.contains("Codec.DecodeMemberDeleteTaskResponse(transport.Call("));

        // The per-op helpers are public static on the generated Codec for cross-assembly use.
        let codec = file_content(&output, "Codec.gen.cs");
        assert!(codec
            .contains("public static MemberID DecodeMemberGetMemberRequest(byte[] data) => Cbor.AsText(Cbor.Decode(data));"));
        assert!(codec.contains(
            "public static System.Collections.Generic.List<Member> DecodeMemberListMembersResponse(byte[] data) =>"
        ));
        assert!(codec
            .contains("public static byte[] EncodeMemberDeleteTaskResponse(bool value) => Cbor.Encode(new CborValue.Bool(value));"));
    }

    #[test]
    fn async_twin_emitted_by_default_with_marked_symbols() {
        // Default `client_style` is `both`: the async twin lives at `ClientAsync.gen.cs`
        // and carries an `Async` marker on its transport, class, and method names so it
        // coexists with the sync client in one namespace.
        let output = render(corndogs_input("csharp-client")).expect("generation ok");
        let twin = file_content(&output, "ClientAsync.gen.cs");

        // The transport seam returns a Task; its interface name is marked.
        assert!(twin.contains("public interface ICsilAsyncTransport"));
        assert!(twin.contains(
            "System.Threading.Tasks.Task<byte[]> Call(string service, string op, byte[] request);"
        ));
        // Marked class + Async-suffixed, Task-returning method awaiting the byte seam.
        assert!(
            twin.contains("public sealed class CorndogsAsyncClient(ICsilAsyncTransport transport)")
        );
        assert!(twin.contains(
            "public async System.Threading.Tasks.Task<Task> SubmitTaskAsync(SubmitTaskRequest submitTaskRequest) =>\n        Codec.Decode<Task>(await transport.Call(\"CorndogsService\", \"submit-task\", Codec.Encode(submitTaskRequest)));"
        ));
        // The shared exception is NOT redefined in the twin (the sync file owns it).
        assert!(!twin.contains("class CsilClientException"));

        // The sync client is untouched: blocking, canonical names, no Task/await.
        let sync = file_content(&output, "Client.gen.cs");
        assert!(sync.contains("public sealed class CorndogsClient(ICsilTransport transport)"));
        assert!(sync.contains("public Task SubmitTask(SubmitTaskRequest submitTaskRequest) =>"));
        assert!(!sync.contains("await "));
        assert!(!sync.contains("System.Threading.Tasks.Task<"));
        assert!(sync.contains("class CsilClientException"));
    }

    #[test]
    fn client_style_async_is_drop_in_at_canonical_path() {
        // `client_style: async` yields a single async client at the canonical filename
        // with the canonical symbol names — a drop-in for a sync consumer.
        let output = render(input_with_options(
            "csharp-client",
            &[("client_style", serde_json::json!("async"))],
        ))
        .expect("generation ok");
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            !paths.contains(&"ClientAsync.gen.cs"),
            "async drop-in emits no separate twin"
        );
        assert!(paths.contains(&"Client.gen.cs"));

        let client = file_content(&output, "Client.gen.cs");
        // Canonical (unmarked) names, but async + Task-returning.
        assert!(client.contains("public interface ICsilTransport"));
        assert!(client.contains(
            "System.Threading.Tasks.Task<byte[]> Call(string service, string op, byte[] request);"
        ));
        assert!(client.contains("public sealed class CorndogsClient(ICsilTransport transport)"));
        assert!(client.contains(
            "public async System.Threading.Tasks.Task<Task> SubmitTask(SubmitTaskRequest submitTaskRequest) =>\n        Codec.Decode<Task>(await transport.Call(\"CorndogsService\", \"submit-task\", Codec.Encode(submitTaskRequest)));"
        ));
        // A standalone drop-in still defines the shared exception.
        assert!(client.contains("class CsilClientException"));
    }

    #[test]
    fn client_style_sync_suppresses_the_twin() {
        let output = render(input_with_options(
            "csharp-client",
            &[("client_style", serde_json::json!("sync"))],
        ))
        .expect("generation ok");
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();
        assert!(!paths.contains(&"ClientAsync.gen.cs"));
        let client = file_content(&output, "Client.gen.cs");
        assert!(!client.contains("await "));
        assert!(!client.contains("System.Threading.Tasks.Task<"));
    }

    #[test]
    fn client_style_invalid_value_is_rejected() {
        // An unknown value fails the whole run (mirrors decimal_mapping's hard error).
        let result = render(input_with_options(
            "csharp-client",
            &[("client_style", serde_json::json!("blocking"))],
        ));
        assert!(result.is_err(), "invalid client_style must fail generation");
    }

    #[test]
    fn codec_emitted_for_records_keyed_by_csil_field_name() {
        let output = render(corndogs_input("csharp-client")).expect("generation ok");
        let codec = file_content(&output, "Codec.gen.cs");
        // The value model + the typed byte surface are both present.
        assert!(codec.contains("public abstract record CborValue"));
        assert!(codec.contains("public static byte[] Encode<T>(T value)"));
        assert!(codec.contains("public static T Decode<T>(byte[] data)"));
        // Map keys are the verbatim CSIL field names, never a case-folded variant.
        assert!(codec.contains("new CborValue.Text(\"current_state\")"));
        assert!(!codec.contains("new CborValue.Text(\"currentState\")"));
        // The per-record codec pair exists for each record.
        assert!(codec.contains("public static CborValue TaskToCborValue(Task value)"));
        assert!(codec.contains("public static Task TaskFromCborValue(CborValue value)"));
        // An absent optional is omitted from the map, present-with-null on decode. The
        // non-null binding is uniquely suffixed (a `is { }` pattern variable leaks into the
        // method scope, so a shared name would collide across optionals).
        assert!(codec.contains("if (value.Priority is { } csilV"));
    }

    /// Every optional field's `is { }` binding must be uniquely named. A C# `is { }`
    /// pattern variable leaks into the enclosing method scope, so reusing one name across a
    /// record's optionals both collides (CS0128) and forces each optional through the first
    /// field's type (CS1503 — e.g. a `bytes` optional fed a `string` binding). This guards
    /// the per-field-unique binding that keeps the generated codec compiling.
    #[test]
    fn record_codec_binds_each_optional_uniquely() {
        let opt = |name: &str, ty: CsilTypeExpression| CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let text = || CsilTypeExpression::Builtin("text".to_string());
        // Differing types prove the binding is typed per-field: the `bytes` optional must
        // bind a `byte[]`, not the `string` an earlier optional would have pinned.
        let group = CsilGroupExpression {
            entries: vec![
                bare_entry("id", text()),
                opt("note", text()),
                opt("blob", CsilTypeExpression::Builtin("bytes".to_string())),
                opt("count", CsilTypeExpression::Builtin("uint".to_string())),
            ],
        };
        let records = std::collections::HashSet::new();
        let aliases = std::collections::HashMap::new();
        let choices = std::collections::HashSet::new();
        let codec = emit_record_codec("Thing", &group, &records, &aliases, &choices);

        // Each optional binds a distinct name, and the bytes optional routes its own binding
        // through CborValue.Bytes (a string binding here is the CS1503 we are guarding).
        let binds: Vec<&str> = codec
            .lines()
            .filter_map(|l| l.trim().strip_prefix("if (value."))
            .filter_map(|l| l.split("is { } ").nth(1))
            .map(|l| l.trim_end_matches(')'))
            .collect();
        assert_eq!(binds.len(), 3, "three optionals must each emit a guard");
        let unique: std::collections::HashSet<&&str> = binds.iter().collect();
        assert_eq!(
            unique.len(),
            binds.len(),
            "bindings must be unique: {binds:?}"
        );
        // Canonical key order is id(0), blob(1), note(2), count(3); the bytes optional `blob`
        // lands at index 1 and must route its own binding through CborValue.Bytes.
        assert!(
            codec.contains("new CborValue.Bytes(csilV1)"),
            "the bytes optional must encode its own typed binding"
        );
    }

    /// A spec whose members PascalCase to their enclosing type's name: record
    /// `Relation` with field `relation` (constrained, so `Validate()` also references
    /// it) and enum `Status` with wire value `"status"`. C# rejects a member spelled
    /// like its containing type (CS0542), so both must escape.
    fn self_named_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let relation_field = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("relation".to_string())),
            value_type: text(),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinLength(1),
            )],
            doc_comments: Vec::new(),
        };
        let relation = CsilRule {
            name: "Relation".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    relation_field,
                    bare_entry(
                        "status",
                        CsilTypeExpression::Reference("Status".to_string()),
                    ),
                ],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let status = CsilRule {
            name: "Status".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Literal(CsilLiteralValue::Text("status".to_string())),
                CsilTypeExpression::Literal(CsilLiteralValue::Text("inactive".to_string())),
            ])),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let err = CsilRule {
            name: "ServiceError".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    bare_entry("code", CsilTypeExpression::Builtin("int".to_string())),
                    bare_entry("message", text()),
                ],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let svc = CsilRule {
            name: "RelationService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "get-relation".to_string(),
                    input_type: CsilTypeExpression::Reference("Relation".to_string()),
                    output_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Reference("Relation".to_string()),
                        CsilTypeExpression::Reference("ServiceError".to_string()),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![relation, status, err, svc],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 1,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    /// An optional `bytes` field carries three distinct states — absent,
    /// present-and-empty, present-and-non-empty — and the codec must decide presence by
    /// whether the value is set, never by whether it is non-empty (cbor-wire-contract.md
    /// "Optional fields"). A `.Length > 0` guard would collapse present-empty into absent
    /// and silently lose a caller's "replace this with nothing".
    #[test]
    fn optional_bytes_encodes_on_presence_not_emptiness() {
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let payload = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("payload".to_string())),
            value_type: CsilTypeExpression::Builtin("bytes".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let rule = CsilRule {
            name: "UpdateRequest".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    bare_entry("id", CsilTypeExpression::Builtin("text".to_string())),
                    payload,
                ],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let mut input = self_named_input("csharp-client");
        input.csil_spec = CsilSpecSerialized {
            rules: vec![rule],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let output = render(input).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");
        let codec = file_content(&output, "Codec.gen.cs");

        // A nullable byte[] distinguishes null (absent) from an empty array
        // (present-and-empty).
        assert!(
            types.contains("public byte[]? Payload { get; init; }"),
            "optional bytes needs a presence-carrying type:\n{types}"
        );
        // Encode gates on the value being set (`is { }`), not on its length.
        assert!(
            codec.contains("if (value.Payload is { } csilV1)"),
            "encode must gate on presence, not emptiness:\n{codec}"
        );
        assert!(
            !codec.contains("value.Payload.Length > 0"),
            "encode must not gate on emptiness:\n{codec}"
        );
        // Decode maps a missing key to null but keeps a present zero-length byte string,
        // so the three states stay distinct.
        assert!(
            codec.contains("Cbor.MapGet(value, \"payload\") is { } csilRaw1"),
            "decode must gate on key presence:\n{codec}"
        );
    }

    /// A member spelled like its enclosing type is CS0542 in C#: `Relation.relation`
    /// must surface as the `Relation_` property (a spelling `pascal_case` can never
    /// produce for another field, since it strips underscores) at every reference
    /// site, while the CBOR wire key stays the verbatim CSIL name.
    #[test]
    fn self_named_members_escape_ident_but_not_wire_key() {
        let output = render(self_named_input("csharp-client")).expect("generation ok");

        let types = file_content(&output, "Types.gen.cs");
        assert!(types.contains("public sealed record Relation\n"));
        assert!(types.contains("public required string Relation_ { get; init; }"));
        assert!(
            !types.contains("string Relation {"),
            "property must not shadow its enclosing record"
        );
        assert!(types.contains("// CBOR key: relation"));
        // Validate() must reach the field through the escaped property.
        assert!(types.contains("Relation_.Length < 1"));
        // The enum member escapes the same way, keeping the wire literal verbatim.
        assert!(types.contains("public enum Status"));
        assert!(types.contains("// wire value: status\n    Status_,"));

        let codec = file_content(&output, "Codec.gen.cs");
        assert!(codec.contains("new CborValue.Text(\"relation\")"));
        assert!(codec.contains("value.Relation_"));
        assert!(codec.contains("Relation_ = csilField"));
        assert!(codec.contains("Status.Status_ => new CborValue.Text(\"status\")"));
        assert!(codec.contains("\"status\" => Status.Status_,"));
    }

    /// The real CS0542 proof: a package generated from the self-named spec must
    /// compile under `dotnet build`. Skips cleanly when no dotnet toolchain is on PATH.
    #[test]
    fn self_named_package_builds_with_dotnet() {
        if !have_dotnet() {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        let mut input = self_named_input("csharp-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        input.config.options.insert(
            "package_name".to_string(),
            serde_json::json!("Csilgen.SelfNamedTest"),
        );
        let output = render(input).expect("generation ok");
        assert!(csproj(&output).is_some(), "package mode emits a .csproj");

        let dir =
            std::env::temp_dir().join(format!("csilgen-csharp-selfnamed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        let run = run_dotnet(&dir, "build");
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet build failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A record whose `Validate()` must emit more than one null-narrowing guard: `label`
    /// carries two checks on the *same* optional field (min + max length), and `tags` /
    /// `score` are two *different* optional fields each with their own check(s). Before the
    /// fix, every guard bound to the literal pattern-variable name `value`, so the second
    /// `if (X is { } value)` in the method — same field or not — was CS0128 (and reading
    /// `value` afterward, typed from whichever declaration the compiler kept, produced
    /// downstream CS0165/CS0019/CS1061 on cascading specs). Modeled directly on the
    /// generated shape that failed in `examples/breaking-changes/api-v2.csil` et al.
    fn constrained_optionals_input(target: &str) -> WasmGeneratorInput {
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let optional_entry = |name: &str, ty: CsilTypeExpression, metadata| CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: Some(CsilOccurrence::Optional),
            metadata,
            doc_comments: Vec::new(),
        };
        let widget = CsilRule {
            name: "Widget".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    optional_entry(
                        "label",
                        CsilTypeExpression::Builtin("text".to_string()),
                        vec![
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(1)),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(50)),
                        ],
                    ),
                    optional_entry(
                        "tags",
                        CsilTypeExpression::Array {
                            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            occurrence: None,
                        },
                        vec![CsilFieldMetadata::Constraint(
                            CsilValidationConstraint::MaxItems(20),
                        )],
                    ),
                    optional_entry(
                        "score",
                        CsilTypeExpression::Builtin("int".to_string()),
                        vec![
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinValue(
                                CsilLiteralValue::Integer(0),
                            )),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxValue(
                                CsilLiteralValue::Integer(100),
                            )),
                        ],
                    ),
                ],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![widget],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 3,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    /// Pins the fixed `Validate()` shape: one null-narrowing guard per optional field
    /// (never one per check), each guard's pattern-variable name unique across the
    /// method, and both of `label`'s checks nested inside its single guard.
    #[test]
    fn multi_constraint_optionals_emit_one_guard_per_field() {
        let output = render(constrained_optionals_input("csharp")).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");

        let guards: Vec<&str> = types
            .lines()
            .filter_map(|line| line.trim().strip_prefix("if ("))
            .filter_map(|line| line.split(" is { } ").nth(1))
            .filter_map(|line| line.strip_suffix(')'))
            .collect();
        assert_eq!(
            guards.len(),
            3,
            "one guard per optional field (label, tags, score), not one per check: {guards:?}"
        );
        let unique: std::collections::HashSet<&&str> = guards.iter().collect();
        assert_eq!(
            unique.len(),
            guards.len(),
            "every guard's pattern-variable name must be unique in Validate(): {guards:?}"
        );

        // `label` has two checks (min + max length); both must live inside its one guard.
        let label_guard_open = format!("if (Label is {{ }} {})", guards[0]);
        assert_eq!(
            types.matches(&label_guard_open).count(),
            1,
            "label's guard must open exactly once even though it has two checks"
        );
        assert!(types.contains(&format!("{}.Length < 1", guards[0])));
        assert!(types.contains(&format!("{}.Length > 50", guards[0])));
    }

    /// The real CS0128/CS0165/CS0019/CS1061 proof: a package with several
    /// multiply-constrained optional fields must compile under `dotnet build`. Skips
    /// cleanly when no dotnet toolchain is on PATH.
    #[test]
    fn multi_constraint_optionals_package_builds_with_dotnet() {
        if !have_dotnet() {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        let mut input = constrained_optionals_input("csharp-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        input.config.options.insert(
            "package_name".to_string(),
            serde_json::json!("Csilgen.ConstrainedOptionalsTest"),
        );
        let output = render(input).expect("generation ok");
        assert!(csproj(&output).is_some(), "package mode emits a .csproj");

        let dir = std::env::temp_dir().join(format!(
            "csilgen-csharp-constrained-optionals-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        let run = run_dotnet(&dir, "build");
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet build failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spec exercising every construct the interop wire locks: a text enum, an int enum,
    /// a `uint / text` tagged union, a tuple with an optional element, and an `any` map. The
    /// generated types must be real (not `global using = object`) and the codec must route
    /// each through its own helper with the locked wire shape.
    fn choices_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let lit_text = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));
        let lit_int = |n: i64| CsilTypeExpression::Literal(CsilLiteralValue::Integer(n));
        let type_rule = |name: &str, ty: CsilTypeExpression| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::TypeDef(ty),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let positional = |ty: CsilTypeExpression, optional: bool| CsilGroupEntry {
            key: None,
            value_type: ty,
            occurrence: optional.then_some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let color = type_rule(
            "Color",
            CsilTypeExpression::Choice(vec![lit_text("red"), lit_text("green"), lit_text("blue")]),
        );
        let priority = type_rule(
            "Priority",
            CsilTypeExpression::Choice(vec![lit_int(1), lit_int(2), lit_int(3)]),
        );
        let id_or_name = type_rule(
            "IdOrName",
            CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Builtin("uint".to_string()),
                text(),
            ]),
        );
        let bag = CsilRule {
            name: "Bag".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    bare_entry("color", CsilTypeExpression::Reference("Color".to_string())),
                    bare_entry(
                        "prio",
                        CsilTypeExpression::Reference("Priority".to_string()),
                    ),
                    bare_entry("who", CsilTypeExpression::Reference("IdOrName".to_string())),
                    bare_entry(
                        "pair",
                        CsilTypeExpression::Tuple(CsilGroupExpression {
                            entries: vec![
                                positional(text(), false),
                                positional(CsilTypeExpression::Builtin("int".to_string()), true),
                            ],
                        }),
                    ),
                    bare_entry(
                        "extra",
                        CsilTypeExpression::Map {
                            key: Box::new(text()),
                            value: Box::new(CsilTypeExpression::Builtin("any".to_string())),
                            occurrence: None,
                        },
                    ),
                ],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![color, priority, id_or_name, bag],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    #[test]
    fn namespace_derives_from_package_name() {
        assert_eq!(csharp_namespace_from_package("interop_api"), "interop_api");
        assert_eq!(csharp_namespace_from_package("Acme.Tasks"), "Acme.Tasks");
        assert_eq!(
            csharp_namespace_from_package("github.com/org/repo/clients/corndogs"),
            "corndogs"
        );
        // A leading digit and stray punctuation are sanitized to a valid identifier.
        assert_eq!(csharp_namespace_from_package("9lives-cat"), "_9lives_cat");

        // A configured package_name (no explicit namespace) becomes the in-code namespace,
        // keeping generated types out of the transport library's `Csilgen.Transport`.
        let output = render(input_with_options(
            "csharp-client",
            &[
                ("emit_packages", serde_json::json!(["csharp"])),
                ("package_name", serde_json::json!("interop_api")),
            ],
        ))
        .expect("generation ok");
        assert!(file_content(&output, "Codec.gen.cs").contains("namespace interop_api;"));
        assert!(file_content(&output, "Types.gen.cs").contains("namespace interop_api;"));
    }

    #[test]
    fn named_choices_emit_real_types_and_codecs() {
        let output = render(choices_input("csharp-client")).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");
        let codec = file_content(&output, "Codec.gen.cs");

        // The enums and union are real types, never stubbed to `object`.
        assert!(types.contains("public enum Color"));
        assert!(types.contains("public enum Priority"));
        assert!(types.contains("public abstract record IdOrName"));
        assert!(types.contains("public sealed record IdOrNameVariant1(ulong Value) : IdOrName"));
        assert!(!types.contains("global using Color = object"));
        // The tuple's optional element is nullable; `any` is the codec's own CborValue.
        assert!(types.contains("(string Field0, long? Field1) Pair"));
        assert!(types.contains("Dictionary<string, CborValue> Extra"));

        // The text enum encodes its bare wire literal; the int enum its bare integer.
        assert!(codec.contains("Color.Green => new CborValue.Text(\"green\")"));
        assert!(codec.contains("Priority.Value2 => new CborValue.Int(2)"));
        // The union is the locked [variant_index, value] tagged sum (0-based).
        assert!(codec.contains(
            "new CborValue.Array(new CborValue[] { new CborValue.Uint(0), (CborValue)new CborValue.Uint(csilArm.Value) })"
        ));
        // The record's fields route through the choice codecs and the positional tuple.
        assert!(codec.contains("ColorToCborValue(value.Color)"));
        assert!(codec.contains("IdOrNameToCborValue(value.Who)"));
        assert!(codec.contains("new CborValue.Text(value.Pair.Field0)"));
        // `any` map values pass through verbatim (no null stub, no AsText).
        assert!(codec.contains("(CborValue)csilKv.Value"));
        assert!(!codec.contains("new CborValue.Text(\"who\"), new CborValue.Null()"));
    }

    #[test]
    fn codec_not_emitted_without_records() {
        // A spec with no record types yields no codec file (nothing to (de)serialize).
        let mut input = corndogs_input("csharp-client");
        input
            .csil_spec
            .rules
            .retain(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)));
        let output = render(input).expect("generation ok");
        assert!(!output.files.iter().any(|f| f.path == "Codec.gen.cs"));
    }

    /// The single `.csproj` among the emitted files, or `None` when package mode is off.
    fn csproj(output: &WasmGeneratorOutput) -> Option<&GeneratedFile> {
        output.files.iter().find(|f| f.path.ends_with(".csproj"))
    }

    fn input_with_options(
        target: &str,
        options: &[(&str, serde_json::Value)],
    ) -> WasmGeneratorInput {
        let mut input = corndogs_input(target);
        for (key, value) in options {
            input
                .config
                .options
                .insert((*key).to_string(), value.clone());
        }
        input
    }

    #[test]
    fn no_csproj_without_emit_packages() {
        // The default (non-package) output never carries a project file.
        let output = render(corndogs_input("csharp-client")).expect("generation ok");
        assert!(csproj(&output).is_none());
    }

    #[test]
    fn no_csproj_when_emit_packages_excludes_csharp() {
        // An `emit_packages` opting in *other* languages must not trip C# package mode.
        let output = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!(["go", "rust"]))],
        ))
        .expect("generation ok");
        assert!(csproj(&output).is_none());
    }

    #[test]
    fn csproj_emitted_with_explicit_coordinates() {
        let output = render(input_with_options(
            "csharp-client",
            &[
                ("emit_packages", serde_json::json!(["csharp"])),
                ("package_name", serde_json::json!("Acme.Tasks")),
                ("package_version", serde_json::json!("2.3.4")),
            ],
        ))
        .expect("generation ok");
        let proj = csproj(&output).expect("a .csproj is emitted");
        // The file is named for the package id and carries the requested coordinates.
        assert_eq!(proj.path, "Acme.Tasks.csproj");
        assert!(proj.content.contains("<Project Sdk=\"Microsoft.NET.Sdk\">"));
        assert!(proj.content.contains("<PackageId>Acme.Tasks</PackageId>"));
        assert!(proj.content.contains("<Version>2.3.4</Version>"));
        assert!(
            proj.content
                .contains("<RootNamespace>Acme.Tasks</RootNamespace>")
        );
        assert!(
            proj.content
                .contains("<TargetFramework>net8.0</TargetFramework>")
        );
        // GeneratePackageOnBuild is opt-in, so it is absent unless asked for.
        assert!(!proj.content.contains("GeneratePackageOnBuild"));
        // The default generated `.cs` files are still emitted alongside the project.
        assert!(output.files.iter().any(|f| f.path == "Types.gen.cs"));
        assert!(output.files.iter().any(|f| f.path == "Codec.gen.cs"));
    }

    #[test]
    fn csproj_defaults_to_namespace_and_zero_one_zero() {
        // With no explicit name/version, the package id is the in-code namespace and the
        // version defaults to 0.1.0.
        let output = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!(["csharp"]))],
        ))
        .expect("generation ok");
        let proj = csproj(&output).expect("a .csproj is emitted");
        assert_eq!(proj.path, "Csilgen.Transport.csproj");
        assert!(
            proj.content
                .contains("<PackageId>Csilgen.Transport</PackageId>")
        );
        assert!(proj.content.contains("<Version>0.1.0</Version>"));
    }

    #[test]
    fn csproj_generate_on_build_opt_in() {
        let output = render(input_with_options(
            "csharp-client",
            &[
                ("emit_packages", serde_json::json!(["csharp"])),
                ("package_generate_on_build", serde_json::json!(true)),
            ],
        ))
        .expect("generation ok");
        let proj = csproj(&output).expect("a .csproj is emitted");
        assert!(
            proj.content
                .contains("<GeneratePackageOnBuild>true</GeneratePackageOnBuild>")
        );
    }

    #[test]
    fn emit_packages_parsed_defensively() {
        // A non-array `emit_packages` is ignored rather than erroring or panicking.
        let output = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!("csharp"))],
        ))
        .expect("generation ok");
        assert!(csproj(&output).is_none());
    }

    /// Generate a `csharp-client` self-contained package into a temp dir and prove it is a
    /// valid, buildable .NET project by running `dotnet build` there (offline, BCL-only).
    /// Skips cleanly when no dotnet toolchain is on PATH; the first restore can be slow.
    #[test]
    fn package_builds_with_dotnet() {
        let probe = std::process::Command::new("dotnet")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        let output = render(input_with_options(
            "csharp-client",
            &[
                ("emit_packages", serde_json::json!(["csharp"])),
                ("package_name", serde_json::json!("Csilgen.PackageTest")),
                ("package_version", serde_json::json!("0.2.0")),
            ],
        ))
        .expect("generation ok");
        // The project file must be one of the emitted files — that is the whole point of
        // package mode: the output directory *is* the buildable project.
        assert!(csproj(&output).is_some(), "package mode emits a .csproj");

        let dir =
            std::env::temp_dir().join(format!("csilgen-csharp-package-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg("build")
            .current_dir(&dir)
            // Keep the build self-contained: confine the NuGet/MSBuild state to the temp
            // dir and never block on telemetry/first-run noise.
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        // The harness sets DOTNET_CLI_HOME; pass it through when present so the SDK has a
        // writable home even if the test inherits a sparse env.
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        let run = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet build failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compile the generated codec + typed client and round-trip a corndogs request
    /// through a loopback transport with `dotnet run`. Skips cleanly when no dotnet
    /// toolchain is on PATH; with one present, this is the real proof the output is
    /// usable. The first restore/build can be slow.
    #[test]
    fn codec_round_trips_through_dotnet() {
        let probe = std::process::Command::new("dotnet")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        let output = render(corndogs_input("csharp-client")).expect("generation ok");

        let dir = std::env::temp_dir().join(format!("csilgen-csharp-codec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(dir.join("Program.cs"), CSHARP_DRIVER).unwrap();
        std::fs::write(dir.join("roundtrip.csproj"), CSHARP_CSPROJ).unwrap();

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg("run")
            .arg("--project")
            .arg(&dir)
            .current_dir(&dir)
            // Keep the build self-contained: confine the NuGet/MSBuild state to the
            // temp dir and never block on telemetry/first-run noise.
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        // The harness sets DOTNET_CLI_HOME; pass it through when present so the SDK has
        // a writable home even if the test inherits a sparse env.
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        let run = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compile the default (`both`) output — sync client, async twin, and codec all in one
    /// project — and round-trip a corndogs request through an async loopback transport via
    /// `await`-ing the twin's `SubmitTaskAsync`. This also proves the sync and async symbols
    /// coexist in one namespace. Skips cleanly when no dotnet toolchain is on PATH.
    #[test]
    fn async_client_round_trips_through_dotnet() {
        let probe = std::process::Command::new("dotnet")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        // Default style is `both`, so the package carries both Client.gen.cs and
        // ClientAsync.gen.cs; the async twin is what the driver exercises.
        let output = render(corndogs_input("csharp-client")).expect("generation ok");
        assert!(
            output.files.iter().any(|f| f.path == "ClientAsync.gen.cs"),
            "default `both` emits an async twin"
        );

        let dir = std::env::temp_dir().join(format!("csilgen-csharp-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(dir.join("Program.cs"), CSHARP_ASYNC_DRIVER).unwrap();
        std::fs::write(dir.join("roundtrip.csproj"), CSHARP_CSPROJ).unwrap();

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg("run")
            .arg("--project")
            .arg(&dir)
            .current_dir(&dir)
            // Keep the build self-contained: confine the NuGet/MSBuild state to the temp
            // dir and never block on telemetry/first-run noise.
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        let run = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whether a dotnet toolchain is on PATH (the harness sources it via env.sh).
    fn have_dotnet() -> bool {
        std::process::Command::new("dotnet")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Absolute path to the in-repo `Csilgen.Transport` library project, for a ProjectReference.
    fn transport_lib_csproj() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../transports/csharp/src/Csilgen.Transport/Csilgen.Transport.csproj")
            .canonicalize()
            .expect("transport lib csproj path")
    }

    /// A project that globs the generated `.cs` plus the dropped-in driver and references the
    /// real transport library. `__OUTPUT_TYPE__`/`__LIBREF__` are filled per test (Exe to run,
    /// Library to compile-only).
    const TRANSPORTS_CSPROJ_TEMPLATE: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>__OUTPUT_TYPE__</OutputType>
    <TargetFramework>net8.0</TargetFramework>
    <Nullable>enable</Nullable>
    <ImplicitUsings>disable</ImplicitUsings>
    <AssemblyName>transports_check</AssemblyName>
    <RootNamespace>TransportsCheck</RootNamespace>
    <Deterministic>true</Deterministic>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="__LIBREF__" />
  </ItemGroup>
</Project>
"#;

    /// Stage a transports package (minus its own `.csproj` and the markdown README) into a
    /// fresh temp dir alongside a project referencing the real transport library.
    fn stage_transports_package(
        label: &str,
        target: &str,
        output_type: &str,
    ) -> std::path::PathBuf {
        let output = render(transports_input(target)).expect("generation ok");
        let dir = std::env::temp_dir().join(format!("csilgen-cs-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            if file.path.ends_with(".csproj") || file.path == "genquickstart.md" {
                continue;
            }
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        let csproj = TRANSPORTS_CSPROJ_TEMPLATE
            .replace("__OUTPUT_TYPE__", output_type)
            .replace("__LIBREF__", transport_lib_csproj().to_str().unwrap());
        std::fs::write(dir.join("transports_check.csproj"), csproj).unwrap();
        dir
    }

    /// Run `dotnet <verb>` in `dir` with the self-contained env the package tests use.
    fn run_dotnet(dir: &std::path::Path, verb: &str) -> std::process::Output {
        // Every staged project `ProjectReference`s the single in-repo transport lib,
        // whose build output (`bin/obj`, including `deps.json`) is shared; running the
        // dotnet tests in parallel makes two builds race on those files. Serialize all
        // dotnet invocations so the shared lib build never overlaps itself.
        static DOTNET_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = DOTNET_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg(verb);
        if verb == "run" {
            cmd.arg("--project").arg(dir);
        }
        cmd.current_dir(dir)
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        cmd.output().unwrap()
    }

    /// The definitive package-consistency check: generate the SINGLE `csharp` package a user
    /// actually publishes (the base `csharp` target, which in package mode now carries BOTH the
    /// client surface and the server/router surface), then compile all three `genquickstart.md`
    /// sections together against that one package + the transport library. This is the seam the
    /// pass guards: the RPC section needs the typed *client*, the Events section needs the
    /// generated *router* (`EchoRouter.RouteChannel`), and every section needs the *codec* — all
    /// of which must resolve from the single package. The RPC and Datagrams examples are
    /// additionally *run* over hermetic in-process carriers; the Events TLS session is a
    /// socket-driven loop, so it is compiled in (its router dispatch type-checks) but not driven.
    /// Skips when no dotnet toolchain is present.
    #[test]
    fn genquickstart_all_sections_compile_against_one_package() {
        if !have_dotnet() {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }
        // The package a user publishes: the base `csharp` target in package mode.
        let output = render(transports_input("csharp")).expect("generation ok");
        let readme = file_content(&output, "genquickstart.md").to_string();
        let rpc = section_block(&readme, "## CSIL-RPC (HTTP)");
        let events = section_block(&readme, "## CSIL-Events (TLS)");
        // Swap the real UDP carrier for a seeded loopback (sockets are killed in the sandbox;
        // the lib loopback exercises the same send/recv codec path in-process).
        let datagrams = section_block(&readme, "## CSIL-Datagrams (UDP)").replace(
            "IDatagramCarrier carrier = OpenUdpCarrier(\"localhost\", 9000);",
            "IDatagramCarrier carrier = Seed.Make();",
        );

        let dir = stage_transports_package("all", "csharp", "Exe");
        std::fs::write(dir.join("Rpc.cs"), &rpc).unwrap();
        std::fs::write(dir.join("Events.cs"), &events).unwrap();
        std::fs::write(dir.join("Datagrams.cs"), &datagrams).unwrap();
        std::fs::write(dir.join("Program.cs"), CSHARP_ALL_SECTIONS_DRIVER).unwrap();

        let run = run_dotnet(&dir, "run");
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success() && stdout.contains("late response") && stdout.contains("ok"),
            "dotnet run of the combined genquickstart sections failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drives the combined single-package check: an in-process `HttpMessageHandler` echoes the
    /// CSIL-RPC request as a status-0 `Pong` (Ping/Pong share `Msg`, so the echo decodes), the
    /// typed client (client surface) calls it, then the Datagrams example runs over a seeded
    /// loopback. The Events section (server/router surface) is compiled in alongside but its
    /// socket session is not driven here. All three surfaces resolve from the one package.
    const CSHARP_ALL_SECTIONS_DRIVER: &str = r#"using System.Net;
using System.Net.Http;
using Csilgen.Transport;
using EchoSdk;

internal sealed class EchoHandler : HttpMessageHandler
{
    protected override HttpResponseMessage Send(HttpRequestMessage request, System.Threading.CancellationToken ct)
    {
        byte[] reqBytes = request.Content!.ReadAsByteArrayAsync().GetAwaiter().GetResult();
        RpcRequest req = RpcRequest.Decode(reqBytes);
        byte[] body = RpcResponse.Ok("Pong", req.Payload).Encode();
        return new HttpResponseMessage(HttpStatusCode.OK)
        {
            Content = new ByteArrayContent(body),
        };
    }

    // SendAsync is abstract; the carrier only calls the sync Send, but the override compiles.
    protected override System.Threading.Tasks.Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, System.Threading.CancellationToken ct) =>
        System.Threading.Tasks.Task.FromResult(Send(request, ct));
}

internal static class Seed
{
    public static IDatagramCarrier Make()
    {
        var lb = new LoopbackDatagramCarrier();
        lb.PushInbound(new Datagram(1, 0, Codec.Encode(new Pong { Msg = "example" })).Encode());
        return lb;
    }
}

internal static class Program
{
    private static void Main()
    {
        // RPC: the typed client (client surface) over the in-process echo handler.
        var client = new EchoClient(new HttpRpcCarrier("http://csil.invalid", new EchoHandler()));
        Pong resp = client.Ping(new Ping { Msg = "hello" });
        if (resp.Msg != "hello")
        {
            System.Console.WriteLine("FAIL: " + resp.Msg);
            System.Environment.Exit(1);
        }

        // Datagrams: the generated codec over the seeded loopback carrier (prints "late response").
        CsilDatagramsExample.Run();

        System.Console.WriteLine("ok");
    }
}
"#;

    const CSHARP_CSPROJ: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net8.0</TargetFramework>
    <Nullable>enable</Nullable>
    <ImplicitUsings>disable</ImplicitUsings>
    <AssemblyName>roundtrip</AssemblyName>
    <RootNamespace>RoundTrip</RootNamespace>
    <Deterministic>true</Deterministic>
  </PropertyGroup>
</Project>
"#;

    const CSHARP_DRIVER: &str = r#"using Csilgen.Transport;

// Loopback is a "server" on the far side of the dumb byte seam: it decodes the typed
// request, then encodes its task as the typed response, exercising both decode and
// encode across the transport boundary.
internal sealed class Loopback : ICsilTransport
{
    public byte[] Call(string service, string op, byte[] request)
    {
        if (service != "CorndogsService" || op != "submit-task")
        {
            throw new System.Exception($"unexpected route {service}/{op}");
        }
        var req = Codec.Decode<SubmitTaskRequest>(request);
        return Codec.Encode(req.Task);
    }
}

internal static class Program
{
    private static void Check(bool cond, string msg)
    {
        if (!cond)
        {
            System.Console.WriteLine("FAIL: " + msg);
            System.Environment.Exit(1);
        }
    }

    private static void Main()
    {
        var task = new Task
        {
            Uuid = "u-123",
            CurrentState = "PENDING",
            Payload = new byte[] { 0xde, 0xad, 0xbe },
            Priority = 7,
            Labels = new System.Collections.Generic.Dictionary<string, long> { ["a"] = 1, ["b"] = 2 },
            Tags = new System.Collections.Generic.List<string> { "x", "y" },
        };
        // A named map alias (StringInt64Map = {* text => int}) is a transparent
        // Dictionary<string, long>; a map-of-record alias (TaskMap = {* text => Task})
        // carries records as values. Both must survive the round-trip, not stub to empty.
        var counts = new StringInt64Map { ["alpha"] = 11, ["beta"] = 22 };
        var tasksById = new TaskMap { ["t1"] = task };
        var req = new SubmitTaskRequest { Task = task, Queue = "default", Counts = counts, TasksById = tasksById };

        // Direct codec round-trip through the nested record.
        var back = Codec.Decode<SubmitTaskRequest>(Codec.Encode(req));
        Check(back.Task.Uuid == "u-123", "uuid");
        Check(System.Linq.Enumerable.SequenceEqual(back.Task.Payload, new byte[] { 0xde, 0xad, 0xbe }), "payload");
        Check(back.Task.Priority == 7, "priority");
        Check(back.Task.Labels.Count == 2 && back.Task.Labels["a"] == 1 && back.Task.Labels["b"] == 2, "labels");
        Check(back.Task.Tags.Count == 2 && back.Task.Tags[0] == "x" && back.Task.Tags[1] == "y", "tags");
        Check(back.Queue == "default", "queue");
        Check(back.Counts.Count == 2 && back.Counts["alpha"] == 11 && back.Counts["beta"] == 22, "map alias");
        Check(back.TasksById.Count == 1 && back.TasksById["t1"].Uuid == "u-123" && back.TasksById["t1"].Priority == 7, "map of record");

        // An absent optional must round-trip to null, not a zero value.
        var task2 = task with { Priority = null };
        var back2 = Codec.Decode<SubmitTaskRequest>(Codec.Encode(new SubmitTaskRequest { Task = task2, Queue = "q", Counts = counts, TasksById = tasksById }));
        Check(back2.Task.Priority is null, "absent optional null");

        // Typed client round-trip over the loopback byte seam.
        var client = new CorndogsClient(new Loopback());
        var resp = client.SubmitTask(req);
        Check(resp.Uuid == "u-123", "client uuid");
        Check(System.Linq.Enumerable.SequenceEqual(resp.Payload, new byte[] { 0xde, 0xad, 0xbe }), "client payload");
        Check(resp.Priority == 7, "client priority");
        Check(resp.Tags.Count == 2 && resp.Tags[1] == "y", "client tags");

        System.Console.WriteLine("ok");
    }
}
"#;

    const CSHARP_ASYNC_DRIVER: &str = r#"using Csilgen.Transport;

// The async loopback rides the same dumb byte seam as the sync one (same wire op string),
// but satisfies the async transport interface by returning a completed Task.
internal sealed class AsyncLoopback : ICsilAsyncTransport
{
    public System.Threading.Tasks.Task<byte[]> Call(string service, string op, byte[] request)
    {
        if (service != "CorndogsService" || op != "submit-task")
        {
            throw new System.Exception($"unexpected route {service}/{op}");
        }
        var req = Codec.Decode<SubmitTaskRequest>(request);
        return System.Threading.Tasks.Task.FromResult(Codec.Encode(req.Task));
    }
}

internal static class Program
{
    private static void Check(bool cond, string msg)
    {
        if (!cond)
        {
            System.Console.WriteLine("FAIL: " + msg);
            System.Environment.Exit(1);
        }
    }

    private static async System.Threading.Tasks.Task Main()
    {
        var task = new Task
        {
            Uuid = "u-123",
            CurrentState = "PENDING",
            Payload = new byte[] { 0xde, 0xad, 0xbe },
            Priority = 7,
            Labels = new System.Collections.Generic.Dictionary<string, long> { ["a"] = 1, ["b"] = 2 },
            Tags = new System.Collections.Generic.List<string> { "x", "y" },
        };
        var counts = new StringInt64Map { ["alpha"] = 11, ["beta"] = 22 };
        var tasksById = new TaskMap { ["t1"] = task };
        var req = new SubmitTaskRequest { Task = task, Queue = "default", Counts = counts, TasksById = tasksById };

        // The async twin's method is Async-suffixed and awaited over the async byte seam.
        var client = new CorndogsAsyncClient(new AsyncLoopback());
        var resp = await client.SubmitTaskAsync(req);
        Check(resp.Uuid == "u-123", "client uuid");
        Check(System.Linq.Enumerable.SequenceEqual(resp.Payload, new byte[] { 0xde, 0xad, 0xbe }), "client payload");
        Check(resp.Priority == 7, "client priority");
        Check(resp.Tags.Count == 2 && resp.Tags[1] == "y", "client tags");

        // The sync client compiles and runs in the same namespace, proving coexistence.
        var sync = new CorndogsClient(new SyncLoopback());
        var syncResp = sync.SubmitTask(req);
        Check(syncResp.Uuid == "u-123", "sync client uuid");

        System.Console.WriteLine("ok");
    }
}

internal sealed class SyncLoopback : ICsilTransport
{
    public byte[] Call(string service, string op, byte[] request)
    {
        var req = Codec.Decode<SubmitTaskRequest>(request);
        return Codec.Encode(req.Task);
    }
}
"#;

    // -----------------------------------------------------------------------
    // Inline (anonymous) choice/group hoisting
    // -----------------------------------------------------------------------

    /// The inline-choice torture spec built as AST: an `InlineChoicePayload` record and an
    /// `InlineChoiceRecord` exercising an inline choice in every hoistable position — a
    /// direct field (open union), a `.default`-wrapped literal field (still an enum), a
    /// mixed union with a reference arm, an array element, a map value, a tuple slot, and a
    /// field inside an inline group (recursion). Mirrors inline-choice-torture.csil.
    fn inline_choice_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let int = || CsilTypeExpression::Builtin("int".to_string());
        let boolean = || CsilTypeExpression::Builtin("bool".to_string());
        let lit_text = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));
        let lit_int = |n: i64| CsilTypeExpression::Literal(CsilLiteralValue::Integer(n));
        // A literal arm carrying a trailing `.default` control operator — the parser binds
        // it to this one arm, so it arrives as `Constrained { Literal, .. }`.
        let defaulted = |s: &str, default: &str| CsilTypeExpression::Constrained {
            base_type: Box::new(lit_text(s)),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                default.to_string(),
            ))],
        };
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let opt_entry = |name: &str, ty: CsilTypeExpression| CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let tuple_slot = |ty: CsilTypeExpression| CsilGroupEntry {
            key: None,
            value_type: ty,
            occurrence: None,
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let group_rule = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: pos(),
            doc_comments: Vec::new(),
        };

        let payload = group_rule("InlineChoicePayload", vec![bare_entry("detail", text())]);
        let record = group_rule(
            "InlineChoiceRecord",
            vec![
                bare_entry(
                    "status",
                    CsilTypeExpression::Choice(vec![
                        text(),
                        lit_text("pending"),
                        lit_text("active"),
                        lit_text("closed"),
                    ]),
                ),
                opt_entry(
                    "priority",
                    CsilTypeExpression::Choice(vec![
                        text(),
                        lit_text("low"),
                        lit_text("normal"),
                        defaulted("high", "normal"),
                    ]),
                ),
                opt_entry(
                    "size",
                    CsilTypeExpression::Choice(vec![
                        lit_text("small"),
                        lit_text("medium"),
                        defaulted("large", "medium"),
                    ]),
                ),
                bare_entry(
                    "payload",
                    CsilTypeExpression::Choice(vec![
                        lit_text("none"),
                        lit_int(42),
                        CsilTypeExpression::Reference("InlineChoicePayload".to_string()),
                    ]),
                ),
                bare_entry(
                    "tags",
                    CsilTypeExpression::Array {
                        element_type: Box::new(CsilTypeExpression::Choice(vec![
                            text(),
                            lit_text("red"),
                            lit_text("green"),
                            lit_text("blue"),
                            int(),
                        ])),
                        occurrence: Some(CsilOccurrence::ZeroOrMore),
                    },
                ),
                bare_entry(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(text()),
                        value: Box::new(CsilTypeExpression::Choice(vec![
                            text(),
                            lit_text("yes"),
                            lit_text("no"),
                            boolean(),
                        ])),
                        occurrence: Some(CsilOccurrence::ZeroOrMore),
                    },
                ),
                bare_entry(
                    "coord",
                    CsilTypeExpression::Tuple(CsilGroupExpression {
                        entries: vec![
                            tuple_slot(int()),
                            tuple_slot(CsilTypeExpression::Choice(vec![
                                text(),
                                lit_text("x"),
                                lit_text("y"),
                                lit_text("z"),
                            ])),
                        ],
                    }),
                ),
                bare_entry(
                    "nested",
                    CsilTypeExpression::Group(CsilGroupExpression {
                        entries: vec![bare_entry(
                            "kind",
                            CsilTypeExpression::Choice(vec![
                                text(),
                                lit_text("a"),
                                lit_text("b"),
                                int(),
                            ]),
                        )],
                    }),
                ),
            ],
        );
        let svc = CsilRule {
            name: "InlineChoiceService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "round-trip".to_string(),
                    input_type: CsilTypeExpression::Reference("InlineChoiceRecord".to_string()),
                    output_type: CsilTypeExpression::Reference("InlineChoiceRecord".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![payload, record, svc],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    #[test]
    fn inline_choice_fields_hoisted_to_named_unions() {
        let output = render(inline_choice_input("csharp")).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");
        // A direct inline open union is hoisted to a named discriminated union and the field
        // is typed as it — no `object` collapse.
        assert!(types.contains("public abstract record InlineChoiceRecordStatus;"));
        assert!(types.contains("public required InlineChoiceRecordStatus Status { get; init; }"));
        // The mixed union with a reference arm carries a variant named after the reference.
        assert!(types.contains(
            "public sealed record InlineChoiceRecordPayloadInlineChoicePayload(InlineChoicePayload Value) : InlineChoiceRecordPayload;"
        ));
        // No inline-choice field falls back to the opaque `object` mapping any more.
        assert!(!types.contains("public required object Status"));
        assert!(!types.contains("List<object>"));
        assert!(!types.contains("Dictionary<string, object>"));
    }

    #[test]
    fn inline_default_literal_choice_stays_a_closed_enum() {
        // Regression for the Constrained-arm bug: `.default "medium"` binds to the last
        // literal arm (`Constrained { Literal("large"), .. }`), which must still classify as
        // a literal enum member rather than pushing the whole choice into the union path.
        let output = render(inline_choice_input("csharp")).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");
        assert!(types.contains("public enum InlineChoiceRecordSize"));
        assert!(!types.contains("abstract record InlineChoiceRecordSize"));
        // The wrapped final arm still appears as an enum member.
        assert!(types.contains("Large,"));
        let codec = file_content(&output, "Codec.gen.cs");
        // Bare-literal wire (not a tagged sum) for the closed enum.
        assert!(codec.contains("InlineChoiceRecordSize.Large => new CborValue.Text(\"large\"),"));
    }

    #[test]
    fn inline_choice_in_array_map_tuple_and_nested_group_hoisted() {
        let output = render(inline_choice_input("csharp")).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");
        assert!(types.contains("System.Collections.Generic.List<InlineChoiceRecordTagsItem> Tags"));
        assert!(types.contains(
            "System.Collections.Generic.Dictionary<string, InlineChoiceRecordLabelsValue> Labels"
        ));
        assert!(types.contains("(long Field0, InlineChoiceRecordCoord1 Field1) Coord"));
        // The inline group field is hoisted, and its own inline choice field is hoisted too
        // (recursion into a hoisted composite).
        assert!(types.contains("public sealed record InlineChoiceRecordNested"));
        assert!(types.contains("public required InlineChoiceRecordNestedKind Kind"));
        let codec = file_content(&output, "Codec.gen.cs");
        // The nested array/map/tuple codecs route each element through the hoisted union
        // codec rather than emitting a CBOR null placeholder.
        assert!(codec.contains("InlineChoiceRecordTagsItemToCborValue(csilElem)"));
        assert!(codec.contains("InlineChoiceRecordLabelsValueToCborValue(csilKv.Value)"));
        assert!(codec.contains("InlineChoiceRecordCoord1ToCborValue(value.Coord.Field1)"));
        // Regression: none of the hoisted positions collapse to a null placeholder.
        assert!(!codec.contains("csilElem => (CborValue)new CborValue.Null()"));
    }

    /// Mirrors `csilgen_common::hoist`'s
    /// `case_insensitive_collision_between_existing_and_synthesized_rule_is_disambiguated`:
    /// an existing rule `UserData` plus a `User` rule whose `data` field is an inline mixed
    /// choice (a text-literal arm and a `UserData` reference arm — a `Union`, so it hoists
    /// regardless of `hoist_all_literal_choices`) hoist-names to `User_data` (owner `User`,
    /// field `data`), which pascal-collides with `UserData` (both canonicalize to
    /// `"userdata"`). The shared hoist pass (now wired into this generator) disambiguates
    /// the synthesized rule name; this test additionally proves the C# generator doesn't go
    /// on to declare two colliding C# types for the two different rules — a duplicate,
    /// non-compiling `record`/`enum` declaration, the actual failure mode a name collision
    /// causes here.
    #[test]
    fn case_insensitive_collision_between_existing_and_synthesized_rule_does_not_duplicate_a_csharp_type()
     {
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let user_data = CsilRule {
            name: "UserData".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry(
                    "value",
                    CsilTypeExpression::Builtin("text".to_string()),
                )],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let user = CsilRule {
            name: "User".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry(
                    "data",
                    CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("x".to_string())),
                        CsilTypeExpression::Reference("UserData".to_string()),
                    ]),
                )],
            }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![user_data, user],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "csharp".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "csharp".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "csharp".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        };
        let output = render(input).expect("generation ok");
        let types = file_content(&output, "Types.gen.cs");

        // The existing UserData group survives, and the synthesized choice's base type is
        // NOT literally also named UserData (that would be a duplicate `record UserData`
        // declaration, non-compiling C#).
        assert!(types.contains("public sealed record UserData\n"));
        assert!(
            !types.contains("public abstract record UserData;"),
            "synthesized choice collided with UserData's record name:\n{types}"
        );

        // Every `record`/`enum` header in the file introduces a unique C# identifier.
        let mut declared: Vec<&str> = Vec::new();
        for raw in types.lines() {
            let line = raw.trim();
            let rest = line
                .strip_prefix("public sealed record ")
                .or_else(|| line.strip_prefix("public abstract record "))
                .or_else(|| line.strip_prefix("public enum "));
            if let Some(rest) = rest {
                let name = rest
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    declared.push(name);
                }
            }
        }
        let mut sorted = declared.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            declared.len(),
            "a C# type name is declared more than once: {declared:?}"
        );
    }

    /// Compile the generated inline-choice torture output and assert the exact canonical CBOR
    /// bytes for every record-field position match the OCaml oracle, then prove the
    /// array/map/tuple positions (which OCaml has no codec for) round-trip stably. Skips
    /// cleanly when no dotnet toolchain is on PATH.
    #[test]
    fn inline_choice_bytes_match_oracle_through_dotnet() {
        let probe = std::process::Command::new("dotnet")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        let output = render(inline_choice_input("csharp-client")).expect("generation ok");
        let dir =
            std::env::temp_dir().join(format!("csilgen-csharp-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in &output.files {
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(dir.join("Program.cs"), CSHARP_INLINE_DRIVER).unwrap();
        std::fs::write(dir.join("roundtrip.csproj"), CSHARP_CSPROJ).unwrap();

        let mut cmd = std::process::Command::new("dotnet");
        cmd.arg("run")
            .arg("--project")
            .arg(&dir)
            .current_dir(&dir)
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .env("NUGET_PACKAGES", dir.join(".nuget"));
        if let Ok(home) = std::env::var("DOTNET_CLI_HOME") {
            cmd.env("DOTNET_CLI_HOME", home);
        }
        let run = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "dotnet run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Driver for the inline-choice byte oracle: encodes each record-field inline choice via
    /// the generated codec and asserts the exact hex against the OCaml-confirmed table, then
    /// round-trips a full record (covering the array/map/tuple positions) for stability.
    const CSHARP_INLINE_DRIVER: &str = r#"using Csilgen.Transport;

internal static class Program
{
    private static string Hex(CborValue v) =>
        System.Convert.ToHexString(Cbor.Encode(v)).ToLowerInvariant();

    private static void Check(string label, CborValue v, string expect)
    {
        var got = Hex(v);
        if (got != expect)
        {
            System.Console.WriteLine($"FAIL {label}: got={got} expect={expect}");
            System.Environment.Exit(1);
        }
    }

    private static void Main()
    {
        Check("status(Pending)",
            Codec.InlineChoiceRecordStatusToCborValue(new InlineChoiceRecordStatusVariant2("pending")),
            "82016770656e64696e67");
        Check("status(Other free)",
            Codec.InlineChoiceRecordStatusToCborValue(new InlineChoiceRecordStatusVariant1("free")),
            "82006466726565");
        Check("priority(High)",
            Codec.InlineChoiceRecordPriorityToCborValue(new InlineChoiceRecordPriorityVariant4("high")),
            "82036468696768");
        Check("size(Medium)",
            Codec.InlineChoiceRecordSizeToCborValue(InlineChoiceRecordSize.Medium),
            "666d656469756d");
        Check("payload(None)",
            Codec.InlineChoiceRecordPayloadToCborValue(new InlineChoiceRecordPayloadVariant1("none")),
            "8200646e6f6e65");
        Check("payload(Inline)",
            Codec.InlineChoiceRecordPayloadToCborValue(
                new InlineChoiceRecordPayloadInlineChoicePayload(new InlineChoicePayload { Detail = "hi" })),
            "8202a16664657461696c626869");
        Check("nested.kind(A)",
            Codec.InlineChoiceRecordNestedKindToCborValue(new InlineChoiceRecordNestedKindVariant2("a")),
            "82016161");
        Check("nested.kind(Text free)",
            Codec.InlineChoiceRecordNestedKindToCborValue(new InlineChoiceRecordNestedKindVariant1("free")),
            "82006466726565");
        Check("nested.kind(Int 7)",
            Codec.InlineChoiceRecordNestedKindToCborValue(new InlineChoiceRecordNestedKindVariant4(7)),
            "820307");

        // Full-record round-trip covers tags/labels/coord (array/map/tuple inline choices),
        // which the OCaml oracle has no codec for; the bytes must be stable and decode-equal.
        var rec = new InlineChoiceRecord
        {
            Status = new InlineChoiceRecordStatusVariant2("pending"),
            Priority = new InlineChoiceRecordPriorityVariant4("high"),
            Size = InlineChoiceRecordSize.Medium,
            Payload = new InlineChoiceRecordPayloadInlineChoicePayload(new InlineChoicePayload { Detail = "hi" }),
            Tags = new System.Collections.Generic.List<InlineChoiceRecordTagsItem>
            {
                new InlineChoiceRecordTagsItemVariant2("red"),
                new InlineChoiceRecordTagsItemVariant1("adhoc"),
                new InlineChoiceRecordTagsItemVariant5(99),
            },
            Labels = new System.Collections.Generic.Dictionary<string, InlineChoiceRecordLabelsValue>
            {
                ["k"] = new InlineChoiceRecordLabelsValueVariant2("yes"),
                ["b"] = new InlineChoiceRecordLabelsValueVariant4(true),
            },
            Coord = (5L, new InlineChoiceRecordCoord1Variant2("x")),
            Nested = new InlineChoiceRecordNested { Kind = new InlineChoiceRecordNestedKindVariant4(7) },
        };
        var bytes = Codec.Encode(rec);
        var back = Codec.Decode<InlineChoiceRecord>(bytes);
        var bytes2 = Codec.Encode(back);
        if (System.Convert.ToHexString(bytes) != System.Convert.ToHexString(bytes2))
        {
            System.Console.WriteLine("FAIL round-trip not stable");
            System.Environment.Exit(1);
        }
        var tag0 = (InlineChoiceRecordTagsItemVariant2)back.Tags[0];
        var coordArm = (InlineChoiceRecordCoord1Variant2)back.Coord.Field1;
        if (tag0.Value != "red" || back.Coord.Field0 != 5 || coordArm.Value != "x")
        {
            System.Console.WriteLine("FAIL decoded inline positions");
            System.Environment.Exit(1);
        }

        System.Console.WriteLine("ok");
    }
}
"#;
}
