//! Kotlin (JVM) code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target kotlin` from `csilgen_kotlin_generator.wasm`.
//! Emits idiomatic Kotlin source: `data class` records, `sealed interface` choices,
//! typed client call-sites, server handler interfaces, and verbose/compact routers.
//! It never emits wire bytes — the transport library owns the wire. Structure mirrors
//! `wasm/csilgen-go-generator`; feature coverage mirrors `wasm/csilgen-python-generator`.

use csilgen_common::{
    ChoiceClass, CsilControlOperator, CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression,
    CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition,
    CsilServiceDirection, CsilServiceOperation, CsilSizeConstraint, CsilTypeExpression,
    CsilValidationConstraint, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, HoistOptions, WarningLevel, WasmGeneratorInput,
    WasmGeneratorOutput, choice_arm_literal, hoist_inline_composites, wasm_interface::*,
};
use std::collections::HashMap;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "kotlin-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Kotlin (JVM) code generator with service support".to_string(),
        target: "kotlin".to_string(),
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
    serialize_and_return_ptr(&metadata)
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
    let result = match deserialize_input(input_ptr, input_len) {
        Ok(input) => process_generation(input),
        Err(error_code) => return create_error_result(error_code),
    };
    match result {
        Ok(output) => serialize_and_return_ptr(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn deserialize_input(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorInput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

/// Which surface a (sub-)target selects, parallel to the Go generator's dispatch.
#[derive(Clone, Copy)]
enum Surface {
    Server,
    Client,
    TypesOnly,
}

/// Which client surface(s) to emit. Only the transport seam (the byte carrier that
/// performs the network round-trip) turns into a `suspend fun`; the generated codec
/// never does I/O and stays synchronous. `Both` is the default so every consumer keeps
/// their blocking client and gains a coroutine-friendly twin for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStyle {
    /// Blocking-only client at `Client.kt`. The host owns its own threads.
    Sync,
    /// Suspending client, a drop-in at `Client.kt` with the canonical symbol names
    /// (just `suspend`). For hosts whose carrier is a coroutine.
    Async,
    /// Emit both — the blocking client at `Client.kt` plus a suspending twin at
    /// `ClientAsync.kt` whose symbols carry an `Async` marker so the two coexist in
    /// one package without name collisions. Default.
    Both,
}

/// Read & validate `client_style` from the generation options. Any value other than
/// `sync`/`async`/`both` is rejected so misconfiguration fails at generation time
/// instead of silently degrading; absent defaults to `Both`. Returns a message that
/// names the offending option, mirroring how a typed-enum option is validated.
fn client_style(options: &HashMap<String, serde_json::Value>) -> Result<ClientStyle, String> {
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

/// The shape of one emitted client file: whether its methods/seam suspend and the
/// symbol marker that keeps a suspending twin distinct from the blocking client when
/// both land in the same package. `marker` is empty for a stand-alone client (sync, or
/// async-as-drop-in) and `"Async"` for the twin in `Both` mode.
#[derive(Debug, Clone, Copy)]
struct ClientShape {
    is_async: bool,
    marker: &'static str,
}

impl ClientShape {
    /// `suspend ` keyword (trailing space) for method/seam declarations, else empty.
    /// Kotlin coroutines need no `await` at the call site, so this is the only token
    /// that turns the client suspending.
    fn suspend_kw(&self) -> &'static str {
        if self.is_async { "suspend " } else { "" }
    }

    /// The byte-transport interface name (`Transport`, or `AsyncTransport` for the twin).
    fn transport_name(&self) -> String {
        format!("{}Transport", self.marker)
    }

    /// A per-service client class name (`FooClient`, or `FooAsyncClient` for the twin).
    fn client_name(&self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }
}

fn process_generation(mut input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    // Lift every inline (anonymous) group/choice to a synthesized named rule up front,
    // so all downstream generation (types, codec, client, services) sees one uniform
    // spec in which those shapes are ordinary named references with real codecs.
    // Kotlin records and choices are nominal (`map_csil_type_to_kotlin`/`kotlin_enc_
    // value`/`kotlin_dec_value` can only route a field through a *named* rule or the
    // opaque `Any`/`CborValue.CNull` fallback), so an ALL-literal choice is hoisted
    // too (`hoist_all_literal_choices: true`) — unlike TypeScript/Go, Kotlin has no
    // bare-inline-enum rendering to fall back to; every choice needs a name.
    input.csil_spec = hoist_inline_composites(
        &input.csil_spec,
        HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
    let config = KotlinConfig::from_options(&input.config.options);
    // Validate `client_style` early so a bad value fails the whole run regardless of the
    // requested surface, mirroring the TypeScript generator's option validation.
    let style = client_style(&input.config.options).map_err(|_| error_codes::GENERATION_ERROR)?;
    let mut warnings = Vec::new();
    let mut files = Vec::new();

    let surface = match input.config.target.as_str() {
        "kotlin" | "kotlin-server" => Surface::Server,
        "kotlin-client" => Surface::Client,
        "kotlin-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let dir = config.package.replace('.', "/");
    // In package mode the sources move under Gradle's conventional `src/main/kotlin`
    // source root so the output directory is a buildable project; otherwise they keep the
    // default flat package-path layout unchanged.
    let source_root = if config.emit_package {
        format!("src/main/kotlin/{dir}")
    } else {
        dir
    };
    let make_path = |filename: &str| -> String { format!("{source_root}/{filename}") };

    if let Some(types_content) = generate_types(&input, &config, &mut warnings) {
        files.push(GeneratedFile {
            path: make_path("Types.kt"),
            content: types_content,
        });
    }

    // The per-record CBOR codec lets a record cross the wire without a hand-written
    // serializer; the typed client encodes/decodes through it. Emitted for every
    // surface (a types-only consumer still needs to (de)serialize), whenever the spec
    // declares record types.
    if let Some(codec_content) = generate_codec(&input, &config) {
        files.push(GeneratedFile {
            path: make_path("Codec.kt"),
            content: codec_content,
        });
    }

    if let Some(validation_content) = generate_validation(&input, &config) {
        files.push(GeneratedFile {
            path: make_path("Validation.kt"),
            content: validation_content,
        });
    }

    if input.csil_spec.service_count > 0 {
        // A package's `genquickstart.md` demonstrates the calling side (the CSIL-RPC and
        // CSIL-Datagrams sections, over the typed client) AND the handling side (the
        // CSIL-Events section, over the channel router + handler interface), so a package
        // must carry BOTH surfaces for its own quickstart to compile — regardless of which
        // (sub-)target was requested. A flat (non-package) build stays byte-identical: it
        // emits only the requested surface. Mirrors the OCaml generator.
        let want_client = matches!(surface, Surface::Client)
            || (config.emit_package && !matches!(surface, Surface::TypesOnly));
        let want_server = matches!(surface, Surface::Server)
            || (config.emit_package && !matches!(surface, Surface::TypesOnly));

        if want_client {
            // `Both` (the default) ships the blocking client at `Client.kt` and a
            // suspending twin at `ClientAsync.kt`; `Async` makes the suspending client
            // a drop-in at `Client.kt` (canonical names); `Sync` is today's output.
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
                    if let Some(c) = generate_client(&input, &config, sync) {
                        files.push(GeneratedFile {
                            path: make_path("Client.kt"),
                            content: c,
                        });
                    }
                }
                ClientStyle::Async => {
                    if let Some(c) = generate_client(&input, &config, async_drop_in) {
                        files.push(GeneratedFile {
                            path: make_path("Client.kt"),
                            content: c,
                        });
                    }
                }
                ClientStyle::Both => {
                    if let Some(c) = generate_client(&input, &config, sync) {
                        files.push(GeneratedFile {
                            path: make_path("Client.kt"),
                            content: c,
                        });
                    }
                    if let Some(c) = generate_client(&input, &config, async_twin) {
                        files.push(GeneratedFile {
                            path: make_path("ClientAsync.kt"),
                            content: c,
                        });
                    }
                }
            }
        }

        if want_server && let Some(services_content) = generate_services(&input, &config) {
            files.push(GeneratedFile {
                path: make_path("Services.kt"),
                content: services_content,
            });
        }
    }

    // The Gradle manifest sits at the project root (not under the package path), so the
    // emitted directory is itself a publishable project.
    if config.emit_package {
        files.push(GeneratedFile {
            path: "build.gradle.kts".to_string(),
            content: gradle_build_kts(&config),
        });
        files.push(GeneratedFile {
            path: "settings.gradle.kts".to_string(),
            content: gradle_settings_kts(&config),
        });
        // Only an explicit `emit_readme: false` suppresses the README; absent or non-bool
        // leaves the publishable package's Quickstart in place.
        if input
            .config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            // Named `genquickstart.md` rather than `README.md` so it never collides with a
            // consumer's own hand-written `README.md`; the consumer supplies that themselves.
            files.push(GeneratedFile {
                path: "genquickstart.md".to_string(),
                content: generate_readme(&input, &config),
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

#[derive(Debug)]
struct KotlinConfig {
    package: String,
    package_description: String,
    // When true, the output directory becomes a self-contained, publishable Gradle
    // (Kotlin/JVM) project: a build script + settings are emitted and the sources move
    // under Gradle's conventional source root. Driven by `emit_packages` containing
    // `"kotlin"`, so the same flag can fan a single generate across many language packages.
    emit_package: bool,
    package_name: String,
    package_version: String,
}

impl KotlinConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Self {
        let package = options
            .get("kotlin_package")
            .and_then(|v| v.as_str())
            .unwrap_or("community.catalyst.csilgen.generated")
            .to_string();
        let package_description = options
            .get("package_description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Parse defensively: a missing key, a non-array value, or an array without the
        // `"kotlin"` string all leave package mode off, so unrelated targets in the same
        // `emit_packages` list never accidentally turn it on.
        let emit_package = options
            .get("emit_packages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|e| e.as_str() == Some("kotlin")))
            .unwrap_or(false);
        let package_name = options
            .get("package_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            // A path-style `package_name` is the cross-ecosystem source of truth; the
            // artifact name wants only its tail. See `package_name_last_segment`.
            .map(|name| csilgen_common::package_name_last_segment(name).to_string())
            .unwrap_or_else(|| derive_package_name(&package));
        let package_version = options
            .get("package_version")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("0.1.0")
            .to_string();
        Self {
            package,
            package_description,
            emit_package,
            package_name,
            package_version,
        }
    }
}

/// The artifact/root-project name when `package_name` is unset: the last dotted segment
/// of the Kotlin package (`com.example.api` → `api`), since that segment is the most
/// specific human-facing name already present in the coordinates.
fn derive_package_name(package: &str) -> String {
    package
        .rsplit('.')
        .find(|s| !s.is_empty())
        .unwrap_or("generated")
        .to_string()
}

/// The Kotlin Gradle plugin version pinned into a generated package's build script — a
/// recent stable release so a freshly emitted project builds without further edits.
const KOTLIN_GRADLE_PLUGIN_VERSION: &str = "2.0.21";

/// The `build.gradle.kts` for a self-contained package: the Kotlin/JVM + `maven-publish`
/// plugins, the CSIL coordinates (group/version), a pinned JVM toolchain, and a publish
/// block. The generated codec runtime carries no third-party libraries, but the
/// `kotlin("jvm")` plugin always contributes a `kotlin-stdlib` dependency, so a repository
/// to resolve it is still required — without one Gradle fails with "no repositories are
/// defined" before it ever compiles a source file.
fn gradle_build_kts(config: &KotlinConfig) -> String {
    let group = kotlin_escape(&config.package);
    let version = kotlin_escape(&config.package_version);
    let mut out = String::new();
    out.push_str("// Code generated by csilgen; DO NOT EDIT.\n\n");
    out.push_str("plugins {\n");
    out.push_str(&format!(
        "    kotlin(\"jvm\") version \"{KOTLIN_GRADLE_PLUGIN_VERSION}\"\n"
    ));
    out.push_str("    `maven-publish`\n");
    out.push_str("}\n\n");
    out.push_str(&format!("group = \"{group}\"\n"));
    out.push_str(&format!("version = \"{version}\"\n\n"));
    out.push_str("repositories {\n    mavenCentral()\n}\n\n");
    // A toolchain pins the JVM target so the published artifact is reproducible regardless
    // of the local JDK; 17 is the current LTS baseline.
    out.push_str("kotlin {\n    jvmToolchain(17)\n}\n\n");
    out.push_str("publishing {\n");
    out.push_str("    publications {\n");
    out.push_str("        create<MavenPublication>(\"maven\") {\n");
    out.push_str("            from(components[\"java\"])\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// The `settings.gradle.kts` for a self-contained package: it names the root project after
/// the artifact, which is what Gradle publishes as the artifact id.
fn gradle_settings_kts(config: &KotlinConfig) -> String {
    let name = kotlin_escape(&config.package_name);
    format!("// Code generated by csilgen; DO NOT EDIT.\n\nrootProject.name = \"{name}\"\n")
}

/// The version of the `csilgen-transport` (Kotlin/JVM) reference library a generated
/// package's Quickstart depends on. Pinned here so the Install block names a concrete
/// coordinate the consumer can resolve.
const TRANSPORT_LIB_COORD: &str = "community.catalyst.csilgen:csilgen-transport:0.1.0";

/// Which transport sections the `genquickstart.md` should carry. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or empty
/// value means "all three". The CLI sets this from its `--readme-csil-*` flags.
fn wanted_transports(input: &WasmGeneratorInput) -> (bool, bool, bool) {
    let listed = match input.config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
            // An array naming none of the known transports (all unknown, or empty) falls
            // back to all three rather than rendering an empty document.
            let any_known = ["rpc", "events", "datagrams"]
                .iter()
                .any(|t| names.contains(t));
            if any_known {
                Some((
                    names.contains("rpc"),
                    names.contains("events"),
                    names.contains("datagrams"),
                ))
            } else {
                None
            }
        }
        _ => None,
    };
    listed.unwrap_or((true, true, true))
}

/// The package `genquickstart.md`: a transport-by-transport Quickstart built on the
/// official `csilgen-transport` library. The generated codec owns CBOR (de)serialization;
/// the library owns the envelope, framing, and connection lifecycle; the consumer supplies
/// only a *carrier* that moves bytes. Each requested section (CSIL-RPC over HTTP,
/// CSIL-Events over TLS, CSIL-Datagrams over UDP) is a complete example built on the
/// library, so the same typed surface rides HTTP/TLS/WebSocket/QUIC/UDP unchanged.
fn generate_readme(input: &WasmGeneratorInput, config: &KotlinConfig) -> String {
    let artifact = &config.package_name;
    let mut out = format!(
        "# {artifact}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the `csilgen-transport` library owns the envelope, framing,\n\
         and connection lifecycle. You supply only a *carrier* that moves bytes, so the same\n\
         typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw UDP unchanged.\n\n\
         ## Install\n\n\
         This package builds to a standard Gradle (Kotlin/JVM) artifact. Publish it to your\n\
         local Maven repository with `./gradlew publishToMavenLocal` — TODO: publish it to a\n\
         shared repository — then depend on it alongside the transport library:\n\n\
         ```kotlin\n\
         dependencies {{\n\
         \x20   implementation(\"{}:{artifact}:{}\")\n\
         \x20   implementation(\"{TRANSPORT_LIB_COORD}\")\n\
         }}\n\
         ```\n\n",
        config.package, config.package_version,
    );

    let (rpc, events, datagrams) = wanted_transports(input);
    let unary = first_unary_example(input);
    let channel = first_channel_example(input);
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

/// CSIL-RPC over HTTP: a carrier implementing the generated `Transport` byte seam that
/// builds the envelope with the library's `RpcRequest` and parses the library's
/// `RpcResponse` (never hand-rolled), POSTing to `{baseUrl}/csil/v1/rpc` with the JDK's
/// blocking `HttpClient`. A non-zero transport status (`asTransportError`) and the typed
/// `ServiceError` application arm are surfaced distinctly; the typed client decodes success.
fn rpc_section(config: &KotlinConfig, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`); you\n\
         bring a carrier that moves bytes. The HTTP carrier below is just one example — swap\n\
         `HttpClient` for any client (it implements the generated `Transport` byte seam).\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```kotlin\n");
    out.push_str(&format!("package {}\n\n", config.package));
    out.push_str(
        "import community.catalyst.csilgen.transport.RpcRequest\n\
         import community.catalyst.csilgen.transport.RpcResponse\n\
         import java.net.URI\n\
         import java.net.http.HttpClient\n\
         import java.net.http.HttpRequest\n\
         import java.net.http.HttpResponse\n\n",
    );
    out.push_str(RPC_CARRIER_KT);
    out.push('\n');
    out.push_str("fun main() {\n");
    out.push_str(&format!(
        "    val client = {}(HttpRpcTransport(\"http://localhost:5080\"))\n",
        ex.client_class
    ));
    if ex.has_request {
        out.push_str(&format!(
            "    val resp = client.{}({})\n",
            ex.method, ex.sample
        ));
    } else {
        out.push_str(&format!("    val resp = client.{}()\n", ex.method));
    }
    out.push_str("    println(resp)\n}\n");
    out.push_str("```\n\n");
    out
}

/// The HTTP carrier body — spec-independent, so a constant. It encodes the request with the
/// library's `RpcRequest`, POSTs it to `{baseUrl}/csil/v1/rpc`, and returns the success
/// payload bytes the typed client decodes. `RpcResponse.asTransportError()` raises on a
/// non-zero transport status; the typed `ServiceError` arm (a status-0 variant) is surfaced
/// separately so the typed client only ever decodes a success payload.
const RPC_CARRIER_KT: &str = r#"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the RpcRequest/
// RpcResponse envelope; the carrier owns only the transport. Swap HttpClient for any client.
class HttpRpcTransport(baseUrl: String) : Transport {
    private val http: HttpClient = HttpClient.newHttpClient()

    // Trim any trailing slash so the joined path is exactly one "/csil/v1/rpc".
    private val baseUrl: String = baseUrl.trimEnd('/')

    override fun call(service: String, op: String, request: ByteArray): ByteArray {
        // The library builds the envelope from the already-encoded request bytes; we never
        // hand-roll the wire form.
        val envelope = RpcRequest(service, op, request).encode()
        val httpReq = HttpRequest.newBuilder()
            .uri(URI.create("$baseUrl/csil/v1/rpc"))
            .header("Content-Type", "application/cbor")
            .header("Accept", "application/cbor")
            .POST(HttpRequest.BodyPublishers.ofByteArray(envelope))
            .build()
        val httpResp = http.send(httpReq, HttpResponse.BodyHandlers.ofByteArray())
        if (httpResp.statusCode() != 200) {
            throw ClientError(message = "csil-rpc $service/$op: http ${httpResp.statusCode()}")
        }

        val resp = RpcResponse.decode(httpResp.body())
        // A non-zero transport status is a StatusException, distinct from an application error.
        resp.asTransportError()?.let { throw ClientError(message = "csil-rpc $service/$op: ${it.message}") }
        // A typed ServiceError arm rides as a status-0 variant; surface it so the typed
        // client decodes a success payload only.
        if (resp.variant == "ServiceError") {
            throw ClientError(message = "csil-rpc $service/$op: ServiceError")
        }
        return resp.payload
    }
}
"#;

/// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as the
/// library's `StreamCarrier` (CSIL length-prefix framing), performs the
/// `$hello`/`$hello-ack` handshake, sends one outbound event via the generated
/// `encode<Service><Op>`, and runs a recv loop that decodes each frame to an `Event`,
/// answers `$ping` with `$pong`, and dispatches typed events to the generated
/// `route<Service>Channel`. When the spec has no usable channel op the dispatch wiring is
/// replaced with a note (the handshake + heartbeat still apply to any connection).
fn events_section(config: &KotlinConfig, ch: Option<&ChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns\n\
         the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and framing; the\n\
         generated router dispatches typed events. The TLS carrier below is just one example —\n\
         a WebSocket/WebTransport/QUIC carrier drops in unchanged.\n\n",
    );
    out.push_str("```kotlin\n");
    out.push_str(&format!("package {}\n\n", config.package));
    out.push_str(
        "import community.catalyst.csilgen.transport.Control\n\
         import community.catalyst.csilgen.transport.Event\n\
         import community.catalyst.csilgen.transport.FrameCarrier\n\
         import community.catalyst.csilgen.transport.Heartbeat\n\
         import community.catalyst.csilgen.transport.Hello\n\
         import community.catalyst.csilgen.transport.HelloAck\n\
         import community.catalyst.csilgen.transport.Profile\n\
         import community.catalyst.csilgen.transport.MAX_FRAME_DEFAULT\n\
         import community.catalyst.csilgen.transport.StreamCarrier\n\
         import javax.net.ssl.SSLSocketFactory\n\n",
    );
    out.push_str(EVENTS_CARRIER_KT);
    out.push('\n');
    match ch {
        Some(ch) => out.push_str(&events_session(config, ch)),
        None => out.push_str(EVENTS_NO_CHANNEL_SESSION_KT),
    }
    out.push_str("```\n\n");
    out
}

/// The TLS `StreamCarrier` adapter — spec-independent. The library's `StreamCarrier` owns
/// the 4-byte length-prefix framing over the socket's streams, so the session logic stays
/// transport-agnostic.
const EVENTS_CARRIER_KT: &str = r#"// One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix. The
// library's StreamCarrier owns the framing; we own only the socket.

// The max-frame guard is a carrier setting, not a generated constant: raise it when a peer
// accepts payloads larger than the 16 MiB default (the envelope adds framing and request
// metadata around the payload, so the limit must exceed the largest payload), or lower it
// to harden an exposed listener. Valid limits are 1..MAX_FRAME_LIMIT and are checked at
// construction.
const val MAX_FRAME: Int = MAX_FRAME_DEFAULT

fun openTlsCarrier(host: String, port: Int): FrameCarrier {
    val socket = SSLSocketFactory.getDefault().createSocket(host, port)
    return StreamCarrier(socket.getInputStream(), socket.getOutputStream(), MAX_FRAME)
}
"#;

/// The channel session body for an Events connection that has a record-typed `<->` op:
/// a `Codec` backed by the generated per-type helpers, the handshake, one outbound event
/// via the generated encoder, and the recv loop that heartbeats and dispatches into the
/// generated router. `handlers` is a parameter (the host's `<Service>` implementation) so
/// the snippet need not stub every operation inline.
fn events_session(config: &KotlinConfig, ch: &ChannelExample) -> String {
    format!(
        r#"
// Back the generated router's Codec with this package's per-type CBOR helpers (inbound
// {inbound}, outbound {outbound}).
private val channelCodec = object : Codec {{
    override fun encode(value: Any?): ByteArray = {pkg}.encode(value)
    override fun <T> decode(data: ByteArray, type: Class<T>): T =
        csilFromCborValue(type.kotlin, CsilCbor.decode(data)) as T
}}

// `handlers` is your {iface} implementation; the generated router dispatches typed events
// into it.
fun session(handlers: {iface}) {{
    val carrier = openTlsCarrier("localhost", 7443)

    // $hello / $hello-ack handshake. The peer's $hello-ack pins the wire profile for the
    // connection's lifetime.
    carrier.sendFrame(Hello(listOf(1uL), listOf("verbose"), "{service}").encode())
    val ackFrame = carrier.recvFrame() ?: throw ClientError(message = "connection closed during handshake")
    val profile = Profile.parse(HelloAck.decode(ackFrame).profile) ?: Profile.VERBOSE

    // Send one outbound event via the generated encoder, framed under the negotiated profile.
    val (event, bytes) = {encode}(channelCodec, {sample})
    carrier.sendFrame(Event.verbose("{service}", event, bytes).encode(profile))

    // Recv loop: decode each frame to an Event, answer $ping with $pong, dispatch the rest
    // to the generated router.
    var frame = carrier.recvFrame()
    while (frame != null) {{
        val ev = Event.decode(frame, profile)
        if (ev.event == Control.PING_NAME) {{
            val ping = Heartbeat.decode(ev.payload)
            carrier.sendFrame(
                Event.verbose("{service}", Control.PONG_NAME, Heartbeat(ping.nonce).encode()).encode(profile),
            )
        }} else {{
            {route}(handlers, channelCodec, ev.event!!, ev.payload)
        }}
        frame = carrier.recvFrame()
    }}
}}
"#,
        pkg = config.package,
        iface = ch.iface,
        service = ch.service_wire,
        inbound = ch.inbound_type,
        outbound = ch.outbound_type,
        encode = ch.encode_fn,
        route = ch.route_fn,
        sample = ch.outbound_sample,
    )
}

/// The Events session body when the spec declares no usable channel op: the handshake and
/// heartbeat still apply to any connection, so they are shown, with a note where the
/// generated channel dispatch would otherwise wire in.
const EVENTS_NO_CHANNEL_SESSION_KT: &str = r#"
fun session() {
    val carrier = openTlsCarrier("localhost", 7443)

    // $hello / $hello-ack handshake (control plane).
    carrier.sendFrame(Hello(listOf(1uL), listOf("verbose")).encode())
    val ackFrame = carrier.recvFrame() ?: error("connection closed during handshake")
    val profile = Profile.parse(HelloAck.decode(ackFrame).profile) ?: Profile.VERBOSE

    // Recv loop: answer $ping with $pong. This package declares no <->/<- operations, so
    // there is no generated channel router to dispatch typed events into.
    var frame = carrier.recvFrame()
    while (frame != null) {
        val ev = Event.decode(frame, profile)
        if (ev.event == Control.PING_NAME) {
            val ping = Heartbeat.decode(ev.payload)
            carrier.sendFrame(
                Event.verbose(null, Control.PONG_NAME, Heartbeat(ping.nonce).encode()).encode(profile),
            )
        }
        frame = carrier.recvFrame()
    }
}
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in the
/// library's `Datagram`, and `sendDatagram` it fire-and-forget. The recv path
/// `Datagram.decode`s an inbound datagram and decodes its payload with the generated codec
/// into the RESPONSE type — there is NO synchronous response.
fn datagrams_section(config: &KotlinConfig, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram` envelope;\n\
         you bring a datagram carrier. The UDP carrier below is one example — a WebRTC\n\
         unreliable DataChannel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no record `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_type), Some(res_type), Some(res_decoder)) =
        (&ex.req_type, &ex.res_type, &ex.res_decoder)
    else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```kotlin\n");
    out.push_str(&format!("package {}\n\n", config.package));
    out.push_str(
        "import community.catalyst.csilgen.transport.Datagram\n\
         import community.catalyst.csilgen.transport.DatagramCarrier\n\
         import java.net.DatagramPacket\n\
         import java.net.DatagramSocket\n\
         import java.net.InetSocketAddress\n\n",
    );
    out.push_str(DATAGRAMS_CARRIER_KT);
    out.push('\n');
    out.push_str(&format!(
        r#"// The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
const val OP_ORD: ULong = {op_ord}uL

fun main() {{
    val carrier = UdpDatagramCarrier("localhost", 9000)

    // Fire-and-forget: encode the `->` request and send it. seq 0 marks an unsequenced datagram.
    val req: {req_type} = {sample}
    carrier.sendDatagram(Datagram(OP_ORD, 0uL, req.toCbor()).encode())

    // Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is NO
    // synchronous response; the caller must tolerate loss and reordering and handle a reply
    // whenever (if ever) it shows up.
    val inbound = carrier.recvDatagram()
    if (inbound != null) {{
        val dg = Datagram.decode(inbound)
        val resp: {res_type} = {res_decoder}FromCbor(dg.payload)
        println("late response: $resp")
    }}
}}
"#,
        op_ord = ex.op_ord,
        req_type = req_type,
        res_type = res_type,
        res_decoder = res_decoder,
        sample = ex.sample,
    ));
    out.push_str("```\n\n");
    out
}

/// The UDP `DatagramCarrier` adapter — spec-independent. `sendDatagram` writes one UDP
/// packet; `recvDatagram` blocks for the next inbound packet. Datagrams are unreliable and
/// unordered, so the carrier never waits for or correlates a reply.
const DATAGRAMS_CARRIER_KT: &str = r#"// One example carrier: UDP via java.net.DatagramSocket. Datagrams are unreliable and
// unordered, so the carrier never correlates a reply.
class UdpDatagramCarrier(host: String, port: Int) : DatagramCarrier {
    private val socket = DatagramSocket()
    private val address = InetSocketAddress(host, port)

    override fun sendDatagram(bytes: ByteArray) {
        socket.send(DatagramPacket(bytes, bytes.size, address))
    }

    override fun recvDatagram(): ByteArray? {
        val buf = ByteArray(2048)
        val packet = DatagramPacket(buf, buf.size)
        socket.receive(packet)
        return packet.data.copyOf(packet.length)
    }
}
"#;

/// The pieces a unary (`->`) example call needs: the client class + method to call, a
/// compiling sample request literal, the request/response record type names and the
/// response decoder stem (so the datagram section can name `req.toCbor()`/
/// `<res>FromCbor`), and the op's datagram ordinal.
struct UnaryExample {
    client_class: String,
    method: String,
    has_request: bool,
    sample: String,
    req_type: Option<String>,
    res_type: Option<String>,
    res_decoder: Option<String>,
    op_ord: u64,
}

/// The first service (in rule order, matching the emitted client) that has a unary `->`
/// operation the typed client actually exposes — success and request both records (or a
/// null request) — reduced to an example call. `None` for a serviceless package.
fn first_unary_example(input: &WasmGeneratorInput) -> Option<UnaryExample> {
    let records = kotlin_record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(def) = &rule.rule_type else {
            continue;
        };
        for op in &def.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let null_input = op_input_is_null(&op.input_type);
            if !is_record_ref(&success, &records)
                || !(null_input || is_record_ref(&op.input_type, &records))
            {
                continue;
            }
            return Some(UnaryExample {
                client_class: format!("{}Client", service_base(&rule.name)),
                method: kotlin_method_name(&op.name),
                has_request: !null_input,
                sample: if null_input {
                    String::new()
                } else {
                    kotlin_sample(input, &op.input_type)
                },
                // The datagram section needs record type names; a null-input op leaves the
                // request type absent (and that section then shows its non-record note).
                req_type: (!null_input)
                    .then(|| reference_name(&op.input_type))
                    .flatten(),
                res_type: reference_name(&success),
                res_decoder: reference_name(&success).map(|_| camel_case(reference_raw(&success))),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.unwrap_or(1),
            });
        }
    }
    None
}

/// The pascal-cased type name a `Reference` names, or `None` for any other type
/// expression (so the datagram section only fires for record references).
fn reference_name(ty: &CsilTypeExpression) -> Option<String> {
    match ty {
        CsilTypeExpression::Reference(n) => Some(pascal_case(n)),
        _ => None,
    }
}

/// The raw (un-cased) name a `Reference` names; only valid when `reference_name` is `Some`.
fn reference_raw(ty: &CsilTypeExpression) -> &str {
    match ty {
        CsilTypeExpression::Reference(n) => n,
        _ => "",
    }
}

/// The pieces the Events session needs: the generated handler interface, channel router,
/// and outbound encoder names; the wire service; the inbound/outbound record type names;
/// and a compiling outbound record literal.
struct ChannelExample {
    iface: String,
    service_wire: String,
    route_fn: String,
    encode_fn: String,
    inbound_type: String,
    outbound_type: String,
    outbound_sample: String,
}

/// The first service (in rule order) with a `<->` op whose inbound (input) and outbound
/// (success output) are both record references, so the generated router + encoder + per-type
/// codec helpers all exist. `None` when no service has a usable channel op — the Events
/// section then shows the handshake/heartbeat without dispatch wiring.
fn first_channel_example(input: &WasmGeneratorInput) -> Option<ChannelExample> {
    let records = kotlin_record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(def) = &rule.rule_type else {
            continue;
        };
        for op in &def.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            if !is_record_ref(&op.input_type, &records) || !is_record_ref(&success, &records) {
                continue;
            }
            let iface = pascal_case(&rule.name);
            return Some(ChannelExample {
                iface: iface.clone(),
                service_wire: rule.name.clone(),
                route_fn: format!("route{iface}Channel"),
                encode_fn: format!("encode{iface}{}", pascal_case(&op.name)),
                inbound_type: pascal_case(reference_raw(&op.input_type)),
                outbound_type: pascal_case(reference_raw(&success)),
                outbound_sample: kotlin_sample(input, &success),
            });
        }
    }
    None
}

/// A compiling Kotlin expression producing a sample value of `ty` for the README example.
/// Records recurse into their constructor (only required, non-defaulted fields, by name);
/// scalars get a representative literal; maps/lists use the empty factories; shapes a
/// generic sample can't fabricate fall back to `TODO()`, which type-checks anywhere.
fn kotlin_sample(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "ByteArray(0)".to_string(),
            "int" => "0L".to_string(),
            "uint" => "0uL".to_string(),
            "float" => "0.0".to_string(),
            "timestamp" => "java.time.Instant.now()".to_string(),
            "decimal" => "java.math.BigDecimal.ZERO".to_string(),
            _ => "TODO()".to_string(),
        },
        CsilTypeExpression::Array { .. } => "emptyList()".to_string(),
        CsilTypeExpression::Map { .. } => "emptyMap()".to_string(),
        CsilTypeExpression::Constrained { base_type, .. } => kotlin_sample(input, base_type),
        CsilTypeExpression::Reference(name) => match find_record(input, name) {
            Some(group) => record_literal(input, name, group),
            // A transparent alias is a `typealias` to its target, so a value of the alias is
            // just a value of the underlying type.
            None => match find_alias(input, name) {
                Some(underlying) => kotlin_sample(input, &underlying),
                None => "TODO()".to_string(),
            },
        },
        _ => "TODO()".to_string(),
    }
}

/// `Class(field = arg, ...)` over a record's constructor: every required field (no
/// optional / `@default`, which the data class already defaults) by name, in declared
/// order, with a typed sample value.
fn record_literal(input: &WasmGeneratorInput, name: &str, group: &CsilGroupExpression) -> String {
    let args: Vec<String> = group
        .entries
        .iter()
        .filter(|e| e.key.is_some() && field_default(e, input).is_none())
        .map(|e| {
            let key = e.key.as_ref().unwrap();
            let prop = kotlin_prop_name(key, &e.metadata);
            format!("{prop} = {}", kotlin_sample(input, &e.value_type))
        })
        .collect();
    format!("{}({})", pascal_case(name), args.join(", "))
}

/// The record group a reference names, if any: a `Name = { ... }` rule (`TypeDef(Group)`)
/// or a bare group rule (`GroupDef`).
fn find_record<'a>(input: &'a WasmGeneratorInput, name: &str) -> Option<&'a CsilGroupExpression> {
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

/// The choice arms a reference names, if that name is a `Name = A / B / …` rule —
/// the lookup that lets an enum-typed field resolve its `.default` to an enum constant.
fn find_choice<'a>(input: &'a WasmGeneratorInput, name: &str) -> Option<&'a [CsilTypeExpression]> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => Some(choices.as_slice()),
            _ => None,
        }
    })
}

/// The raw (un-cased) name a field's type expression ultimately references, seeing
/// through a `.default`/constraint wrapper (which a defaulted enum field carries).
fn referenced_type_name(type_expr: &CsilTypeExpression) -> Option<&str> {
    match type_expr {
        CsilTypeExpression::Reference(name) => Some(name),
        CsilTypeExpression::Constrained { base_type, .. } => referenced_type_name(base_type),
        _ => None,
    }
}

/// An enum-typed field's `.default` rendered as the enum constant (`Status.Active`),
/// not the raw wire literal — the property's declared type is the enum, so a bare
/// `"active"` / `1` would not typecheck. `None` when the field is not enum-typed.
fn enum_default_kotlin(
    value: &CsilLiteralValue,
    value_type: &CsilTypeExpression,
    input: &WasmGeneratorInput,
) -> Option<String> {
    let name = referenced_type_name(value_type)?;
    let choices = find_choice(input, name)?;
    let iface = pascal_case(name);
    match classify_choice(choices) {
        ChoiceKind::EnumText => match value {
            CsilLiteralValue::Text(s) => Some(format!("{iface}.{}", enum_text_variant(s))),
            _ => None,
        },
        ChoiceKind::EnumInt => match value {
            CsilLiteralValue::Integer(n) => Some(format!("{iface}.{}", enum_int_variant(*n))),
            _ => None,
        },
        ChoiceKind::MixedEnum => mixed_enum_members(choices)
            .into_iter()
            .find(|(_, lit)| *lit == value)
            .map(|(variant, _)| format!("{iface}.{variant}")),
        ChoiceKind::Union => None,
    }
}

/// The underlying type of a transparent `typealias` a reference names, or `None` when the
/// name is not such an alias (a record group / choice has its own type).
fn find_alias(input: &WasmGeneratorInput, name: &str) -> Option<CsilTypeExpression> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some(other.clone()),
            },
            _ => None,
        }
    })
}

/// Standard file header: the doc block, the generated-code marker, and the
/// `package` declaration every emitted Kotlin file shares.
fn file_header(config: &KotlinConfig, summary: &str) -> String {
    let mut out = String::new();
    if config.package_description.is_empty() {
        out.push_str(&format!("// {summary}\n"));
    } else {
        for line in config.package_description.lines() {
            out.push_str(&format!("// {line}\n"));
        }
    }
    out.push_str("// Code generated by csilgen; DO NOT EDIT.\n\n");
    out.push_str(&format!("package {}\n\n", config.package));
    out
}

fn generate_types(
    input: &WasmGeneratorInput,
    config: &KotlinConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Option<String> {
    let mut body = String::new();
    let mut has_types = false;

    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                has_types = true;
                emit_data_class(&mut body, &rule.name, group, input, warnings);
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                has_types = true;
                emit_data_class(&mut body, &rule.name, group, input, warnings);
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => {
                has_types = true;
                emit_named_choice_type(&mut body, &rule.name, choices);
            }
            CsilRuleType::TypeDef(type_expr) => {
                has_types = true;
                let kt = map_csil_type_to_kotlin(type_expr, &None);
                body.push_str(&format!("/** Type alias for {}. */\n", rule.name));
                body.push_str(&format!(
                    "typealias {} = {}\n\n",
                    pascal_case(&rule.name),
                    kt
                ));
            }
            CsilRuleType::TypeChoice(choices) => {
                has_types = true;
                emit_type_choice(&mut body, &rule.name, choices);
            }
            CsilRuleType::GroupChoice(choices) => {
                has_types = true;
                emit_group_choice(&mut body, &rule.name, choices, input, warnings);
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    if !has_types {
        return None;
    }
    let mut content = file_header(config, "Generated types.");
    content.push_str(&body);
    Some(content)
}

/// A record (`group`) becomes a Kotlin `data class`. Optional fields are nullable
/// with a `null` default; a `@default`/`.default` becomes the constructor default.
/// A `ByteArray` member breaks `data class` structural equality (array identity),
/// so when one is present we override `equals`/`hashCode` with content semantics —
/// without this the conformance/value comparisons silently compare by reference.
fn emit_data_class(
    body: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    input: &WasmGeneratorInput,
    _warnings: &mut Vec<GeneratorWarning>,
) {
    let class_name = pascal_case(name);
    body.push_str(&format!("/** {name} record. */\n"));

    let mut has_byte_array = false;
    let fields: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();
    // A Kotlin `data class` must have at least one primary-constructor property; an empty
    // record degrades to a plain (singleton-friendly) class so it still compiles.
    if fields.is_empty() {
        body.push_str(&format!("class {class_name}\n\n"));
        return;
    }
    body.push_str(&format!("data class {class_name}(\n"));
    for (idx, entry) in fields.iter().enumerate() {
        let key = entry.key.as_ref().unwrap();
        let prop = kotlin_prop_name(key, &entry.metadata);
        let wire = wire_name_from_key(key);
        let kt_type = type_override(&entry.metadata)
            .unwrap_or_else(|| map_csil_type_to_kotlin(&entry.value_type, &entry.occurrence));
        if kt_type.contains("ByteArray") {
            has_byte_array = true;
        }
        if let Some(desc) = field_description(&entry.metadata) {
            body.push_str(&format!("    // {desc}\n"));
        }
        // The wire key is the CSIL field name verbatim; surface it so a reader can
        // see the camelCase property maps to the snake_case CBOR map key.
        if prop != wire {
            body.push_str(&format!("    // wire key: {wire}\n"));
        }
        let default = field_default(entry, input);
        let trailing = if idx + 1 < fields.len() { "," } else { "" };
        match default {
            Some(d) => body.push_str(&format!("    val {prop}: {kt_type} = {d}{trailing}\n")),
            None => body.push_str(&format!("    val {prop}: {kt_type}{trailing}\n")),
        }
    }

    if has_byte_array {
        body.push_str(") {\n");
        emit_byte_array_equality(body, &class_name, &fields);
        body.push_str("}\n\n");
    } else {
        body.push_str(")\n\n");
    }
}

/// `equals`/`hashCode` overrides that compare `ByteArray` members by content. Every
/// other property keeps value semantics; the array members use `contentEquals` /
/// `contentHashCode` so two records with equal bytes compare equal.
fn emit_byte_array_equality(body: &mut String, class_name: &str, fields: &[&CsilGroupEntry]) {
    // Each member: (property, is-bytes, is-nullable). A non-null `ByteArray` uses a plain
    // `contentEquals`; the null-safe form on it would warn "unnecessary safe call".
    let props: Vec<(String, bool, bool)> = fields
        .iter()
        .map(|e| {
            let key = e.key.as_ref().unwrap();
            let prop = kotlin_prop_name(key, &e.metadata);
            let kt = type_override(&e.metadata)
                .unwrap_or_else(|| map_csil_type_to_kotlin(&e.value_type, &e.occurrence));
            (prop, kt.contains("ByteArray"), kt.ends_with('?'))
        })
        .collect();

    body.push_str("    override fun equals(other: Any?): Boolean {\n");
    body.push_str("        if (this === other) return true\n");
    body.push_str(&format!(
        "        if (other !is {class_name}) return false\n"
    ));
    for (prop, is_bytes, nullable) in &props {
        match (is_bytes, nullable) {
            (true, true) => body.push_str(&format!(
                "        if (!({prop}?.contentEquals(other.{prop}) ?: (other.{prop} == null))) return false\n"
            )),
            (true, false) => body.push_str(&format!(
                "        if (!{prop}.contentEquals(other.{prop})) return false\n"
            )),
            _ => body.push_str(&format!(
                "        if ({prop} != other.{prop}) return false\n"
            )),
        }
    }
    body.push_str("        return true\n");
    body.push_str("    }\n\n");

    body.push_str("    override fun hashCode(): Int {\n");
    let mut first = true;
    for (prop, is_bytes, nullable) in &props {
        let expr = match (is_bytes, nullable) {
            (true, true) => format!("({prop}?.contentHashCode() ?: 0)"),
            (true, false) => format!("{prop}.contentHashCode()"),
            _ => format!("{prop}.hashCode()"),
        };
        if first {
            body.push_str(&format!("        var result = {expr}\n"));
            first = false;
        } else {
            body.push_str(&format!("        result = 31 * result + {expr}\n"));
        }
    }
    if first {
        body.push_str("        return 0\n");
    } else {
        body.push_str("        return result\n");
    }
    body.push_str("    }\n");
}

/// A type-choice (`A / B / C`) becomes a `sealed interface` whose arms are wrapper
/// `data class`es — Kotlin has no anonymous union, and a sealed hierarchy gives the
/// host an exhaustive `when`.
fn emit_type_choice(body: &mut String, name: &str, choices: &[CsilTypeExpression]) {
    let iface = pascal_case(name);
    body.push_str(&format!("/** {name}: one of {} arms. */\n", choices.len()));
    body.push_str(&format!("sealed interface {iface}\n"));
    for (i, choice) in choices.iter().enumerate() {
        let kt = map_csil_type_to_kotlin(choice, &None);
        let arm = format!("{iface}Arm{}", i + 1);
        body.push_str(&format!("data class {arm}(val value: {kt}) : {iface}\n"));
    }
    body.push('\n');
}

/// A group-choice becomes a `sealed interface` whose arms are full `data class`
/// records, each implementing the interface (exhaustive `when`).
fn emit_group_choice(
    body: &mut String,
    name: &str,
    choices: &[CsilGroupExpression],
    input: &WasmGeneratorInput,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/** {name}: one of {} group arms. */\n",
        choices.len()
    ));
    body.push_str(&format!("sealed interface {iface}\n\n"));
    for (i, choice) in choices.iter().enumerate() {
        let arm_name = format!("{name}Choice{}", i + 1);
        emit_data_class_impl(body, &arm_name, choice, &iface, input, warnings);
    }
}

/// Like `emit_data_class`, but the class implements `iface`. Kept separate so the
/// plain record path stays the common case and free of an unused supertype.
fn emit_data_class_impl(
    body: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    iface: &str,
    input: &WasmGeneratorInput,
    _warnings: &mut Vec<GeneratorWarning>,
) {
    let class_name = pascal_case(name);
    let fields: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();
    // An empty arm still needs to implement the interface, but a `data class` cannot be
    // empty, so emit a `data object` (the idiomatic unit-arm shape for a sealed interface).
    if fields.is_empty() {
        body.push_str(&format!("data object {class_name} : {iface}\n\n"));
        return;
    }
    body.push_str(&format!("data class {class_name}(\n"));
    for (idx, entry) in fields.iter().enumerate() {
        let key = entry.key.as_ref().unwrap();
        let prop = kotlin_prop_name(key, &entry.metadata);
        let kt_type = type_override(&entry.metadata)
            .unwrap_or_else(|| map_csil_type_to_kotlin(&entry.value_type, &entry.occurrence));
        let default = field_default(entry, input);
        let trailing = if idx + 1 < fields.len() { "," } else { "" };
        match default {
            Some(d) => body.push_str(&format!("    val {prop}: {kt_type} = {d}{trailing}\n")),
            None => body.push_str(&format!("    val {prop}: {kt_type}{trailing}\n")),
        }
    }
    body.push_str(&format!(") : {iface}\n\n"));
}

/// The `ClientError` type, shared by the blocking client and its suspending twin. Only
/// the primary file (the sync/drop-in client) declares it; the twin rides in the same
/// package and reuses it, so it is never redeclared.
const CLIENT_ERROR_KT: &str = "\
/**
 * ClientError is thrown by a generated client call: a structured error the service
 * returned (code/message), or a transport-level failure (cause).
 */
class ClientError(
    val code: Long = 0,
    override val message: String = \"\",
    cause: Throwable? = null,
) : RuntimeException(message, cause)
";

/// The caller-supplied byte carrier the generated client delegates to. The seam is a
/// `suspend fun` for the async shape (it owns the network round-trip); the blocking shape
/// keeps the plain `fun`. The interface name is marked (`AsyncTransport`) only for the
/// twin so it coexists with the blocking `Transport` in one package.
fn transport_interface_kt(shape: ClientShape) -> String {
    let name = shape.transport_name();
    let suspend = shape.suspend_kw();
    // The carrier note tracks the seam's concurrency model so a reader of the generated
    // source knows whether to supply a thread-blocking or coroutine-aware implementation.
    let carrier_note = if shape.is_async {
        "Suspending by design — the seam performs the network round-trip on a coroutine; the host supplies a coroutine-aware carrier."
    } else {
        "Synchronous by design — no coroutines; the host owns its own threads."
    };
    format!(
        "/**\n * {name} is the caller-supplied byte carrier: it performs the call named by\n * (service, op) with the already-encoded request bytes and returns the response\n * bytes, or throws. The generated client owns (de)serialization; the carrier only\n * moves bytes. {carrier_note}\n */\ninterface {name} {{\n    {suspend}fun call(service: String, op: String, request: ByteArray): ByteArray\n}}\n"
    )
}

/// Client scaffolding emitted once at the top of the client file: the shared error type
/// (primary file only) and the caller-supplied transport seam for this shape.
fn client_prelude_kt(shape: ClientShape) -> String {
    let mut out = String::new();
    if shape.marker.is_empty() {
        out.push_str(CLIENT_ERROR_KT);
        out.push('\n');
    }
    out.push_str(&transport_interface_kt(shape));
    out
}

fn generate_client(
    input: &WasmGeneratorInput,
    config: &KotlinConfig,
    shape: ClientShape,
) -> Option<String> {
    let records = kotlin_record_names(input);
    // The full set of references the codec can (de)serialize (records + named choices) and
    // the transparent aliases, so an op with a scalar/array/map/tuple/union boundary can
    // ride the per-op codec helpers instead of being dropped.
    let named = kotlin_codec_named(input);
    let aliases = codec_aliases(input);
    let mut body = String::new();
    body.push_str(&client_prelude_kt(shape));
    body.push('\n');

    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_class(
                &mut body, &rule.name, service, &records, &named, &aliases, shape,
            );
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = file_header(config, "Generated service clients.");
    content.push_str(&body);
    Some(content)
}

fn emit_client_class(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    records: &std::collections::HashSet<String>,
    named: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    shape: ClientShape,
) {
    let base = service_base(name);
    let client = shape.client_name(&base);
    let transport = shape.transport_name();
    let suspend = shape.suspend_kw();

    body.push_str(&format!(
        "/** Typed client for the {name} service. The client owns (de)serialization;\n * the carrier only moves bytes. */\n"
    ));
    body.push_str(&format!(
        "class {client}(private val transport: {transport}) {{\n"
    ));

    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            body.push_str(&format!(
                "    // channel operation '{}' is not part of the RPC client\n",
                operation.name
            ));
            continue;
        }
        let success = success_type(&operation.output_type);
        let null_input = op_input_is_null(&operation.input_type);
        let req_ok =
            null_input || kotlin_op_boundary_expressible(&operation.input_type, named, aliases);
        // Only a genuinely inexpressible boundary (an inline multi-variant choice with no
        // wire discriminator, or an unmodeled reference) is skipped now; scalar/array/map/
        // tuple/union shapes ride the per-op codec helpers, so every other op gets a method.
        if !req_ok || !kotlin_op_boundary_expressible(&success, named, aliases) {
            body.push_str(&format!(
                "    // operation '{}' has a payload csilgen can't (de)serialize; handle it manually\n",
                operation.name
            ));
            continue;
        }
        let method = kotlin_method_name(&operation.name);
        let output_type = map_csil_type_to_kotlin(&success, &None);
        let stem = op_codec_stem(name, operation);
        // A null input carries no request body (empty bytes); a record reuses the generic
        // `encode`; any other shape uses the op's per-op request encoder.
        let req_bytes = if null_input {
            "ByteArray(0)".to_string()
        } else if is_record_ref(&operation.input_type, records) {
            "encode(request)".to_string()
        } else {
            format!("encode{stem}Request(request)")
        };
        // Wire strings are the verbatim CSIL service and operation names
        // (csil-rpc-transport.md §1.1/§1.3), distinct from the Kotlin identifiers.
        let call = format!(
            "transport.call(\"{name}\", \"{wire_op}\", {req_bytes})",
            wire_op = operation.name
        );
        // A record success reuses the generic reified `decode`; any other shape uses the
        // op's per-op response decoder.
        let decode_resp = if is_record_ref(&success, records) {
            format!("decode<{output_type}>({call})")
        } else {
            format!("decode{stem}Response({call})")
        };
        if null_input {
            body.push_str(&format!("    {suspend}fun {method}(): {output_type} {{\n"));
        } else {
            let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
            body.push_str(&format!(
                "    {suspend}fun {method}(request: {input_type}): {output_type} {{\n"
            ));
        }
        body.push_str(&format!("        return {decode_resp}\n"));
        body.push_str("    }\n");
    }
    body.push_str("}\n\n");
}

/// Whether a type is a reference to a record the codec can (de)serialize, so the
/// typed client method can encode/decode it through the generated codec.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(n) if records.contains(&pascal_case(n)))
}

/// Every reference name the codec carries a `toCborValue`/`<name>FromCborValue` for:
/// records plus named choices (enums + unions). A field or op boundary referencing one
/// resolves through the same enc/dec helpers a record does, so client and codec agree on
/// the wire for it.
fn kotlin_codec_named(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    let mut set = kotlin_record_names(input);
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::TypeDef(CsilTypeExpression::Choice(_)) = &rule.rule_type {
            set.insert(pascal_case(&rule.name));
        }
    }
    set
}

/// Whether `kotlin_enc_value`/`kotlin_dec_value` model an op-boundary type faithfully (so a
/// per-op codec helper is correct rather than silently lossy). Records, scalars, transparent
/// aliases, named choices (enums/unions), arrays, maps, and tuples all resolve to real codec
/// helpers. An inline multi-variant choice has no wire discriminator, and an unmodeled
/// reference has no codec, so those two keep the skip-with-note path the client falls back to.
fn kotlin_op_boundary_expressible(
    ty: &CsilTypeExpression,
    named: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> bool {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(_) => true,
        CsilTypeExpression::Reference(name) => {
            named.contains(&pascal_case(name)) || aliases.contains_key(&pascal_case(name))
        }
        CsilTypeExpression::Array { element_type, .. } => {
            kotlin_op_boundary_expressible(element_type, named, aliases)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            kotlin_op_boundary_expressible(key, named, aliases)
                && kotlin_op_boundary_expressible(value, named, aliases)
        }
        CsilTypeExpression::Tuple(_) => true,
        _ => false,
    }
}

/// The `<Base><Method>` stem shared by an op's per-op codec helpers and the client method
/// that calls them, so the two never drift.
fn op_codec_stem(service_name: &str, op: &CsilServiceOperation) -> String {
    format!("{}{}", service_base(service_name), pascal_case(&op.name))
}

/// Codec + handler-outcome prelude emitted once at the top of `Services.kt` when any
/// service has channel ops. Codec is consumer-supplied so the runtime never owns
/// serialization, matching the Go/Python generators.
const SERVICES_CHANNEL_PRELUDE_KT: &str = "\
/**
 * Codec is the consumer-supplied (de)serialization layer for channel messages. The
 * generator is codec-agnostic; the implementer wires this to CBOR, JSON, or anything
 * its protocol expects.
 */
interface Codec {
    fun encode(value: Any?): ByteArray
    fun <T> decode(data: ByteArray, type: Class<T>): T
}
";

fn generate_services(input: &WasmGeneratorInput, config: &KotlinConfig) -> Option<String> {
    let mut body = String::new();
    let needs_channel = spec_has_channel_ops(input);
    if needs_channel {
        body.push_str(SERVICES_CHANNEL_PRELUDE_KT);
        body.push('\n');
    }

    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_service_interface(&mut body, &rule.name, service);
            emit_wire_ids(&mut body, &rule.name, service);
            if service_has_channel_ops(service) {
                emit_channel_router(&mut body, &rule.name, service);
                emit_channel_router_compact(&mut body, &rule.name, service);
                emit_channel_encoders(&mut body, &rule.name, service);
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = file_header(config, "Generated service interfaces and routers.");
    content.push_str(&body);
    Some(content)
}

fn emit_service_interface(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/** {name} service interface (host implements). */\n"
    ));
    body.push_str(&format!("interface {iface} {{\n"));
    for operation in &service.operations {
        let method = kotlin_method_name(&operation.name);
        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                let output_type =
                    map_csil_type_to_kotlin(&success_type(&operation.output_type), &None);
                if op_input_is_null(&operation.input_type) {
                    body.push_str(&format!("    fun {method}(): {output_type}\n"));
                } else {
                    let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
                    body.push_str(&format!(
                        "    fun {method}(request: {input_type}): {output_type}\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
                // Fire-and-forget inbound: the host's plumbing pulls a frame off the
                // wire and hands it to the router, which decodes and dispatches here.
                body.push_str(&format!("    fun {method}(message: {input_type})\n"));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound method on the server side.
            }
        }
    }
    body.push_str("}\n\n");
}

/// Emit `const`-style wire-id ordinals (as a top-level `object`) exposing the
/// `@wire-id(N)` values. Purely additive: nothing is emitted for a wire-id-free
/// service, keeping that output byte-identical.
fn emit_wire_ids(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let prefix = pascal_case(name);
    body.push_str(&format!(
        "/** Wire-id ordinals for {name} (transport compact profiles). */\n"
    ));
    body.push_str(&format!("object {prefix}WireIds {{\n"));
    body.push_str(&format!("    const val SERVICE: ULong = {service_id}uL\n"));
    for operation in &service.operations {
        if let Some(op_id) = operation.wire_id {
            let op_const = screaming_snake(&operation.name);
            body.push_str(&format!("    const val OP_{op_const}: ULong = {op_id}uL\n"));
        }
    }
    body.push_str("}\n\n");
}

fn emit_channel_router(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/**\n * Decode one inbound channel frame and dispatch to the matching {name}\n * method by its verbose wire op name. The host feeds bytes from its connection.\n */\n"
    ));
    body.push_str(&format!(
        "fun route{iface}Channel(handlers: {iface}, codec: Codec, op: String, data: ByteArray) {{\n"
    ));
    body.push_str("    when (op) {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = kotlin_method_name(&operation.name);
        let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
        // The wire op key is the verbatim CSIL operation name (csil-rpc-transport.md
        // §1.3), matching what the peer's op encoder frames.
        body.push_str(&format!("        \"{}\" -> {{\n", operation.name));
        body.push_str(&format!(
            "            val message = codec.decode(data, {input_type}::class.java)\n"
        ));
        body.push_str(&format!("            handlers.{method}(message)\n"));
        body.push_str("        }\n");
    }
    body.push_str("        else -> throw IllegalArgumentException(\"unknown channel op '$op'\")\n");
    body.push_str("    }\n");
    body.push_str("}\n\n");
}

/// Compact-profile twin: dispatch on the operation's `@wire-id` ordinal instead of
/// the verbose name. Emitted only for wire-id-bearing services so wire-id-free
/// output stays byte-identical.
fn emit_channel_router_compact(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    if service.wire_id.is_none() {
        return;
    }
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/**\n * Decode one inbound channel frame by its @wire-id ordinal (compact profile)\n * and dispatch to the matching {name} method. The verbose twin is route{iface}Channel;\n * the host calls whichever matches the profile negotiated on the wire.\n */\n"
    ));
    body.push_str(&format!(
        "fun route{iface}ChannelCompact(handlers: {iface}, codec: Codec, op: ULong, data: ByteArray) {{\n"
    ));
    body.push_str("    when (op) {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let Some(op_id) = operation.wire_id else {
            continue;
        };
        let method = kotlin_method_name(&operation.name);
        let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
        body.push_str(&format!("        {op_id}uL -> {{\n"));
        body.push_str(&format!(
            "            val message = codec.decode(data, {input_type}::class.java)\n"
        ));
        body.push_str(&format!("            handlers.{method}(message)\n"));
        body.push_str("        }\n");
    }
    body.push_str(
        "        else -> throw IllegalArgumentException(\"unknown channel ordinal $op\")\n",
    );
    body.push_str("    }\n");
    body.push_str("}\n\n");
}

/// Outbound encoders for server-pushed (bidirectional/reverse) messages: the host
/// frames (op, bytes) onto its connection.
fn emit_channel_encoders(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let iface = pascal_case(name);
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let output_type = map_csil_type_to_kotlin(&operation.output_type, &None);
        let fn_name = format!("encode{iface}{}", pascal_case(&operation.name));
        body.push_str(&format!(
            "/** Encode a `{}` message the server pushes to a peer. */\n",
            operation.name
        ));
        body.push_str(&format!(
            "fun {fn_name}(codec: Codec, message: {output_type}): Pair<String, ByteArray> {{\n"
        ));
        body.push_str(&format!(
            "    return Pair(\"{}\", codec.encode(message))\n",
            operation.name
        ));
        body.push_str("}\n\n");
    }
}

// ---------------------------------------------------------------------------
// Codec (Codec.kt)
// ---------------------------------------------------------------------------

/// The Kotlin type names of the record (`data class`) rules — the types whose CBOR
/// form is a map and which the codec covers with `toCborValue`/`fromCborValue`.
fn kotlin_record_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(pascal_case(&r.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(pascal_case(&r.name)),
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference / tuple (NOT a record group or a choice, which
/// have their own handling). A field referencing one (`StringInt64Map = {* text => int}`,
/// `Tags = [* text]`, `Uuid = text`) has no codec of its own, so it must encode/decode as
/// its underlying type rather than fall through to the `CborValue.CNull` stub a bare
/// non-record reference would yield. Keyed by `pascal_case` to match the reference
/// lookups, which already `pascal_case` the referenced name like the record set does.
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
                other => Some((pascal_case(&rule.name), other.clone())),
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

// `choice_arm_literal` is shared machinery now (see `csilgen_common::choice`, THE
// normative classification contract) — imported above so every existing
// `choice_arm_literal(...)` call site in this file keeps working unchanged.

// Inline (anonymous) group/choice hoisting now runs through the shared
// `csilgen_common::hoist_inline_composites` (see `crates/csilgen-common/src/
// hoist.rs`) — see `process_generation`'s call site. It generalizes past this
// generator's own former local copy in one respect worth noting: the shared
// hoister also hoists an inline composite that appears as a MAP KEY (this
// generator's old local `hoist_type` recursed into a map's VALUE but passed a
// map's KEY through unchanged — a real gap the shared pass closes for free).

/// A Kotlin expression building a `CborValue` from `expr` (a typed value).
fn kotlin_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("CborValue.CInt({expr})"),
            "uint" => format!("CborValue.CUint({expr})"),
            "float" | "float64" | "double" => format!("CborValue.CFloat({expr})"),
            "text" | "tstr" => format!("CborValue.CText({expr})"),
            "bytes" | "bstr" => format!("CborValue.CBytes({expr})"),
            "bool" => format!("CborValue.CBool({expr})"),
            // timestamp is tag 0 + RFC3339 UTC text; Instant.toString() is RFC3339 with `Z`.
            "timestamp" => format!("CborValue.CTag(0uL, CborValue.CText(({expr}).toString()))"),
            // decimal is tag 4 [exponent, mantissa]; BigDecimal = unscaled * 10^-scale.
            "decimal" => format!(
                "CborValue.CTag(4uL, CborValue.CArray(listOf(CborValue.CInt((-({expr}).scale()).toLong()), CborValue.CInt(({expr}).unscaledValue().longValueExact()))))"
            ),
            "nil" | "null" => "CborValue.CNull".to_string(),
            // `any` is already a CborValue (see map_csil_type_to_kotlin); pass it through.
            "any" => expr.to_string(),
            _ => "CborValue.CNull".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&pascal_case(name)) => {
            format!("{expr}.toCborValue()")
        }
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`) has no
        // codec of its own; encode it as its underlying map/array/scalar type. The Kotlin
        // alias-typed field is the same `Map`/`List`/scalar the underlying encoder expects,
        // so the same `expr` flows through unchanged.
        CsilTypeExpression::Reference(name) if aliases.contains_key(&pascal_case(name)) => {
            kotlin_enc_value(&aliases[&pascal_case(name)], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = kotlin_enc_value(element_type, "csilE", records, aliases);
            format!("CborValue.CArray(({expr}).map {{ csilE -> {inner} }})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let k = kotlin_enc_value(key, "csilK", records, aliases);
            let v = kotlin_enc_value(value, "csilV", records, aliases);
            format!("CborValue.CMap(({expr}).map {{ (csilK, csilV) -> {k} to {v} }})")
        }
        // A fixed-shape tuple is a positional CBOR array; an absent optional element is a
        // `null` held in place so the array length is fixed (the locked wire).
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let kt = map_csil_type_to_kotlin(&e.value_type, &None);
                    if matches!(e.occurrence, Some(CsilOccurrence::Optional)) {
                        let inner = kotlin_enc_value(&e.value_type, "csilTup", records, aliases);
                        format!(
                            "(({expr})[{i}] as {kt}?)?.let {{ csilTup -> {inner} }} ?: CborValue.CNull"
                        )
                    } else {
                        kotlin_enc_value(
                            &e.value_type,
                            &format!("(({expr})[{i}] as {kt})"),
                            records,
                            aliases,
                        )
                    }
                })
                .collect();
            format!("CborValue.CArray(listOf({}))", parts.join(", "))
        }
        CsilTypeExpression::Choice(choices) if choice_is_stringy(choices) => {
            format!("CborValue.CText({expr})")
        }
        CsilTypeExpression::Literal(lit) => kotlin_literal_cbor_expr(lit),
        _ => "CborValue.CNull".to_string(),
    }
}

/// A Kotlin expression decoding a typed value from `expr` (a `CborValue`).
fn kotlin_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("CsilCbor.asLong({expr})"),
            "uint" => format!("CsilCbor.asULong({expr})"),
            "float" | "float64" | "double" => format!("CsilCbor.asDouble({expr})"),
            "text" | "tstr" => format!("CsilCbor.asText({expr})"),
            "bytes" | "bstr" => format!("CsilCbor.asBytes({expr})"),
            "bool" => format!("CsilCbor.asBoolean({expr})"),
            "timestamp" => format!("java.time.Instant.parse(CsilCbor.asTaggedText({expr}, 0uL))"),
            "decimal" => format!("CsilCbor.asDecimal({expr})"),
            // `any` keeps the decoded CBOR value verbatim (passes through unchanged).
            "any" => expr.to_string(),
            _ => format!("CsilCbor.asText({expr})"),
        },
        CsilTypeExpression::Reference(name) if records.contains(&pascal_case(name)) => {
            format!("{}FromCborValue({expr})", camel_case(name))
        }
        // A reference to a transparent alias decodes as its underlying map/array/scalar
        // type; the resulting `Map`/`List`/scalar is assignable to the alias-typed field.
        CsilTypeExpression::Reference(name) if aliases.contains_key(&pascal_case(name)) => {
            kotlin_dec_value(&aliases[&pascal_case(name)], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = kotlin_dec_value(element_type, "csilE", records, aliases);
            format!("CsilCbor.asArray({expr}).map {{ csilE -> {inner} }}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let k = kotlin_dec_value(key, "csilK", records, aliases);
            let v = kotlin_dec_value(value, "csilV", records, aliases);
            format!("CsilCbor.asMap({expr}).associate {{ (csilK, csilV) -> {k} to {v} }}")
        }
        // Tuple: a positional CBOR array; optional elements decode through a `null`-in-place
        // guard. The result is a `List<Any?>` matching the generated field type.
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    if matches!(e.occurrence, Some(CsilOccurrence::Optional)) {
                        let inner = kotlin_dec_value(&e.value_type, "csilTup", records, aliases);
                        format!(
                            "(csilArr[{i}]).let {{ csilTup -> if (csilTup is CborValue.CNull) null else {inner} }}"
                        )
                    } else {
                        kotlin_dec_value(&e.value_type, &format!("csilArr[{i}]"), records, aliases)
                    }
                })
                .collect();
            format!(
                "CsilCbor.asArray({expr}).let {{ csilArr -> listOf<Any?>({}) }}",
                parts.join(", ")
            )
        }
        CsilTypeExpression::Choice(choices) if choice_is_stringy(choices) => {
            format!("CsilCbor.asText({expr})")
        }
        CsilTypeExpression::Literal(lit) => {
            let expected = kotlin_literal_cbor_expr(lit);
            let value = kotlin_literal_value_expr(lit);
            format!("CsilCbor.expectLiteral({expr}, {expected}, {value})")
        }
        _ => format!("CsilCbor.asText({expr})"),
    }
}

/// Whether every arm of a choice is "some text" (the open `text`/`tstr` builtin or a
/// string literal) — such a choice carries no more than a `String` on the wire, so the
/// codec treats it as text.
fn choice_is_stringy(choices: &[CsilTypeExpression]) -> bool {
    !choices.is_empty()
        && choices.iter().all(|c| match c {
            CsilTypeExpression::Builtin(n) => n == "text" || n == "tstr",
            // See through a trailing-`.default` wrapper on the arm (see choice_arm_literal).
            _ => matches!(choice_arm_literal(c), Some(CsilLiteralValue::Text(_))),
        })
}

/// How a named `A = X / Y / …` choice is realized in Kotlin: an all-text-literal or
/// all-int-literal choice is a bare-literal enum (the literal is its own discriminant,
/// read/written via a single-kind CBOR accessor); a MIXED-kind all-literal choice
/// (`"a" / 1`) is likewise a bare-literal enum, just compared by structural equality
/// against each member's own `CborValue` rendering rather than a single-kind
/// extractor (`mixed_enum_members`/`kotlin_literal_cbor_expr`) — same CSIL wire
/// contract, no tag, just no uniform accessor to read it back with; anything else
/// (at least one non-literal arm) is a tagged-sum union (`[variant_index, value]` on
/// the wire).
enum ChoiceKind {
    EnumText,
    EnumInt,
    MixedEnum,
    Union,
}

/// Routes the ENUM-vs-UNION split through the shared `csilgen_common::classify_choice`
/// (THE normative contract: ALL-literal, ANY kind mix — `"a" / 1` — is an enum, per
/// `csilgen_common::choice`'s module docs) and only layers this generator's OWN
/// literal-kind sub-classification on top: a uniform text/int vocabulary keeps its
/// historical single-kind-accessor rendering; any other all-literal mix (including a
/// uniform float/bool/bytes/null vocabulary, not just an actual kind MIX) is
/// `MixedEnum`, since Kotlin has no dedicated bare-float/bare-bool enum rendering of
/// its own to special-case the way EnumText/EnumInt do.
fn classify_choice(choices: &[CsilTypeExpression]) -> ChoiceKind {
    match csilgen_common::classify_choice(choices) {
        ChoiceClass::Enum(literals) => classify_enum(&literals),
        ChoiceClass::Union(_) => ChoiceKind::Union,
    }
}

fn classify_enum(literals: &[&CsilLiteralValue]) -> ChoiceKind {
    if literals
        .iter()
        .all(|l| matches!(l, CsilLiteralValue::Text(_)))
    {
        ChoiceKind::EnumText
    } else if literals
        .iter()
        .all(|l| matches!(l, CsilLiteralValue::Integer(_)))
    {
        ChoiceKind::EnumInt
    } else {
        ChoiceKind::MixedEnum
    }
}

/// The enum-constant name for a text literal: PascalCase of the literal text (mirrors the
/// reference generators, e.g. `"green"` → `Green`).
fn enum_text_variant(s: &str) -> String {
    pascal_case(s)
}

/// The enum-constant name for an int literal: `V<n>` (a negative `n` becomes `VNeg<abs>`
/// so the identifier stays legal), matching the reference generators' `V1`/`V2`/`V3`.
fn enum_int_variant(n: i64) -> String {
    if n < 0 {
        format!("VNeg{}", n.unsigned_abs())
    } else {
        format!("V{n}")
    }
}

/// A name basis for a literal's enum constant in a `MixedEnum` — text/int reuse
/// `enum_text_variant`/`enum_int_variant` so the SAME literal gets the SAME constant
/// name whether or not the rest of the choice happens to share its kind. Bool/Float/
/// Null/Bytes get their own basis; an Array literal choice arm does not occur from
/// real CSIL source (see the module doc on `csilgen_common::choice`) but is handled
/// defensively via a stable placeholder so this never panics.
fn mixed_enum_variant_base(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Text(s) => enum_text_variant(s),
        CsilLiteralValue::Integer(n) => enum_int_variant(*n),
        CsilLiteralValue::Bool(true) => "True".to_string(),
        CsilLiteralValue::Bool(false) => "False".to_string(),
        CsilLiteralValue::Float(f) => {
            let sign = if *f < 0.0 { "Neg" } else { "" };
            let digits = f.abs().to_string().replace('.', "_");
            format!("F{sign}{digits}")
        }
        CsilLiteralValue::Null => "Null".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            format!("Bytes{hex}")
        }
        CsilLiteralValue::Array(_) => "Arr".to_string(),
    }
}

/// Disambiguate `base` against every name already claimed in `seen` with a numeric
/// suffix, and record the result — so two literals whose natural basis collides
/// (`"true"` the text literal and `true` the bool literal both basing to `True`)
/// still get distinct Kotlin enum constant names.
fn unique_variant(seen: &mut Vec<String>, base: String) -> String {
    let mut candidate = base.clone();
    let mut n = 2;
    while seen.contains(&candidate) {
        candidate = format!("{base}{n}");
        n += 1;
    }
    seen.push(candidate.clone());
    candidate
}

/// The `(variant name, literal)` pairs for a `ChoiceKind::MixedEnum`, in declaration
/// order, with duplicate-name disambiguation applied ONCE — `emit_named_choice_type`
/// (the declaration) and `emit_choice_codec` (the wire) both call this so they can
/// never disagree on what a given literal's constant is named.
fn mixed_enum_members(choices: &[CsilTypeExpression]) -> Vec<(String, &CsilLiteralValue)> {
    let mut seen: Vec<String> = Vec::new();
    choices
        .iter()
        .filter_map(choice_arm_literal)
        .map(|lit| (unique_variant(&mut seen, mixed_enum_variant_base(lit)), lit))
        .collect()
}

/// Emit the Kotlin type for a named `TypeDef` choice rule: an enum for an all-literal
/// choice, or a sealed-interface union for a mixed choice.
fn emit_named_choice_type(body: &mut String, name: &str, choices: &[CsilTypeExpression]) {
    let iface = pascal_case(name);
    match classify_choice(choices) {
        ChoiceKind::EnumText => {
            let variants: Vec<String> = choices
                .iter()
                .filter_map(|c| match choice_arm_literal(c) {
                    Some(CsilLiteralValue::Text(s)) => Some(enum_text_variant(s)),
                    _ => None,
                })
                .collect();
            body.push_str(&format!("/** {name} enum (bare-literal wire). */\n"));
            body.push_str(&format!(
                "enum class {iface} {{ {} }}\n\n",
                variants.join(", ")
            ));
        }
        ChoiceKind::EnumInt => {
            let variants: Vec<String> = choices
                .iter()
                .filter_map(|c| match choice_arm_literal(c) {
                    Some(CsilLiteralValue::Integer(n)) => Some(enum_int_variant(*n)),
                    _ => None,
                })
                .collect();
            body.push_str(&format!("/** {name} enum (bare-literal wire). */\n"));
            body.push_str(&format!(
                "enum class {iface} {{ {} }}\n\n",
                variants.join(", ")
            ));
        }
        ChoiceKind::MixedEnum => {
            let variants: Vec<String> = mixed_enum_members(choices)
                .into_iter()
                .map(|(variant, _)| variant)
                .collect();
            body.push_str(&format!(
                "/** {name} enum (bare-literal wire, mixed literal kinds). */\n"
            ));
            body.push_str(&format!(
                "enum class {iface} {{ {} }}\n\n",
                variants.join(", ")
            ));
        }
        ChoiceKind::Union => {
            body.push_str(&format!(
                "/** {name}: tagged-sum union of {} arms. */\n",
                choices.len()
            ));
            body.push_str(&format!("sealed interface {iface}\n"));
            for (i, choice) in choices.iter().enumerate() {
                let kt = map_csil_type_to_kotlin(choice, &None);
                body.push_str(&format!(
                    "data class {iface}Variant{i}(val value: {kt}) : {iface}\n"
                ));
            }
            body.push('\n');
        }
    }
}

/// Emit the codec for a named choice rule: a bare-literal enum codec, or a tagged-sum
/// union codec. Mirrors the reference generators so the wire is byte-identical.
fn emit_choice_codec(
    body: &mut String,
    name: &str,
    choices: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) {
    let tn = pascal_case(name);
    let dec = camel_case(name);
    match classify_choice(choices) {
        ChoiceKind::EnumText => {
            let lits: Vec<(String, String)> = choices
                .iter()
                .filter_map(|c| match choice_arm_literal(c) {
                    Some(CsilLiteralValue::Text(s)) => Some((enum_text_variant(s), s.clone())),
                    _ => None,
                })
                .collect();
            body.push_str(&format!(
                "/** Encode a {tn} enum as its bare literal value. */\n"
            ));
            body.push_str(&format!(
                "fun {tn}.toCborValue(): CborValue = when (this) {{\n"
            ));
            for (variant, lit) in &lits {
                body.push_str(&format!(
                    "    {tn}.{variant} -> CborValue.CText(\"{}\")\n",
                    kotlin_escape(lit)
                ));
            }
            body.push_str("}\n\n");
            body.push_str(&format!(
                "/** Decode a bare literal value into a {tn} enum. */\n"
            ));
            body.push_str(&format!(
                "fun {dec}FromCborValue(cbor: CborValue): {tn} = when (CsilCbor.asText(cbor)) {{\n"
            ));
            for (variant, lit) in &lits {
                body.push_str(&format!(
                    "    \"{}\" -> {tn}.{variant}\n",
                    kotlin_escape(lit)
                ));
            }
            body.push_str(&format!(
                "    else -> throw CborError(\"unknown {tn} value\")\n}}\n\n"
            ));
        }
        ChoiceKind::EnumInt => {
            let lits: Vec<(String, i64)> = choices
                .iter()
                .filter_map(|c| match choice_arm_literal(c) {
                    Some(CsilLiteralValue::Integer(n)) => Some((enum_int_variant(*n), *n)),
                    _ => None,
                })
                .collect();
            body.push_str(&format!(
                "/** Encode a {tn} enum as its bare literal value. */\n"
            ));
            body.push_str(&format!(
                "fun {tn}.toCborValue(): CborValue = when (this) {{\n"
            ));
            for (variant, n) in &lits {
                body.push_str(&format!("    {tn}.{variant} -> CborValue.CInt({n})\n"));
            }
            body.push_str("}\n\n");
            body.push_str(&format!(
                "/** Decode a bare literal value into a {tn} enum. */\n"
            ));
            body.push_str(&format!(
                "fun {dec}FromCborValue(cbor: CborValue): {tn} = when (CsilCbor.asLong(cbor)) {{\n"
            ));
            for (variant, n) in &lits {
                body.push_str(&format!("    {n}L -> {tn}.{variant}\n"));
            }
            body.push_str(&format!(
                "    else -> throw CborError(\"unknown {tn} value\")\n}}\n\n"
            ));
        }
        ChoiceKind::MixedEnum => {
            // No single-kind accessor (`asText`/`asLong`) covers a mixed literal
            // vocabulary, so decode compares the whole `CborValue` against each
            // member's own rendering (`kotlin_literal_cbor_expr`) — CBOR's major
            // types are mutually exclusive, so this is unambiguous and, unlike a
            // single-kind accessor, still rejects a well-typed-but-undeclared value
            // (parity with the python/ocaml/go/php/ruby/elixir codecs' membership
            // check on enum decode).
            let members = mixed_enum_members(choices);
            body.push_str(&format!(
                "/** Encode a {tn} enum as its bare literal value. */\n"
            ));
            body.push_str(&format!(
                "fun {tn}.toCborValue(): CborValue = when (this) {{\n"
            ));
            for (variant, lit) in &members {
                body.push_str(&format!(
                    "    {tn}.{variant} -> {}\n",
                    kotlin_literal_cbor_expr(lit)
                ));
            }
            body.push_str("}\n\n");
            body.push_str(&format!(
                "/** Decode a bare literal value into a {tn} enum. */\n"
            ));
            body.push_str(&format!(
                "fun {dec}FromCborValue(cbor: CborValue): {tn} = when (cbor) {{\n"
            ));
            for (variant, lit) in &members {
                body.push_str(&format!(
                    "    {} -> {tn}.{variant}\n",
                    kotlin_literal_cbor_expr(lit)
                ));
            }
            body.push_str(&format!(
                "    else -> throw CborError(\"unknown {tn} value\")\n}}\n\n"
            ));
        }
        ChoiceKind::Union => {
            body.push_str(&format!(
                "/** Encode a {tn} union as a tagged sum [variant_index, value]. */\n"
            ));
            body.push_str(&format!(
                "fun {tn}.toCborValue(): CborValue = when (this) {{\n"
            ));
            for (i, arm) in choices.iter().enumerate() {
                let enc = kotlin_enc_value(arm, "this.value", records, aliases);
                body.push_str(&format!(
                    "    is {tn}Variant{i} -> CborValue.CArray(listOf(CborValue.CUint({i}uL), {enc}))\n"
                ));
            }
            body.push_str("}\n\n");
            body.push_str(&format!(
                "/** Decode a tagged sum [variant_index, value] into a {tn} union. */\n"
            ));
            body.push_str(&format!(
                "fun {dec}FromCborValue(cbor: CborValue): {tn} {{\n"
            ));
            body.push_str("    val csilArr = CsilCbor.asArray(cbor)\n");
            body.push_str(
                "    if (csilArr.size != 2) throw CborError(\"union expects [index, value]\")\n",
            );
            body.push_str("    return when (CsilCbor.asULong(csilArr[0])) {\n");
            for (i, arm) in choices.iter().enumerate() {
                let dec = kotlin_dec_value(arm, "csilArr[1]", records, aliases);
                body.push_str(&format!("        {i}uL -> {tn}Variant{i}({dec})\n"));
            }
            body.push_str(&format!(
                "        else -> throw CborError(\"unknown {tn} variant\")\n    }}\n}}\n\n"
            ));
        }
    }
}

/// Emit one record's codec: `toCborValue`/`toCbor` (canonical key order) plus the
/// free `<type>FromCborValue`/`<type>FromCbor` decoders (declaration order).
fn emit_struct_codec(
    body: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) {
    let type_name = pascal_case(name);
    let decoder = camel_case(name);
    // (prop, wire, entry) in declaration order, and a canonical-key-order copy for the
    // encoder so the wire map is deterministic.
    let named: Vec<(String, String, &CsilGroupEntry)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let key = e.key.as_ref()?;
            let prop = kotlin_prop_name(key, &e.metadata);
            let wire = wire_name_from_key(key);
            Some((prop, wire, e))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    body.push_str(&format!(
        "/** The CBOR value tree for a {type_name} (deep, canonical key order). */\n"
    ));
    body.push_str(&format!("fun {type_name}.toCborValue(): CborValue {{\n"));
    body.push_str("    val csilEntries = ArrayList<Pair<CborValue, CborValue>>()\n");
    for (prop, wire, entry) in &canonical {
        let wire_lit = kotlin_escape(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let enc = kotlin_enc_value(&entry.value_type, "csilV", records, aliases);
            body.push_str(&format!(
                "    this.{prop}?.let {{ csilV -> csilEntries.add(CborValue.CText(\"{wire_lit}\") to {enc}) }}\n"
            ));
        } else {
            let enc =
                kotlin_enc_value(&entry.value_type, &format!("this.{prop}"), records, aliases);
            body.push_str(&format!(
                "    csilEntries.add(CborValue.CText(\"{wire_lit}\") to {enc})\n"
            ));
        }
    }
    body.push_str("    return CborValue.CMap(csilEntries)\n}\n\n");

    body.push_str(&format!(
        "/** Encode a {type_name} to canonical CSIL CBOR bytes. */\n"
    ));
    body.push_str(&format!(
        "fun {type_name}.toCbor(): ByteArray = CsilCbor.encode(this.toCborValue())\n\n"
    ));

    body.push_str(&format!(
        "/** Reconstruct a {type_name} from a decoded CBOR value tree. */\n"
    ));
    body.push_str(&format!(
        "fun {decoder}FromCborValue(cbor: CborValue): {type_name} {{\n"
    ));
    let mut init_args: Vec<String> = Vec::new();
    for (prop, wire, entry) in &named {
        let wire_lit = kotlin_escape(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            let dec = kotlin_dec_value(&entry.value_type, "csilV", records, aliases);
            body.push_str(&format!(
                "    val {prop} = CsilCbor.mapGet(cbor, \"{wire_lit}\")?.let {{ csilV -> {dec} }}\n"
            ));
        } else {
            let dec = kotlin_dec_value(
                &entry.value_type,
                &format!("CsilCbor.require(cbor, \"{wire_lit}\")"),
                records,
                aliases,
            );
            body.push_str(&format!("    val {prop} = {dec}\n"));
        }
        init_args.push(format!("{prop} = {prop}"));
    }
    body.push_str(&format!(
        "    return {type_name}({})\n}}\n\n",
        init_args.join(", ")
    ));

    body.push_str(&format!(
        "/** Decode CSIL CBOR bytes into a {type_name}. */\n"
    ));
    body.push_str(&format!(
        "fun {decoder}FromCbor(bytes: ByteArray): {type_name} = {decoder}FromCborValue(CsilCbor.decode(bytes))\n\n"
    ));
}

/// The generic `encode`/`decode` entry points and their per-record dispatch tables.
/// The typed client calls `encode(request)` / `decode<Resp>(bytes)`; both resolve a
/// concrete record by its runtime/`reified` type.
fn emit_codec_dispatch(body: &mut String, records: &[(String, String)]) {
    body.push_str(
        "/** Encode a generated CSIL record to canonical CBOR bytes. */\nfun <T> encode(value: T): ByteArray = CsilCbor.encode(csilToCborValue(value))\n\n",
    );
    body.push_str("private fun csilToCborValue(value: Any?): CborValue = when (value) {\n");
    body.push_str("    null -> CborValue.CNull\n");
    for (type_name, _) in records {
        body.push_str(&format!("    is {type_name} -> value.toCborValue()\n"));
    }
    body.push_str("    else -> throw CborError(\"no CSIL CBOR codec for ${value::class}\")\n}\n\n");

    body.push_str(
        "/** Decode canonical CBOR bytes into a generated CSIL record of type [T]. */\ninline fun <reified T> decode(bytes: ByteArray): T =\n    csilFromCborValue(T::class, CsilCbor.decode(bytes)) as T\n\n",
    );
    body.push_str(
        "fun csilFromCborValue(type: kotlin.reflect.KClass<*>, cbor: CborValue): Any = when (type) {\n",
    );
    for (type_name, decoder) in records {
        body.push_str(&format!(
            "    {type_name}::class -> {decoder}FromCborValue(cbor)\n"
        ));
    }
    body.push_str("    else -> throw CborError(\"no CSIL CBOR codec for $type\")\n}\n");
}

/// Exported per-op CBOR helpers so a server in another module can compose a
/// `decode(request)/encode(response)` pair for every op — scalar-id requests and
/// `[]T`/map/scalar responses included, not just record↔record. The client calls the same
/// helpers, so a single surface owns the wire for both directions. Records keep the generic
/// `encode`/`decode<T>` path (byte-identical), so only non-record boundaries get a per-op
/// helper here.
fn emit_op_codecs(
    body: &mut String,
    input: &WasmGeneratorInput,
    named: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) {
    let records = kotlin_record_names(input);
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
                null_input || kotlin_op_boundary_expressible(&op.input_type, named, aliases);
            if !req_ok || !kotlin_op_boundary_expressible(&success, named, aliases) {
                continue;
            }
            let stem = op_codec_stem(&rule.name, op);
            if !null_input && !is_record_ref(&op.input_type, &records) {
                emit_op_codec_pair(
                    body,
                    &format!("{stem}Request"),
                    &op.input_type,
                    named,
                    aliases,
                );
            }
            if !is_record_ref(&success, &records) {
                emit_op_codec_pair(body, &format!("{stem}Response"), &success, named, aliases);
            }
        }
    }
}

/// One `encode<Name>`/`decode<Name>` pair over the value builders the record codec already
/// uses, so an arbitrary op-boundary shape gets the same byte seam a record type has.
fn emit_op_codec_pair(
    body: &mut String,
    helper: &str,
    ty: &CsilTypeExpression,
    named: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) {
    let kt_type = map_csil_type_to_kotlin(ty, &None);
    let enc = kotlin_enc_value(ty, "value", named, aliases);
    let dec = kotlin_dec_value(ty, "csilRoot", named, aliases);
    body.push_str(&format!(
        "/** Encode the {helper} payload to canonical CSIL CBOR bytes. */\n\
         fun encode{helper}(value: {kt_type}): ByteArray = CsilCbor.encode({enc})\n\n"
    ));
    body.push_str(&format!(
        "/** Decode canonical CSIL CBOR bytes into the {helper} payload. */\n\
         fun decode{helper}(bytes: ByteArray): {kt_type} {{\n\
         \x20   val csilRoot = CsilCbor.decode(bytes)\n\
         \x20   return {dec}\n}}\n\n"
    ));
}

/// Build `Codec.kt`: the self-contained canonical-CBOR runtime, a codec per record,
/// and the generic dispatch. `None` when the spec declares no record types.
fn generate_codec(input: &WasmGeneratorInput, config: &KotlinConfig) -> Option<String> {
    let records = kotlin_record_names(input);
    if records.is_empty() {
        return None;
    }
    let aliases = codec_aliases(input);
    // Named choices (enums + unions) carry their own `toCborValue`/`<name>FromCborValue`
    // codec, so a field referencing one resolves exactly like a record does — fold their
    // names into the same set the enc/dec helpers consult.
    let codec_named = kotlin_codec_named(input);
    let mut body = String::new();
    let mut dispatch: Vec<(String, String)> = Vec::new();
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(g) | CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => {
                emit_struct_codec(&mut body, &rule.name, g, &codec_named, &aliases);
                dispatch.push((pascal_case(&rule.name), camel_case(&rule.name)));
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => {
                emit_choice_codec(&mut body, &rule.name, choices, &codec_named, &aliases);
            }
            _ => {}
        }
    }
    emit_codec_dispatch(&mut body, &dispatch);
    // Per-op byte helpers for non-record op boundaries (scalar-id requests, `[]T`/map/scalar
    // responses, …), so the client and a consumer-side server share one codec surface for
    // every op — not just record↔record.
    emit_op_codecs(&mut body, input, &codec_named, &aliases);

    let mut content = file_header(
        config,
        "Generated CBOR (de)serializers for the CSIL value types.",
    );
    content.push_str(CODEC_RUNTIME_KT);
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

/// The self-contained canonical-CBOR runtime the generated codecs build on, so the
/// output stays standalone (no third-party CBOR dependency). `bytes` is a `ByteArray`
/// encoded as a CBOR byte string (major type 2) by construction.
const CODEC_RUNTIME_KT: &str = r#"/** A minimal canonical-CBOR (RFC 8949 subset) value model. */
sealed class CborValue {
    data class CUint(val value: ULong) : CborValue()
    data class CInt(val value: Long) : CborValue()
    data class CBool(val value: Boolean) : CborValue()
    data class CFloat(val value: Double) : CborValue()
    data object CNull : CborValue()
    data class CText(val value: String) : CborValue()
    // ByteArray breaks data-class structural equality (array identity), so compare by content.
    class CBytes(val value: ByteArray) : CborValue() {
        override fun equals(other: Any?): Boolean =
            this === other || (other is CBytes && value.contentEquals(other.value))
        override fun hashCode(): Int = value.contentHashCode()
    }
    data class CArray(val items: List<CborValue>) : CborValue()
    data class CMap(val entries: List<Pair<CborValue, CborValue>>) : CborValue()
    data class CTag(val tag: ULong, val value: CborValue) : CborValue()
}

/** Raised by the CBOR runtime on malformed input or a type/field mismatch. */
class CborError(message: String) : RuntimeException(message)

/** The canonical-CBOR encoder/decoder plus typed accessors over [CborValue]. */
object CsilCbor {
    fun encode(value: CborValue): ByteArray {
        val out = ArrayList<Byte>()
        enc(value, out)
        return out.toByteArray()
    }

    private fun head(major: Int, n: ULong, out: MutableList<Byte>) {
        val mt = major shl 5
        when {
            n < 24uL -> out.add((mt or n.toInt()).toByte())
            n < 0x100uL -> {
                out.add((mt or 24).toByte())
                out.add(n.toByte())
            }
            n < 0x10000uL -> {
                out.add((mt or 25).toByte())
                out.add((n shr 8).toByte())
                out.add(n.toByte())
            }
            n < 0x100000000uL -> {
                out.add((mt or 26).toByte())
                var i = 24
                while (i >= 0) {
                    out.add((n shr i).toByte())
                    i -= 8
                }
            }
            else -> {
                out.add((mt or 27).toByte())
                var i = 56
                while (i >= 0) {
                    out.add((n shr i).toByte())
                    i -= 8
                }
            }
        }
    }

    private fun enc(v: CborValue, out: MutableList<Byte>) {
        when (v) {
            is CborValue.CUint -> head(0, v.value, out)
            is CborValue.CInt -> {
                val n = v.value
                if (n >= 0) head(0, n.toULong(), out) else head(1, (-(n + 1)).toULong(), out)
            }
            is CborValue.CBool -> out.add((if (v.value) 0xf5 else 0xf4).toByte())
            is CborValue.CNull -> out.add(0xf6.toByte())
            is CborValue.CFloat -> {
                out.add(0xfb.toByte())
                val bits = v.value.toRawBits()
                var i = 56
                while (i >= 0) {
                    out.add((bits ushr i).toByte())
                    i -= 8
                }
            }
            is CborValue.CText -> {
                val u = v.value.toByteArray(Charsets.UTF_8)
                head(3, u.size.toULong(), out)
                for (b in u) out.add(b)
            }
            is CborValue.CBytes -> {
                head(2, v.value.size.toULong(), out)
                for (b in v.value) out.add(b)
            }
            is CborValue.CArray -> {
                head(4, v.items.size.toULong(), out)
                for (x in v.items) enc(x, out)
            }
            is CborValue.CMap -> {
                head(5, v.entries.size.toULong(), out)
                for ((k, value) in v.entries) {
                    enc(k, out)
                    enc(value, out)
                }
            }
            is CborValue.CTag -> {
                head(6, v.tag, out)
                enc(v.value, out)
            }
        }
    }

    private class Cursor(val b: ByteArray) {
        var pos = 0
    }

    fun decode(b: ByteArray): CborValue {
        val cur = Cursor(b)
        val v = dec(cur, 0)
        if (cur.pos != b.size) throw CborError("trailing bytes after CBOR value")
        return v
    }

    private fun readArg(cur: Cursor, low: Int): ULong {
        if (low < 24) {
            cur.pos += 1
            return low.toULong()
        }
        val width = when (low) { 24 -> 1; 25 -> 2; 26 -> 4; 27 -> 8; else -> 0 }
        if (width == 0 || cur.pos >= cur.b.size || cur.b.size - cur.pos - 1 < width) {
            throw CborError("truncated CBOR argument")
        }
        return when (low) {
            24 -> {
                val v = cur.b[cur.pos + 1].toUByte().toULong()
                cur.pos += 2
                v
            }
            25 -> {
                val v = (cur.b[cur.pos + 1].toUByte().toULong() shl 8) or cur.b[cur.pos + 2].toUByte().toULong()
                cur.pos += 3
                v
            }
            26 -> {
                var v = 0uL
                for (i in 1..4) v = (v shl 8) or cur.b[cur.pos + i].toUByte().toULong()
                cur.pos += 5
                v
            }
            27 -> {
                var v = 0uL
                for (i in 1..8) v = (v shl 8) or cur.b[cur.pos + i].toUByte().toULong()
                cur.pos += 9
                v
            }
            else -> throw CborError("malformed CBOR additional info")
        }
    }

    private fun decodeUtf8(b: ByteArray, off: Int, len: Int): String = try {
        Charsets.UTF_8.newDecoder()
            .onMalformedInput(java.nio.charset.CodingErrorAction.REPORT)
            .onUnmappableCharacter(java.nio.charset.CodingErrorAction.REPORT)
            .decode(java.nio.ByteBuffer.wrap(b, off, len)).toString()
    } catch (_: java.nio.charset.CharacterCodingException) {
        throw CborError("invalid UTF-8 text string")
    }

    private fun dec(cur: Cursor, depth: Int): CborValue {
        if (depth > 64) throw CborError("CBOR nesting limit exceeded")
        if (cur.pos >= cur.b.size) throw CborError("unexpected end of CBOR input")
        val ib = cur.b[cur.pos].toUByte().toInt()
        val major = ib shr 5
        val low = ib and 0x1f
        if (major == 7) {
            return when (low) {
                20 -> {
                    cur.pos += 1
                    CborValue.CBool(false)
                }
                21 -> {
                    cur.pos += 1
                    CborValue.CBool(true)
                }
                22, 23 -> {
                    cur.pos += 1
                    CborValue.CNull
                }
                26 -> {
                    val bits = readArg(cur, low)
                    CborValue.CFloat(Float.fromBits((bits and 0xffffffffuL).toInt()).toDouble())
                }
                27 -> {
                    val bits = readArg(cur, low)
                    CborValue.CFloat(Double.fromBits(bits.toLong()))
                }
                else -> throw CborError("malformed CBOR simple/float")
            }
        }
        val arg = readArg(cur, low)
        return when (major) {
            0 -> CborValue.CUint(arg)
            1 -> {
                if (arg > Long.MAX_VALUE.toULong()) throw CborError("negative integer out of range")
                CborValue.CInt(-1L - arg.toLong())
            }
            2 -> {
                if (arg > (cur.b.size - cur.pos).toULong()) throw CborError("truncated byte string")
                val n = arg.toInt()
                val slice = cur.b.copyOfRange(cur.pos, cur.pos + n)
                cur.pos += n
                CborValue.CBytes(slice)
            }
            3 -> {
                if (arg > (cur.b.size - cur.pos).toULong()) throw CborError("truncated text string")
                val n = arg.toInt()
                val s = decodeUtf8(cur.b, cur.pos, n)
                cur.pos += n
                CborValue.CText(s)
            }
            4 -> {
                if (arg > (cur.b.size - cur.pos).toULong()) throw CborError("array length exceeds remaining input")
                val n = arg.toInt()
                val items = ArrayList<CborValue>(n)
                repeat(n) { items.add(dec(cur, depth + 1)) }
                CborValue.CArray(items)
            }
            5 -> {
                if (arg > (cur.b.size - cur.pos).toULong()) throw CborError("map length exceeds remaining input")
                val n = arg.toInt()
                val entries = ArrayList<Pair<CborValue, CborValue>>(n)
                repeat(n) {
                    val k = dec(cur, depth + 1)
                    val value = dec(cur, depth + 1)
                    entries.add(k to value)
                }
                CborValue.CMap(entries)
            }
            6 -> CborValue.CTag(arg, dec(cur, depth + 1))
            else -> throw CborError("malformed CBOR major type")
        }
    }

    fun mapGet(v: CborValue, key: String): CborValue? {
        if (v is CborValue.CMap) {
            for ((k, value) in v.entries) {
                if (k is CborValue.CText && k.value == key) return value
            }
        }
        return null
    }

    fun require(v: CborValue, key: String): CborValue =
        mapGet(v, key) ?: throw CborError("missing field '$key'")

    fun <T> expectLiteral(actual: CborValue, expected: CborValue, value: T): T {
        if (actual != expected) throw CborError("literal mismatch")
        return value
    }

    fun asLong(v: CborValue): Long = when (v) {
        is CborValue.CUint -> {
            if (v.value > Long.MAX_VALUE.toULong()) throw CborError("integer out of Long range")
            v.value.toLong()
        }
        is CborValue.CInt -> v.value
        else -> throw CborError("expected integer")
    }

    fun asULong(v: CborValue): ULong = when (v) {
        is CborValue.CUint -> v.value
        is CborValue.CInt -> {
            if (v.value < 0) throw CborError("expected unsigned integer")
            v.value.toULong()
        }
        else -> throw CborError("expected unsigned integer")
    }

    fun asDouble(v: CborValue): Double = when (v) {
        is CborValue.CFloat -> v.value
        is CborValue.CUint -> v.value.toDouble()
        is CborValue.CInt -> v.value.toDouble()
        else -> throw CborError("expected float")
    }

    fun asBoolean(v: CborValue): Boolean =
        (v as? CborValue.CBool)?.value ?: throw CborError("expected bool")

    fun asText(v: CborValue): String =
        (v as? CborValue.CText)?.value ?: throw CborError("expected text")

    fun asBytes(v: CborValue): ByteArray =
        (v as? CborValue.CBytes)?.value ?: throw CborError("expected bytes")

    fun asArray(v: CborValue): List<CborValue> =
        (v as? CborValue.CArray)?.items ?: throw CborError("expected array")

    fun asMap(v: CborValue): List<Pair<CborValue, CborValue>> =
        (v as? CborValue.CMap)?.entries ?: throw CborError("expected map")

    fun asTaggedText(v: CborValue, tag: ULong): String {
        if (v is CborValue.CTag && v.tag == tag) return asText(v.value)
        throw CborError("expected tag $tag")
    }

    // tag 4 decimal fraction [exponent, mantissa]; value = mantissa * 10^exponent, and
    // BigDecimal(unscaled, scale) = unscaled * 10^-scale, so scale = -exponent.
    fun asDecimal(v: CborValue): java.math.BigDecimal {
        if (v is CborValue.CTag && v.tag == 4uL) {
            val arr = asArray(v.value)
            if (arr.size == 2) {
                val exp = asLong(arr[0])
                val mant = asLong(arr[1])
                return java.math.BigDecimal(java.math.BigInteger.valueOf(mant), (-exp).toInt())
            }
        }
        throw CborError("expected tag 4 decimal")
    }
}
"#;

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn generate_validation(input: &WasmGeneratorInput, config: &KotlinConfig) -> Option<String> {
    let mut body = String::new();

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        let Some(group) = group else { continue };
        if !group.entries.iter().any(entry_has_check) {
            continue;
        }

        let class_name = pascal_case(&rule.name);
        body.push_str(&format!(
            "/** Validate a {class_name}; throws IllegalArgumentException on a constraint breach. */\n"
        ));
        body.push_str(&format!("fun {class_name}.validate() {{\n"));
        for entry in &group.entries {
            if let Some(key) = &entry.key {
                let prop = kotlin_prop_name(key, &entry.metadata);
                let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
                let field = FieldRef {
                    prop: &prop,
                    optional,
                };
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Constraint(constraint) = metadata {
                        emit_metadata_constraint(&mut body, field, &entry.value_type, constraint);
                    }
                }
                if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                    for op in constraints {
                        emit_control_op_check(&mut body, field, &entry.value_type, op);
                    }
                }
            }
        }
        body.push_str("}\n\n");
    }

    if body.is_empty() {
        return None;
    }
    let mut content = file_header(config, "Generated validation functions.");
    content.push_str(&body);
    Some(content)
}

/// A field's Kotlin property name plus whether it is nullable (optional). An
/// optional field's checks are guarded behind a `?.let { ... }` so a null value is
/// skipped rather than throwing on access.
#[derive(Clone, Copy)]
struct FieldRef<'a> {
    prop: &'a str,
    optional: bool,
}

/// Emit a `require(...)` guarded by optionality. `cond` is the *valid* condition; an
/// optional field only checks when present. The value is bound to `v` inside the
/// guard so `cond`/`message` reference `v`.
fn push_require(body: &mut String, field: FieldRef, cond: &str, message: &str) {
    if field.optional {
        // The value is bound to `v` inside the `?.let`, so an absent (null) optional
        // is skipped rather than dereferenced.
        body.push_str(&format!("    {}?.let {{ v ->\n", field.prop));
        body.push_str(&format!("        require({cond}) {{ \"{message}\" }}\n"));
        body.push_str("    }\n");
    } else {
        // A required field inlines its property name where the guard would bind `v`.
        // Replace only the standalone `v` placeholder token, never a literal `v` inside a
        // regex pattern or string bound, which a blanket char replace would corrupt.
        body.push_str(&format!(
            "    require({}) {{ \"{message}\" }}\n",
            substitute_placeholder(cond, field.prop)
        ));
    }
}

/// Replace the standalone `v` binding placeholder in a guard condition with `prop`,
/// leaving any `v` that is part of a larger identifier or inside a quoted string literal
/// (e.g. a regex pattern like `"^v[0-9]+$"`) untouched.
fn substitute_placeholder(cond: &str, prop: &str) -> String {
    let chars: Vec<char> = cond.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(cond.len());
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        let prev_ident = i > 0 && is_ident(chars[i - 1]);
        let next_ident = i + 1 < chars.len() && is_ident(chars[i + 1]);
        if c == 'v' && !prev_ident && !next_ident {
            out.push_str(prop);
        } else {
            out.push(c);
        }
    }
    out
}

fn emit_metadata_constraint(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
) {
    let name = field.prop;
    match constraint {
        CsilValidationConstraint::MinLength(n) => push_require(
            body,
            field,
            &format!("v.length >= {n}"),
            &format!("field '{name}' must have at least {n} characters"),
        ),
        CsilValidationConstraint::MaxLength(n) => push_require(
            body,
            field,
            &format!("v.length <= {n}"),
            &format!("field '{name}' must have at most {n} characters"),
        ),
        CsilValidationConstraint::MinItems(n) => push_require(
            body,
            field,
            &format!("v.size >= {n}"),
            &format!("field '{name}' must have at least {n} items"),
        ),
        CsilValidationConstraint::MaxItems(n) => push_require(
            body,
            field,
            &format!("v.size <= {n}"),
            &format!("field '{name}' must have at most {n} items"),
        ),
        CsilValidationConstraint::MinValue(v) => {
            emit_ordered_check(body, field, value_type, (">=", "at least"), v)
        }
        CsilValidationConstraint::MaxValue(v) => {
            emit_ordered_check(body, field, value_type, ("<=", "at most"), v)
        }
        CsilValidationConstraint::Custom { name: cname, value } => {
            if cname == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                emit_regex_check(body, field, pattern);
            }
        }
    }
}

fn emit_control_op_check(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
) {
    match op {
        CsilControlOperator::GreaterEqual(v) => {
            emit_ordered_check(body, field, value_type, (">=", "at least"), v)
        }
        CsilControlOperator::LessEqual(v) => {
            emit_ordered_check(body, field, value_type, ("<=", "at most"), v)
        }
        CsilControlOperator::GreaterThan(v) => {
            emit_ordered_check(body, field, value_type, (">", "greater than"), v)
        }
        CsilControlOperator::LessThan(v) => {
            emit_ordered_check(body, field, value_type, ("<", "less than"), v)
        }
        CsilControlOperator::Equal(v) => {
            emit_ordered_check(body, field, value_type, ("==", "equal to"), v)
        }
        CsilControlOperator::NotEqual(v) => {
            emit_ordered_check(body, field, value_type, ("!=", "not equal to"), v)
        }
        CsilControlOperator::Size(size) => emit_size_check(body, field, value_type, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(body, field, pattern),
        // Applied as a constructor default, not validated.
        CsilControlOperator::Default(_) => {}
        // Encoding-only operators carry no runtime check.
        _ => {}
    }
}

/// Emit one ordered comparison. Numeric/decimal/timestamp fields share one shape:
/// `decimal` (BigDecimal) and `timestamp` (Instant) both have `compareTo`, so the
/// emitted `v <op> bound` works for all three once the bound is the right type.
fn emit_ordered_check(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
) {
    let (kt_op, desc) = op;
    let name = field.prop;
    let kind = ordered_field_kind(value_type);
    let (bound_expr, shown) = match kind {
        OrderedKind::Numeric => {
            // The bound must carry the field's numeric suffix (`1uL`/`1L`/`1.0`); a bare
            // `Int` literal does not compare against a `ULong`/`Long`/`Double` field.
            (
                literal_to_kotlin_typed(value, value_type),
                literal_to_kotlin(value),
            )
        }
        OrderedKind::Decimal => {
            let Some(text) = literal_as_text(value) else {
                return;
            };
            (
                format!("java.math.BigDecimal(\"{text}\")"),
                kotlin_escape(&text),
            )
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_text(value) else {
                return;
            };
            (
                format!("java.time.Instant.parse(\"{text}\")"),
                kotlin_escape(&text),
            )
        }
    };
    // BigDecimal/Instant compare through compareTo; the `<op>` operator desugars to
    // it, but `==`/`!=` on those types is structural-but-scale-sensitive, so use an
    // explicit compareTo for the equality forms to match value ordering.
    let cond = match (kind, kt_op) {
        (OrderedKind::Numeric, _) => format!("v {kt_op} {bound_expr}"),
        (_, "==") => format!("v.compareTo({bound_expr}) == 0"),
        (_, "!=") => format!("v.compareTo({bound_expr}) != 0"),
        (_, _) => format!("v {kt_op} {bound_expr}"),
    };
    let message = format!("field '{name}' must be {desc} {shown}");
    push_require(body, field, &cond, &message);
}

fn emit_size_check(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    size: &CsilSizeConstraint,
) {
    let name = field.prop;
    // Kotlin's `String` exposes `.length`; `ByteArray`/`List` expose `.size`. A `.size`
    // constraint on a text field would not compile against `.size`, so pick the accessor
    // (and the noun) from the field's base type.
    let (accessor, unit) = match size_accessor(value_type) {
        SizeAccessor::Length => ("length", "characters"),
        SizeAccessor::Size => ("size", "elements"),
    };
    let mut one = |kt_op: &str, n: u64, word: &str| {
        push_require(
            body,
            field,
            &format!("v.{accessor} {kt_op} {n}"),
            &format!("field '{name}' must have {word} {n} {unit}"),
        );
    };
    match size {
        CsilSizeConstraint::Exact(n) => one("==", *n, "exactly"),
        CsilSizeConstraint::Min(n) => one(">=", *n, "at least"),
        CsilSizeConstraint::Max(n) => one("<=", *n, "at most"),
        CsilSizeConstraint::Range { min, max } => {
            one(">=", *min, "at least");
            one("<=", *max, "at most");
        }
    }
}

fn emit_regex_check(body: &mut String, field: FieldRef, pattern: &str) {
    let name = field.prop;
    let escaped = kotlin_escape(pattern);
    push_require(
        body,
        field,
        &format!("Regex(\"{escaped}\").containsMatchIn(v)"),
        &format!("field '{name}' must match pattern '{escaped}'"),
    );
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

// ---------------------------------------------------------------------------
// Type & name mapping
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum OrderedKind {
    Numeric,
    Decimal,
    Timestamp,
}

/// Which Kotlin length member a `.size` constraint compiles against: `String` has
/// `.length`, while `ByteArray`/`List` have `.size`.
enum SizeAccessor {
    Length,
    Size,
}

fn size_accessor(value_type: &CsilTypeExpression) -> SizeAccessor {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    match base {
        CsilTypeExpression::Builtin(name) if name == "text" || name == "tstr" => {
            SizeAccessor::Length
        }
        _ => SizeAccessor::Size,
    }
}

fn ordered_field_kind(value_type: &CsilTypeExpression) -> OrderedKind {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    if let CsilTypeExpression::Builtin(name) = base {
        match name.as_str() {
            "decimal" => OrderedKind::Decimal,
            "timestamp" => OrderedKind::Timestamp,
            _ => OrderedKind::Numeric,
        }
    } else {
        OrderedKind::Numeric
    }
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` arm of a `Res / ServiceError` union — the error half is surfaced
/// as a thrown `ClientError`, not part of the typed response.
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

fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

fn map_csil_type_to_kotlin(
    type_expr: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
) -> String {
    let base = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => "Long".to_string(),
            "uint" => "ULong".to_string(),
            "float" | "float64" | "double" => "Double".to_string(),
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "ByteArray".to_string(),
            "bool" => "Boolean".to_string(),
            // CBOR tag 0 RFC3339 instant; java.time.Instant is exact and stdlib.
            "timestamp" => "java.time.Instant".to_string(),
            // CBOR tag 4 exact decimal; BigDecimal is the JVM-idiomatic exact type.
            "decimal" => "java.math.BigDecimal".to_string(),
            "nil" | "null" => "Unit".to_string(),
            // `any` rides as the codec's own CBOR value model so it passes through
            // byte-identically, the JVM analog of Rust's `CsilCborValue` mapping.
            "any" => "CborValue".to_string(),
            other => pascal_case(other),
        },
        CsilTypeExpression::Reference(name) => pascal_case(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("List<{}>", map_csil_type_to_kotlin(element_type, &None))
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "Map<{}, {}>",
            map_csil_type_to_kotlin(key, &None),
            map_csil_type_to_kotlin(value, &None)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => {
            return map_csil_type_to_kotlin(base_type, occurrence);
        }
        // Kotlin has no anonymous tuple; a fixed-shape array degrades to List<Any?>.
        CsilTypeExpression::Tuple(_) => "List<Any?>".to_string(),
        CsilTypeExpression::Literal(lit) => match lit {
            CsilLiteralValue::Integer(_) => "Long".to_string(),
            CsilLiteralValue::Float(_) => "Double".to_string(),
            CsilLiteralValue::Text(_) => "String".to_string(),
            CsilLiteralValue::Bool(_) => "Boolean".to_string(),
            CsilLiteralValue::Bytes(_) => "ByteArray".to_string(),
            CsilLiteralValue::Null => "Unit".to_string(),
            CsilLiteralValue::Array(_) => "Any".to_string(),
        },
        CsilTypeExpression::Choice(choices) => {
            let reduced = success_type(&CsilTypeExpression::Choice(choices.clone()));
            match reduced {
                CsilTypeExpression::Choice(_) => "Any".to_string(),
                other => map_csil_type_to_kotlin(&other, &None),
            }
        }
        _ => "Any".to_string(),
    };
    match occurrence {
        Some(CsilOccurrence::Optional) => format!("{base}?"),
        _ => base,
    }
}

/// The default expression for a `data class` field: an explicit `@default`/`.default`
/// literal, or `null` for an optional field with no declared default.
fn field_default(entry: &CsilGroupEntry, input: &WasmGeneratorInput) -> Option<String> {
    if let Some(value) = entry_default_value(entry) {
        if let Some(rendered) = enum_default_kotlin(value, &entry.value_type, input) {
            return Some(rendered);
        }
        return Some(literal_to_kotlin_typed(value, &entry.value_type));
    }
    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
        return Some("null".to_string());
    }
    None
}

fn entry_default_value(entry: &CsilGroupEntry) -> Option<&CsilLiteralValue> {
    for metadata in &entry.metadata {
        if let CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, value }) =
            metadata
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

fn literal_to_kotlin(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{}\"", kotlin_escape(s)),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| format!("{b}.toByte()"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("byteArrayOf({values})")
        }
        CsilLiteralValue::Array(elements) => {
            let inner: Vec<String> = elements.iter().map(literal_to_kotlin).collect();
            format!("listOf({})", inner.join(", "))
        }
    }
}

fn kotlin_literal_cbor_expr(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) if *i >= 0 => format!("CborValue.CUint({i}uL)"),
        CsilLiteralValue::Integer(i) => format!("CborValue.CInt({i}L)"),
        CsilLiteralValue::Float(f) => format!("CborValue.CFloat({f})"),
        CsilLiteralValue::Text(s) => format!("CborValue.CText(\"{}\")", kotlin_escape(s)),
        CsilLiteralValue::Bool(b) => format!("CborValue.CBool({b})"),
        CsilLiteralValue::Null => "CborValue.CNull".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| format!("{b}.toByte()"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("CborValue.CBytes(byteArrayOf({values}))")
        }
        CsilLiteralValue::Array(items) => {
            let values = items
                .iter()
                .map(kotlin_literal_cbor_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("CborValue.CArray(listOf({values}))")
        }
    }
}

fn kotlin_literal_value_expr(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => format!("{i}L"),
        CsilLiteralValue::Null => "Unit".to_string(),
        other => literal_to_kotlin(other),
    }
}

/// A literal rendered for a specifically-typed field: `decimal`/`timestamp` bounds
/// parse into BigDecimal/Instant; everything else is a plain literal.
fn literal_to_kotlin_typed(value: &CsilLiteralValue, value_type: &CsilTypeExpression) -> String {
    match ordered_field_kind(value_type) {
        OrderedKind::Decimal => {
            if let Some(text) = literal_as_text(value) {
                return format!("java.math.BigDecimal(\"{}\")", kotlin_escape(&text));
            }
        }
        OrderedKind::Timestamp => {
            if let Some(text) = literal_as_text(value) {
                return format!("java.time.Instant.parse(\"{}\")", kotlin_escape(&text));
            }
        }
        OrderedKind::Numeric => {
            // A bare integer literal is `Int` in Kotlin, which does not implicitly widen
            // to the field's mapped numeric type: `ULong`/`Long` need a `uL`/`L` suffix
            // and `Double` a decimal point, or the data-class default fails to compile
            // ("type mismatch: expected ULong, actual Int").
            if let CsilLiteralValue::Integer(i) = value {
                match map_csil_type_to_kotlin(value_type, &None).as_str() {
                    "ULong" => return format!("{i}uL"),
                    "Long" => return format!("{i}L"),
                    "Double" => return format!("{i}.0"),
                    _ => {}
                }
            }
        }
    }
    literal_to_kotlin(value)
}

fn literal_as_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
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

/// Strip a trailing `Service` suffix and PascalCase the remainder, used only for
/// Kotlin identifiers (the client class name and per-op codec stems). Wire strings
/// carry the verbatim CSIL service name instead (csil-rpc-transport.md §1.1).
fn service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

fn wire_name_from_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

/// The Kotlin property name for a field key: a `@kotlin_name` override, otherwise
/// the wire name lowerCamelCased.
fn kotlin_prop_name(key: &CsilGroupKey, metadata: &[CsilFieldMetadata]) -> String {
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "kotlin_name"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(kt_name) = &param.value
        {
            return kt_name.clone();
        }
    }
    kotlin_safe_ident(&camel_case(&wire_name_from_key(key)))
}

/// Backtick-escape a name that collides with a Kotlin hard keyword (`when`, `is`, `in`,
/// …) so it is a legal identifier wherever a property/argument name appears. A
/// field like `when: timestamp` would otherwise emit `val when: …`, a parse error.
fn kotlin_safe_ident(name: &str) -> String {
    const HARD_KEYWORDS: &[&str] = &[
        "as",
        "break",
        "class",
        "continue",
        "do",
        "else",
        "false",
        "for",
        "fun",
        "if",
        "in",
        "interface",
        "is",
        "null",
        "object",
        "package",
        "return",
        "super",
        "this",
        "throw",
        "true",
        "try",
        "typealias",
        "typeof",
        "val",
        "var",
        "when",
        "while",
    ];
    if HARD_KEYWORDS.contains(&name) {
        format!("`{name}`")
    } else {
        name.to_string()
    }
}

fn type_override(metadata: &[CsilFieldMetadata]) -> Option<String> {
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "kotlin_type"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(kt_type) = &param.value
        {
            return Some(kt_type.clone());
        }
    }
    None
}

fn field_description(metadata: &[CsilFieldMetadata]) -> Option<String> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.replace(['\n', '\r'], " "))
        } else {
            None
        }
    })
}

fn kotlin_method_name(name: &str) -> String {
    camel_case(name)
}

/// PascalCase for type names: split on `_`/`-`, capitalize each word.
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

/// lowerCamelCase for properties/functions: PascalCase, then lowercase the leading
/// character.
fn camel_case(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
    }
}

/// SCREAMING_SNAKE_CASE for wire-id `const` names.
fn screaming_snake(s: &str) -> String {
    s.split(['_', '-'])
        .map(|w| w.to_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Escape a string for a Kotlin double-quoted literal.
fn kotlin_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn serialize_and_return_ptr<T: serde::Serialize>(data: &T) -> *mut u8 {
    let serialized = match serde_json::to_string(data) {
        Ok(json) => json,
        Err(_) => return std::ptr::null_mut(),
    };
    let bytes = serialized.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }
    ptr
}

fn create_error_result(error_code: i32) -> *mut u8 {
    let error_output = WasmGeneratorOutput {
        files: vec![],
        warnings: vec![GeneratorWarning {
            level: WarningLevel::Warning,
            message: format!("Generator failed with error code: {error_code}"),
            location: None,
            suggestion: None,
        }],
        stats: GenerationStats::default(),
    };
    serialize_and_return_ptr(&error_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_checks_declared_lengths_before_conversion_or_allocation() {
        assert_eq!(
            CODEC_RUNTIME_KT
                .matches("if (arg > (cur.b.size - cur.pos).toULong())")
                .count(),
            4
        );
        assert!(CODEC_RUNTIME_KT.contains("array length exceeds remaining input"));
        assert!(CODEC_RUNTIME_KT.contains("map length exceeds remaining input"));
        assert!(CODEC_RUNTIME_KT.contains("if (depth > 64)"));
        assert!(CODEC_RUNTIME_KT.contains("CodingErrorAction.REPORT"));
        assert!(CODEC_RUNTIME_KT.contains("cur.b.size - cur.pos - 1 < width"));
    }
    use csilgen_common::{
        CsilPosition, CsilRule, CsilServiceOperation, CsilSpecSerialized, GeneratorConfig,
    };

    fn spec(target: &str, rules: Vec<CsilRule>) -> WasmGeneratorInput {
        let service_count = rules
            .iter()
            .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
            .count();
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules,
                source_content: None,
                service_count,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "kotlin".to_string(),
                version: "0.1.0".to_string(),
                description: String::new(),
                target: "kotlin".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    fn entry(
        name: &str,
        value_type: CsilTypeExpression,
        occurrence: Option<CsilOccurrence>,
    ) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type,
            occurrence,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    fn builtin(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Builtin(name.to_string())
    }

    fn reference(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Reference(name.to_string())
    }

    fn op(
        name: &str,
        input: CsilTypeExpression,
        output: CsilTypeExpression,
        direction: CsilServiceDirection,
        wire_id: Option<u64>,
    ) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: input,
            output_type: output,
            direction,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id,
        }
    }

    fn content(out: &WasmGeneratorOutput, suffix: &str) -> String {
        out.files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .map(|f| f.content.clone())
            .unwrap_or_else(|| panic!("no file ending in {suffix}"))
    }

    /// An optional `bytes` field carries three distinct states — absent,
    /// present-and-empty, present-and-non-empty — and the codec must decide presence by
    /// whether the value is set, never by whether it is non-empty (cbor-wire-contract.md
    /// "Optional fields"). An `isNotEmpty()` guard would collapse present-empty into
    /// absent and silently lose a caller's "replace this with nothing".
    #[test]
    fn optional_bytes_encodes_on_presence_not_emptiness() {
        let record = CsilGroupExpression {
            entries: vec![
                entry("id", CsilTypeExpression::Builtin("text".to_string()), None),
                entry(
                    "payload",
                    CsilTypeExpression::Builtin("bytes".to_string()),
                    Some(CsilOccurrence::Optional),
                ),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("UpdateRequest", CsilRuleType::GroupDef(record))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        let codec = content(&out, "Codec.kt");

        // A nullable ByteArray distinguishes null (absent) from an empty array
        // (present-and-empty).
        assert!(
            types.contains("val payload: ByteArray? = null"),
            "optional bytes needs a presence-carrying type:\n{types}"
        );
        // Encode gates on presence (`?.let`), not on emptiness.
        assert!(
            codec.contains("this.payload?.let { csilV ->"),
            "encode must gate on presence, not emptiness:\n{codec}"
        );
        assert!(
            !codec.contains("payload.isNotEmpty()"),
            "encode must not gate on emptiness:\n{codec}"
        );
        // Decode gates on the key being present in the map, so a present empty byte
        // string stays non-null rather than collapsing to absent.
        assert!(
            codec.contains("CsilCbor.mapGet(cbor, \"payload\")?.let"),
            "decode must gate on key presence:\n{codec}"
        );
    }

    #[test]
    fn numeric_default_carries_kotlin_type_suffix() {
        // A bare integer literal is `Int`; a `ULong`/`Long`/`Double` field default needs
        // the matching suffix or decimal point, or the data-class default fails to
        // compile with a type mismatch.
        let uint = CsilTypeExpression::Builtin("uint".to_string());
        assert_eq!(
            literal_to_kotlin_typed(&CsilLiteralValue::Integer(50), &uint),
            "50uL"
        );
        let int = CsilTypeExpression::Builtin("int".to_string());
        assert_eq!(
            literal_to_kotlin_typed(&CsilLiteralValue::Integer(7), &int),
            "7L"
        );
        let float = CsilTypeExpression::Builtin("float".to_string());
        assert_eq!(
            literal_to_kotlin_typed(&CsilLiteralValue::Integer(3), &float),
            "3.0"
        );
    }

    #[test]
    fn enum_field_default_renders_as_enum_constant() {
        // An enum-typed field's `.default` is the wire literal, but the property's
        // declared type is the enum — so a bare `"green"` / `2` would not typecheck.
        // The default must render as the enum constant (`Color.Green` / `Priority.V2`),
        // for both the optional (`?`) and required forms. Regression for the Kotlin
        // generator emitting a raw `String` default for enum fields.
        let color = || CsilTypeExpression::Constrained {
            base_type: Box::new(reference("Color")),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "green".to_string(),
            ))],
        };
        let prio = || CsilTypeExpression::Constrained {
            base_type: Box::new(reference("Priority")),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Integer(2))],
        };
        let record = CsilGroupExpression {
            entries: vec![
                entry("opt_tone", color(), Some(CsilOccurrence::Optional)),
                entry("req_tone", color(), None),
                entry("opt_rank", prio(), Some(CsilOccurrence::Optional)),
                entry("req_rank", prio(), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![
                rule(
                    "Color",
                    CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("red".to_string())),
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("green".to_string())),
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("blue".to_string())),
                    ])),
                ),
                rule(
                    "Priority",
                    CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
                        CsilTypeExpression::Literal(CsilLiteralValue::Integer(2)),
                        CsilTypeExpression::Literal(CsilLiteralValue::Integer(3)),
                    ])),
                ),
                rule("Palette", CsilRuleType::GroupDef(record)),
            ],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("val optTone: Color? = Color.Green"));
        assert!(types.contains("val reqTone: Color = Color.Green"));
        assert!(types.contains("val optRank: Priority? = Priority.V2"));
        assert!(types.contains("val reqRank: Priority = Priority.V2"));
        // The raw-string form is exactly the bug, so it must not appear.
        assert!(!types.contains("= \"green\""));
    }

    #[test]
    fn case_helpers() {
        assert_eq!(pascal_case("user_name"), "UserName");
        assert_eq!(pascal_case("deposit-claim"), "DepositClaim");
        assert_eq!(camel_case("user_name"), "userName");
        assert_eq!(camel_case("deposit-claim"), "depositClaim");
        assert_eq!(screaming_snake("deposit-claim"), "DEPOSIT_CLAIM");
    }

    #[test]
    fn data_class_camel_fields_and_nullable_optional() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("user_name", builtin("text"), None),
                entry(
                    "created_at",
                    builtin("timestamp"),
                    Some(CsilOccurrence::Optional),
                ),
                entry("retry_count", builtin("int"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("UserProfile", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("data class UserProfile("));
        assert!(types.contains("val userName: String"));
        assert!(types.contains("// wire key: user_name"));
        assert!(types.contains("val createdAt: java.time.Instant? = null"));
        assert!(types.contains("val retryCount: Long"));
    }

    #[test]
    fn byte_array_gets_content_equality() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("blob", builtin("bytes"), None),
                entry("sig", builtin("bytes"), Some(CsilOccurrence::Optional)),
                entry("name", builtin("text"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Payload", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("val blob: ByteArray"));
        assert!(types.contains("override fun equals(other: Any?): Boolean"));
        assert!(types.contains("override fun hashCode(): Int"));
        // Non-null ByteArray: plain contentEquals (a safe call would warn "unnecessary").
        assert!(types.contains("if (!blob.contentEquals(other.blob)) return false"));
        assert!(types.contains("var result = blob.contentHashCode()"));
        // Nullable ByteArray: null-safe content compare.
        assert!(types.contains("sig?.contentEquals(other.sig) ?: (other.sig == null)"));
        assert!(types.contains("(sig?.contentHashCode() ?: 0)"));
    }

    #[test]
    fn type_mapping_covers_builtins_and_containers() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("a", builtin("uint"), None),
                entry("b", builtin("float"), None),
                entry("c", builtin("bool"), None),
                entry("d", builtin("decimal"), None),
                entry(
                    "e",
                    CsilTypeExpression::Array {
                        element_type: Box::new(builtin("text")),
                        occurrence: None,
                    },
                    None,
                ),
                entry(
                    "f",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                    None,
                ),
                entry("g", reference("other_type"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Mix", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("val a: ULong"));
        assert!(types.contains("val b: Double"));
        assert!(types.contains("val c: Boolean"));
        assert!(types.contains("val d: java.math.BigDecimal"));
        assert!(types.contains("val e: List<String>"));
        assert!(types.contains("val f: Map<String, Long>"));
        assert!(types.contains("val g: OtherType"));
    }

    #[test]
    fn type_choice_is_sealed_interface() {
        let out = process_generation(spec(
            "kotlin",
            vec![rule(
                "StringOrNumber",
                CsilRuleType::TypeChoice(vec![builtin("text"), builtin("int")]),
            )],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("sealed interface StringOrNumber"));
        assert!(
            types.contains("data class StringOrNumberArm1(val value: String) : StringOrNumber")
        );
        assert!(types.contains("data class StringOrNumberArm2(val value: Long) : StringOrNumber"));
    }

    #[test]
    fn group_choice_is_sealed_interface_of_records() {
        let g1 = CsilGroupExpression {
            entries: vec![entry("x", builtin("int"), None)],
        };
        let g2 = CsilGroupExpression {
            entries: vec![entry("y", builtin("text"), None)],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Shape", CsilRuleType::GroupChoice(vec![g1, g2]))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("sealed interface Shape"));
        assert!(types.contains("data class ShapeChoice1("));
        assert!(types.contains(") : Shape"));
        assert!(types.contains("val x: Long"));
        assert!(types.contains("val y: String"));
    }

    /// The confirmed mixed-kind misclassification bug: `"a" / 1` is ALL-literal
    /// (a text literal and an integer literal), so per the shared
    /// `csilgen_common::classify_choice` contract it must be an `Enum`, not a
    /// `Union` — before this fix, `ChoiceKind` only had `EnumText`/`EnumInt`, so a
    /// mixed-kind literal choice fell through to `Union` and rendered as a
    /// `sealed interface` tagged sum instead of a bare-literal enum.
    #[test]
    fn mixed_kind_literal_choice_is_a_bare_enum_not_a_union() {
        let grade = rule(
            "Grade",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Literal(CsilLiteralValue::Text("a".to_string())),
                CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
            ])),
        );
        // `generate_codec` only emits `Codec.kt` when the spec has at least one
        // record; a holder field forces the codec (and thus the enum codec) to
        // actually be emitted.
        let holder = rule(
            "Holder",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![entry("grade", reference("Grade"), None)],
            }),
        );
        let out = process_generation(spec("kotlin", vec![grade, holder])).unwrap();
        let types = content(&out, "Types.kt");
        assert!(
            types.contains("enum class Grade { A, V1 }"),
            "expected a bare-literal enum, got:\n{types}"
        );
        assert!(
            !types.contains("sealed interface Grade"),
            "must not classify as a union, got:\n{types}"
        );

        let codec = content(&out, "Codec.kt");
        assert!(codec.contains("Grade.A -> CborValue.CText(\"a\")"));
        assert!(codec.contains("Grade.V1 -> CborValue.CUint(1uL)"));
        assert!(codec.contains("CborValue.CText(\"a\") -> Grade.A"));
        assert!(codec.contains("CborValue.CUint(1uL) -> Grade.V1"));
        assert!(codec.contains("else -> throw CborError(\"unknown Grade value\")"));
        // No tagged-sum union codec artifacts (variant wrapper classes/indices).
        assert!(!codec.contains("GradeVariant"));
    }

    /// A choice mixing three-plus literal kinds, including a value whose natural
    /// name basis would collide (`"true"` the text literal PascalCases to `True`,
    /// the same basis `Bool(true)` uses) — `unique_variant` must disambiguate so
    /// the emitted enum still compiles with two distinct constant names.
    #[test]
    fn mixed_kind_literal_choice_disambiguates_colliding_variant_names() {
        let flexible = rule(
            "Flexible",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Literal(CsilLiteralValue::Text("true".to_string())),
                CsilTypeExpression::Literal(CsilLiteralValue::Bool(true)),
                CsilTypeExpression::Literal(CsilLiteralValue::Float(1.5)),
            ])),
        );
        let holder = rule(
            "Holder",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![entry("flexible", reference("Flexible"), None)],
            }),
        );
        let out = process_generation(spec("kotlin", vec![flexible, holder])).unwrap();
        let types = content(&out, "Types.kt");
        assert!(
            types.contains("enum class Flexible { True, True2, F1_5 }"),
            "expected disambiguated variant names, got:\n{types}"
        );
        let codec = content(&out, "Codec.kt");
        assert!(codec.contains("Flexible.True -> CborValue.CText(\"true\")"));
        assert!(codec.contains("Flexible.True2 -> CborValue.CBool(true)"));
        assert!(codec.contains("Flexible.F1_5 -> CborValue.CFloat(1.5)"));
    }

    /// A `.default` on a mixed-kind enum field must render as the enum constant
    /// (`Grade.V1`), not the raw wire literal — parity with the EnumText/EnumInt
    /// default-rendering path (`enum_default_kotlin`).
    #[test]
    fn mixed_kind_enum_default_renders_as_enum_constant() {
        let grade = rule(
            "Grade",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Literal(CsilLiteralValue::Text("a".to_string())),
                CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
            ])),
        );
        let holder = rule(
            "Holder",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("grade".to_string())),
                    value_type: CsilTypeExpression::Constrained {
                        base_type: Box::new(reference("Grade")),
                        constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Integer(
                            1,
                        ))],
                    },
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
        );
        let out = process_generation(spec("kotlin", vec![grade, holder])).unwrap();
        let types = content(&out, "Types.kt");
        assert!(
            types.contains("Grade.V1"),
            "expected the default to render as the Grade.V1 enum constant, got:\n{types}"
        );
    }

    /// Pascal-collision regression (mirrors `crates/csilgen-common/src/hoist.rs`'s
    /// own `case_insensitive_collision_between_existing_and_synthesized_rule_is_
    /// disambiguated` test): an existing rule named `UserData` and a synthesized
    /// name `User_data` (owner `User`, inline mixed-choice field `data`)
    /// pascal-collide — both canonicalize to `"userdata"`. The shared hoister's
    /// case-insensitive reservation must disambiguate the later one, or this
    /// generator would emit two Kotlin declarations for the same identifier.
    #[test]
    fn hoisted_inline_composite_name_disambiguates_against_existing_collision() {
        let user_data = rule(
            "UserData",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![entry("value", builtin("text"), None)],
            }),
        );
        // `User.data` is an inline mixed choice, so it hoists to a synthesized rule
        // named `User_data` — which pascal-collides with `UserData` above.
        let user = rule(
            "User",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![entry(
                    "data",
                    CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("x".to_string())),
                        reference("UserData"),
                    ]),
                    None,
                )],
            }),
        );
        let out = process_generation(spec("kotlin", vec![user_data, user])).unwrap();
        let types = content(&out, "Types.kt");
        // `UserData` survives exactly once; the synthesized name must not also
        // render as `UserData` (a duplicate, non-compiling declaration).
        assert_eq!(
            types.matches("data class UserData(").count(),
            1,
            "UserData must be declared exactly once, got:\n{types}"
        );
        assert!(
            types.contains("val data: User"),
            "expected the User record's data field, got:\n{types}"
        );
    }

    #[test]
    fn server_interface_routers_and_wire_ids() {
        let service = CsilServiceDefinition {
            operations: vec![
                op(
                    "list-events",
                    reference("Query"),
                    reference("Result"),
                    CsilServiceDirection::Unidirectional,
                    Some(1),
                ),
                op(
                    "play",
                    reference("Move"),
                    builtin("null"),
                    CsilServiceDirection::Bidirectional,
                    Some(2),
                ),
            ],
            wire_id: Some(7),
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("MatchService", CsilRuleType::ServiceDef(service))],
        ))
        .unwrap();
        let services = content(&out, "Services.kt");
        assert!(services.contains("interface Codec"));
        assert!(services.contains("interface MatchService {"));
        assert!(services.contains("fun listEvents(request: Query): Result"));
        assert!(services.contains("fun play(message: Move)"));
        assert!(services.contains("object MatchServiceWireIds"));
        assert!(services.contains("const val SERVICE: ULong = 7uL"));
        assert!(services.contains("const val OP_PLAY: ULong = 2uL"));
        assert!(services.contains(
            "fun routeMatchServiceChannel(handlers: MatchService, codec: Codec, op: String, data: ByteArray)"
        ));
        // The verbose router dispatches on the verbatim (kebab-case) wire op name.
        assert!(services.contains("\"play\" -> {"));
        assert!(services.contains("handlers.play(message)"));
        assert!(services.contains("fun routeMatchServiceChannelCompact(handlers: MatchService, codec: Codec, op: ULong, data: ByteArray)"));
        assert!(services.contains("2uL -> {"));
        assert!(services.contains("fun encodeMatchServicePlay(codec: Codec, message:"));
        assert!(services.contains("Pair(\"play\", codec.encode(message))"));
    }

    #[test]
    fn client_target_typed_client_strips_service_error() {
        let service = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                reference("SubmitTaskRequest"),
                CsilTypeExpression::Choice(vec![
                    reference("SubmitTaskResponse"),
                    reference("ServiceError"),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-client",
            vec![
                rule(
                    "SubmitTaskRequest",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("queue", builtin("text"), None)],
                    }),
                ),
                rule(
                    "SubmitTaskResponse",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("ok", builtin("bool"), None)],
                    }),
                ),
                rule("CorndogsService", CsilRuleType::ServiceDef(service)),
            ],
        ))
        .unwrap();
        let client = content(&out, "Client.kt");
        assert!(client.contains("interface Transport"));
        assert!(client.contains("class ClientError"));
        // The carrier seam is raw bytes.
        assert!(
            client.contains("fun call(service: String, op: String, request: ByteArray): ByteArray")
        );
        assert!(client.contains("class CorndogsClient(private val transport: Transport)"));
        assert!(client.contains("fun submitTask(request: SubmitTaskRequest): SubmitTaskResponse"));
        // Typed byte seam: the request serializes itself, the carrier moves bytes, the
        // response decodes back. Wire strings are the verbatim CSIL service and op names
        // so a Kotlin client reaches the same endpoint as its peers.
        assert!(client.contains(
            "decode<SubmitTaskResponse>(transport.call(\"CorndogsService\", \"submit-task\", encode(request)))"
        ));
        assert!(!client.contains(" as SubmitTaskResponse"));
        assert!(!client.contains("\"corndogs\""));
        assert!(!client.contains("\"SubmitTask\""));
        assert!(!out.files.iter().any(|f| f.path.ends_with("Services.kt")));
    }

    #[test]
    fn typesonly_skips_service_surface() {
        let service = CsilServiceDefinition {
            operations: vec![op(
                "ping",
                builtin("null"),
                reference("Pong"),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-typesonly",
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("v", builtin("int"), None)],
                    }),
                ),
                rule("PingService", CsilRuleType::ServiceDef(service)),
            ],
        ))
        .unwrap();
        assert!(out.files.iter().any(|f| f.path.ends_with("Types.kt")));
        assert!(!out.files.iter().any(|f| f.path.ends_with("Services.kt")));
        assert!(!out.files.iter().any(|f| f.path.ends_with("Client.kt")));
    }

    #[test]
    fn unknown_subtarget_errors() {
        let r = process_generation(spec(
            "kotlin-bogus",
            vec![rule(
                "X",
                CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            )],
        ));
        assert!(r.is_err());
    }

    #[test]
    fn validation_guards_optional_and_inlines_required() {
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        };
        let mut name_entry = entry("name", builtin("text"), Some(CsilOccurrence::Optional));
        name_entry.metadata = vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinLength(2),
        )];
        let group = CsilGroupExpression {
            entries: vec![name_entry, entry("tags", constrained, None)],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Form", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let validation = content(&out, "Validation.kt");
        assert!(validation.contains("fun Form.validate()"));
        assert!(validation.contains("name?.let { v ->"));
        assert!(validation.contains("v.length >= 2"));
        // `tags` is a text field, so a `.size` constraint must compile against `.length`
        // (Kotlin's `String` has no `.size`), with the property inlined for a required field.
        assert!(validation.contains("require(tags.length >= 3)"));
    }

    #[test]
    fn empty_record_is_not_an_empty_data_class() {
        // A Kotlin `data class` with no primary-constructor properties does not compile.
        let out = process_generation(spec(
            "kotlin",
            vec![rule(
                "Empty",
                CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            )],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("class Empty\n"));
        assert!(!types.contains("data class Empty("));
    }

    #[test]
    fn size_check_accessor_matches_kotlin_type() {
        let text_field = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Max(8))],
        };
        let bytes_field = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("bytes")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Exact(32))],
        };
        let group = CsilGroupExpression {
            entries: vec![
                entry("label", text_field, None),
                entry("digest", bytes_field, None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Item", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let validation = content(&out, "Validation.kt");
        // String → `.length` (a `.size` here would not compile); ByteArray → `.size`.
        assert!(validation.contains("require(label.length <= 8)"));
        assert!(validation.contains("8 characters"));
        assert!(validation.contains("require(digest.size == 32)"));
        assert!(validation.contains("32 elements"));
    }

    #[test]
    fn required_regex_keeps_literal_v_in_pattern() {
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            // A pattern containing 'v' must survive placeholder substitution intact.
            constraints: vec![CsilControlOperator::Regex("^v[0-9]+$".to_string())],
        };
        let group = CsilGroupExpression {
            entries: vec![entry("version_tag", constrained, None)],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Tagged", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let validation = content(&out, "Validation.kt");
        // The `$` is escaped for the Kotlin string literal; the leading `v` survives.
        assert!(validation.contains("Regex(\"^v[0-9]+\\$\").containsMatchIn(versionTag)"));
    }

    #[test]
    fn package_option_controls_path_and_declaration() {
        let group = CsilGroupExpression {
            entries: vec![entry("v", builtin("int"), None)],
        };
        let mut input = spec("kotlin", vec![rule("Thing", CsilRuleType::GroupDef(group))]);
        input.config.options.insert(
            "kotlin_package".to_string(),
            serde_json::Value::String("com.example.api".to_string()),
        );
        let out = process_generation(input).unwrap();
        let f = out
            .files
            .iter()
            .find(|f| f.path.ends_with("Types.kt"))
            .unwrap();
        assert_eq!(f.path, "com/example/api/Types.kt");
        assert!(f.content.contains("package com.example.api"));
    }

    // --- codec --------------------------------------------------------------

    /// A corndogs-shaped spec: text, bytes, an optional int, a map, a list, a nested
    /// record, and a service whose output is a `Task / ServiceError` choice.
    fn corndogs_rules() -> Vec<CsilRule> {
        let map_ty = CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(builtin("int")),
            occurrence: None,
        };
        let list_ty = CsilTypeExpression::Array {
            element_type: Box::new(builtin("text")),
            occurrence: None,
        };
        let task = CsilGroupExpression {
            entries: vec![
                entry("uuid", builtin("text"), None),
                entry("current_state", builtin("text"), None),
                entry("payload", builtin("bytes"), None),
                entry("priority", builtin("int"), Some(CsilOccurrence::Optional)),
                entry("labels", map_ty, None),
                entry("tags", list_ty, None),
            ],
        };
        let req = CsilGroupExpression {
            entries: vec![
                entry("task", reference("Task"), None),
                entry("queue", builtin("text"), None),
            ],
        };
        let svc_err = CsilGroupExpression {
            entries: vec![entry("code", builtin("int"), None)],
        };
        let service = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                reference("SubmitTaskRequest"),
                CsilTypeExpression::Choice(vec![reference("Task"), reference("ServiceError")]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        vec![
            rule("Task", CsilRuleType::GroupDef(task)),
            rule("SubmitTaskRequest", CsilRuleType::GroupDef(req)),
            rule("ServiceError", CsilRuleType::GroupDef(svc_err)),
            rule("CorndogsService", CsilRuleType::ServiceDef(service)),
        ]
    }

    #[test]
    fn codec_emitted_with_typed_client() {
        let out = process_generation(spec("kotlin-client", corndogs_rules())).unwrap();
        let codec = content(&out, "Codec.kt");
        // Self-contained runtime: value model + encoder/decoder.
        assert!(codec.contains("sealed class CborValue"));
        assert!(codec.contains("object CsilCbor"));
        assert!(codec.contains("fun encode(value: CborValue): ByteArray"));
        assert!(codec.contains("fun decode(b: ByteArray): CborValue"));
        // Per-record codec.
        assert!(codec.contains("fun Task.toCborValue(): CborValue"));
        assert!(codec.contains("fun Task.toCbor(): ByteArray"));
        assert!(
            codec
                .contains("fun submitTaskRequestFromCborValue(cbor: CborValue): SubmitTaskRequest")
        );
        assert!(
            codec.contains("fun submitTaskRequestFromCbor(bytes: ByteArray): SubmitTaskRequest")
        );
        // bytes -> CBOR byte string (major type 2); text -> text; nested record recurses.
        assert!(codec.contains("CborValue.CBytes(this.payload)"));
        assert!(codec.contains("CborValue.CText(this.uuid)"));
        assert!(codec.contains("this.task.toCborValue()"));
        // Optional field is omitted when absent.
        assert!(codec.contains("this.priority?.let { csilV -> csilEntries.add(CborValue.CText(\"priority\") to CborValue.CInt(csilV)) }"));
        // Map/list recurse into a CborValue tree.
        assert!(
            codec.contains("CborValue.CArray((this.tags).map { csilE -> CborValue.CText(csilE) })")
        );
        assert!(codec.contains("CborValue.CMap((this.labels).map { (csilK, csilV) -> CborValue.CText(csilK) to CborValue.CInt(csilV) })"));
        // Generic dispatch the typed client uses.
        assert!(codec.contains("fun <T> encode(value: T): ByteArray"));
        assert!(codec.contains("inline fun <reified T> decode(bytes: ByteArray): T"));
        assert!(codec.contains("is Task -> value.toCborValue()"));
        assert!(codec.contains("Task::class -> taskFromCborValue(cbor)"));

        // Canonical key order within Task: `tags`/`uuid` (len 4) precede longer keys,
        // `current_state` (len 13) is last.
        let body = codec.split("fun Task.toCborValue").nth(1).unwrap();
        let pos_tags = body.find("\"tags\"").unwrap();
        let pos_uuid = body.find("\"uuid\"").unwrap();
        let pos_state = body.find("\"current_state\"").unwrap();
        assert!(pos_tags < pos_uuid && pos_uuid < pos_state);

        // Typed byte-seam client.
        let client = content(&out, "Client.kt");
        assert!(client.contains("fun submitTask(request: SubmitTaskRequest): Task"));
        assert!(client.contains(
            "decode<Task>(transport.call(\"CorndogsService\", \"submit-task\", encode(request)))"
        ));
        assert!(
            client.contains("fun call(service: String, op: String, request: ByteArray): ByteArray")
        );
    }

    #[test]
    fn async_twin_emitted_by_default_with_marked_symbols() {
        // Default client_style is `both`: a suspending twin at ClientAsync.kt whose symbols
        // carry an `Async` marker so it coexists with the blocking client in one package.
        let out = process_generation(spec("kotlin-client", corndogs_rules())).unwrap();
        let twin = content(&out, "ClientAsync.kt");

        // Suspending transport seam, marked interface name.
        assert!(twin.contains("interface AsyncTransport {"));
        assert!(twin.contains(
            "suspend fun call(service: String, op: String, request: ByteArray): ByteArray"
        ));
        // Marked per-service client over the marked seam.
        assert!(
            twin.contains("class CorndogsAsyncClient(private val transport: AsyncTransport) {")
        );
        // Methods suspend and route through the byte seam (no `await` in Kotlin coroutines).
        assert!(twin.contains("suspend fun submitTask(request: SubmitTaskRequest): Task {"));
        assert!(twin.contains(
            "decode<Task>(transport.call(\"CorndogsService\", \"submit-task\", encode(request)))"
        ));
        // The twin reuses the package's ClientError (declared in Client.kt), never redeclares it.
        assert!(!twin.contains("class ClientError("));

        // The sync client is untouched: blocking seam + canonical names, no `suspend`.
        let sync = content(&out, "Client.kt");
        assert!(sync.contains("class CorndogsClient(private val transport: Transport) {"));
        assert!(sync.contains("fun submitTask(request: SubmitTaskRequest): Task {"));
        assert!(!sync.contains("suspend "));
        assert!(!sync.contains("AsyncTransport"));
        assert!(sync.contains("class ClientError("));
    }

    #[test]
    fn client_style_async_is_drop_in_at_canonical_path() {
        // `client_style: async` yields a single suspending client at the canonical path
        // with the canonical symbol names — a drop-in for a blocking consumer.
        let mut input = spec("kotlin-client", corndogs_rules());
        input
            .config
            .options
            .insert("client_style".to_string(), serde_json::json!("async"));
        let out = process_generation(input).unwrap();
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("Client.kt")));
        assert!(
            !paths.iter().any(|p| p.ends_with("ClientAsync.kt")),
            "async drop-in emits no separate twin"
        );

        let client = content(&out, "Client.kt");
        // Canonical (unmarked) names, but suspending.
        assert!(client.contains("interface Transport {"));
        assert!(client.contains(
            "suspend fun call(service: String, op: String, request: ByteArray): ByteArray"
        ));
        assert!(client.contains("class CorndogsClient(private val transport: Transport) {"));
        assert!(client.contains("suspend fun submitTask(request: SubmitTaskRequest): Task {"));
        // The drop-in is the primary file, so it still declares the shared ClientError.
        assert!(client.contains("class ClientError("));
    }

    #[test]
    fn client_style_sync_suppresses_the_twin() {
        let mut input = spec("kotlin-client", corndogs_rules());
        input
            .config
            .options
            .insert("client_style".to_string(), serde_json::json!("sync"));
        let out = process_generation(input).unwrap();
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("Client.kt")));
        assert!(!paths.iter().any(|p| p.ends_with("ClientAsync.kt")));
        let client = content(&out, "Client.kt");
        assert!(!client.contains("suspend "));
        assert!(!client.contains("AsyncTransport"));
    }

    #[test]
    fn client_style_invalid_value_is_rejected() {
        // A bad value fails the whole run regardless of surface.
        let mut input = spec("kotlin-client", corndogs_rules());
        input
            .config
            .options
            .insert("client_style".to_string(), serde_json::json!("blocking"));
        assert!(process_generation(input).is_err());

        // The validator names the offending option so the failure is actionable.
        let mut opts = HashMap::new();
        opts.insert("client_style".to_string(), serde_json::json!("blocking"));
        let err = client_style(&opts).expect_err("invalid client_style must be rejected");
        assert!(
            err.contains("client_style"),
            "error should name the option: {err}"
        );
    }

    // The CborValue variants are prefixed (CInt/CFloat/CArray/CMap/…) rather than named after
    // kotlin builtins because nested members are in scope unqualified inside the sealed class;
    // a variant named `Int` would shadow `kotlin.Int` and break the `hashCode(): Int` override
    // (and any future bare builtin annotation in that scope). This guards against re-introducing
    // that shadow.
    #[test]
    fn codec_variants_do_not_shadow_kotlin_builtins() {
        let out = process_generation(spec("kotlin", corndogs_rules())).unwrap();
        let codec = content(&out, "Codec.kt");

        // The CBOR runtime carves out exactly the sealed-class body; the shadow can only happen
        // there because that is the only scope where variant names are visible unqualified.
        let sealed = codec.split("sealed class CborValue {").nth(1).unwrap();
        let sealed_body = sealed.split("\n}\n").next().unwrap();

        // No variant is named after a kotlin builtin, so an unqualified `Int`/`Float`/`Array`/…
        // in this scope resolves to the kotlin type, not a CborValue member.
        for shadowing in [
            "data class Int(",
            "data class Float(",
            "data class Array(",
            "data class Map(",
            "data object Null ",
            "class Bytes(",
        ] {
            assert!(
                !sealed_body.contains(shadowing),
                "variant `{shadowing}` shadows a kotlin builtin in the CborValue scope"
            );
        }

        // The renamed variants are present and the `hashCode` override is a bare `Int` that now
        // safely resolves to `kotlin.Int` (no `CInt` shadow exists).
        assert!(sealed_body.contains("data class CInt("));
        assert!(sealed_body.contains("class CBytes("));
        assert!(sealed_body.contains("override fun hashCode(): Int = value.contentHashCode()"));

        // Per-type codec emission uses the renamed variants end to end.
        assert!(codec.contains("CborValue.CText(this.uuid)"));
        assert!(codec.contains("CborValue.CBytes(this.payload)"));
        assert!(codec.contains("CborValue.CInt("));
        assert!(codec.contains("CborValue.CMap("));
        assert!(codec.contains("CborValue.CArray("));
    }

    #[test]
    fn codec_handles_timestamp_and_decimal_tags() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("at", builtin("timestamp"), None),
                entry("amount", builtin("decimal"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Entry", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let codec = content(&out, "Codec.kt");
        // timestamp -> tag 0 RFC3339 text (Instant.toString() is RFC3339 UTC).
        assert!(codec.contains("CborValue.CTag(0uL, CborValue.CText((this.at).toString()))"));
        assert!(codec.contains(
            "java.time.Instant.parse(CsilCbor.asTaggedText(CsilCbor.require(cbor, \"at\"), 0uL))"
        ));
        // decimal -> tag 4 [exponent, mantissa].
        assert!(codec.contains("CborValue.CTag(4uL, CborValue.CArray(listOf(CborValue.CInt((-(this.amount).scale()).toLong()), CborValue.CInt((this.amount).unscaledValue().longValueExact()))))"));
        assert!(codec.contains("CsilCbor.asDecimal(CsilCbor.require(cbor, \"amount\"))"));
    }

    #[test]
    fn codec_resolves_transparent_named_aliases() {
        // A named map alias (`StringInt64Map = {* text => int}`), a named list alias
        // (`Tags = [* text]`), a named scalar alias (`Uuid = text`), and a map-of-record
        // alias (`Members = {* text => Member}`) all referenced from a record's fields.
        let string_int64_map = CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(builtin("int")),
            occurrence: None,
        };
        let tags = CsilTypeExpression::Array {
            element_type: Box::new(builtin("text")),
            occurrence: None,
        };
        let members = CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(reference("Member")),
            occurrence: None,
        };
        let member = CsilGroupExpression {
            entries: vec![entry("id", builtin("text"), None)],
        };
        let holder = CsilGroupExpression {
            entries: vec![
                entry("counts", reference("StringInt64Map"), None),
                entry("labels", reference("Tags"), None),
                entry("uuid", reference("Uuid"), None),
                entry("members", reference("Members"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![
                rule("StringInt64Map", CsilRuleType::TypeDef(string_int64_map)),
                rule("Tags", CsilRuleType::TypeDef(tags)),
                rule("Uuid", CsilRuleType::TypeDef(builtin("text"))),
                rule("Members", CsilRuleType::TypeDef(members)),
                rule("Member", CsilRuleType::GroupDef(member)),
                rule("Holder", CsilRuleType::GroupDef(holder)),
            ],
        ))
        .unwrap();
        let codec = content(&out, "Codec.kt");

        // The named map alias resolves to a real CBOR map, not the `CborValue.CNull` stub.
        assert!(codec.contains("CborValue.CMap((this.counts).map { (csilK, csilV) -> CborValue.CText(csilK) to CborValue.CInt(csilV) })"));
        assert!(codec.contains(
            "CsilCbor.asMap(CsilCbor.require(cbor, \"counts\")).associate { (csilK, csilV) -> CsilCbor.asText(csilK) to CsilCbor.asLong(csilV) }"
        ));
        // The named list alias resolves to a CBOR array.
        assert!(
            codec.contains(
                "CborValue.CArray((this.labels).map { csilE -> CborValue.CText(csilE) })"
            )
        );
        // The named scalar alias resolves to text.
        assert!(codec.contains("CborValue.CText(this.uuid)"));
        // The map-of-record alias recurses to the record codec on both seams.
        assert!(codec.contains("CborValue.CMap((this.members).map { (csilK, csilV) -> CborValue.CText(csilK) to csilV.toCborValue() })"));
        assert!(codec.contains(
            "CsilCbor.asMap(CsilCbor.require(cbor, \"members\")).associate { (csilK, csilV) -> CsilCbor.asText(csilK) to memberFromCborValue(csilV) }"
        ));

        // No field above degraded to the null/text stub a bare non-record reference yields.
        let holder_enc = codec.split("fun Holder.toCborValue").nth(1).unwrap();
        let holder_enc = holder_enc.split("fun ").next().unwrap();
        assert!(!holder_enc.contains("to CborValue.CNull"));
    }

    #[test]
    fn null_input_client_sends_empty_bytes() {
        let service = CsilServiceDefinition {
            operations: vec![op(
                "room-delta",
                builtin("null"),
                reference("RoomDelta"),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-client",
            vec![
                rule(
                    "RoomDelta",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("seq", builtin("int"), None)],
                    }),
                ),
                rule("WorldService", CsilRuleType::ServiceDef(service)),
            ],
        ))
        .unwrap();
        let client = content(&out, "Client.kt");
        assert!(client.contains("fun roomDelta(): RoomDelta"));
        // A null-input op sends an empty request body.
        assert!(client.contains(
            "decode<RoomDelta>(transport.call(\"WorldService\", \"room-delta\", ByteArray(0)))"
        ));
    }

    #[test]
    fn non_record_op_boundaries_get_client_methods_and_per_op_codecs() {
        // Mirrors tests/fixtures/services/nonrecord-ops.csil: a scalar-id request, a
        // bare-array response, a scalar response, and a map response must each yield a
        // client method (not a drop-note), riding per-op codec helpers.
        let member = CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![
                entry("id", reference("MemberID"), None),
                entry("name", builtin("text"), None),
            ],
        });
        let list_req = CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![entry(
                "limit",
                builtin("uint"),
                Some(CsilOccurrence::Optional),
            )],
        });
        let member_array = CsilTypeExpression::Array {
            element_type: Box::new(reference("Member")),
            occurrence: None,
        };
        let text_map = CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(builtin("text")),
            occurrence: None,
        };
        let service = CsilServiceDefinition {
            operations: vec![
                op(
                    "create-member",
                    reference("Member"),
                    reference("Member"),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                op(
                    "get-member",
                    reference("MemberID"),
                    reference("Member"),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                op(
                    "list-members",
                    reference("ListMembersRequest"),
                    member_array,
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                op(
                    "delete-task",
                    reference("TaskID"),
                    builtin("bool"),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                op(
                    "member-names",
                    reference("ListMembersRequest"),
                    text_map,
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
            ],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-client",
            vec![
                rule("MemberID", CsilRuleType::TypeDef(builtin("text"))),
                rule("TaskID", CsilRuleType::TypeDef(builtin("text"))),
                rule("Member", member),
                rule("ListMembersRequest", list_req),
                rule("MemberService", CsilRuleType::ServiceDef(service)),
            ],
        ))
        .unwrap();
        let client = content(&out, "Client.kt");

        // Every op gets a method — scalar-id request, bare-array and scalar responses included.
        assert!(client.contains("fun getMember(request: MemberID): Member"));
        assert!(client.contains("fun listMembers(request: ListMembersRequest): List<Member>"));
        assert!(client.contains("fun deleteTask(request: TaskID): Boolean"));
        assert!(
            client.contains("fun memberNames(request: ListMembersRequest): Map<String, String>")
        );
        // No op is dropped with a note anymore.
        assert!(!client.contains("(de)serialize it manually"));
        // A record boundary keeps the generic `encode`/`decode<T>` path (byte-identical);
        // non-record boundaries ride the per-op helpers.
        assert!(client.contains("encode(request)"));
        assert!(client.contains("encodeMemberGetMemberRequest(request)"));
        assert!(client.contains("decodeMemberListMembersResponse("));
        assert!(client.contains("decodeMemberDeleteTaskResponse("));

        let codec = content(&out, "Codec.kt");
        // Per-op helpers for the non-record shapes are exported so a consumer-side server
        // can compose decode(request)/encode(response) for every op.
        assert!(codec.contains("fun decodeMemberGetMemberRequest(bytes: ByteArray): MemberID"));
        assert!(
            codec.contains("fun encodeMemberListMembersResponse(value: List<Member>): ByteArray")
        );
        assert!(codec.contains("fun encodeMemberDeleteTaskResponse(value: Boolean): ByteArray"));
        assert!(codec.contains(
            "fun decodeMemberMemberNamesResponse(bytes: ByteArray): Map<String, String>"
        ));
    }

    #[test]
    fn wire_strings_are_verbatim_csil_names() {
        // `CorndogsService` must hit the wire as "CorndogsService" and `submit-task`
        // as "submit-task" — verbatim CSIL names (csil-rpc-transport.md §1.1/§1.3),
        // while the Kotlin class name still strips the `Service` suffix.
        let service = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                reference("SubmitTaskRequest"),
                reference("Task"),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-client",
            vec![
                rule(
                    "SubmitTaskRequest",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("queue", builtin("text"), None)],
                    }),
                ),
                rule(
                    "Task",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("uuid", builtin("text"), None)],
                    }),
                ),
                rule("CorndogsService", CsilRuleType::ServiceDef(service)),
            ],
        ))
        .unwrap();
        let client = content(&out, "Client.kt");
        assert!(client.contains("\"CorndogsService\", \"submit-task\""));
        assert!(client.contains("class CorndogsClient"));
        assert!(!client.contains("\"corndogs\""));
        assert!(!client.contains("\"SubmitTask\""));
    }

    #[test]
    fn no_records_means_no_codec_file() {
        // A spec with only a service (no record types) emits no Codec.kt.
        let service = CsilServiceDefinition {
            operations: vec![],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("EmptyService", CsilRuleType::ServiceDef(service))],
        ))
        .unwrap();
        assert!(!out.files.iter().any(|f| f.path.ends_with("Codec.kt")));
    }

    // --- self-contained package mode ----------------------------------------

    /// A one-record spec carrying the given option overrides, used by the package-mode
    /// tests so each only states the options under test.
    fn package_input(options: Vec<(&str, serde_json::Value)>) -> WasmGeneratorInput {
        let group = CsilGroupExpression {
            entries: vec![entry("v", builtin("int"), None)],
        };
        let mut input = spec("kotlin", vec![rule("Thing", CsilRuleType::GroupDef(group))]);
        for (k, v) in options {
            input.config.options.insert(k.to_string(), v);
        }
        input
    }

    #[test]
    fn package_mode_emits_gradle_manifest_and_source_layout() {
        let out = process_generation(package_input(vec![
            (
                "kotlin_package",
                serde_json::Value::String("com.example.api".to_string()),
            ),
            (
                "package_name",
                serde_json::Value::String("example-api".to_string()),
            ),
            (
                "package_version",
                serde_json::Value::String("1.2.3".to_string()),
            ),
            ("emit_packages", serde_json::json!(["kotlin"])),
        ]))
        .unwrap();

        // Generated sources land under Gradle's standard source root (not the flat path).
        let types = out
            .files
            .iter()
            .find(|f| f.path.ends_with("Types.kt"))
            .unwrap();
        assert_eq!(types.path, "src/main/kotlin/com/example/api/Types.kt");
        assert!(types.content.contains("package com.example.api"));

        // build.gradle.kts carries the plugins and the CSIL coordinates.
        let build = content(&out, "build.gradle.kts");
        assert!(build.contains("kotlin(\"jvm\") version \"2.0.21\""));
        assert!(build.contains("`maven-publish`"));
        assert!(build.contains("group = \"com.example.api\""));
        assert!(build.contains("version = \"1.2.3\""));
        assert!(build.contains("jvmToolchain(17)"));
        // No third-party dependency is pulled in; the codec runtime is self-contained.
        assert!(!build.contains("dependencies {"));
        // A repository is still required: the `kotlin("jvm")` plugin contributes
        // `kotlin-stdlib`, which Gradle cannot resolve with no repositories defined.
        assert!(build.contains("mavenCentral()"));

        // settings.gradle.kts names the root project after the artifact.
        let settings = content(&out, "settings.gradle.kts");
        assert!(settings.contains("rootProject.name = \"example-api\""));
    }

    #[test]
    fn without_emit_packages_no_gradle_and_flat_layout() {
        let out = process_generation(package_input(vec![(
            "kotlin_package",
            serde_json::Value::String("com.example.api".to_string()),
        )]))
        .unwrap();
        let types = out
            .files
            .iter()
            .find(|f| f.path.ends_with("Types.kt"))
            .unwrap();
        // Default output is unchanged: flat package path, no Gradle files.
        assert_eq!(types.path, "com/example/api/Types.kt");
        assert!(
            !out.files
                .iter()
                .any(|f| f.path.ends_with("build.gradle.kts"))
        );
        assert!(
            !out.files
                .iter()
                .any(|f| f.path.ends_with("settings.gradle.kts"))
        );
    }

    #[test]
    fn emit_packages_without_kotlin_is_unchanged() {
        let out = process_generation(package_input(vec![
            (
                "kotlin_package",
                serde_json::Value::String("com.example.api".to_string()),
            ),
            ("emit_packages", serde_json::json!(["go", "python"])),
        ]))
        .unwrap();
        let types = out
            .files
            .iter()
            .find(|f| f.path.ends_with("Types.kt"))
            .unwrap();
        assert_eq!(types.path, "com/example/api/Types.kt");
        assert!(
            !out.files
                .iter()
                .any(|f| f.path.ends_with("build.gradle.kts"))
        );
    }

    #[test]
    fn emit_packages_non_array_is_ignored() {
        // A bare string (not a JSON array) must not trigger package mode.
        let out = process_generation(package_input(vec![(
            "emit_packages",
            serde_json::Value::String("kotlin".to_string()),
        )]))
        .unwrap();
        assert!(
            !out.files
                .iter()
                .any(|f| f.path.ends_with("build.gradle.kts"))
        );
    }

    #[test]
    fn package_name_and_version_default_when_absent() {
        // No package_name → derived from the last package segment; no version → "0.1.0".
        let out = process_generation(package_input(vec![
            (
                "kotlin_package",
                serde_json::Value::String("com.example.widgets".to_string()),
            ),
            ("emit_packages", serde_json::json!(["kotlin"])),
        ]))
        .unwrap();
        let settings = content(&out, "settings.gradle.kts");
        assert!(settings.contains("rootProject.name = \"widgets\""));
        let build = content(&out, "build.gradle.kts");
        assert!(build.contains("version = \"0.1.0\""));
    }

    /// A corndogs `kotlin-client` package input with the given options overlaid.
    fn corndogs_package_input(options: Vec<(&str, serde_json::Value)>) -> WasmGeneratorInput {
        let mut input = spec("kotlin-client", corndogs_rules());
        for (k, v) in options {
            input.config.options.insert(k.to_string(), v);
        }
        input
    }

    #[test]
    fn readme_emitted_only_in_package_mode() {
        // No emit_packages: no README (the flat default output).
        let plain = process_generation(spec("kotlin-client", corndogs_rules())).unwrap();
        assert!(!plain.files.iter().any(|f| f.path == "genquickstart.md"));

        let pkg = process_generation(corndogs_package_input(vec![(
            "emit_packages",
            serde_json::json!(["kotlin"]),
        )]))
        .unwrap();
        assert!(pkg.files.iter().any(|f| f.path == "genquickstart.md"));
    }

    /// `emit_readme: false` suppresses only the README; the rest of the package (notably the
    /// gradle manifests) is unchanged.
    #[test]
    fn emit_readme_false_suppresses_only_readme() {
        let on = process_generation(corndogs_package_input(vec![(
            "emit_packages",
            serde_json::json!(["kotlin"]),
        )]))
        .unwrap();
        assert!(on.files.iter().any(|f| f.path == "genquickstart.md"));

        let off = process_generation(corndogs_package_input(vec![
            ("emit_packages", serde_json::json!(["kotlin"])),
            ("emit_readme", serde_json::json!(false)),
        ]))
        .unwrap();
        assert!(!off.files.iter().any(|f| f.path == "genquickstart.md"));
        // Everything other than the README is still emitted.
        assert!(off.files.iter().any(|f| f.path == "build.gradle.kts"));
        let on_without_readme: Vec<_> = on
            .files
            .iter()
            .filter(|f| f.path != "genquickstart.md")
            .map(|f| &f.path)
            .collect();
        let off_paths: Vec<_> = off.files.iter().map(|f| &f.path).collect();
        assert_eq!(on_without_readme, off_paths);
    }

    /// Corndogs records plus a record-typed `<->` channel op (`watch-tasks`), so the
    /// genquickstart exercises all three transport sections (the unary `submit-task` drives
    /// RPC + Datagrams; the channel op drives Events).
    fn three_transport_rules() -> Vec<CsilRule> {
        let mut rules = corndogs_rules();
        let watch_req = CsilGroupExpression {
            entries: vec![entry("uuid", builtin("text"), None)],
        };
        let status_update = CsilGroupExpression {
            entries: vec![entry("state", builtin("text"), None)],
        };
        // Append the channel op to the existing CorndogsService and add its record types.
        if let Some(svc_rule) = rules
            .iter_mut()
            .find(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
            && let CsilRuleType::ServiceDef(def) = &mut svc_rule.rule_type
        {
            def.operations.push(op(
                "watch-tasks",
                reference("WatchRequest"),
                reference("StatusUpdate"),
                CsilServiceDirection::Bidirectional,
                None,
            ));
        }
        rules.insert(0, rule("WatchRequest", CsilRuleType::GroupDef(watch_req)));
        rules.insert(
            1,
            rule("StatusUpdate", CsilRuleType::GroupDef(status_update)),
        );
        rules
    }

    fn three_transport_input(options: Vec<(&str, serde_json::Value)>) -> WasmGeneratorInput {
        let mut input = spec("kotlin", three_transport_rules());
        for (k, v) in options {
            input.config.options.insert(k.to_string(), v);
        }
        input
    }

    /// The genquickstart is a three-transport, library-based Quickstart: CSIL-RPC over an
    /// HTTP carrier (lib `RpcRequest`/`RpcResponse`), CSIL-Events over a TLS frame carrier
    /// (lib `$hello` handshake + heartbeat + the generated router), and CSIL-Datagrams over
    /// UDP (lib `Datagram`). No kotlinc toolchain is available here, so each section is
    /// asserted by content, not compiled or run — runtime verify is not possible.
    #[test]
    fn genquickstart_has_three_lib_based_sections() {
        let out = process_generation(three_transport_input(vec![
            (
                "kotlin_package",
                serde_json::Value::String("community.catalyst.demo".to_string()),
            ),
            (
                "package_name",
                serde_json::Value::String("corndogs-client".to_string()),
            ),
            (
                "package_version",
                serde_json::Value::String("1.2.3".to_string()),
            ),
            ("emit_packages", serde_json::json!(["kotlin"])),
        ]))
        .unwrap();
        let c = content(&out, "genquickstart.md");

        // Title, resolved coordinates, and the transport-library dependency in Install.
        assert!(c.starts_with("# corndogs-client\n"));
        assert!(c.contains("implementation(\"community.catalyst.demo:corndogs-client:1.2.3\")"));
        assert!(
            c.contains("implementation(\"community.catalyst.csilgen:csilgen-transport:0.1.0\")")
        );
        assert!(c.contains("`csilgen-transport` library owns the envelope"));

        // --- CSIL-RPC (HTTP) ------------------------------------------------------------
        assert!(c.contains("## CSIL-RPC (HTTP)"));
        // The carrier implements the generated Transport seam over the lib's envelope.
        assert!(c.contains("import community.catalyst.csilgen.transport.RpcRequest"));
        assert!(c.contains("import community.catalyst.csilgen.transport.RpcResponse"));
        assert!(c.contains("class HttpRpcTransport(baseUrl: String) : Transport"));
        assert!(c.contains("val envelope = RpcRequest(service, op, request).encode()"));
        assert!(c.contains("import java.net.http.HttpClient"));
        assert!(c.contains("/csil/v1/rpc"));
        assert!(c.contains(".POST(HttpRequest.BodyPublishers.ofByteArray(envelope))"));
        // Non-zero transport status + typed ServiceError arm handled distinctly.
        assert!(c.contains("val resp = RpcResponse.decode(httpResp.body())"));
        assert!(c.contains("resp.asTransportError()?.let"));
        assert!(c.contains("if (resp.variant == \"ServiceError\")"));
        // Typed client + the first `->` call with a generated sample literal.
        assert!(c.contains("val client = CorndogsClient(HttpRpcTransport("));
        assert!(c.contains("client.submitTask(SubmitTaskRequest(task = Task("));
        assert!(c.contains("queue = \"example\""));

        // --- CSIL-Events (TLS) ----------------------------------------------------------
        assert!(c.contains("## CSIL-Events (TLS)"));
        assert!(c.contains("import community.catalyst.csilgen.transport.Hello"));
        assert!(c.contains("import community.catalyst.csilgen.transport.StreamCarrier"));
        assert!(c.contains("fun openTlsCarrier(host: String, port: Int): FrameCarrier"));
        // The $hello handshake + the $ping/$pong heartbeat from the lib.
        assert!(
            c.contains("Hello(listOf(1uL), listOf(\"verbose\"), \"CorndogsService\").encode()")
        );
        assert!(c.contains("HelloAck.decode(ackFrame).profile"));
        assert!(c.contains("if (ev.event == Control.PING_NAME)"));
        assert!(c.contains("Control.PONG_NAME, Heartbeat(ping.nonce).encode()"));
        // One outbound event via the generated encoder + dispatch into the generated router.
        assert!(c.contains("encodeCorndogsServiceWatchTasks(channelCodec, StatusUpdate("));
        assert!(c.contains("Event.verbose(\"CorndogsService\", event, bytes).encode(profile)"));
        assert!(c.contains(
            "routeCorndogsServiceChannel(handlers, channelCodec, ev.event!!, ev.payload)"
        ));

        // --- CSIL-Datagrams (UDP) -------------------------------------------------------
        assert!(c.contains("## CSIL-Datagrams (UDP)"));
        assert!(c.contains("import community.catalyst.csilgen.transport.Datagram"));
        assert!(c.contains("class UdpDatagramCarrier(host: String, port: Int) : DatagramCarrier"));
        // Encode the `->` request via the generated codec, wrap in the lib's Datagram, send.
        assert!(c.contains("val req: SubmitTaskRequest = SubmitTaskRequest(task = Task("));
        assert!(c.contains("carrier.sendDatagram(Datagram(OP_ORD, 0uL, req.toCbor()).encode())"));
        // The recv path decodes the RESPONSE type, with the explicit "may arrive later" note.
        assert!(c.contains("val resp: Task = taskFromCbor(dg.payload)"));
        assert!(c.contains("MAY arrive later — or never"));

        // The whole document is space-indented like the rest of the surface.
        assert!(!c.contains('\t'));
    }

    /// A package built from the default (`kotlin`/server) target must still be SELF-CONTAINED:
    /// its genquickstart references the typed client (CSIL-RPC/Datagrams), the channel router
    /// and handler interface (CSIL-Events), and the codec (all sections), so the emitted file
    /// set must carry ALL THREE surfaces together — Client.kt (client), Services.kt
    /// (router/iface), and Codec.kt (codec) — even though a flat server build emits only
    /// Services.kt. No kotlinc here, so this is assert-only: the file set proves the
    /// quickstart's symbols all resolve against the single package. Mirrors the OCaml
    /// generator's package mode.
    #[test]
    fn package_mode_is_self_contained_with_client_router_and_codec() {
        let out = process_generation(three_transport_input(vec![(
            "emit_packages",
            serde_json::json!(["kotlin"]),
        )]))
        .unwrap();
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();

        // The package carries the RPC client AND the channel router AND the codec together.
        assert!(
            paths.iter().any(|p| p.ends_with("/Client.kt")),
            "package missing the RPC client surface; paths: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("/Services.kt")),
            "package missing the channel-router surface; paths: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("/Codec.kt")),
            "package missing the codec; paths: {paths:?}"
        );

        // Services.kt actually defines the router + handler interface the Events section calls.
        let services = content(&out, "Services.kt");
        assert!(services.contains("interface CorndogsService"));
        assert!(services.contains(
            "fun routeCorndogsServiceChannel(handlers: CorndogsService, codec: Codec, op: String, data: ByteArray)"
        ));
        // Client.kt defines the typed client the RPC/Datagrams sections call.
        let client = content(&out, "Client.kt");
        assert!(client.contains("class CorndogsClient"));

        // The Events section dispatches via the generated router + encoder (not codec-direct).
        let quickstart = content(&out, "genquickstart.md");
        assert!(quickstart.contains(
            "routeCorndogsServiceChannel(handlers, channelCodec, ev.event!!, ev.payload)"
        ));
        assert!(quickstart.contains("encodeCorndogsServiceWatchTasks(channelCodec, StatusUpdate("));

        // A flat (non-package) server build stays surface-only: just Services.kt, no Client.kt.
        let flat = process_generation(spec("kotlin", three_transport_rules())).unwrap();
        let flat_paths: Vec<&str> = flat.files.iter().map(|f| f.path.as_str()).collect();
        assert!(flat_paths.iter().any(|p| p.ends_with("Services.kt")));
        assert!(
            !flat_paths.iter().any(|p| p.ends_with("Client.kt")),
            "flat server build must stay surface-only; paths: {flat_paths:?}"
        );
    }

    /// `genquickstart_transports` selects a subset of sections; an absent value renders all
    /// three.
    #[test]
    fn genquickstart_transports_subset_selects_sections() {
        let only_events = process_generation(three_transport_input(vec![
            ("emit_packages", serde_json::json!(["kotlin"])),
            ("genquickstart_transports", serde_json::json!(["events"])),
        ]))
        .unwrap();
        let c = content(&only_events, "genquickstart.md");
        assert!(c.contains("## CSIL-Events (TLS)"));
        assert!(!c.contains("## CSIL-RPC (HTTP)"));
        assert!(!c.contains("## CSIL-Datagrams (UDP)"));

        // An empty / unknown-only array falls back to all three.
        let all = process_generation(three_transport_input(vec![
            ("emit_packages", serde_json::json!(["kotlin"])),
            ("genquickstart_transports", serde_json::json!([])),
        ]))
        .unwrap();
        let c = content(&all, "genquickstart.md");
        assert!(c.contains("## CSIL-RPC (HTTP)"));
        assert!(c.contains("## CSIL-Events (TLS)"));
        assert!(c.contains("## CSIL-Datagrams (UDP)"));
    }

    /// A null-input op yields a no-argument RPC call, and a serviceless package degrades each
    /// section to its note (no `->` op, no channel op) rather than referencing missing symbols.
    #[test]
    fn genquickstart_handles_null_input_and_serviceless() {
        let pong = CsilGroupExpression {
            entries: vec![entry("ok", builtin("bool"), None)],
        };
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "ping",
                builtin("null"),
                reference("Pong"),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let mut input = spec(
            "kotlin-client",
            vec![
                rule("Pong", CsilRuleType::GroupDef(pong)),
                rule("HealthService", CsilRuleType::ServiceDef(svc)),
            ],
        );
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["kotlin"]));
        let out = process_generation(input).unwrap();
        let c = content(&out, "genquickstart.md");
        // RPC: null-input op calls with no args; no record `->` payload so Datagrams notes it.
        assert!(c.contains("val resp = client.ping()"));
        assert!(c.contains("non-record payloads"));
        // No channel op: the Events section shows the handshake without dispatch wiring.
        assert!(c.contains("there is no generated channel router"));

        // A serviceless spec degrades every section to its note, referencing no client/router.
        let typed = process_generation(package_input(vec![(
            "emit_packages",
            serde_json::json!(["kotlin"]),
        )]))
        .unwrap();
        let tc = content(&typed, "genquickstart.md");
        assert!(tc.contains("no `->` operations"));
        assert!(!tc.contains(": Transport"));
    }

    fn lit_text(s: &str) -> CsilTypeExpression {
        CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
    }

    fn lit_int(n: i64) -> CsilTypeExpression {
        CsilTypeExpression::Literal(CsilLiteralValue::Integer(n))
    }

    // A choice arm carrying a trailing `.default`, exactly the `Constrained { Literal, .. }`
    // shape CSIL's parser produces for the last arm of `a / b / c .default d`.
    fn default_arm(s: &str, default: &str) -> CsilTypeExpression {
        CsilTypeExpression::Constrained {
            base_type: Box::new(lit_text(s)),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                default.to_string(),
            ))],
        }
    }

    // The torture record: an inline open string choice, an all-literal `.default` choice,
    // a mixed union, and inline choices in an array element, a map value, a tuple element,
    // and inside an inline group field.
    fn inline_torture_spec() -> WasmGeneratorInput {
        let payload = CsilGroupExpression {
            entries: vec![entry("detail", builtin("text"), None)],
        };
        let open_status = CsilTypeExpression::Choice(vec![
            builtin("text"),
            lit_text("pending"),
            lit_text("active"),
            lit_text("closed"),
        ]);
        let closed_size = CsilTypeExpression::Choice(vec![
            lit_text("small"),
            lit_text("medium"),
            default_arm("large", "medium"),
        ]);
        let mixed_payload = CsilTypeExpression::Choice(vec![
            lit_text("none"),
            lit_int(42),
            reference("InlineChoicePayload"),
        ]);
        let tags = CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Choice(vec![
                builtin("text"),
                lit_text("red"),
                lit_text("green"),
                lit_text("blue"),
                builtin("int"),
            ])),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        };
        let labels = CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(CsilTypeExpression::Choice(vec![
                builtin("text"),
                lit_text("yes"),
                lit_text("no"),
                builtin("bool"),
            ])),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        };
        let coord = CsilTypeExpression::Tuple(CsilGroupExpression {
            entries: vec![
                entry("_0", builtin("int"), None),
                entry(
                    "_1",
                    CsilTypeExpression::Choice(vec![
                        builtin("text"),
                        lit_text("x"),
                        lit_text("y"),
                        lit_text("z"),
                    ]),
                    None,
                ),
            ],
        });
        let nested = CsilTypeExpression::Group(CsilGroupExpression {
            entries: vec![entry(
                "kind",
                CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    lit_text("a"),
                    lit_text("b"),
                    builtin("int"),
                ]),
                None,
            )],
        });
        let record = CsilGroupExpression {
            entries: vec![
                entry("status", open_status, None),
                entry(
                    "priority",
                    default_arm_choice(),
                    Some(CsilOccurrence::Optional),
                ),
                entry("size", closed_size, Some(CsilOccurrence::Optional)),
                entry("payload", mixed_payload, None),
                entry("tags", tags, None),
                entry("labels", labels, None),
                entry("coord", coord, None),
                entry("nested", nested, None),
            ],
        };
        spec(
            "kotlin",
            vec![
                rule("InlineChoicePayload", CsilRuleType::GroupDef(payload)),
                rule("InlineChoiceRecord", CsilRuleType::GroupDef(record)),
            ],
        )
    }

    fn default_arm_choice() -> CsilTypeExpression {
        CsilTypeExpression::Choice(vec![
            builtin("text"),
            lit_text("low"),
            lit_text("normal"),
            default_arm("high", "normal"),
        ])
    }

    #[test]
    fn inline_choice_fields_are_hoisted_to_named_types() {
        let out = process_generation(inline_torture_spec()).unwrap();
        let types = content(&out, "Types.kt");
        // Every inline-composite position routes the field through a synthesized named type
        // rather than the opaque `Any`/`List<Any>`/`Map<..,Any>` fallback.
        assert!(types.contains("val status: InlineChoiceRecordStatus,"));
        assert!(types.contains("val priority: InlineChoiceRecordPriority? = null,"));
        assert!(types.contains("val size: InlineChoiceRecordSize? = null,"));
        assert!(types.contains("val payload: InlineChoiceRecordPayload,"));
        assert!(types.contains("val tags: List<InlineChoiceRecordTagsItem>,"));
        assert!(types.contains("val labels: Map<String, InlineChoiceRecordLabelsValue>,"));
        assert!(types.contains("val nested: InlineChoiceRecordNested"));
        // The inline group's own inline-choice field is hoisted recursively.
        assert!(types.contains("data class InlineChoiceRecordNested("));
        assert!(types.contains("val kind: InlineChoiceRecordNestedKind"));
        // The tuple element with no name of its own borrows an index suffix.
        assert!(types.contains("sealed interface InlineChoiceRecordCoord1"));
        // No field falls back to a bare opaque type.
        assert!(!types.contains("val status: Any"));
        assert!(!types.contains("val payload: Any"));
    }

    #[test]
    fn inline_all_literal_choice_with_default_arm_is_bare_enum() {
        // Regression for the `Constrained { Literal, .. }` arm bug: `.default` on the last
        // arm must not knock a closed all-literal choice out of the bare-literal enum shape.
        let out = process_generation(inline_torture_spec()).unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("enum class InlineChoiceRecordSize { Small, Medium, Large }"));
        let codec = content(&out, "Codec.kt");
        // Bare-literal wire on encode/decode, including the `.default`-wrapped `Large` arm.
        assert!(codec.contains("InlineChoiceRecordSize.Large -> CborValue.CText(\"large\")"));
        assert!(codec.contains("\"large\" -> InlineChoiceRecordSize.Large"));
        // It must NOT have become a tagged-sum union.
        assert!(!codec.contains("InlineChoiceRecordSizeVariant"));
    }

    #[test]
    fn inline_union_codec_is_literal_first_index_dispatch() {
        let out = process_generation(inline_torture_spec()).unwrap();
        let codec = content(&out, "Codec.kt");
        // Open string choice: the `text` base is arm 0 (bare text), the literals carry their
        // declaration-order index and encode as their own literal value.
        assert!(codec.contains(
            "is InlineChoiceRecordStatusVariant0 -> CborValue.CArray(listOf(CborValue.CUint(0uL), CborValue.CText(this.value)))"
        ));
        assert!(codec.contains(
            "is InlineChoiceRecordStatusVariant1 -> CborValue.CArray(listOf(CborValue.CUint(1uL), CborValue.CText(\"pending\")))"
        ));
        // Decode dispatches on the index and validates the literal by equality.
        assert!(codec.contains(
            "1uL -> InlineChoiceRecordStatusVariant1(CsilCbor.expectLiteral(csilArr[1], CborValue.CText(\"pending\"), \"pending\"))"
        ));
        // Mixed union: literal arms (text + int) plus a reference arm at its own index.
        assert!(codec.contains(
            "is InlineChoiceRecordPayloadVariant0 -> CborValue.CArray(listOf(CborValue.CUint(0uL), CborValue.CText(\"none\")))"
        ));
        assert!(codec.contains(
            "is InlineChoiceRecordPayloadVariant1 -> CborValue.CArray(listOf(CborValue.CUint(1uL), CborValue.CUint(42uL)))"
        ));
        assert!(codec.contains(
            "is InlineChoiceRecordPayloadVariant2 -> CborValue.CArray(listOf(CborValue.CUint(2uL), this.value.toCborValue()))"
        ));
        // The priority union's `.default`-wrapped `high` arm still encodes its literal.
        assert!(codec.contains(
            "is InlineChoiceRecordPriorityVariant3 -> CborValue.CArray(listOf(CborValue.CUint(3uL), CborValue.CText(\"high\")))"
        ));
    }

    #[test]
    fn inline_choice_container_positions_get_codecs() {
        let out = process_generation(inline_torture_spec()).unwrap();
        let codec = content(&out, "Codec.kt");
        // Array element, map value, and tuple element each route through their hoisted codec
        // rather than the dropped-value `CborValue.CNull` fallback.
        assert!(codec.contains("csilE -> csilE.toCborValue()"));
        assert!(codec.contains("CborValue.CText(csilK) to csilV.toCborValue()"));
        assert!(codec.contains("((this.coord)[1] as InlineChoiceRecordCoord1).toCborValue()"));
        assert!(codec.contains("inlineChoiceRecordCoord1FromCborValue(csilArr[1])"));
        // No position silently drops its value to null anymore.
        assert!(!codec.contains("csilE -> CborValue.CNull"));
        assert!(!codec.contains("to CborValue.CNull))"));
    }

    #[test]
    fn choice_arm_literal_sees_through_default_wrapper() {
        assert!(matches!(
            choice_arm_literal(&default_arm("large", "medium")),
            Some(CsilLiteralValue::Text(s)) if s == "large"
        ));
        assert!(matches!(
            choice_arm_literal(&lit_int(5)),
            Some(CsilLiteralValue::Integer(5))
        ));
        assert!(choice_arm_literal(&builtin("text")).is_none());
    }
}
