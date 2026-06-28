//! C# code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target csharp` from `csilgen_csharp_generator.wasm`.
//! Emits idiomatic modern C# (net8.0 / C# 12): file-scoped `namespace Csilgen.Transport;`,
//! `sealed record` types with `required`/nullable `init` properties, closed
//! discriminated-union emulation for variants, a primary-constructor client, and a
//! server interface + verbose/compact channel routers — never the wire bytes.

use csilgen_common::{
    CsilControlOperator, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
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

impl CsharpConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
        let namespace = options
            .get("csharp_namespace")
            .or_else(|| options.get("namespace"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Csilgen.Transport")
            .to_string();

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
pub fn render(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
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

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
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
            Surface::Server => {
                if let Some(services) = generate_services(&input, &config) {
                    files.push(GeneratedFile {
                        path: "Services.gen.cs".to_string(),
                        content: services,
                    });
                }
            }
        }
    }

    // Self-contained publishable-package mode: when `emit_packages` opts the `csharp`
    // target in, the output directory additionally carries an SDK-style `.csproj` so the
    // directory itself is a valid, NuGet-packable .NET project. SDK-style projects glob
    // `**/*.cs` by default, so the flat generated files compile in with no extra wiring.
    // The default (non-package) output is otherwise byte-identical.
    if package_requested(&input.config.options, "csharp") {
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
                path: "README.md".to_string(),
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
// Package README (with a copy-paste CSIL-RPC Quickstart)
// ---------------------------------------------------------------------------

/// The package README. For a client package it carries a copy-paste **Quickstart**: a
/// complete, dependency-free CSIL-RPC carrier (it reuses this package's own generated
/// CBOR codec for the envelope, so it adds no third-party dependency — not even the
/// in-box `System.Formats.Cbor`), the typed client over it, and one example call. A
/// serviceless / server package gets the consume-the-types section instead.
fn readme_file(
    input: &WasmGeneratorInput,
    config: &CsharpConfig,
    coords: &PackageCoords,
) -> String {
    let title = &coords.package_id;
    let mut out = format!(
        "# {title}\n\n\
         Generated by csilgen. A typed, transport-agnostic CSIL-RPC client: the generated\n\
         codec owns CBOR (de)serialization; you supply a *carrier* that only moves bytes.\n\n\
         ## Install\n\n\
         ```sh\n\
         # TODO: a published NuGet artifact is pending. Until then, reference the generated\n\
         # project directly from your app:\n\
         dotnet add reference path/to/{title}.csproj\n\
         ```\n\n"
    );

    let records = record_names(input);
    // The carrier implements the sync `ICsilTransport` seam, which only the client surface
    // emits — so the full Quickstart is client-only; everything else gets the types section.
    let example = if input.config.target == "csharp-client" {
        first_readme_example(input, &records, config)
    } else {
        None
    };
    match example {
        Some(ex) => out.push_str(&readme_quickstart(config, &records, &ex)),
        None => out.push_str(&format!(
            "## Quickstart\n\n\
             This package exposes generated types and a self-contained CBOR codec. Encode\n\
             and decode them with the generated `Codec`:\n\n\
             ```csharp\n\
             using {ns};\n\n\
             // byte[] bytes = Codec.Encode(value);\n\
             // var value = Codec.Decode<YourType>(bytes);\n\
             ```\n",
            ns = config.namespace
        )),
    }
    out
}

/// The client Quickstart section: prose + a single fenced C# block containing the carrier
/// and the example call.
fn readme_quickstart(
    config: &CsharpConfig,
    records: &std::collections::HashSet<String>,
    ex: &ReadmeExample,
) -> String {
    let carrier = readme_carrier(&config.namespace, records.contains("ServiceError"));
    let example = readme_example(ex);
    format!(
        "## Quickstart\n\n\
         A complete CSIL-RPC carrier — **no third-party dependency**, it reuses this\n\
         package's own generated CBOR codec (`CborValue`/`Cbor`) to build and parse the\n\
         envelope. It POSTs to `{{baseUrl}}/csil/v1/rpc` with the stdlib `HttpClient` and\n\
         hands the response payload bytes back to the generated client to decode. Change\n\
         the one base-URL string.\n\n\
         ```csharp\n{carrier}\n{example}```\n"
    )
}

/// The CSIL-RPC carrier source. The generated codec exports a generic CBOR value tree
/// (`CborValue`, with tag support) and a `Cbor.Encode`/`Cbor.Decode` pair, so the carrier
/// builds the fixed envelope with those rather than pulling `System.Formats.Cbor` — path 1
/// of the README hybrid rule (zero third-party deps). `__SERVICE_ERROR_ARM__` is filled
/// with the typed decode only when the spec actually declares a `ServiceError` record.
fn readme_carrier(namespace: &str, has_service_error: bool) -> String {
    let arm = if has_service_error {
        "            ServiceError se = Codec.Decode<ServiceError>(inner);\n            throw new CsilClientException(se.Code, se.Message);"
    } else {
        "            throw new CsilClientException(0, $\"csil-rpc {service}/{op}: service error\");"
    };
    CARRIER_CSHARP
        .replace("__NAMESPACE__", namespace)
        .replace("__SERVICE_ERROR_ARM__", arm)
}

/// The carrier body. A constant with two placeholders (the package namespace and the
/// `ServiceError` arm) so its many C# braces need no `format!` escaping.
const CARRIER_CSHARP: &str = r#"// CSIL-RPC carrier — DEPENDENCY-FREE. It reuses THIS package's generated CBOR
// (CborValue/Cbor, from Codec.gen.cs) to build and parse the envelope, so it needs no
// third-party CBOR library — not even the in-box System.Formats.Cbor. The generated Codec
// owns your types' (de)serialization; this carrier only moves bytes over stdlib HttpClient.
using System;
using System.Net.Http;
using System.Net.Http.Headers;
using __NAMESPACE__;

public sealed class CsilRpcHttpTransport : ICsilTransport, IDisposable
{
    private const ulong TagEncodedCbor = 24; // RFC 8949 §3.4.5.1 embedded CBOR
    private readonly string _url;
    private readonly HttpClient _http;

    public CsilRpcHttpTransport(string baseUrl) : this(baseUrl, new HttpClientHandler()) { }

    // The handler seam keeps the carrier testable: inject a stub HttpMessageHandler to
    // exercise it in-process with no sockets.
    public CsilRpcHttpTransport(string baseUrl, HttpMessageHandler handler)
    {
        _url = baseUrl.TrimEnd('/') + "/csil/v1/rpc";
        _http = new HttpClient(handler);
    }

    public byte[] Call(string service, string op, byte[] request)
    {
        // CsilRpcRequest = { v, service, op, payload: #6.24(bstr) }. The payload is the
        // already-encoded request wrapped in CBOR tag 24 (embedded CBOR).
        var envelope = new CborValue.Map(new (CborValue, CborValue)[]
        {
            (new CborValue.Text("v"), new CborValue.Uint(1)),
            (new CborValue.Text("service"), new CborValue.Text(service)),
            (new CborValue.Text("op"), new CborValue.Text(op)),
            (new CborValue.Text("payload"), new CborValue.Tag(TagEncodedCbor, new CborValue.Bytes(request))),
        });

        using var msg = new HttpRequestMessage(HttpMethod.Post, _url)
        {
            Content = new ByteArrayContent(Cbor.Encode(envelope)),
        };
        msg.Content.Headers.ContentType = new MediaTypeHeaderValue("application/cbor");
        msg.Headers.Accept.ParseAdd("application/cbor");

        using HttpResponseMessage resp = _http.Send(msg);
        byte[] body = resp.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult();
        if (!resp.IsSuccessStatusCode)
            throw new CsilClientException(0, $"csil-rpc {service}/{op}: http {(int)resp.StatusCode}");

        // CsilRpcResponse = { v, status, ? variant, ? error, payload: #6.24(bstr) }.
        var env = (CborValue.Map)Cbor.Decode(body);
        long status = EnvInt(env, "status");
        if (status != 0)
            throw new CsilClientException(status, EnvText(env, "error") ?? $"transport status {status}");

        byte[] inner = EnvPayload(env);
        // A typed "ServiceError" arm is an application error, distinct from a transport failure.
        if (EnvText(env, "variant") == "ServiceError")
        {
__SERVICE_ERROR_ARM__
        }
        return inner;
    }

    public void Dispose() => _http.Dispose();

    private static CborValue? EnvLookup(CborValue.Map map, string key)
    {
        foreach (var (k, v) in map.Entries)
            if (k is CborValue.Text t && t.Value == key) return v;
        return null;
    }

    private static long EnvInt(CborValue.Map map, string key) => EnvLookup(map, key) switch
    {
        CborValue.Uint u => (long)u.Value,
        CborValue.Int i => i.Value,
        _ => 0,
    };

    private static string? EnvText(CborValue.Map map, string key) =>
        EnvLookup(map, key) is CborValue.Text t ? t.Value : null;

    // The response payload is a tag-24 byte string; hand its raw inner bytes to the codec.
    private static byte[] EnvPayload(CborValue.Map map) =>
        EnvLookup(map, "payload") is CborValue.Tag { Inner: CborValue.Bytes b }
            ? b.Value
            : throw new CsilClientException(0, "csil-rpc: response payload is not a tag-24 byte string");
}
"#;

/// The example call: construct the typed client over the carrier and invoke the first
/// unary op with a generated sample request literal.
fn readme_example(ex: &ReadmeExample) -> String {
    let call = match &ex.sample {
        Some(sample) => format!("client.{}({sample})", ex.method),
        None => format!("client.{}()", ex.method),
    };
    format!(
        "// Construct the client over the carrier and make one call. Change the URL only.\n\
         public static class Example\n{{\n    \
         public static void Run()\n    {{\n        \
         var client = new {client}(new CsilRpcHttpTransport(\"http://localhost:5080\"));\n        \
         var response = {call};\n        \
         System.Console.WriteLine(response);\n    }}\n}}\n",
        client = ex.client_class,
        call = call
    )
}

/// The pieces the Quickstart's example call needs: the client class to construct, the
/// method to call, and a compiling C# request literal (`None` when the op takes no input).
struct ReadmeExample {
    client_class: String,
    method: String,
    sample: Option<String>,
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
            });
        }
    }
    None
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
                    pascal_ident(&wire_key(key)),
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
            let prop = pascal_ident(&wire);
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
                let field = FieldRef {
                    prop: pascal_ident(&wire_key(key)),
                    optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
                };
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Constraint(constraint) = metadata {
                        emit_metadata_constraint(
                            body,
                            &field,
                            &entry.value_type,
                            constraint,
                            config,
                        );
                    }
                }
                if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                    for op in constraints {
                        emit_control_op_check(body, &field, &entry.value_type, op, config);
                    }
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
    if !choices.is_empty() && choices.iter().all(is_literal_choice) {
        emit_enum(body, name, choices);
        return;
    }

    let base = pascal_ident(name);
    body.push_str(
        "// Closed discriminated union; consume with an exhaustive `switch` expression.\n",
    );
    body.push_str(&format!("public abstract record {base};\n"));
    for (index, choice) in choices.iter().enumerate() {
        match choice {
            CsilTypeExpression::Reference(reference) => {
                let arm = pascal_ident(reference);
                let inner = pascal_ident(reference);
                // The arm wraps the referenced type; the CSIL variant wire name is the
                // reference verbatim so a decoder can map the tag back to this arm.
                body.push_str(&format!("// variant '{reference}'\n"));
                body.push_str(&format!(
                    "public sealed record {base}{arm}({inner} Value) : {base};\n"
                ));
            }
            other => {
                let arm = format!("Variant{}", index + 1);
                let inner = map_csil_type(other, config);
                body.push_str(&format!(
                    "public sealed record {base}{arm}({inner} Value) : {base};\n"
                ));
            }
        }
    }
    body.push('\n');
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
                let prop = pascal_ident(&wire);
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

fn emit_enum(body: &mut String, name: &str, choices: &[CsilTypeExpression]) {
    let enum_name = pascal_ident(name);
    body.push_str(&format!("public enum {enum_name}\n{{\n"));
    for choice in choices {
        if let CsilTypeExpression::Literal(literal) = choice {
            match literal {
                CsilLiteralValue::Text(text) => {
                    // The literal text is the wire value verbatim; the member name is a
                    // generator-side PascalCase mapping of it.
                    body.push_str(&format!("    // wire value: {text}\n"));
                    body.push_str(&format!("    {},\n", pascal_ident(text)));
                }
                CsilLiteralValue::Integer(value) => {
                    body.push_str(&format!("    Value{value} = {value},\n"));
                }
                _ => {}
            }
        }
    }
    body.push_str("}\n\n");
}

fn is_literal_choice(choice: &CsilTypeExpression) -> bool {
    matches!(
        choice,
        CsilTypeExpression::Literal(CsilLiteralValue::Text(_))
            | CsilTypeExpression::Literal(CsilLiteralValue::Integer(_))
    )
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

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_class(&mut body, &rule.name, service, config, &records, shape);
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

fn emit_client_class(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
    records: &std::collections::HashSet<String>,
    shape: ClientShape,
) {
    let base = service_base(name);
    let client = shape.class_name(&base);
    let transport = shape.transport_name();
    // Canonical wire strings (the wire contract): service lowercased, op PascalCased.
    // These never change with the client shape — the async twin rides the same wire.
    let wire_service = base.to_lowercase();

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
        // The typed-codec path needs a record success type (and a record or null
        // request) so the method can round-trip through the generated codec. Anything
        // else is skipped with a note rather than emitting an uncompilable call.
        let resp_ok = is_record_ref(&success, records);
        let req_ok = op_input_is_null(&operation.input_type)
            || is_record_ref(&operation.input_type, records);
        if !resp_ok || !req_ok {
            body.push_str(&format!(
                "    // operation '{}' has a non-record payload; (de)serialize it manually\n",
                operation.name
            ));
            continue;
        }
        // The `Async` suffix rides the marker so the twin's `SubmitTaskAsync` coexists with
        // the sync `SubmitTask`, while the drop-in keeps the canonical name.
        let method = format!("{}{}", pascal_ident(&operation.name), shape.marker);
        let wire_op = wire_op_string(&operation.name);
        let output = map_csil_type(&success, config);
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
        match op_param(&operation.input_type) {
            None => {
                body.push_str(&format!(
                    "    public {async_kw}{ret} {method}() =>\n        Codec.Decode<{output}>({await_kw}transport.Call(\"{wire_service}\", \"{wire_op}\", System.Array.Empty<byte>()));\n"
                ));
            }
            Some(param) => {
                let input = map_csil_type(&operation.input_type, config);
                body.push_str(&format!(
                    "    public {async_kw}{ret} {method}({input} {param}) =>\n        Codec.Decode<{output}>({await_kw}transport.Call(\"{wire_service}\", \"{wire_op}\", Codec.Encode({param})));\n"
                ));
            }
        }
    }

    body.push_str("}\n\n");
}

/// The wire `op` string: the operation name PascalCased with the simple rule
/// (capitalize after `_`/`-`), matching the other generators (`submit-task` →
/// `"SubmitTask"`). Never keyword-escaped — the wire string is raw, unlike the C#
/// method identifier.
fn wire_op_string(name: &str) -> String {
    pascal_case(name)
}

/// Whether a type is a reference to a record the codec can (de)serialize, so a typed
/// client method can round-trip it through the generated `Codec`.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(&pascal_ident(name)))
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
            "nil" | "null" => "new CborValue.Null()".to_string(),
            _ => "new CborValue.Null()".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&pascal_ident(name)) => {
            format!("{}ToCborValue({expr})", pascal_ident(name))
        }
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`) carries
        // no codec of its own; encode it as its underlying map/array/scalar. The C# alias
        // is a `global using` synonym, so `expr` is already the underlying type the
        // resolved encoder expects.
        CsilTypeExpression::Reference(name) if aliases.contains_key(&pascal_ident(name)) => {
            csharp_enc_value(&aliases[&pascal_ident(name)], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = csharp_enc_value(element_type, "csilElem", records, aliases);
            format!("new CborValue.Array({expr}.Select(csilElem => (CborValue){inner}).ToList())")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kenc = csharp_enc_value(key, "csilKv.Key", records, aliases);
            let venc = csharp_enc_value(value, "csilKv.Value", records, aliases);
            format!(
                "new CborValue.Map({expr}.Select(csilKv => ((CborValue){kenc}, (CborValue){venc})).ToList())"
            )
        }
        // A shape the codec cannot model precisely (a non-record reference, choice,
        // tuple, `any`) is carried as null rather than emitting uncompilable code.
        _ => "new CborValue.Null()".to_string(),
    }
}

/// A C# expression decoding a typed value from `expr` (a `CborValue`).
fn csharp_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
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
            _ => format!("Cbor.AsText({expr})"),
        },
        CsilTypeExpression::Reference(name) if records.contains(&pascal_ident(name)) => {
            format!("{}FromCborValue({expr})", pascal_ident(name))
        }
        // A reference to a transparent alias decodes as its underlying map/array/scalar;
        // the value the resolved decoder returns is assignable to the alias-typed field
        // because the C# alias is a `global using` synonym for that same type.
        CsilTypeExpression::Reference(name) if aliases.contains_key(&pascal_ident(name)) => {
            csharp_dec_value(&aliases[&pascal_ident(name)], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = csharp_dec_value(element_type, "csilElem", records, aliases);
            format!("Cbor.AsArray({expr}).Select(csilElem => {inner}).ToList()")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kdec = csharp_dec_value(key, "csilKv.Key", records, aliases);
            let vdec = csharp_dec_value(value, "csilKv.Value", records, aliases);
            format!("Cbor.AsMap({expr}).ToDictionary(csilKv => {kdec}, csilKv => {vdec})")
        }
        _ => format!("Cbor.AsText({expr})"),
    }
}

/// Emit the per-record `ToCborValue`/`FromCborValue` pair. The encoder lays keys in
/// canonical RFC 8949 order; the decoder reads by key in declaration order (order is
/// irrelevant on decode). Both methods are members of the generated `Codec` class.
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
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
            Some((pascal_ident(&wire), wire, e))
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
    for (prop, wire, entry) in &canonical {
        let wire_lit = csharp_escape(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // An absent optional is omitted from the map entirely (wire contract).
            let enc = csharp_enc_value(&entry.value_type, "csilV", records, aliases);
            out.push_str(&format!(
                "        if (value.{prop} is {{ }} csilV)\n        {{\n            csilEntries.Add((new CborValue.Text(\"{wire_lit}\"), {enc}));\n        }}\n"
            ));
        } else {
            let enc = csharp_enc_value(
                &entry.value_type,
                &format!("value.{prop}"),
                records,
                aliases,
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
    if !spec_has_records(input) {
        return None;
    }
    let records = record_names(input);
    let aliases = codec_aliases(input);
    let uses_timestamp = spec_uses_builtin(input, "timestamp");
    let uses_decimal = spec_uses_builtin(input, "decimal");

    let mut methods = String::new();
    let mut to_arms = String::new();
    let mut from_arms = String::new();
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            let pascal = pascal_ident(&rule.name);
            methods.push_str(&emit_record_codec(&rule.name, group, &records, &aliases));
            to_arms.push_str(&format!(
                "        {pascal} csilTyped => {pascal}ToCborValue(csilTyped),\n"
            ));
            from_arms.push_str(&format!(
                "        if (csilType == typeof({pascal})) return {pascal}FromCborValue(value);\n"
            ));
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
        body.push_str(&format!("            case \"{method}\":\n            {{\n"));
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
        body.push_str(&format!("        return (\"{method}\", data);\n"));
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
/// check emitters so each can guard a null optional with `if (X is { } value)`.
struct FieldRef {
    prop: String,
    optional: bool,
}

impl FieldRef {
    /// The expression a check reads. An optional field is unwrapped to the bound
    /// non-null `value` inside its guard; a required field reads the property directly.
    fn access(&self) -> &str {
        if self.optional { "value" } else { &self.prop }
    }

    /// Wrap a check, guarding it behind a null test when the field is optional.
    fn wrap(&self, cond: &str, message: &str) -> String {
        if self.optional {
            format!(
                "        if ({prop} is {{ }} value)\n        {{\n            if ({cond})\n            {{\n                throw new System.ArgumentException(\"{message}\");\n            }}\n        }}\n",
                prop = self.prop
            )
        } else {
            format!(
                "        if ({cond})\n        {{\n            throw new System.ArgumentException(\"{message}\");\n        }}\n"
            )
        }
    }
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
    body.push_str(&field.wrap(&cond, &message));
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
    body.push_str(&field.wrap(&cond, &message));
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
            body.push_str(&field.wrap(&cond, &message));
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
            body.push_str(&field.wrap(&cond, &message));
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
            body.push_str(&field.wrap(&cond, &message));
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            let bound = format!("System.DateTimeOffset.Parse(\"{}\")", csharp_escape(&text));
            let cond = format!("{access} {vop} {bound}");
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.wrap(&cond, &message));
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
            "float" => "double".to_string(),
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
            // CDDL's open `any`/`nil`/`null` are the untyped CBOR item — `object?` in C#.
            "any" | "nil" | "null" => "object?".to_string(),
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
            let field_type = map_csil_type_inner(&entry.value_type, config, qualify);
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
/// name with any trailing `Service` removed.
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
        var v = Dec(b, ref csilPos);
        if (csilPos != b.Length) { throw new CborException("trailing bytes"); }
        return v;
    }

    static ulong ReadArg(byte[] b, ref int csilPos, byte low)
    {
        if (low < 24) { csilPos += 1; return low; }
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

    static CborValue Dec(byte[] b, ref int csilPos)
    {
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
                var n = (int)arg;
                var slice = new byte[n];
                System.Array.Copy(b, csilPos, slice, 0, n);
                csilPos += n;
                return new CborValue.Bytes(slice);
            }
            case 3:
            {
                var n = (int)arg;
                var s = System.Text.Encoding.UTF8.GetString(b, csilPos, n);
                csilPos += n;
                return new CborValue.Text(s);
            }
            case 4:
            {
                var n = (int)arg;
                var items = new System.Collections.Generic.List<CborValue>(n);
                for (int csilI = 0; csilI < n; csilI++) { items.Add(Dec(b, ref csilPos)); }
                return new CborValue.Array(items);
            }
            case 5:
            {
                var n = (int)arg;
                var kvs = new System.Collections.Generic.List<(CborValue, CborValue)>(n);
                for (int csilI = 0; csilI < n; csilI++)
                {
                    var k = Dec(b, ref csilPos);
                    var val = Dec(b, ref csilPos);
                    kvs.Add((k, val));
                }
                return new CborValue.Map(kvs);
            }
            case 6:
            {
                var inner = Dec(b, ref csilPos);
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
    fn any_maps_to_object() {
        // CDDL `any` is the open CBOR item; C# has no `Any` type, so it must be `object?`.
        let c = config();
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("any".to_string()), &c),
            "object?"
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

    /// Extract the body of the first ```csharp fenced block in a README.
    fn csharp_block(readme: &str) -> &str {
        let start = readme
            .find("```csharp\n")
            .map(|i| i + "```csharp\n".len())
            .expect("README has a ```csharp block");
        let rest = &readme[start..];
        let end = rest.find("\n```").expect("the ```csharp block is closed");
        &rest[..end]
    }

    #[test]
    fn package_readme_has_quickstart_carrier_and_example() {
        let mut input = pingpong_input("csharp-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        input
            .config
            .options
            .insert("package_name".to_string(), serde_json::json!("Acme.Ping"));
        let output = render(input).expect("generation ok");
        let readme = file_content(&output, "README.md");

        // Title + install one-liner for the language's package manager.
        assert!(readme.starts_with("# Acme.Ping\n"));
        assert!(readme.contains("dotnet add reference"));
        // The hybrid posture is stated: dependency-free, reusing the generated CBOR codec.
        assert!(readme.contains("no third-party dependency"));

        let block = csharp_block(readme);
        // The carrier: a type implementing the generated sync transport seam, building the
        // CSIL-RPC envelope and POSTing it.
        assert!(block.contains("public sealed class CsilRpcHttpTransport : ICsilTransport"));
        assert!(block.contains("byte[] Call(string service, string op, byte[] request)"));
        assert!(block.contains("\"/csil/v1/rpc\""));
        assert!(block.contains("application/cbor"));
        assert!(block.contains("_http.Send(msg)"));
        // It reuses the generated generic CBOR (no System.Formats.Cbor) for the envelope,
        // wrapping the payload in tag 24.
        assert!(block.contains("Cbor.Encode(envelope)"));
        assert!(block.contains("new CborValue.Tag(TagEncodedCbor, new CborValue.Bytes(request))"));
        assert!(!block.contains("using System.Formats.Cbor"));
        // status / ServiceError handling.
        assert!(block.contains("if (status != 0)"));
        assert!(block.contains("EnvText(env, \"variant\") == \"ServiceError\""));
        assert!(block.contains("Codec.Decode<ServiceError>(inner)"));
        // Client construction over the carrier + the one example call with a sample literal.
        assert!(block.contains("new PingClient(new CsilRpcHttpTransport("));
        assert!(block.contains("client.Ping(new PingRequest { Message = \"example\" })"));
    }

    #[test]
    fn readme_emitted_only_in_package_mode() {
        let plain = render(corndogs_input("csharp-client")).expect("generation ok");
        assert!(!plain.files.iter().any(|f| f.path == "README.md"));
        let packaged = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!(["csharp"]))],
        ))
        .expect("generation ok");
        assert!(packaged.files.iter().any(|f| f.path == "README.md"));
    }

    #[test]
    fn emit_readme_false_suppresses_only_the_readme() {
        // Default package mode: the README is present.
        let with_readme = render(input_with_options(
            "csharp-client",
            &[("emit_packages", serde_json::json!(["csharp"]))],
        ))
        .expect("generation ok");
        assert!(with_readme.files.iter().any(|f| f.path == "README.md"));

        // Only an explicit `emit_readme: false` drops it; the `.csproj` stays.
        let without_readme = render(input_with_options(
            "csharp-client",
            &[
                ("emit_packages", serde_json::json!(["csharp"])),
                ("emit_readme", serde_json::json!(false)),
            ],
        ))
        .expect("generation ok");
        assert!(!without_readme.files.iter().any(|f| f.path == "README.md"));
        assert!(
            without_readme
                .files
                .iter()
                .any(|f| f.path.ends_with(".csproj"))
        );
    }

    #[test]
    fn server_package_readme_has_no_carrier() {
        // The carrier implements the client-only sync seam; a server package must not emit
        // it, falling back to the types/codec section instead.
        let output = render(input_with_options(
            "csharp",
            &[("emit_packages", serde_json::json!(["csharp"]))],
        ))
        .expect("generation ok");
        let readme = file_content(&output, "README.md");
        assert!(!readme.contains("ICsilTransport"));
        assert!(readme.contains("Codec.Decode<YourType>"));
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
        // Canonical wire strings: service lowercased, op PascalCased; round-trip through
        // the generated codec rather than a host-supplied serializer.
        assert!(client.contains(
            "public Task SubmitTask(SubmitTaskRequest submitTaskRequest) =>\n        Codec.Decode<Task>(transport.Call(\"corndogs\", \"SubmitTask\", Codec.Encode(submitTaskRequest)));"
        ));
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
            "public async System.Threading.Tasks.Task<Task> SubmitTaskAsync(SubmitTaskRequest submitTaskRequest) =>\n        Codec.Decode<Task>(await transport.Call(\"corndogs\", \"SubmitTask\", Codec.Encode(submitTaskRequest)));"
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
            "public async System.Threading.Tasks.Task<Task> SubmitTask(SubmitTaskRequest submitTaskRequest) =>\n        Codec.Decode<Task>(await transport.Call(\"corndogs\", \"SubmitTask\", Codec.Encode(submitTaskRequest)));"
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
        // An absent optional is omitted from the map, present-with-null on decode.
        assert!(codec.contains("if (value.Priority is { } csilV)"));
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

    /// Compile the README's Quickstart carrier against the real generated ping/pong
    /// package and round-trip a request through it with NO cross-process socket: a custom
    /// HttpMessageHandler echoes the tag-24 inner payload in-process, so the typed client's
    /// field must survive the envelope build/parse the carrier performs. Skips cleanly when
    /// no dotnet toolchain is on PATH; the first restore/build can be slow.
    #[test]
    fn readme_carrier_compiles_and_round_trips_through_dotnet() {
        let probe = std::process::Command::new("dotnet")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no dotnet toolchain on PATH");
            return;
        }

        // Generate the ping/pong package (so the README is emitted), then extract its
        // carrier. Ping/pong shares the `message` field across request and response so the
        // echo of the request bytes decodes cleanly as the response.
        let mut input = pingpong_input("csharp-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["csharp"]));
        let output = render(input).expect("generation ok");
        let readme = file_content(&output, "README.md").to_string();
        let carrier = csharp_block(&readme).to_string();

        let dir =
            std::env::temp_dir().join(format!("csilgen-csharp-readme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Write every generated source except the library `.csproj` (a single Exe project
        // owns the dir for `dotnet run`) and the README (markdown, not compiled).
        for file in &output.files {
            if file.path.ends_with(".csproj") || file.path == "README.md" {
                continue;
            }
            std::fs::write(dir.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(dir.join("Carrier.cs"), &carrier).unwrap();
        std::fs::write(dir.join("Program.cs"), CSHARP_README_ECHO_DRIVER).unwrap();
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

    /// The driver for the README round-trip: an in-process `HttpMessageHandler` that decodes
    /// the CSIL-RPC request the carrier built and echoes its tag-24 inner payload back in a
    /// `status:0` response, then calls the typed client over the README carrier. The
    /// ping/pong `message` field round-trips through the real carrier + codec with no socket.
    const CSHARP_README_ECHO_DRIVER: &str = r#"using System.Net;
using System.Net.Http;
using Csilgen.Transport;

internal sealed class EchoHandler : HttpMessageHandler
{
    protected override HttpResponseMessage Send(HttpRequestMessage request, System.Threading.CancellationToken ct)
    {
        byte[] reqBytes = request.Content!.ReadAsByteArrayAsync().GetAwaiter().GetResult();
        var reqEnv = (CborValue.Map)Cbor.Decode(reqBytes);
        byte[] inner = Inner(reqEnv);
        var respEnv = new CborValue.Map(new (CborValue, CborValue)[]
        {
            (new CborValue.Text("v"), new CborValue.Uint(1)),
            (new CborValue.Text("status"), new CborValue.Uint(0)),
            (new CborValue.Text("payload"), new CborValue.Tag(24, new CborValue.Bytes(inner))),
        });
        return new HttpResponseMessage(HttpStatusCode.OK)
        {
            Content = new ByteArrayContent(Cbor.Encode(respEnv)),
        };
    }

    // SendAsync is abstract on HttpMessageHandler; the carrier only calls the sync Send,
    // but the override is required to compile.
    protected override System.Threading.Tasks.Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, System.Threading.CancellationToken ct) =>
        System.Threading.Tasks.Task.FromResult(Send(request, ct));

    private static byte[] Inner(CborValue.Map map)
    {
        foreach (var (k, v) in map.Entries)
            if (k is CborValue.Text { Value: "payload" } && v is CborValue.Tag { Inner: CborValue.Bytes b })
                return b.Value;
        throw new System.Exception("no tag-24 payload in request envelope");
    }
}

internal static class Program
{
    private static void Main()
    {
        var transport = new CsilRpcHttpTransport("http://csil.invalid", new EchoHandler());
        var client = new PingClient(transport);
        var resp = client.Ping(new PingRequest { Message = "hello" });
        if (resp.Message != "hello")
        {
            System.Console.WriteLine("FAIL: " + resp.Message);
            System.Environment.Exit(1);
        }
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
        if (service != "corndogs" || op != "SubmitTask")
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
        if (service != "corndogs" || op != "SubmitTask")
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
}
