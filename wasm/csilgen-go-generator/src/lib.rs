//! Go Code Generator for CSIL
//!
//! This example generator demonstrates how to create a fully functional
//! CSIL generator that produces Go code with struct definitions and service interfaces.

use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::HashMap;

/// Generate Go code from CSIL specifications
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "go-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Go code generator with service support".to_string(),
        target: "go".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/go-generator".to_string()),
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
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;

    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

fn process_generation(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = GoConfig::from_options(&input.config.options)?;
    let mut warnings = Vec::new();
    let mut files = Vec::new();

    // Helper to build output path with optional subdirectory
    let make_path = |filename: &str| -> String {
        if config.output_subdir.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", config.output_subdir, filename)
        }
    };

    // Generate types file
    if let Some(types_content) = generate_types(&input, &config, &mut warnings)? {
        files.push(GeneratedFile {
            path: make_path("types.gen.go"),
            content: types_content,
        });
    }

    // The exact-decimal helper is self-contained and only worth emitting when the
    // spec actually uses `decimal` under the default (`csil`) mapping; the library
    // mapping pulls the type from shopspring instead, so no helper is generated.
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(&input, "decimal") {
        files.push(GeneratedFile {
            path: make_path("csil_decimal.gen.go"),
            content: csil_decimal_file(&config),
        });
    }

    // The self-contained per-type CBOR codec: emitted whenever the spec has record
    // types, since that codec is what every payload (de)serializes through now (the
    // typed client owns the wire; no reflection/derive path remains).
    if let Some(codec_content) = generate_codec(&input, &config) {
        files.push(GeneratedFile {
            path: make_path("codec.gen.go"),
            content: codec_content,
        });
    }

    // Dispatch on target: the base `go` (and explicit `go-server`) target emits
    // the server interface; `go-client` emits a transport-agnostic client;
    // `go-typesonly` emits the types (and their validation/constructors) alone.
    // An unrecognized sub-target is an error, not a silent fall-through.
    enum Surface {
        Server,
        Client,
        TypesOnly,
    }
    let surface = match input.config.target.as_str() {
        "go" | "go-server" => Surface::Server,
        "go-client" => Surface::Client,
        "go-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    // In self-contained package mode the genquickstart demonstrates both the calling
    // side (CSIL-RPC/Datagrams over the typed client) and the handling side (CSIL-Events
    // over the channel router), so the package must carry both surfaces for its own
    // quickstart to compile — regardless of which surface the target requested. Flat
    // (non-package) output stays byte-identical: it emits only the requested surface.
    // Mirrors the OCaml generator.
    let pkg_mode = emit_packages_includes_go(&input.config.options);
    let want_client =
        matches!(surface, Surface::Client) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    let want_server =
        matches!(surface, Surface::Server) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    if input.csil_spec.service_count > 0 {
        if want_client
            && let Some(client_content) = generate_client(&input, &config, &mut warnings)?
        {
            files.push(GeneratedFile {
                path: make_path("client.gen.go"),
                content: client_content,
            });
        }
        if want_server
            && let Some(services_content) = generate_services(&input, &config, &mut warnings)?
        {
            files.push(GeneratedFile {
                path: make_path("services.gen.go"),
                content: services_content,
            });
        }
    }

    // Generate validation file if there are constraints. Constraints arrive via
    // two parallel systems — `@`-annotations (counted in fields_with_metadata_count)
    // and `.`-control-operators carried inline on the field's type — so the gate
    // alone is not authoritative; generate_validation returns None when neither
    // surface actually yields a check.
    let validation_content = generate_validation(&input, &config, &mut warnings)?;
    // The timestamp must-parser is defined in the validation file when a timestamp
    // comparison lands there; the constructor file then references it rather than
    // re-declaring it (one definition per package).
    let timestamp_helper_defined = validation_content
        .as_deref()
        .is_some_and(|c| c.contains("func mustParseTimestamp"));
    if let Some(validation_content) = validation_content {
        files.push(GeneratedFile {
            path: make_path("validation.gen.go"),
            content: validation_content,
        });
    }

    // Generate constructors file if there are types with defaults
    if config.generate_constructors
        && let Some(constructors_content) =
            generate_constructors(&input, &config, &mut warnings, timestamp_helper_defined)?
    {
        files.push(GeneratedFile {
            path: make_path("constructors.gen.go"),
            content: constructors_content,
        });
    }

    // Self-contained publishable-package mode: when `emit_packages` includes "go",
    // emit the module manifest (and a README) alongside the source so the OUTPUT
    // directory is itself a valid, `go build`-able module. A consumer points a
    // require at the published repo path and imports it — no extra scaffolding, and
    // no go.sum because the generated code is stdlib-only and dependency-free.
    if emit_packages_includes_go(&input.config.options) {
        let module_path = resolve_module_path(&input);
        files.push(GeneratedFile {
            // The manifest lives at the module root regardless of any output subdir;
            // packages nested under a subdir are still part of this one module.
            path: "go.mod".to_string(),
            content: format!("module {module_path}\n\ngo 1.21\n"),
        });
        // The README is opt-out: only an explicit `emit_readme: false` suppresses it,
        // so a missing or non-bool value (and `true`) keeps the default-on behavior.
        if wants_readme(&input.config.options) {
            files.push(GeneratedFile {
                path: "genquickstart.md".to_string(),
                content: package_readme(&input, &module_path, &config),
            });
        }
    }

    let total_size: usize = files.iter().map(|f| f.content.len()).sum();

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: total_size,
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 100, // Mock generation time for WASM
        peak_memory_bytes: Some(estimate_memory_usage()),
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings,
        stats,
    })
}

/// Run the full Go generation for a prepared input and return the emitted files.
/// This is the exact path the wasm `generate` export drives; it is exposed so the
/// crate's own tests can run generation — and compile the emitted module — without
/// going through the pointer-based wasm boundary.
pub fn generate_go_files(input: WasmGeneratorInput) -> Result<Vec<GeneratedFile>, i32> {
    process_generation(input).map(|output| output.files)
}

/// True only when the `emit_packages` generation option is an array containing the
/// `"go"` token. Parsed defensively against an arbitrary `serde_json::Value`: a
/// missing option, a non-array value, or an array without `"go"` all leave the
/// output as source-only. The token match is case-insensitive to be forgiving of
/// callers that pass `"Go"`.
fn emit_packages_includes_go(options: &HashMap<String, serde_json::Value>) -> bool {
    options
        .get("emit_packages")
        .and_then(|v| v.as_array())
        .is_some_and(|tokens| {
            tokens
                .iter()
                .filter_map(|v| v.as_str())
                .any(|token| token.eq_ignore_ascii_case("go"))
        })
}

/// Whether to emit the package `genquickstart.md`. Default true; only an explicit
/// `emit_readme: false` suppresses it, so a missing or non-bool value (and `true`)
/// leaves the README in place.
fn wants_readme(options: &HashMap<String, serde_json::Value>) -> bool {
    options.get("emit_readme").and_then(|v| v.as_bool()) != Some(false)
}

/// The module path written into `go.mod`. Precedence: an explicit `go_module`
/// option wins; otherwise an explicit `package_name`; otherwise an `example.com/`
/// path derived from the first service's wire base (falling back to a generic
/// client name). A bare token is a legal Go module path, so a lone `package_name`
/// is acceptable here even though it has no domain.
fn resolve_module_path(input: &WasmGeneratorInput) -> String {
    let options = &input.config.options;
    if let Some(module) = options
        .get("go_module")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return module.to_string();
    }
    if let Some(package) = options
        .get("package_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return package.to_string();
    }
    format!("example.com/{}", default_package_token(input))
}

/// Fallback identifier used when neither a module path nor a package name was
/// configured: the first service's lowercased wire base, else a generic client name.
fn default_package_token(input: &WasmGeneratorInput) -> String {
    input
        .csil_spec
        .rules
        .iter()
        .find_map(|rule| match &rule.rule_type {
            CsilRuleType::ServiceDef(_) => Some(go_service_base(&rule.name).to_lowercase()),
            _ => None,
        })
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| "csilgenclient".to_string())
}

/// Reduce a module path (or any slash/dot-bearing coordinate) to a legal Go package
/// identifier: the last path segment, lowercased, with every character outside
/// `[a-z0-9_]` dropped. Go's `package` clause must be a bare identifier even though
/// the module path naming that same package is a full slash path, so
/// `github.com/org/corndogsapi` collapses to `corndogsapi`. A leading digit (illegal
/// to start a Go identifier) is prefixed with `_`, and a segment that sanitizes to
/// nothing falls back to `api`, so the emitted clause always compiles.
fn go_package_ident(source: &str) -> String {
    let segment = source.rsplit('/').next().unwrap_or(source);
    let ident: String = segment
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    match ident.chars().next() {
        None => "api".to_string(),
        Some(first) if first.is_ascii_digit() => format!("_{ident}"),
        Some(_) => ident,
    }
}

/// The package version. Go modules carry their version in VCS tags rather than in
/// `go.mod`, so this feeds only the README; the accessor is kept so it matches the
/// `package_version` option the sibling language generators read.
fn resolve_package_version(options: &HashMap<String, serde_json::Value>) -> String {
    options
        .get("package_version")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("0.1.0")
        .to_string()
}

/// The Go import path and package name of the official transport library. It is not
/// yet published, so a consumer vendors it or adds a `replace` directive; the
/// genquickstart spells that out in its Install section.
const GO_TRANSPORT_MODULE: &str = "github.com/catalystcommunity/csilgen/transports/go";

/// Which transport sections a consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or empty
/// value (or one that names none of the three) means "all three". Mirrors the
/// TypeScript reference so the CLI's `--readme-csil-*` flags drive every generator the
/// same way.
fn wanted_transports(options: &HashMap<String, serde_json::Value>) -> (bool, bool, bool) {
    let listed = match options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
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

/// README for the emitted module: a transport-by-transport **Quickstart** built on the
/// official `transports/go` library. The generated codec owns CBOR (de)serialization
/// of your types and the library owns the envelope, framing, and connection lifecycle;
/// you supply only a *carrier* that moves bytes, so the same typed surface rides HTTP,
/// TLS, a WebSocket, QUIC, or raw UDP unchanged. Each requested section (CSIL-RPC over
/// HTTP, CSIL-Events over TLS, CSIL-Datagrams over UDP) is a complete, copy-paste
/// program built on the library.
fn package_readme(input: &WasmGeneratorInput, module_path: &str, config: &GoConfig) -> String {
    let version = resolve_package_version(&input.config.options);
    let import_path = if config.output_subdir.is_empty() {
        module_path.to_string()
    } else {
        format!("{module_path}/{}", config.output_subdir)
    };
    let mut out = format!(
        "# {module_path}\n\n\
         Version {version}\n\n\
         Code generated by csilgen; DO NOT EDIT.\n\n\
         A typed CSIL client: the generated codec owns CBOR (de)serialization and the\n\
         [`transports/go`]({GO_TRANSPORT_MODULE}) library owns the envelope, framing,\n\
         and connection lifecycle. You supply only a *carrier* that moves bytes, so the\n\
         same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw UDP unchanged.\n\n\
         ## Install\n\n\
         ```sh\n\
         go get {import_path}\n\
         go get {GO_TRANSPORT_MODULE}\n\
         ```\n\n\
         The CSIL transport library is not yet published; until it is, vendor it or add\n\
         a `replace` directive in your `go.mod` pointing at a local checkout.\n\n"
    );

    let (rpc, events, datagrams) = wanted_transports(&input.config.options);
    // `package_readme` only renders in package mode, where the client surface is always
    // emitted (every target carries both surfaces, mirroring OCaml), so the typed RPC
    // example is meaningful regardless of which target was requested.
    let unary = first_unary_go_example(input, config);
    let channel = first_channel_go_example(input, config);
    if rpc {
        out.push_str(&go_rpc_section(&import_path, unary.as_ref()));
    }
    if events {
        out.push_str(&go_events_section(&import_path, channel.as_ref()));
    }
    if datagrams {
        out.push_str(&go_datagrams_section(&import_path, unary.as_ref()));
    }
    out
}

/// The pieces a unary (`->`) example call needs: the typed client constructor suffix
/// (`EchoClient` -> `api.NewEchoClient`), the method, a compiling sample request
/// literal (empty when the op takes no request), the request/response record type names
/// (so the datagram section can name `api.Encode<Req>`/`api.Decode<Res>`), and the op's
/// datagram ordinal.
struct GoExample {
    ctor: String,
    method: String,
    has_request: bool,
    sample: String,
    req_type: Option<String>,
    res_type: Option<String>,
    op_ord: u64,
}

/// The pieces the Events session needs to dispatch through the generated channel
/// router (`Route<Service>Channel`) and outbound encoder (`Encode<Service><Op>`):
/// the service interface name (which also prefixes the router/encoder), the wire
/// service name, the channel method name, the inbound record the router decodes and
/// hands to a handler (the op's input), the outbound record the encoder serializes
/// (the op's success output), and a compiling sample literal for the outbound record.
struct GoChannelExample {
    service_iface: String,
    wire_service: String,
    method: String,
    inbound_type: String,
    outbound_type: String,
    outbound_sample: String,
}

/// The first service (in declaration order) with a unidirectional op the typed client
/// actually emits (record success, and a null or record request), reduced to one
/// example call. `None` for a serviceless spec. The gating mirrors `emit_client_struct`
/// so the example always names a method that exists on the generated client.
fn first_unary_go_example(input: &WasmGeneratorInput, config: &GoConfig) -> Option<GoExample> {
    let records = record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        let base = go_service_base(&rule.name);
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = go_success_type(&op.output_type);
            let null_input = op_input_is_null(&op.input_type);
            if !is_record_ref(&success, &records)
                || !(null_input || is_record_ref(&op.input_type, &records))
            {
                continue;
            }
            return Some(GoExample {
                ctor: format!("{base}Client"),
                method: go_method_name(&op.name),
                has_request: !null_input,
                sample: if null_input {
                    String::new()
                } else {
                    go_request_sample(input, &op.input_type, &records, config)
                },
                req_type: (!null_input).then(|| type_ref_name(&op.input_type)),
                res_type: Some(type_ref_name(&success)),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.unwrap_or(1),
            });
        }
    }
    None
}

/// The first service (in declaration order) with a `<->` op whose inbound and outbound
/// are both records, so the generated per-type codec helpers exist for a compiling
/// Events session. `None` when no service has a usable channel op — the Events section
/// then shows the handshake/heartbeat without typed dispatch.
fn first_channel_go_example(
    input: &WasmGeneratorInput,
    config: &GoConfig,
) -> Option<GoChannelExample> {
    let records = record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = go_success_type(&op.output_type);
            if !is_record_ref(&success, &records) || !is_record_ref(&op.input_type, &records) {
                continue;
            }
            // The Events session dispatches through the generated server-side channel
            // router + encoder: the router decodes the op's input (inbound, handed to a
            // handler) and the encoder serializes the op's success output (outbound).
            return Some(GoChannelExample {
                service_iface: rule.name.clone(),
                wire_service: go_service_base(&rule.name).to_lowercase(),
                method: go_method_name(&op.name),
                inbound_type: type_ref_name(&op.input_type),
                outbound_type: type_ref_name(&success),
                outbound_sample: go_request_sample(input, &success, &records, config),
            });
        }
    }
    None
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `Transport` seam that
/// builds the request with the library's `RpcRequest` and parses the reply with
/// `RpcResponse` (never hand-rolled), POSTing to `{baseURL}/csil/v1/rpc`. The typed
/// client decodes the success payload; a non-zero transport status and the
/// `ServiceError` arm are surfaced distinctly. Rendered only when the package emits a
/// typed client (the `go-client` target); otherwise a short note points there.
fn go_rpc_section(import_path: &str, ex: Option<&GoExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`);\n\
         you bring a carrier that moves bytes. The HTTP carrier below is just one\n\
         example — swap `net/http` for any client (it satisfies the generated `Transport`\n\
         seam structurally).\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package emits no typed RPC client (generate the `go-client` target for\n\
             one), so there is no CSIL-RPC call to make here.\n\n",
        );
        return out;
    };
    out.push_str("```go\n");
    out.push_str("package main\n\n");
    out.push_str("import (\n");
    out.push_str(
        "\t\"bytes\"\n\t\"context\"\n\t\"fmt\"\n\t\"io\"\n\t\"net/http\"\n\t\"strings\"\n\n",
    );
    out.push_str(&format!(
        "\ttransport \"{GO_TRANSPORT_MODULE}\"\n\tapi \"{import_path}\"\n)\n\n"
    ));
    out.push_str(RPC_CARRIER_GO);
    out.push('\n');
    out.push_str("func main() {\n");
    out.push_str(&format!(
        "\tclient := api.New{}(&HTTPRpcCarrier{{BaseURL: \"http://localhost:5080\"}})\n",
        ex.ctor
    ));
    if ex.has_request {
        out.push_str(&format!(
            "\tresp, err := client.{}(context.Background(), {})\n",
            ex.method, ex.sample
        ));
    } else {
        out.push_str(&format!(
            "\tresp, err := client.{}(context.Background())\n",
            ex.method
        ));
    }
    out.push_str("\tif err != nil {\n\t\tpanic(err)\n\t}\n");
    out.push_str("\tfmt.Printf(\"%+v\\n\", resp)\n");
    out.push_str("}\n```\n\n");
    out
}

/// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as the
/// library's `StreamCarrier` (CSIL length-prefix framing), performs the
/// `$hello`/`$hello-ack` handshake, sends one outbound event whose payload the generated
/// codec encodes, and runs a recv loop that decodes each frame to an `Event`, answers
/// `$ping` with `$pong`, and decodes typed channel events with the generated codec. When
/// the spec has no usable channel op the typed dispatch is replaced with a note (the
/// handshake + heartbeat still apply to any connection).
fn go_events_section(import_path: &str, ch: Option<&GoChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library\n\
         owns the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and\n\
         framing; the generated channel router dispatches typed events. The TLS carrier\n\
         below is just one example — a WebSocket/WebTransport/QUIC carrier drops in\n\
         unchanged.\n\n",
    );
    out.push_str("```go\n");
    out.push_str("package main\n\n");
    out.push_str("import (\n");
    match ch {
        // The router dispatch needs a context, the codec adapter and handler need fmt.
        Some(_) => out.push_str("\t\"context\"\n\t\"crypto/tls\"\n\t\"fmt\"\n\n"),
        None => out.push_str("\t\"crypto/tls\"\n\t\"fmt\"\n\n"),
    }
    match ch {
        Some(_) => out.push_str(&format!(
            "\ttransport \"{GO_TRANSPORT_MODULE}\"\n\tapi \"{import_path}\"\n)\n\n"
        )),
        None => out.push_str(&format!("\ttransport \"{GO_TRANSPORT_MODULE}\"\n)\n\n")),
    }
    out.push_str(EVENTS_CARRIER_GO);
    out.push('\n');
    match ch {
        Some(ch) => {
            out.push_str(&go_channel_codec_and_handler(ch));
            out.push('\n');
            out.push_str(&go_events_session(ch));
        }
        None => out.push_str(EVENTS_NO_CHANNEL_SESSION_GO),
    }
    out.push_str("```\n\n");
    out
}

/// The codec adapter and handler the Events session feeds to the generated router.
/// The adapter bridges the router's byte-oriented `Codec` seam to the generated
/// per-type codec (decoding the inbound record, encoding the outbound record); the
/// handler implements the generated service interface — only the channel method is
/// exercised here, so the request/response methods ride the embedded interface.
fn go_channel_codec_and_handler(ch: &GoChannelExample) -> String {
    format!(
        r#"// channelCodec adapts the generated per-type codec to the router's Codec seam:
// Decode produces the inbound {inbound} the router hands to a handler; Encode
// serializes the outbound {outbound} the generated encoder pushes.
type channelCodec struct{{}}

func (channelCodec) Encode(value any) ([]byte, error) {{
	switch v := value.(type) {{
	case api.{outbound}:
		return api.Encode{outbound}(v), nil
	default:
		return nil, fmt.Errorf("unsupported channel type %T", value)
	}}
}}

func (channelCodec) Decode(data []byte, out any) error {{
	switch o := out.(type) {{
	case *api.{inbound}:
		decoded, err := api.Decode{inbound}(data)
		if err != nil {{
			return err
		}}
		*o = decoded
		return nil
	default:
		return fmt.Errorf("unsupported channel type %T", out)
	}}
}}

// eventHandlers implements the generated {service} interface. Only the channel method
// {method} is exercised on the events path; the request/response methods ride the
// embedded interface and are never called here.
type eventHandlers struct {{
	api.{service}
}}

func (eventHandlers) {method}(ctx context.Context, msg api.{inbound}) error {{
	fmt.Printf("channel event {method}: %+v\n", msg)
	return nil
}}
"#,
        inbound = ch.inbound_type,
        outbound = ch.outbound_type,
        service = ch.service_iface,
        method = ch.method,
    )
}

/// The channel session body for an Events connection that has a `<->` op: handshake,
/// one outbound event built by the generated encoder, and the recv loop that heartbeats
/// and dispatches inbound typed events into the generated channel router.
fn go_events_session(ch: &GoChannelExample) -> String {
    format!(
        r#"func session(carrier transport.FrameCarrier) error {{
	service := "{wire_service}"
	ctx := context.Background()
	codec := channelCodec{{}}
	handlers := eventHandlers{{}}

	// $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the
	// wire profile for the connection's lifetime.
	helloMsg := transport.Hello{{
		Versions: []uint64{{transport.VERSION}},
		Profiles: []string{{transport.ProfileVerbose.String()}},
		Service:  &service,
	}}
	hello, err := helloMsg.Encode()
	if err != nil {{
		return err
	}}
	if err := carrier.SendFrame(hello); err != nil {{
		return err
	}}
	ackFrame, err := carrier.RecvFrame()
	if err != nil {{
		return err
	}}
	if ackFrame == nil {{
		return fmt.Errorf("connection closed during handshake")
	}}
	ack, err := transport.DecodeHelloAck(ackFrame)
	if err != nil {{
		return err
	}}
	profile, ok := transport.ParseProfile(ack.Profile)
	if !ok {{
		return fmt.Errorf("unsupported profile %q", ack.Profile)
	}}

	// Send one outbound event: the generated encoder serializes the typed payload, the
	// library frames it as a verbose Event.
	method, payload, err := api.Encode{service}{method}(codec, {outbound_sample})
	if err != nil {{
		return err
	}}
	out, err := transport.NewVerboseEvent(&service, method, payload).Encode(profile)
	if err != nil {{
		return err
	}}
	if err := carrier.SendFrame(out); err != nil {{
		return err
	}}

	// Recv loop: decode each frame to an Event, answer $ping with $pong (the library
	// heartbeat), and dispatch typed channel events into the generated router.
	for {{
		frame, err := carrier.RecvFrame()
		if err != nil {{
			return err
		}}
		if frame == nil {{
			return nil // clean end of stream
		}}
		ev, err := transport.DecodeEvent(frame, profile)
		if err != nil {{
			return err
		}}
		if ev.Event != nil && *ev.Event == transport.PingName {{
			ping, err := transport.DecodeHeartbeat(ev.Payload)
			if err != nil {{
				return err
			}}
			pongMsg := transport.Heartbeat{{Nonce: ping.Nonce}}
			pongPayload, err := pongMsg.Encode()
			if err != nil {{
				return err
			}}
			pong, err := transport.NewVerboseEvent(&service, transport.PongName, pongPayload).Encode(profile)
			if err != nil {{
				return err
			}}
			if err := carrier.SendFrame(pong); err != nil {{
				return err
			}}
			continue
		}}
		if ev.Event != nil {{
			if err := api.Route{service}Channel(handlers, ctx, codec, *ev.Event, ev.Payload); err != nil {{
				return err
			}}
		}}
	}}
}}

func main() {{
	carrier, err := openTLSCarrier("localhost:7443")
	if err != nil {{
		panic(err)
	}}
	if err := session(carrier); err != nil {{
		panic(err)
	}}
}}
"#,
        wire_service = ch.wire_service,
        service = ch.service_iface,
        method = ch.method,
        outbound_sample = ch.outbound_sample,
    )
}

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in
/// the library's `Datagram`, and `SendDatagram` it fire-and-forget. The recv path
/// `DecodeDatagram`s an inbound datagram and decodes its payload with the generated
/// codec into the response type — there is NO synchronous response. The body lives in a
/// `runDatagrams(carrier)` helper so a test can drive it over a loopback carrier.
fn go_datagrams_section(import_path: &str, ex: Option<&GoExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram`\n\
         envelope; you bring a datagram carrier. The UDP carrier below is one example —\n\
         a WebRTC unreliable channel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no record `->` operations, so there is no datagram\n\
             payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_type), Some(res_type)) = (&ex.req_type, &ex.res_type) else {
        out.push_str(
            "This package's `->` operations have null or non-record payloads;\n\
             (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```go\n");
    out.push_str("package main\n\n");
    out.push_str("import (\n\t\"fmt\"\n\t\"net\"\n\n");
    out.push_str(&format!(
        "\ttransport \"{GO_TRANSPORT_MODULE}\"\n\tapi \"{import_path}\"\n)\n\n"
    ));
    out.push_str(&format!(
        r#"// opOrd is the operation's datagram ordinal — its @wire-id, or a channel-agreed number.
const opOrd = {op_ord}

// openUDPCarrier dials a UDP socket and wraps it as the library's DatagramCarrier.
func openUDPCarrier(addr string) (transport.DatagramCarrier, error) {{
	udpAddr, err := net.ResolveUDPAddr("udp", addr)
	if err != nil {{
		return nil, err
	}}
	conn, err := net.DialUDP("udp", nil, udpAddr)
	if err != nil {{
		return nil, err
	}}
	return transport.NewUDPDatagramCarrier(conn), nil
}}

// runDatagrams sends one `->` request as a Datagram fire-and-forget, then tries to
// decode a late inbound response. There is NO synchronous response: a datagram of the
// response type MAY arrive later — or never — so the caller must tolerate loss and
// reordering.
func runDatagrams(carrier transport.DatagramCarrier) error {{
	req := {req_sample}
	// seq 0 marks an unsequenced datagram.
	datagram, err := transport.NewDatagram(opOrd, 0, api.Encode{req_type}(req)).Encode()
	if err != nil {{
		return err
	}}
	if err := carrier.SendDatagram(datagram); err != nil {{
		return err
	}}

	inbound, err := carrier.RecvDatagram()
	if err != nil {{
		return err
	}}
	if inbound != nil {{
		dg, err := transport.DecodeDatagram(inbound)
		if err != nil {{
			return err
		}}
		resp, err := api.Decode{res_type}(dg.Payload)
		if err != nil {{
			return err
		}}
		fmt.Printf("late response: %+v\n", resp)
	}}
	return nil
}}

func main() {{
	carrier, err := openUDPCarrier("localhost:9000")
	if err != nil {{
		panic(err)
	}}
	if err := runDatagrams(carrier); err != nil {{
		panic(err)
	}}
}}
"#,
        op_ord = ex.op_ord,
        req_sample = ex.sample,
        req_type = req_type,
        res_type = res_type,
    ));
    out.push_str("```\n\n");
    out
}

/// A compiling Go literal for a unary op's request: `api.<Record>{ <required scalar
/// fields> }`. Only fields whose sample value needs no extra import are filled in;
/// the rest are left at their Go zero value, which keeps the snippet's import block
/// fixed and the literal valid (keyed struct literals do not require every field).
fn go_request_sample(
    input: &WasmGeneratorInput,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    config: &GoConfig,
) -> String {
    match ty {
        CsilTypeExpression::Reference(name) if records.contains(name) => {
            match find_record(input, name) {
                Some(group) => go_record_literal(input, name, group, records, config),
                None => format!("api.{name}{{}}"),
            }
        }
        _ => "nil".to_string(),
    }
}

/// `api.<Record>{ Field: <sample>, ... }` over the record's required fields, keyed by
/// the PascalCase names the generated Go struct uses. Fields whose type has no
/// import-free sample (timestamp, decimal, collections, choices) are omitted.
fn go_record_literal(
    input: &WasmGeneratorInput,
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    config: &GoConfig,
) -> String {
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            let key = e.key.as_ref()?;
            let sample = go_scalar_sample(input, &e.value_type, records, config)?;
            let field = go_field_name_from_key_with_metadata(key, &e.metadata);
            Some(format!("{field}: {sample}"))
        })
        .collect();
    if fields.is_empty() {
        format!("api.{name}{{}}")
    } else {
        format!("api.{name}{{{}}}", fields.join(", "))
    }
}

/// An import-free Go sample value for a field type, or `None` when no value can be
/// fabricated without pulling in another import (timestamp, decimal) or modelling a
/// composite. A nested record recurses into its own literal.
fn go_scalar_sample(
    input: &WasmGeneratorInput,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    config: &GoConfig,
) -> Option<String> {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => Some("\"example\"".to_string()),
            "bool" => Some("false".to_string()),
            "int" | "nint" | "uint" | "float" | "float64" | "double" => Some("0".to_string()),
            "bytes" | "bstr" => Some("nil".to_string()),
            _ => None,
        },
        CsilTypeExpression::Reference(rname) if records.contains(rname) => {
            find_record(input, rname).map(|g| go_record_literal(input, rname, g, records, config))
        }
        _ => None,
    }
}

/// The record a type reference names, if any. A `Name = { ... }` rule parses as
/// `TypeDef(Group(..))`, a bare group rule as `GroupDef(..)`; both are records.
fn find_record<'a>(input: &'a WasmGeneratorInput, name: &str) -> Option<&'a CsilGroupExpression> {
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

/// The CSIL-RPC HTTP carrier the Quickstart embeds — spec-independent, so a constant.
/// It builds the request envelope with the library's `RpcRequest`, POSTs it to
/// `{baseURL}/csil/v1/rpc`, and parses the reply with `RpcResponse`. `AsTransportError`
/// surfaces a non-zero transport status; the typed `ServiceError` arm (a status-0
/// variant) is surfaced separately so the typed client decodes success only. It
/// satisfies the generated `Transport` interface structurally.
const RPC_CARRIER_GO: &str = r#"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (RpcRequest/RpcResponse); the carrier owns only the transport. It satisfies the
// generated Transport interface structurally — swap net/http for any HTTP client.
type HTTPRpcCarrier struct {
	BaseURL string
	HTTP    *http.Client // optional; defaults to http.DefaultClient
}

func (t *HTTPRpcCarrier) Call(ctx context.Context, service, op string, req []byte) ([]byte, error) {
	envelope, err := transport.NewRpcRequest(service, op, req).Encode()
	if err != nil {
		return nil, err
	}
	url := strings.TrimRight(t.BaseURL, "/") + "/csil/v1/rpc"
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(envelope))
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", "application/cbor")
	httpReq.Header.Set("Accept", "application/cbor")
	client := t.HTTP
	if client == nil {
		client = http.DefaultClient
	}
	httpResp, err := client.Do(httpReq)
	if err != nil {
		return nil, err
	}
	defer httpResp.Body.Close()
	if httpResp.StatusCode < 200 || httpResp.StatusCode >= 300 {
		return nil, fmt.Errorf("csil-rpc %s/%s: http %d", service, op, httpResp.StatusCode)
	}
	body, err := io.ReadAll(httpResp.Body)
	if err != nil {
		return nil, err
	}
	resp, err := transport.DecodeRpcResponse(body)
	if err != nil {
		return nil, err
	}
	// AsTransportError returns a StatusError for any non-zero transport status.
	if err := resp.AsTransportError(); err != nil {
		return nil, err
	}
	// A typed application error rides as a status-0 "ServiceError" variant — distinct
	// from a transport failure. Surface it so the typed client decodes success only.
	if resp.Variant != nil && *resp.Variant == "ServiceError" {
		return nil, fmt.Errorf("csil-rpc %s/%s: ServiceError", service, op)
	}
	return resp.Payload, nil
}
"#;

/// The TLS `StreamCarrier` opener — spec-independent. It dials a TLS byte stream and
/// wraps it in the library's `StreamCarrier`, whose 4-byte length-prefix framing keeps
/// the session logic transport-agnostic.
const EVENTS_CARRIER_GO: &str = r#"// One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix
// via the library's StreamCarrier. Swap tls.Dial for a WebSocket/QUIC stream.
func openTLSCarrier(addr string) (transport.FrameCarrier, error) {
	conn, err := tls.Dial("tcp", addr, &tls.Config{})
	if err != nil {
		return nil, err
	}
	return transport.NewStreamCarrier(conn), nil
}
"#;

/// The Events session body when the spec declares no usable channel op: the handshake
/// and heartbeat still apply, so they are shown, with a note where typed dispatch would
/// go. Spec-independent, so a constant.
const EVENTS_NO_CHANNEL_SESSION_GO: &str = r#"func session(carrier transport.FrameCarrier) error {
	// $hello / $hello-ack handshake (control plane).
	helloMsg := transport.Hello{
		Versions: []uint64{transport.VERSION},
		Profiles: []string{transport.ProfileVerbose.String()},
	}
	hello, err := helloMsg.Encode()
	if err != nil {
		return err
	}
	if err := carrier.SendFrame(hello); err != nil {
		return err
	}
	ackFrame, err := carrier.RecvFrame()
	if err != nil {
		return err
	}
	if ackFrame == nil {
		return fmt.Errorf("connection closed during handshake")
	}
	ack, err := transport.DecodeHelloAck(ackFrame)
	if err != nil {
		return err
	}
	profile, ok := transport.ParseProfile(ack.Profile)
	if !ok {
		return fmt.Errorf("unsupported profile %q", ack.Profile)
	}

	// Recv loop: answer $ping with $pong. This package declares no <->/<- operations,
	// so there is no typed channel event to decode with the generated codec.
	for {
		frame, err := carrier.RecvFrame()
		if err != nil {
			return err
		}
		if frame == nil {
			return nil // clean end of stream
		}
		ev, err := transport.DecodeEvent(frame, profile)
		if err != nil {
			return err
		}
		if ev.Event != nil && *ev.Event == transport.PingName {
			ping, err := transport.DecodeHeartbeat(ev.Payload)
			if err != nil {
				return err
			}
			pongMsg := transport.Heartbeat{Nonce: ping.Nonce}
			pongPayload, err := pongMsg.Encode()
			if err != nil {
				return err
			}
			pong, err := transport.NewVerboseEvent(nil, transport.PongName, pongPayload).Encode(profile)
			if err != nil {
				return err
			}
			if err := carrier.SendFrame(pong); err != nil {
				return err
			}
		}
	}
}

func main() {
	carrier, err := openTLSCarrier("localhost:7443")
	if err != nil {
		panic(err)
	}
	if err := session(carrier); err != nil {
		panic(err)
	}
}
"#;

/// In-memory Go type selected for the CSIL `decimal` core type. The wire form is
/// CBOR tag 4 either way; this only changes what the generated struct field is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper (no third-party dependency).
    Csil,
    /// Use `github.com/shopspring/decimal.Decimal`.
    Library,
}

#[derive(Debug)]
struct GoConfig {
    package_name: String,
    output_subdir: String,
    use_json_tags: bool,
    use_yaml_tags: bool,
    generate_validation: bool,
    generate_constructors: bool,
    decimal_mapping: DecimalMapping,
    indent_style: String,
    go_imports: Vec<String>,
}

impl GoConfig {
    /// The Go type a `decimal` field maps to under the active mapping. Both forms
    /// carry the identical CBOR tag-4 wire value; only the in-memory type differs.
    fn decimal_go_type(&self) -> &'static str {
        match self.decimal_mapping {
            DecimalMapping::Csil => "CsilDecimal",
            DecimalMapping::Library => "decimal.Decimal",
        }
    }

    /// Parse options into a config. A `decimal_mapping` other than `"csil"`
    /// (default) or `"library"` is a hard error so misconfiguration surfaces at
    /// generation time rather than silently degrading, matching the validate-early
    /// idiom used for `ts_bidirectional_transport`.
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
        let go_package = options.get("go_package").and_then(|v| v.as_str());

        // The Go *package clause* must be a bare identifier, yet the same coordinate
        // doubles as the `go.mod` module path / import path, which is a slash path.
        // Derive the clause from the last path segment of whichever coordinate was
        // supplied (`go_package` taking precedence over `package_name`) so a single
        // path-style value — e.g. `github.com/org/corndogsapi` — yields both a valid
        // `module github.com/org/corndogsapi` line and a valid `package corndogsapi`.
        let ident_source = go_package
            .or_else(|| options.get("package_name").and_then(|v| v.as_str()))
            .unwrap_or("api");
        let package_name = go_package_ident(ident_source);

        // Optionally derive output subdirectory from go_module and go_package.
        // If go_module is provided, strip it from go_package to get the relative path.
        // e.g., go_module="github.com/foo/bar", go_package="github.com/foo/bar/v1/internal/config"
        // -> output_subdir="v1/internal/config"
        // If go_module is NOT provided, output_subdir remains empty (files go to --output dir).
        let output_subdir = options
            .get("go_module")
            .and_then(|v| v.as_str())
            .and_then(|module| {
                go_package.and_then(|pkg| {
                    pkg.strip_prefix(module)
                        .map(|s| s.trim_start_matches('/').to_string())
                })
            })
            .unwrap_or_default();

        // Parse go_imports as array of strings
        let go_imports = options
            .get("go_imports")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let decimal_mapping = match options.get("decimal_mapping") {
            None => DecimalMapping::Csil,
            Some(v) => match v.as_str() {
                Some("csil") => DecimalMapping::Csil,
                Some("library") => DecimalMapping::Library,
                _ => return Err(error_codes::GENERATION_ERROR),
            },
        };

        Ok(Self {
            package_name,
            output_subdir,
            use_json_tags: options
                .get("use_json_tags")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            use_yaml_tags: options
                .get("use_yaml_tags")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            generate_validation: options
                .get("generate_validation")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            generate_constructors: options
                .get("generate_constructors")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            decimal_mapping,
            indent_style: "\t".to_string(), // Go convention is tabs
            go_imports,
        })
    }
}

fn generate_types(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();

    // Package-level documentation
    let package_description = input
        .config
        .options
        .get("package_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !package_description.is_empty() {
        // Add custom package description
        for line in package_description.lines() {
            content.push_str(&format!("// {line}\n"));
        }
        content.push_str("//\n");
    } else {
        // Default package comment
        content.push_str(&format!(
            "// Package {} contains generated types.\n",
            config.package_name
        ));
        content.push_str("//\n");
    }

    // Generated code warning
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");

    // Package declaration
    content.push_str(&format!("package {}\n\n", config.package_name));

    // Imports are the caller-configured set plus whatever the mapped types force:
    // `timestamp` needs `time`, and a `decimal` under the library mapping needs
    // shopspring. The default decimal mapping pulls no third-party package here —
    // its CsilDecimal lives in the same package, in its own generated file.
    let mut imports = config.go_imports.clone();
    if spec_uses_builtin(input, "timestamp") {
        imports.push("time".to_string());
    }
    if config.decimal_mapping == DecimalMapping::Library && spec_uses_builtin(input, "decimal") {
        imports.push("github.com/shopspring/decimal".to_string());
    }
    if !imports.is_empty() {
        content.push_str("import (\n");
        for import_path in &imports {
            content.push_str(&format!("{}\"{}\"", config.indent_style, import_path));
            content.push('\n');
        }
        content.push_str(")\n\n");
    }

    // Generate type definitions
    let mut has_types = false;
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                has_types = true;
                content.push_str(&format!(
                    "// {} represents a structured data type\n",
                    rule.name
                ));
                content.push_str(&format!("type {} struct {{\n", rule.name));

                for entry in &group.entries {
                    if let Some(key) = &entry.key {
                        let field_name = go_field_name_from_key_with_metadata(key, &entry.metadata);
                        // Check for @go_type override first, otherwise map CSIL type
                        let go_type = get_go_type_override(&entry.metadata).unwrap_or_else(|| {
                            map_csil_type_to_go(
                                &entry.value_type,
                                &entry.occurrence,
                                config.decimal_go_type(),
                            )
                        });

                        // Add field documentation
                        if let Some(description) = get_field_description(&entry.metadata) {
                            content
                                .push_str(&format!("{}// {}\n", config.indent_style, description));
                        }

                        if let Some(depends) = get_depends_comment(&entry.metadata) {
                            content.push_str(&format!(
                                "{}// depends-on: {depends}\n",
                                config.indent_style
                            ));
                        }

                        content.push_str(&format!(
                            "{}{} {}",
                            config.indent_style, field_name, go_type
                        ));

                        // Add struct tags
                        let mut tag_parts = Vec::new();

                        // Add JSON tags if enabled
                        if config.use_json_tags {
                            let json_name = go_json_name_from_key(key);

                            // Add omitempty for optional fields
                            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                tag_parts.push(format!("json:\"{},omitempty\"", json_name));
                            } else {
                                tag_parts.push(format!("json:\"{}\"", json_name));
                            }

                            // Check field visibility
                            let visibility = get_field_visibility(&entry.metadata);
                            match visibility {
                                CsilFieldVisibility::SendOnly => {
                                    tag_parts.push("json:\"-\" # send-only".to_string());
                                    warnings.push(GeneratorWarning {
                                        level: WarningLevel::Info,
                                        message: format!("Field '{field_name}' marked as send-only, consider separate request/response types"),
                                        location: None,
                                        suggestion: Some("Create separate request and response structs for better type safety".to_string()),
                                    });
                                }
                                CsilFieldVisibility::ReceiveOnly => {
                                    tag_parts.push("# receive-only".to_string());
                                }
                                _ => {}
                            }
                        }

                        // Add YAML tags if enabled
                        if config.use_yaml_tags {
                            let yaml_name = go_json_name_from_key(key);

                            // Check if this is a map type that should be inlined
                            // Map types with occurrence indicator should use inline
                            let is_inline_map =
                                matches!(&entry.value_type, CsilTypeExpression::Map { .. });

                            if is_inline_map {
                                tag_parts.push("yaml:\",inline\"".to_string());
                            } else if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                tag_parts.push(format!("yaml:\"{},omitempty\"", yaml_name));
                            } else {
                                tag_parts.push(format!("yaml:\"{}\"", yaml_name));
                            }
                        }

                        if !tag_parts.is_empty() {
                            content.push_str(&format!(" `{}`", tag_parts.join(" ")));
                        }

                        content.push('\n');
                    }
                }

                content.push_str("}\n\n");
            }
            CsilRuleType::TypeDef(type_expr) => {
                has_types = true;

                // Special case: if TypeDef contains a Group expression, expand it as a struct
                if let CsilTypeExpression::Group(group) = type_expr {
                    content.push_str(&format!(
                        "// {} represents a structured data type\n",
                        rule.name
                    ));
                    content.push_str(&format!("type {} struct {{\n", rule.name));

                    for entry in &group.entries {
                        if let Some(key) = &entry.key {
                            let field_name =
                                go_field_name_from_key_with_metadata(key, &entry.metadata);
                            // Check for @go_type override first, otherwise map CSIL type
                            let go_type =
                                get_go_type_override(&entry.metadata).unwrap_or_else(|| {
                                    map_csil_type_to_go(
                                        &entry.value_type,
                                        &entry.occurrence,
                                        config.decimal_go_type(),
                                    )
                                });

                            if let Some(description) = get_field_description(&entry.metadata) {
                                content.push_str(&format!(
                                    "{}// {}\n",
                                    config.indent_style, description
                                ));
                            }

                            if let Some(depends) = get_depends_comment(&entry.metadata) {
                                content.push_str(&format!(
                                    "{}// depends-on: {depends}\n",
                                    config.indent_style
                                ));
                            }

                            content.push_str(&format!(
                                "{}{} {}",
                                config.indent_style, field_name, go_type
                            ));

                            let mut tag_parts = Vec::new();

                            if config.use_json_tags {
                                let json_name = go_json_name_from_key(key);
                                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                    tag_parts.push(format!("json:\"{},omitempty\"", json_name));
                                } else {
                                    tag_parts.push(format!("json:\"{}\"", json_name));
                                }
                            }

                            if config.use_yaml_tags {
                                let yaml_name = go_json_name_from_key(key);
                                let is_inline_map =
                                    matches!(&entry.value_type, CsilTypeExpression::Map { .. });

                                if is_inline_map {
                                    tag_parts.push("yaml:\",inline\"".to_string());
                                } else if matches!(entry.occurrence, Some(CsilOccurrence::Optional))
                                {
                                    tag_parts.push(format!("yaml:\"{},omitempty\"", yaml_name));
                                } else {
                                    tag_parts.push(format!("yaml:\"{}\"", yaml_name));
                                }
                            }

                            if !tag_parts.is_empty() {
                                content.push_str(&format!(" `{}`", tag_parts.join(" ")));
                            }

                            content.push('\n');
                        }
                    }

                    content.push_str("}\n\n");
                } else {
                    // Regular type alias
                    let go_type = map_csil_type_to_go(type_expr, &None, config.decimal_go_type());
                    content.push_str(&format!("// {} is a type alias\n", rule.name));
                    content.push_str(&format!("type {} {}\n\n", rule.name, go_type));
                }
            }
            _ => {} // Services handled separately
        }
    }

    if has_types {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

/// Client scaffolding emitted once at the top of `client.gen.go`: the error type
/// and the caller-supplied `Transport` every per-service client delegates to.
const CLIENT_PRELUDE_GO: &str = "\
// ClientError is returned by a generated client call: a structured error the
// service returned (Code/Message), or a transport-level failure (Err).
type ClientError struct {
\tCode    int64
\tMessage string
\tErr     error
}

func (e *ClientError) Error() string {
\tif e.Err != nil {
\t\treturn \"transport error: \" + e.Err.Error()
\t}
\treturn fmt.Sprintf(\"service error %d: %s\", e.Code, e.Message)
}

// Transport is the caller-supplied byte carrier: it performs the call named by
// (service, op) with the already-encoded request bytes and returns the response
// bytes, or an error. The generated client owns (de)serialization via the codec;
// the carrier only moves bytes, so it can be HTTP, a queue, or an in-process loop.
type Transport interface {
\tCall(ctx context.Context, service string, op string, req []byte) ([]byte, error)
}
";

/// The body of the self-contained `CsilDecimal` helper, injected as its own file
/// only when the spec uses `decimal` under the default mapping. It holds the exact
/// value (int exponent + big.Int mantissa); the generated `codec.gen.go` owns its
/// CBOR tag-4 wire form directly (no third-party CBOR library). Conversion to/from
/// shopspring is via String() and ParseCsilDecimal, so the helper itself takes no
/// dependency on shopspring either.
const CSIL_DECIMAL_GO: &str = r#"// CsilDecimal is the exact, base-10 `decimal` core type. On the wire it is CBOR
// tag 4 (decimal fraction): a two-element array [exponent, mantissa] whose value
// is Mantissa * 10^Exponent. The value is kept as exact integers, never a float,
// so no precision is lost. The wire (de)serialization lives in codec.gen.go, which
// reads/writes Exponent and mantissa() directly, so this type depends on no CBOR
// library. Interop with github.com/shopspring/decimal is via String()/ParseCsilDecimal.
type CsilDecimal struct {
	Exponent int64
	Mantissa *big.Int
}

// mantissa treats the zero value as 0 so a never-assigned CsilDecimal is usable.
func (d CsilDecimal) mantissa() *big.Int {
	if d.Mantissa == nil {
		return big.NewInt(0)
	}
	return d.Mantissa
}

// String renders the exact value as canonical decimal text. This is the lossless
// bridge to other decimal libraries, e.g. shopspring.NewFromString(d.String()).
func (d CsilDecimal) String() string {
	m := d.mantissa()
	if d.Exponent == 0 {
		return m.String()
	}
	if d.Exponent > 0 {
		scale := new(big.Int).Exp(big.NewInt(10), big.NewInt(d.Exponent), nil)
		return new(big.Int).Mul(m, scale).String()
	}
	neg := m.Sign() < 0
	digits := new(big.Int).Abs(m).String()
	scale := int(-d.Exponent)
	sign := ""
	if neg {
		sign = "-"
	}
	if len(digits) <= scale {
		return sign + "0." + strings.Repeat("0", scale-len(digits)) + digits
	}
	return sign + digits[:len(digits)-scale] + "." + digits[len(digits)-scale:]
}

// ParseCsilDecimal parses canonical decimal text (what String produces, and what
// shopspring.Decimal.String emits) into an exact CsilDecimal.
func ParseCsilDecimal(s string) (CsilDecimal, error) {
	s = strings.TrimSpace(s)
	neg := false
	switch {
	case strings.HasPrefix(s, "-"):
		neg = true
		s = s[1:]
	case strings.HasPrefix(s, "+"):
		s = s[1:]
	}
	intPart, fracPart := s, ""
	if i := strings.IndexByte(s, '.'); i >= 0 {
		intPart, fracPart = s[:i], s[i+1:]
	}
	digits := intPart + fracPart
	if digits == "" {
		digits = "0"
	}
	m, ok := new(big.Int).SetString(digits, 10)
	if !ok {
		return CsilDecimal{}, fmt.Errorf("CsilDecimal: invalid decimal string %q", s)
	}
	if neg {
		m.Neg(m)
	}
	return CsilDecimal{Exponent: -int64(len(fracPart)), Mantissa: m}, nil
}

// mustParseCsilDecimal parses a bound literal embedded at generation time. The text
// is fixed by the spec, so a parse failure signals a generator bug, not bad input.
func mustParseCsilDecimal(s string) CsilDecimal {
	d, err := ParseCsilDecimal(s)
	if err != nil {
		panic(err)
	}
	return d
}

// Cmp returns -1, 0, or +1 as d is less than, equal to, or greater than other.
// The comparison is exact: both values are scaled to a common exponent and their
// integer mantissas compared, so no float rounding can flip the result.
func (d CsilDecimal) Cmp(other CsilDecimal) int {
	dm := new(big.Int).Set(d.mantissa())
	om := new(big.Int).Set(other.mantissa())
	switch {
	case d.Exponent > other.Exponent:
		scale := new(big.Int).Exp(big.NewInt(10), big.NewInt(d.Exponent-other.Exponent), nil)
		dm.Mul(dm, scale)
	case other.Exponent > d.Exponent:
		scale := new(big.Int).Exp(big.NewInt(10), big.NewInt(other.Exponent-d.Exponent), nil)
		om.Mul(om, scale)
	}
	return dm.Cmp(om)
}
"#;

/// Assemble the standalone `csil_decimal.gen.go` file: package header, the imports
/// the helper needs (`math/big`, `strings`, the cbor codec), and the helper body.
fn csil_decimal_file(config: &GoConfig) -> String {
    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains the exact-decimal helper.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));
    content.push_str("import (\n");
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    content.push_str(&format!("{}\"math/big\"\n", config.indent_style));
    content.push_str(&format!("{}\"strings\"\n", config.indent_style));
    content.push_str(")\n\n");
    content.push_str(CSIL_DECIMAL_GO);
    content
}

// ---------------------------------------------------------------------------
// Per-type CBOR codec (codec.gen.go)
//
// CSIL is the CBOR Service Interface Language; the canonical wire is a CBOR map
// keyed by the CSIL field name verbatim. Go could lean on a reflection codec
// (fxamacker), but that drags in a third-party dependency and keys by reflection
// at runtime; the references for this batch (C/Zig/OCaml/Dart/Swift) instead emit
// a self-contained per-type codec so the bytes are owned by generated code. Go
// follows the same shape here so every target agrees byte-for-byte.
// ---------------------------------------------------------------------------

/// The names of every record type in the spec (a `GroupDef`, or a `TypeDef` that
/// wraps a `Group`). Only records get a codec, so a `Reference` to one of these is
/// what a field/operation payload (de)serializes through.
fn record_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(_) => Some(rule.name.clone()),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(rule.name.clone()),
            _ => None,
        })
        .collect()
}

/// Whether the spec declares any record type (and so wants a `codec.gen.go`).
fn spec_has_records(input: &WasmGeneratorInput) -> bool {
    input.csil_spec.rules.iter().any(|rule| {
        matches!(
            &rule.rule_type,
            CsilRuleType::GroupDef(_) | CsilRuleType::TypeDef(CsilTypeExpression::Group(_))
        )
    })
}

/// The CBOR encoding of a text key. Comparing these byte slices lexicographically
/// is exactly RFC 8949 §4.2.1 canonical key ordering, computed here at generation
/// time so the emitted encoder lays a record's map keys down in canonical order.
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

/// A Go expression building a `cborValue` from `expr` (a typed Go value of the
/// field's mapped type). Composite types map via the generic `cborEncArray`/
/// `cborEncMap` runtime helpers so nesting composes cleanly.
fn go_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    config: &GoConfig,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("cborInt({expr})"),
            "uint" => format!("cborUint({expr})"),
            "float" | "float64" | "double" => format!("cborFloat({expr})"),
            "text" | "tstr" => format!("cborText({expr})"),
            "bytes" | "bstr" => format!("cborBytes({expr})"),
            "bool" => format!("cborBool({expr})"),
            "timestamp" => format!("csilEncTimestamp({expr})"),
            "decimal" => format!("csilEncDecimal({expr})"),
            "nil" | "null" => "cborNull{}".to_string(),
            _ => "cborNull{}".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(name) => {
            format!("csilEnc{name}({expr})")
        }
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`,
        // `Tags = [* text]`, `Uuid = text`) has no codec of its own; encode it as its
        // underlying type. The named Go type is assignable to the unnamed underlying
        // the map/array/scalar encoder expects, so the same `expr` flows through.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            go_enc_value(&aliases[name], expr, records, aliases, config)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let elem_ty = map_csil_type_to_go(element_type, &None, config.decimal_go_type());
            let inner = go_enc_value(element_type, "csilElem", records, aliases, config);
            format!("cborEncArray({expr}, func(csilElem {elem_ty}) cborValue {{ return {inner} }})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let key_ty = map_csil_type_to_go(key, &None, config.decimal_go_type());
            let val_ty = map_csil_type_to_go(value, &None, config.decimal_go_type());
            let kenc = go_enc_value(key, "csilK", records, aliases, config);
            let venc = go_enc_value(value, "csilV", records, aliases, config);
            format!(
                "cborEncMap({expr}, func(csilK {key_ty}) cborValue {{ return {kenc} }}, func(csilV {val_ty}) cborValue {{ return {venc} }})"
            )
        }
        // A type the codec cannot model precisely (a choice, a tuple, `any`) is
        // carried as null rather than emitting uncompilable code.
        _ => "cborNull{}".to_string(),
    }
}

/// A Go expression of function type `func(cborValue) (<GoType>, error)` decoding a
/// typed value from a `cborValue`. Builtins resolve to a bare runtime accessor
/// name; composites wrap the generic `cborDecArray`/`cborDecMap` helpers.
fn go_dec_func(
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    config: &GoConfig,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => "cborAsI64".to_string(),
            "uint" => "cborAsU64".to_string(),
            "float" | "float64" | "double" => "cborAsF64".to_string(),
            "text" | "tstr" => "cborAsText".to_string(),
            "bytes" | "bstr" => "cborAsBytes".to_string(),
            "bool" => "cborAsBool".to_string(),
            "timestamp" => "csilAsTimestamp".to_string(),
            "decimal" => "csilAsDecimal".to_string(),
            other => codec_zero_decoder(&map_csil_type_to_go(
                &CsilTypeExpression::Builtin(other.to_string()),
                &None,
                config.decimal_go_type(),
            )),
        },
        CsilTypeExpression::Reference(name) if records.contains(name) => format!("csilDec{name}"),
        // A reference to a transparent alias (or enum) decodes via its underlying type,
        // then converts the bare underlying the runtime returns to the named field type.
        // Go does NOT implicitly assign a `string` to a named `HouseID`, nor a `[]string`
        // to a named `Tags`, so the wrapper makes the decoder yield the field's exact
        // type — which also lets `cborDecArray`/`cborDecMap` infer the right element type
        // (`[]MemberID`, not `[]string`).
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            let inner = go_dec_func(&aliases[name], records, aliases, config);
            let go_type = map_csil_type_to_go(ty, &None, config.decimal_go_type());
            format!(
                "func(csilV cborValue) ({go_type}, error) {{ csilInner, csilErr := ({inner})(csilV); return {go_type}(csilInner), csilErr }}"
            )
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let elem_ty = map_csil_type_to_go(element_type, &None, config.decimal_go_type());
            let inner = go_dec_func(element_type, records, aliases, config);
            format!(
                "func(csilV cborValue) ([]{elem_ty}, error) {{ return cborDecArray(csilV, {inner}) }}"
            )
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let key_ty = map_csil_type_to_go(key, &None, config.decimal_go_type());
            let val_ty = map_csil_type_to_go(value, &None, config.decimal_go_type());
            let kf = go_dec_func(key, records, aliases, config);
            let vf = go_dec_func(value, records, aliases, config);
            format!(
                "func(csilV cborValue) (map[{key_ty}]{val_ty}, error) {{ return cborDecMap(csilV, {kf}, {vf}) }}"
            )
        }
        // The codec cannot reconstruct this shape; yield its zero value so the
        // generated decoder still compiles against the field's Go type.
        other => codec_zero_decoder(&map_csil_type_to_go(other, &None, config.decimal_go_type())),
    }
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference (NOT a record group or a choice, which have
/// their own handling). A field referencing one must encode as the underlying type
/// rather than the `null` stub a bare non-record reference would yield.
fn codec_aliases(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) => None,
                // An enum (a choice of scalar literals) has no codec of its own, but it
                // round-trips as its backing scalar: alias it to that builtin so the
                // shared encode (`cborText`) and decode-with-conversion paths handle it
                // exactly like a `Name = text` scalar alias. A true (non-literal) union
                // stays unaliased and falls back to the null stub.
                CsilTypeExpression::Choice(choices) => enum_scalar_builtin(choices)
                    .map(|builtin| (rule.name.clone(), CsilTypeExpression::Builtin(builtin))),
                other => Some((rule.name.clone(), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Whether an optional field maps to a Go pointer. `map_csil_type_to_go` wraps a
/// scalar/record in `*T` for an optional, but a slice (`array`), map, or tuple is
/// already nil-able in place and is returned unwrapped — so those are not pointers.
fn optional_field_is_pointer(ty: &CsilTypeExpression) -> bool {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => optional_field_is_pointer(base_type),
        CsilTypeExpression::Array { .. }
        | CsilTypeExpression::Map { .. }
        | CsilTypeExpression::Tuple(_) => false,
        _ => true,
    }
}

/// A `func(cborValue) (<go_type>, error)` that ignores the value and returns the
/// zero value — the decode fallback for a payload shape the codec cannot model.
fn codec_zero_decoder(go_type: &str) -> String {
    format!("func(cborValue) ({go_type}, error) {{ var csilZero {go_type}; return csilZero, nil }}")
}

/// Emit the `csilEnc<T>`/`csilDec<T>` pair plus the public `Encode<T>`/`Decode<T>`
/// byte wrappers for one record. The encoder lays keys in canonical order; the
/// decoder reads by key in declaration order (order is irrelevant on decode).
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    config: &GoConfig,
) -> String {
    // (member, wire, entry) in declaration order, plus a canonical-key-order copy
    // for the encoder so the emitted map is deterministic across languages.
    let named: Vec<(String, String, &CsilGroupEntry)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let key = e.key.as_ref()?;
            let member = go_field_name_from_key_with_metadata(key, &e.metadata);
            let wire = go_json_name_from_key(key);
            Some((member, wire, e))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    let i = &config.indent_style;
    let mut out = String::new();

    out.push_str(&format!(
        "// csilEnc{name} builds the canonical CBOR value tree for a {name}.\n"
    ));
    out.push_str(&format!("func csilEnc{name}(csilV {name}) cborValue {{\n"));
    out.push_str(&format!(
        "{i}csilEntries := make(cborMap, 0, {})\n",
        named.len()
    ));
    for (member, wire, entry) in &canonical {
        let wire_lit = go_string_lit(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // An absent optional is omitted from the map entirely (wire contract). A
            // scalar/record optional is a Go pointer (deref to read); an optional
            // slice/map is nil-able in place, so it is read without a deref.
            let read = if optional_field_is_pointer(&entry.value_type) {
                format!("(*csilV.{member})")
            } else {
                format!("csilV.{member}")
            };
            let enc = go_enc_value(&entry.value_type, &read, records, aliases, config);
            out.push_str(&format!(
                "{i}if csilV.{member} != nil {{\n{i}{i}csilEntries = append(csilEntries, cborEntry{{cborText({wire_lit}), {enc}}})\n{i}}}\n"
            ));
        } else {
            let enc = go_enc_value(
                &entry.value_type,
                &format!("csilV.{member}"),
                records,
                aliases,
                config,
            );
            out.push_str(&format!(
                "{i}csilEntries = append(csilEntries, cborEntry{{cborText({wire_lit}), {enc}}})\n"
            ));
        }
    }
    out.push_str(&format!("{i}return csilEntries\n}}\n\n"));

    out.push_str(&format!(
        "// csilDec{name} reconstructs a {name} from a decoded CBOR value tree.\n"
    ));
    out.push_str(&format!(
        "func csilDec{name}(csilRoot cborValue) ({name}, error) {{\n"
    ));
    out.push_str(&format!("{i}var csilOut {name}\n"));
    for (member, wire, entry) in &named {
        let wire_lit = go_string_lit(wire);
        // `go_dec_func` returns a decoder whose result is the field's exact Go type
        // (it converts a named scalar alias / enum to its named type), so the assignment
        // is a plain copy — no conversion needed here.
        let dec = go_dec_func(&entry.value_type, records, aliases, config);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // A missing optional key leaves the field at its zero value (nil); a
            // present one decodes into a fresh local. A pointer field stores its
            // address; a nil-able slice/map field stores the value directly.
            let assign = if optional_field_is_pointer(&entry.value_type) {
                format!("csilOut.{member} = &csilVal")
            } else {
                format!("csilOut.{member} = csilVal")
            };
            out.push_str(&format!(
                "{i}if csilField, csilOk := cborMapGet(csilRoot, {wire_lit}); csilOk {{\n\
                 {i}{i}csilVal, csilErr := ({dec})(csilField)\n\
                 {i}{i}if csilErr != nil {{\n{i}{i}{i}return csilOut, csilErr\n{i}{i}}}\n\
                 {i}{i}{assign}\n{i}}}\n"
            ));
        } else {
            out.push_str(&format!(
                "{i}{{\n\
                 {i}{i}csilField, csilErr := cborRequire(csilRoot, {wire_lit})\n\
                 {i}{i}if csilErr != nil {{\n{i}{i}{i}return csilOut, csilErr\n{i}{i}}}\n\
                 {i}{i}csilVal, csilErr := ({dec})(csilField)\n\
                 {i}{i}if csilErr != nil {{\n{i}{i}{i}return csilOut, csilErr\n{i}{i}}}\n\
                 {i}{i}csilOut.{member} = csilVal\n{i}}}\n"
            ));
        }
    }
    out.push_str(&format!("{i}return csilOut, nil\n}}\n\n"));

    out.push_str(&format!(
        "// Encode{name} encodes a {name} to canonical CSIL CBOR bytes.\n"
    ));
    out.push_str(&format!(
        "func Encode{name}(csilV {name}) []byte {{\n{i}return cborEncode(csilEnc{name}(csilV))\n}}\n\n"
    ));
    out.push_str(&format!(
        "// Decode{name} decodes canonical CSIL CBOR bytes into a {name}.\n"
    ));
    out.push_str(&format!(
        "func Decode{name}(csilData []byte) ({name}, error) {{\n\
         {i}csilRoot, csilErr := cborDecode(csilData)\n\
         {i}if csilErr != nil {{\n{i}{i}var csilZero {name}\n{i}{i}return csilZero, csilErr\n{i}}}\n\
         {i}return csilDec{name}(csilRoot)\n}}\n\n"
    ));
    out
}

/// Build `codec.gen.go`: the self-contained canonical-CBOR runtime plus an
/// `Encode`/`Decode` pair per record. `None` when the spec declares no records.
fn generate_codec(input: &WasmGeneratorInput, config: &GoConfig) -> Option<String> {
    if !spec_has_records(input) {
        return None;
    }
    let records = record_names(input);
    let aliases = codec_aliases(input);
    let uses_timestamp = spec_uses_builtin(input, "timestamp");
    let uses_decimal = spec_uses_builtin(input, "decimal");

    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains the self-contained canonical-CBOR codec.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    content.push_str("import (\n");
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    content.push_str(&format!("{}\"math\"\n", config.indent_style));
    if uses_timestamp {
        content.push_str(&format!("{}\"time\"\n", config.indent_style));
    }
    if uses_decimal {
        // The bignum mantissa fallback (a tag-2/3 byte string) keeps the exact
        // value when it exceeds 64 bits.
        content.push_str(&format!("{}\"math/big\"\n", config.indent_style));
        if config.decimal_mapping == DecimalMapping::Library {
            content.push('\n');
            content.push_str(&format!(
                "{}\"github.com/shopspring/decimal\"\n",
                config.indent_style
            ));
        }
    }
    content.push_str(")\n\n");

    content.push_str(CODEC_RUNTIME_GO);
    if uses_timestamp {
        content.push('\n');
        content.push_str(CODEC_TIMESTAMP_GO);
    }
    if uses_decimal {
        content.push('\n');
        content.push_str(CODEC_BIGINT_GO);
        content.push('\n');
        content.push_str(match config.decimal_mapping {
            DecimalMapping::Csil => CODEC_DECIMAL_CSIL_GO,
            DecimalMapping::Library => CODEC_DECIMAL_LIBRARY_GO,
        });
    }
    content.push('\n');

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            content.push_str(&emit_record_codec(
                &rule.name, group, &records, &aliases, config,
            ));
        }
    }
    Some(content)
}

/// The self-contained canonical-CBOR (RFC 8949 subset) value model, encoder,
/// decoder, generic composite helpers, and accessors every generated codec builds
/// on. `bytes` is a Go `[]byte` carried as a CBOR byte string (major type 2) by
/// construction, never an array of integers.
const CODEC_RUNTIME_GO: &str = r#"// cborValue is a minimal canonical-CBOR value tree: a closed set of variants the
// generated codec builds and walks. The marker method keeps the set closed.
type cborValue interface{ isCbor() }

type cborUint uint64
type cborInt int64
type cborBool bool
type cborFloat float64
type cborNull struct{}
type cborText string
type cborBytes []byte
type cborArray []cborValue

// cborEntry is one map key/value pair; cborMap keeps entries ordered (a CBOR map is
// an ordered list of pairs), so the encoder controls the wire order explicitly.
type cborEntry struct {
	key cborValue
	val cborValue
}
type cborMap []cborEntry
type cborTag struct {
	num   uint64
	inner cborValue
}

func (cborUint) isCbor()  {}
func (cborInt) isCbor()   {}
func (cborBool) isCbor()  {}
func (cborFloat) isCbor() {}
func (cborNull) isCbor()  {}
func (cborText) isCbor()  {}
func (cborBytes) isCbor() {}
func (cborArray) isCbor() {}
func (cborMap) isCbor()   {}
func (cborTag) isCbor()   {}

// cborEncode serializes a value tree to canonical CBOR bytes.
func cborEncode(v cborValue) []byte {
	var out []byte
	cborEnc(v, &out)
	return out
}

func cborHead(major byte, n uint64, out *[]byte) {
	mt := major << 5
	switch {
	case n < 24:
		*out = append(*out, mt|byte(n))
	case n < 0x100:
		*out = append(*out, mt|24, byte(n))
	case n < 0x10000:
		*out = append(*out, mt|25, byte(n>>8), byte(n))
	case n < 0x100000000:
		*out = append(*out, mt|26, byte(n>>24), byte(n>>16), byte(n>>8), byte(n))
	default:
		*out = append(*out, mt|27,
			byte(n>>56), byte(n>>48), byte(n>>40), byte(n>>32),
			byte(n>>24), byte(n>>16), byte(n>>8), byte(n))
	}
}

func cborEnc(v cborValue, out *[]byte) {
	switch x := v.(type) {
	case cborUint:
		cborHead(0, uint64(x), out)
	case cborInt:
		if x >= 0 {
			cborHead(0, uint64(x), out)
		} else {
			cborHead(1, uint64(-(int64(x) + 1)), out)
		}
	case cborBool:
		if bool(x) {
			*out = append(*out, 0xf5)
		} else {
			*out = append(*out, 0xf4)
		}
	case cborNull:
		*out = append(*out, 0xf6)
	case cborFloat:
		bits := math.Float64bits(float64(x))
		*out = append(*out, 0xfb,
			byte(bits>>56), byte(bits>>48), byte(bits>>40), byte(bits>>32),
			byte(bits>>24), byte(bits>>16), byte(bits>>8), byte(bits))
	case cborText:
		s := []byte(x)
		cborHead(3, uint64(len(s)), out)
		*out = append(*out, s...)
	case cborBytes:
		cborHead(2, uint64(len(x)), out)
		*out = append(*out, x...)
	case cborArray:
		cborHead(4, uint64(len(x)), out)
		for _, e := range x {
			cborEnc(e, out)
		}
	case cborMap:
		cborHead(5, uint64(len(x)), out)
		for _, e := range x {
			cborEnc(e.key, out)
			cborEnc(e.val, out)
		}
	case cborTag:
		cborHead(6, x.num, out)
		cborEnc(x.inner, out)
	}
}

// cborDecode parses a full CBOR item and rejects trailing bytes, so a payload that
// is not exactly one value is an error rather than a silently-truncated read.
func cborDecode(b []byte) (cborValue, error) {
	pos := 0
	v, err := cborDec(b, &pos)
	if err != nil {
		return nil, err
	}
	if pos != len(b) {
		return nil, fmt.Errorf("csil cbor: %d trailing bytes", len(b)-pos)
	}
	return v, nil
}

func cborReadArg(b []byte, pos *int, low byte) (uint64, error) {
	if low < 24 {
		*pos++
		return uint64(low), nil
	}
	switch low {
	case 24:
		if *pos+2 > len(b) {
			return 0, fmt.Errorf("csil cbor: truncated argument")
		}
		v := uint64(b[*pos+1])
		*pos += 2
		return v, nil
	case 25:
		if *pos+3 > len(b) {
			return 0, fmt.Errorf("csil cbor: truncated argument")
		}
		v := uint64(b[*pos+1])<<8 | uint64(b[*pos+2])
		*pos += 3
		return v, nil
	case 26:
		if *pos+5 > len(b) {
			return 0, fmt.Errorf("csil cbor: truncated argument")
		}
		var v uint64
		for i := 1; i <= 4; i++ {
			v = v<<8 | uint64(b[*pos+i])
		}
		*pos += 5
		return v, nil
	case 27:
		if *pos+9 > len(b) {
			return 0, fmt.Errorf("csil cbor: truncated argument")
		}
		var v uint64
		for i := 1; i <= 8; i++ {
			v = v<<8 | uint64(b[*pos+i])
		}
		*pos += 9
		return v, nil
	default:
		return 0, fmt.Errorf("csil cbor: reserved additional info %d", low)
	}
}

func cborDec(b []byte, pos *int) (cborValue, error) {
	if *pos >= len(b) {
		return nil, fmt.Errorf("csil cbor: unexpected end of input")
	}
	ib := b[*pos]
	major := ib >> 5
	low := ib & 0x1f
	if major == 7 {
		switch low {
		case 20:
			*pos++
			return cborBool(false), nil
		case 21:
			*pos++
			return cborBool(true), nil
		case 22, 23:
			*pos++
			return cborNull{}, nil
		case 26:
			bits, err := cborReadArg(b, pos, low)
			if err != nil {
				return nil, err
			}
			return cborFloat(float64(math.Float32frombits(uint32(bits)))), nil
		case 27:
			bits, err := cborReadArg(b, pos, low)
			if err != nil {
				return nil, err
			}
			return cborFloat(math.Float64frombits(bits)), nil
		default:
			return nil, fmt.Errorf("csil cbor: unsupported simple value %d", low)
		}
	}
	arg, err := cborReadArg(b, pos, low)
	if err != nil {
		return nil, err
	}
	switch major {
	case 0:
		return cborUint(arg), nil
	case 1:
		if arg > uint64(math.MaxInt64) {
			return nil, fmt.Errorf("csil cbor: negative integer out of range")
		}
		return cborInt(-1 - int64(arg)), nil
	case 2:
		n := int(arg)
		if *pos+n > len(b) {
			return nil, fmt.Errorf("csil cbor: truncated byte string")
		}
		slice := make([]byte, n)
		copy(slice, b[*pos:*pos+n])
		*pos += n
		return cborBytes(slice), nil
	case 3:
		n := int(arg)
		if *pos+n > len(b) {
			return nil, fmt.Errorf("csil cbor: truncated text string")
		}
		s := string(b[*pos : *pos+n])
		*pos += n
		return cborText(s), nil
	case 4:
		n := int(arg)
		items := make(cborArray, 0, n)
		for i := 0; i < n; i++ {
			item, err := cborDec(b, pos)
			if err != nil {
				return nil, err
			}
			items = append(items, item)
		}
		return items, nil
	case 5:
		n := int(arg)
		entries := make(cborMap, 0, n)
		for i := 0; i < n; i++ {
			k, err := cborDec(b, pos)
			if err != nil {
				return nil, err
			}
			val, err := cborDec(b, pos)
			if err != nil {
				return nil, err
			}
			entries = append(entries, cborEntry{k, val})
		}
		return entries, nil
	case 6:
		inner, err := cborDec(b, pos)
		if err != nil {
			return nil, err
		}
		return cborTag{num: arg, inner: inner}, nil
	default:
		return nil, fmt.Errorf("csil cbor: unexpected major type %d", major)
	}
}

// cborEncArray maps a typed slice to a CBOR array via the per-element encoder.
func cborEncArray[E any](xs []E, f func(E) cborValue) cborValue {
	items := make(cborArray, 0, len(xs))
	for _, x := range xs {
		items = append(items, f(x))
	}
	return items
}

// cborEncMap maps a typed map to a CBOR map. Go map iteration is unordered, so the
// inner map's entry order is not canonicalized; the record's own keys (laid down at
// generation time) are what the cross-language wire contract pins.
func cborEncMap[K comparable, V any](m map[K]V, kf func(K) cborValue, vf func(V) cborValue) cborValue {
	entries := make(cborMap, 0, len(m))
	for k, v := range m {
		entries = append(entries, cborEntry{kf(k), vf(v)})
	}
	return entries
}

func cborDecArray[E any](v cborValue, f func(cborValue) (E, error)) ([]E, error) {
	arr, err := cborAsArray(v)
	if err != nil {
		return nil, err
	}
	out := make([]E, 0, len(arr))
	for _, e := range arr {
		x, err := f(e)
		if err != nil {
			return nil, err
		}
		out = append(out, x)
	}
	return out, nil
}

func cborDecMap[K comparable, V any](v cborValue, kf func(cborValue) (K, error), vf func(cborValue) (V, error)) (map[K]V, error) {
	entries, err := cborAsMap(v)
	if err != nil {
		return nil, err
	}
	out := make(map[K]V, len(entries))
	for _, e := range entries {
		k, err := kf(e.key)
		if err != nil {
			return nil, err
		}
		val, err := vf(e.val)
		if err != nil {
			return nil, err
		}
		out[k] = val
	}
	return out, nil
}

func cborMapGet(v cborValue, key string) (cborValue, bool) {
	if m, ok := v.(cborMap); ok {
		for _, e := range m {
			if k, ok := e.key.(cborText); ok && string(k) == key {
				return e.val, true
			}
		}
	}
	return nil, false
}

func cborRequire(v cborValue, key string) (cborValue, error) {
	if x, ok := cborMapGet(v, key); ok {
		return x, nil
	}
	return nil, fmt.Errorf("csil cbor: missing field %q", key)
}

func cborAsI64(v cborValue) (int64, error) {
	switch x := v.(type) {
	case cborUint:
		if uint64(x) > uint64(math.MaxInt64) {
			return 0, fmt.Errorf("csil cbor: integer overflows int64")
		}
		return int64(x), nil
	case cborInt:
		return int64(x), nil
	default:
		return 0, fmt.Errorf("csil cbor: expected integer, got %T", v)
	}
}

func cborAsU64(v cborValue) (uint64, error) {
	switch x := v.(type) {
	case cborUint:
		return uint64(x), nil
	case cborInt:
		if x < 0 {
			return 0, fmt.Errorf("csil cbor: negative integer where unsigned expected")
		}
		return uint64(x), nil
	default:
		return 0, fmt.Errorf("csil cbor: expected unsigned integer, got %T", v)
	}
}

func cborAsF64(v cborValue) (float64, error) {
	switch x := v.(type) {
	case cborFloat:
		return float64(x), nil
	case cborUint:
		return float64(x), nil
	case cborInt:
		return float64(x), nil
	default:
		return 0, fmt.Errorf("csil cbor: expected float, got %T", v)
	}
}

func cborAsBool(v cborValue) (bool, error) {
	if b, ok := v.(cborBool); ok {
		return bool(b), nil
	}
	return false, fmt.Errorf("csil cbor: expected bool, got %T", v)
}

func cborAsText(v cborValue) (string, error) {
	if s, ok := v.(cborText); ok {
		return string(s), nil
	}
	return "", fmt.Errorf("csil cbor: expected text, got %T", v)
}

func cborAsBytes(v cborValue) ([]byte, error) {
	if b, ok := v.(cborBytes); ok {
		return []byte(b), nil
	}
	return nil, fmt.Errorf("csil cbor: expected byte string, got %T", v)
}

func cborAsArray(v cborValue) ([]cborValue, error) {
	if a, ok := v.(cborArray); ok {
		return []cborValue(a), nil
	}
	return nil, fmt.Errorf("csil cbor: expected array, got %T", v)
}

func cborAsMap(v cborValue) (cborMap, error) {
	if m, ok := v.(cborMap); ok {
		return m, nil
	}
	return nil, fmt.Errorf("csil cbor: expected map, got %T", v)
}
"#;

/// Timestamp (CBOR tag 0, RFC3339, always UTC) codec, emitted only when the spec
/// uses `timestamp` so `time` is never an unused import.
const CODEC_TIMESTAMP_GO: &str = r#"// csilEncTimestamp encodes a time.Time as CBOR tag 0 RFC3339 text in UTC, per the
// wire contract; sub-second precision is preserved when present.
func csilEncTimestamp(t time.Time) cborValue {
	return cborTag{num: 0, inner: cborText(t.UTC().Format(time.RFC3339Nano))}
}

// csilAsTimestamp decodes a CBOR tag 0 RFC3339 timestamp back to a UTC time.Time.
func csilAsTimestamp(v cborValue) (time.Time, error) {
	t, ok := v.(cborTag)
	if !ok || t.num != 0 {
		return time.Time{}, fmt.Errorf("csil cbor: expected CBOR tag 0 timestamp")
	}
	s, ok := t.inner.(cborText)
	if !ok {
		return time.Time{}, fmt.Errorf("csil cbor: timestamp content must be text")
	}
	parsed, err := time.Parse(time.RFC3339, string(s))
	if err != nil {
		return time.Time{}, err
	}
	return parsed.UTC(), nil
}
"#;

/// Exact-integer (de)serialization for a decimal mantissa: a 64-bit integer when it
/// fits, otherwise a CBOR bignum (tag 2 non-negative / tag 3 negative). Emitted only
/// alongside the decimal codec so `math/big` is never an unused import.
const CODEC_BIGINT_GO: &str = r#"// csilEncBigInt encodes an exact integer mantissa: a CBOR integer when it fits in
// 64 bits, otherwise a bignum (RFC 8949 §3.4.3) so the value stays exact.
func csilEncBigInt(m *big.Int) cborValue {
	if m.IsInt64() {
		return cborInt(m.Int64())
	}
	if m.IsUint64() {
		return cborUint(m.Uint64())
	}
	if m.Sign() >= 0 {
		return cborTag{num: 2, inner: cborBytes(m.Bytes())}
	}
	// A negative bignum encodes the magnitude of -1 - value.
	n := new(big.Int).Sub(new(big.Int).Neg(m), big.NewInt(1))
	return cborTag{num: 3, inner: cborBytes(n.Bytes())}
}

func csilDecBigInt(v cborValue) (*big.Int, error) {
	switch x := v.(type) {
	case cborUint:
		return new(big.Int).SetUint64(uint64(x)), nil
	case cborInt:
		return big.NewInt(int64(x)), nil
	case cborTag:
		bs, ok := x.inner.(cborBytes)
		if !ok {
			return nil, fmt.Errorf("csil cbor: bignum content must be a byte string")
		}
		mag := new(big.Int).SetBytes(bs)
		switch x.num {
		case 2:
			return mag, nil
		case 3:
			return new(big.Int).Sub(new(big.Int).Neg(mag), big.NewInt(1)), nil
		default:
			return nil, fmt.Errorf("csil cbor: unexpected bignum tag %d", x.num)
		}
	default:
		return nil, fmt.Errorf("csil cbor: expected integer mantissa, got %T", v)
	}
}
"#;

/// Decimal codec under the default `csil` mapping: the generated `CsilDecimal`
/// (exponent + big.Int mantissa) maps straight onto CBOR tag 4 `[exponent, mantissa]`.
const CODEC_DECIMAL_CSIL_GO: &str = r#"// csilEncDecimal encodes a CsilDecimal as CBOR tag 4: [exponent, mantissa].
func csilEncDecimal(d CsilDecimal) cborValue {
	return cborTag{num: 4, inner: cborArray{cborInt(d.Exponent), csilEncBigInt(d.mantissa())}}
}

// csilAsDecimal decodes a CBOR tag 4 decimal fraction into an exact CsilDecimal.
func csilAsDecimal(v cborValue) (CsilDecimal, error) {
	t, ok := v.(cborTag)
	if !ok || t.num != 4 {
		return CsilDecimal{}, fmt.Errorf("csil cbor: expected CBOR tag 4 decimal")
	}
	arr, ok := t.inner.(cborArray)
	if !ok || len(arr) != 2 {
		return CsilDecimal{}, fmt.Errorf("csil cbor: tag 4 content must be [exponent, mantissa]")
	}
	exp, err := cborAsI64(arr[0])
	if err != nil {
		return CsilDecimal{}, err
	}
	mant, err := csilDecBigInt(arr[1])
	if err != nil {
		return CsilDecimal{}, err
	}
	return CsilDecimal{Exponent: exp, Mantissa: mant}, nil
}
"#;

/// Decimal codec under the `library` mapping: shopspring's Decimal carries the same
/// exact value (coefficient * 10^exponent), so it maps onto CBOR tag 4 directly.
const CODEC_DECIMAL_LIBRARY_GO: &str = r#"// csilEncDecimal encodes a shopspring Decimal as CBOR tag 4: [exponent, mantissa].
func csilEncDecimal(d decimal.Decimal) cborValue {
	return cborTag{num: 4, inner: cborArray{cborInt(int64(d.Exponent())), csilEncBigInt(d.Coefficient())}}
}

// csilAsDecimal decodes a CBOR tag 4 decimal fraction into a shopspring Decimal.
func csilAsDecimal(v cborValue) (decimal.Decimal, error) {
	t, ok := v.(cborTag)
	if !ok || t.num != 4 {
		return decimal.Decimal{}, fmt.Errorf("csil cbor: expected CBOR tag 4 decimal")
	}
	arr, ok := t.inner.(cborArray)
	if !ok || len(arr) != 2 {
		return decimal.Decimal{}, fmt.Errorf("csil cbor: tag 4 content must be [exponent, mantissa]")
	}
	exp, err := cborAsI64(arr[0])
	if err != nil {
		return decimal.Decimal{}, err
	}
	mant, err := csilDecBigInt(arr[1])
	if err != nil {
		return decimal.Decimal{}, err
	}
	return decimal.NewFromBigInt(mant, int32(exp)), nil
}
"#;

/// Strip a trailing `Service` suffix and PascalCase the remainder, matching the
/// wire service base used across the TypeScript/Rust/Python clients.
fn go_service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

fn generate_client(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();

    content.push_str(&format!(
        "// Package {} contains generated service clients.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    content.push_str("import (\n");
    content.push_str(&format!("{}\"context\"\n", config.indent_style));
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    content.push_str(")\n\n");

    content.push_str(CLIENT_PRELUDE_GO);
    content.push('\n');

    let records = record_names(input);
    let mut emitted_any = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_struct(&mut content, &rule.name, service, config, &records);
            emitted_any = true;
        }
    }

    if emitted_any {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

/// Whether a type is a reference to a record the codec can (de)serialize, so a typed
/// client method can call the generated `Encode<T>`/`Decode<T>` directly.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(name))
}

fn emit_client_struct(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
    records: &std::collections::HashSet<String>,
) {
    let base = go_service_base(name);
    let client = format!("{base}Client");
    // Canonical wire strings (the wire contract): service lowercased, op PascalCased.
    let wire_service = base.to_lowercase();

    content.push_str(&format!(
        "// {client} is a typed client for the {name} service. The client owns\n\
         // (de)serialization via the generated codec; the transport only moves bytes.\n"
    ));
    content.push_str(&format!("type {client} struct {{\n"));
    content.push_str(&format!("{}transport Transport\n", config.indent_style));
    content.push_str("}\n\n");

    content.push_str(&format!(
        "func New{client}(transport Transport) *{client} {{\n"
    ));
    content.push_str(&format!(
        "{}return &{client}{{transport: transport}}\n",
        config.indent_style
    ));
    content.push_str("}\n\n");

    for operation in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the router/encoder surface emitted by the base `go` target.
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            content.push_str(&format!(
                "// channel operation {} is not part of the RPC client\n\n",
                operation.name
            ));
            continue;
        }
        let success = go_success_type(&operation.output_type);
        let null_input = op_input_is_null(&operation.input_type);
        // The typed-codec path needs a record success type (and a record or null
        // request) so the method can call the generated Encode/Decode. Anything else
        // is skipped with a note rather than emitting an uncompilable call.
        if !is_record_ref(&success, records)
            || !(null_input || is_record_ref(&operation.input_type, records))
        {
            content.push_str(&format!(
                "// operation {} has a non-record payload; (de)serialize it manually\n\n",
                operation.name
            ));
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let output_type = map_csil_type_to_go(&success, &None, config.decimal_go_type());
        let params = if null_input {
            "ctx context.Context".to_string()
        } else {
            let input_type =
                map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
            format!("ctx context.Context, req {input_type}")
        };
        content.push_str(&format!(
            "func (c *{client}) {method_name}({params}) ({output_type}, error) {{\n"
        ));
        content.push_str(&format!(
            "{}var csilZero {output_type}\n",
            config.indent_style
        ));
        // A null input carries no request body, so the transport gets a nil payload;
        // otherwise the request record is encoded to its canonical CBOR bytes first.
        if null_input {
            content.push_str(&format!(
                "{}csilResp, csilErr := c.transport.Call(ctx, \"{wire_service}\", \"{method_name}\", nil)\n",
                config.indent_style
            ));
        } else {
            let req_type = type_ref_name(&operation.input_type);
            content.push_str(&format!(
                "{}csilResp, csilErr := c.transport.Call(ctx, \"{wire_service}\", \"{method_name}\", Encode{req_type}(req))\n",
                config.indent_style
            ));
        }
        content.push_str(&format!(
            "{i}if csilErr != nil {{\n{i}{i}return csilZero, csilErr\n{i}}}\n",
            i = config.indent_style
        ));
        let resp_type = type_ref_name(&success);
        content.push_str(&format!(
            "{}return Decode{resp_type}(csilResp)\n",
            config.indent_style
        ));
        content.push_str("}\n\n");
    }
}

/// The bare type name of a record `Reference`. Only called after `is_record_ref`
/// has confirmed the type is a record reference, so the fallback is never reached.
fn type_ref_name(ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Reference(name) => name.clone(),
        _ => String::new(),
    }
}

fn generate_services(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();

    content.push_str(&format!(
        "// Package {} contains generated service interfaces.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    let needs_channel = spec_has_channel_ops(input);

    content.push_str("import (\n");
    content.push_str(&format!("{}\"context\"\n", config.indent_style));
    if needs_channel {
        // fmt.Errorf for the router's unknown-method case.
        content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    }
    content.push_str(")\n\n");

    if needs_channel {
        // Same shape across all generators: the codec is consumer-supplied so
        // the runtime never owns serialization.
        content.push_str(
            "// Codec is the consumer-supplied (de)serialization layer for channel\n\
             // messages. The generator is codec-agnostic; the implementer wires this\n\
             // to CBOR, JSON, or anything else its protocol expects.\n\
             type Codec interface {\n",
        );
        content.push_str(&format!(
            "{}Encode(value any) ([]byte, error)\n",
            config.indent_style
        ));
        content.push_str(&format!(
            "{}Decode(data []byte, out any) error\n",
            config.indent_style
        ));
        content.push_str("}\n\n");
    }

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_service_interface(&mut content, &rule.name, service, config);
            emit_wire_ids(&mut content, &rule.name, service);

            if service_has_channel_ops(service) {
                emit_channel_router(&mut content, &rule.name, service, config);
                // Compact-profile twin, emitted only for wire-id-bearing services
                // so wire-id-free specs stay byte-identical.
                emit_channel_router_compact(&mut content, &rule.name, service, config);
                emit_channel_encoders(&mut content, &rule.name, service, config);
            }
        }
    }

    Ok(Some(content))
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

fn emit_service_interface(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    content.push_str(&format!("// {name} defines the service interface\n"));
    content.push_str(&format!("type {name} interface {{\n"));

    for operation in &service.operations {
        let method_name = go_method_name(&operation.name);
        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                let output_type = map_csil_type_to_go(
                    &go_success_type(&operation.output_type),
                    &None,
                    config.decimal_go_type(),
                );
                let params = if op_input_is_null(&operation.input_type) {
                    "ctx context.Context".to_string()
                } else {
                    let input_type =
                        map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
                    format!("ctx context.Context, req {input_type}")
                };
                content.push_str(&format!(
                    "{}{method_name}({params}) ({output_type}, error)\n",
                    config.indent_style
                ));
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the implementer's plumbing pulls a
                // frame off the wire, hands it to Route<Service>Channel, which
                // decodes and dispatches here.
                let input_type =
                    map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
                content.push_str(&format!(
                    "{}{}(ctx context.Context, msg {}) error\n",
                    config.indent_style, method_name, input_type
                ));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound method on the server side.
            }
        }
    }

    content.push_str("}\n\n");
}

/// Emit `const` wire-id ordinals exposing the `@wire-id(N)` values so a host can
/// reference them instead of hardcoding. Purely additive: emits nothing unless
/// the service carries a wire-id, keeping wire-id-free output byte-identical.
fn emit_wire_ids(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let prefix = pascal_case(name);
    content.push_str(&format!(
        "// Wire-id ordinals for the {name} service (transport compact profiles).\n"
    ));
    content.push_str(&format!(
        "const {prefix}ServiceWireID uint64 = {service_id}\n"
    ));
    for operation in &service.operations {
        if let Some(op_id) = operation.wire_id {
            // The `Op` infix keeps operation ordinals distinct from the service
            // ordinal: an op named `service` emits `<Service>OpServiceWireID`,
            // never `<Service>ServiceWireID`, so the two can't redeclare a name.
            let op_exported = go_method_name(&operation.name);
            content.push_str(&format!(
                "const {prefix}Op{op_exported}WireID uint64 = {op_id}\n"
            ));
        }
    }
    content.push('\n');
}

fn emit_channel_router(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    let route_fn = format!("Route{service_name}Channel");
    content.push_str(&format!(
        "// {route_fn} decodes one inbound channel frame and dispatches to the\n\
         // matching {service_name} method. The implementer feeds bytes from its\n\
         // connection here; the generator never owns the wire.\n"
    ));
    content.push_str(&format!(
        "func {route_fn}(handlers {service_name}, ctx context.Context, codec Codec, method string, data []byte) error {{\n"
    ));
    content.push_str(&format!("{}switch method {{\n", config.indent_style));
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let input_type =
            map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
        content.push_str(&format!("{}case \"{method_name}\":\n", config.indent_style));
        content.push_str(&format!(
            "{}{}var msg {input_type}\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}if err := codec.Decode(data, &msg); err != nil {{\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}{}return err\n",
            config.indent_style, config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}}}\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}return handlers.{method_name}(ctx, msg)\n",
            config.indent_style, config.indent_style
        ));
    }
    content.push_str(&format!("{}default:\n", config.indent_style));
    content.push_str(&format!(
        "{}{}return fmt.Errorf(\"unknown channel method %q\", method)\n",
        config.indent_style, config.indent_style
    ));
    content.push_str(&format!("{}}}\n", config.indent_style));
    content.push_str("}\n\n");
}

/// The compact-profile twin of `emit_channel_router`: when the service carries
/// `@wire-id` ordinals, emit `Route<Service>ChannelCompact` that dispatches on
/// the operation ordinal (`uint64`) instead of the wire method name. The profile
/// is negotiated on the wire (never declared in CSIL), so a host keeps both
/// routers and calls whichever the peer selected. Emits nothing for wire-id-free
/// services, keeping their output byte-identical.
fn emit_channel_router_compact(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    if service.wire_id.is_none() {
        return;
    }
    let route_fn = format!("Route{service_name}ChannelCompact");
    content.push_str(&format!(
        "// {route_fn} decodes one inbound channel frame by its @wire-id ordinal\n\
         // (compact transport profile) and dispatches to the matching\n\
         // {service_name} method. The verbose-profile twin is Route{service_name}Channel;\n\
         // the host calls whichever matches the profile negotiated on the wire.\n"
    ));
    content.push_str(&format!(
        "func {route_fn}(handlers {service_name}, ctx context.Context, codec Codec, op uint64, data []byte) error {{\n"
    ));
    content.push_str(&format!("{}switch op {{\n", config.indent_style));
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        // The all-or-nothing wire-id rule (enforced by the validator) means a
        // bidirectional op on a wire-id-bearing service always has an ordinal.
        let Some(op_id) = operation.wire_id else {
            continue;
        };
        let method_name = go_method_name(&operation.name);
        let input_type =
            map_csil_type_to_go(&operation.input_type, &None, config.decimal_go_type());
        content.push_str(&format!("{}case {op_id}:\n", config.indent_style));
        content.push_str(&format!(
            "{}{}var msg {input_type}\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}if err := codec.Decode(data, &msg); err != nil {{\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}{}return err\n",
            config.indent_style, config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}}}\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}return handlers.{method_name}(ctx, msg)\n",
            config.indent_style, config.indent_style
        ));
    }
    content.push_str(&format!("{}default:\n", config.indent_style));
    content.push_str(&format!(
        "{}{}return fmt.Errorf(\"unknown channel ordinal %d\", op)\n",
        config.indent_style, config.indent_style
    ));
    content.push_str(&format!("{}}}\n", config.indent_style));
    content.push_str("}\n\n");
}

fn emit_channel_encoders(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let output_type =
            map_csil_type_to_go(&operation.output_type, &None, config.decimal_go_type());
        let fn_name = format!("Encode{service_name}{method_name}");
        content.push_str(&format!(
            "// {fn_name} encodes a `{method_name}` message the server pushes to a peer;\n\
             // the implementer frames (method, bytes) onto its connection.\n"
        ));
        content.push_str(&format!(
            "func {fn_name}(codec Codec, msg {output_type}) (string, []byte, error) {{\n"
        ));
        content.push_str("\tdata, err := codec.Encode(msg)\n");
        content.push_str("\tif err != nil {\n");
        content.push_str("\t\treturn \"\", nil, err\n");
        content.push_str("\t}\n");
        content.push_str(&format!("\treturn \"{method_name}\", data, nil\n"));
        content.push_str("}\n\n");
    }
}

fn generate_validation(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    if !config.generate_validation {
        return Ok(None);
    }

    // Both constraint systems share one Validate() per type: `@`-annotation
    // ValidationConstraints (in metadata) and `.`-control-operators (carried
    // inline on the field's type). The body is built first so the import block can
    // pull in a package only when a check that needs it actually lands.
    let mut body = String::new();
    let mut imports = ValidationImports::default();

    for rule in &input.csil_spec.rules {
        // A record rule reaches us as either `GroupDef` or a `TypeDef` wrapping a
        // `Group`; both produce a struct, so both must produce a Validate().
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            if !group.entries.iter().any(entry_has_check) {
                continue;
            }

            body.push_str(&format!(
                "// Validate{0} validates the {0} struct\n",
                rule.name
            ));
            body.push_str(&format!("func (v *{}) Validate() error {{\n", rule.name));

            for entry in &group.entries {
                if let Some(key) = &entry.key {
                    let field_name = go_field_name_from_key_with_metadata(key, &entry.metadata);
                    // An optional field is a Go pointer; every check on it is guarded
                    // and dereferenced so a nil optional is skipped rather than panicking.
                    let field = FieldRef {
                        name: &field_name,
                        optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
                    };

                    for metadata in &entry.metadata {
                        if let CsilFieldMetadata::Constraint(constraint) = metadata {
                            emit_metadata_constraint(
                                &mut body,
                                config,
                                field,
                                &entry.value_type,
                                constraint,
                                &mut imports,
                            );
                        }
                    }

                    if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                        for op in constraints {
                            emit_control_op_check(
                                &mut body,
                                config,
                                field,
                                &entry.value_type,
                                op,
                                &mut imports,
                            );
                        }
                    }
                }
            }

            body.push_str(&format!("{}return nil\n", config.indent_style));
            body.push_str("}\n\n");
        }
    }

    if body.is_empty() {
        return Ok(None);
    }

    // A `timestamp` bound is parsed at runtime via a package-local must-parser; emit
    // it once, only when a timestamp comparison actually landed.
    if imports.time {
        body.push_str(TIMESTAMP_HELPER_GO);
    }

    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains generated validation functions.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    content.push_str("import (\n");
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    if imports.regexp {
        // regexp.MatchString backs both the `@regex` annotation and the `.regex`
        // control operator; it is imported only when a pattern check is emitted so
        // the file never carries an unused import (a Go compile error).
        content.push_str(&format!("{}\"regexp\"\n", config.indent_style));
    }
    if imports.time {
        // `time.Parse(time.RFC3339, ...)` parses a `timestamp` bound for comparison.
        content.push_str(&format!("{}\"time\"\n", config.indent_style));
    }
    if imports.decimal_lib {
        // Only the library decimal mapping references shopspring here; the default
        // CsilDecimal mapping compares via the in-package helper instead.
        content.push('\n');
        content.push_str(&format!(
            "{}\"github.com/shopspring/decimal\"\n",
            config.indent_style
        ));
    }
    content.push_str(")\n\n");

    content.push_str(&body);

    Ok(Some(content))
}

/// Which generated-import packages a Validate() body forces. Each is set only when
/// a check that needs it is emitted, so the import block never carries an unused
/// package (a Go compile error).
#[derive(Default)]
struct ValidationImports {
    regexp: bool,
    time: bool,
    decimal_lib: bool,
}

/// Runtime parser for an RFC3339 `timestamp` bound. The bound text is fixed at
/// generation time, so a parse failure is a generator bug, not bad runtime input —
/// hence the panic rather than a returned error.
const TIMESTAMP_HELPER_GO: &str = "\
// mustParseTimestamp parses an RFC3339 bound embedded at generation time. The text
// is fixed by the spec, so a parse failure signals a generator bug, not bad input.
func mustParseTimestamp(s string) time.Time {
\tt, err := time.Parse(time.RFC3339, s)
\tif err != nil {
\t\tpanic(err)
\t}
\treturn t
}
";

/// Whether a field's (possibly constrained) base type is an ordered core type that
/// needs a typed comparison rather than a plain `<`/`>` on a Go scalar: `decimal`
/// compares through its decimal library's `Cmp`, `timestamp` through `time.Time`'s
/// `Before`/`After`/`Equal`. Everything else is a numeric scalar.
enum OrderedKind {
    Numeric,
    Decimal,
    Timestamp,
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

/// Escape a string for safe inclusion inside a Go double-quoted literal so an
/// embedded quote/backslash/newline can never break the surrounding literal.
fn go_escape(s: &str) -> String {
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

/// A complete, always-valid Go double-quoted string literal for `s`.
fn go_string_lit(s: &str) -> String {
    format!("\"{}\"", go_escape(s))
}

/// A field's Go name plus whether it is optional (a Go pointer). Threaded through
/// the check emitters so each can consistently guard and dereference a nil optional.
#[derive(Clone, Copy)]
struct FieldRef<'a> {
    name: &'a str,
    optional: bool,
}

impl FieldRef<'_> {
    /// The expression that reads the field's value inside a check. An optional field
    /// is a pointer, so it is dereferenced explicitly; the surrounding check is
    /// guarded so the deref is never reached on a nil pointer.
    fn read_expr(&self) -> String {
        if self.optional {
            format!("(*v.{})", self.name)
        } else {
            format!("v.{}", self.name)
        }
    }
}

/// Emit a runtime check, guarding it behind a nil test when the field is optional.
/// An optional `decimal`/`timestamp`/`text` field is a pointer; reaching its value
/// (a `Cmp`, a `Before`/`After`, or a `len`/deref) on a nil pointer would panic, so
/// the check only runs when the pointer is set. A required field emits `check`
/// verbatim. `check` is authored at one indent level and re-indented under the guard.
fn push_optional_guard(content: &mut String, config: &GoConfig, field: FieldRef, check: &str) {
    if !field.optional {
        content.push_str(check);
        return;
    }
    let i = &config.indent_style;
    let name = field.name;
    content.push_str(&format!("{i}if v.{name} != nil {{\n"));
    for line in check.lines() {
        if line.is_empty() {
            content.push('\n');
        } else {
            content.push_str(i);
            content.push_str(line);
            content.push('\n');
        }
    }
    content.push_str(&format!("{i}}}\n"));
}

/// Emit a `len()`-based check (`@min-length`/`.size`/etc.) honoring optionality.
/// `message_tail` completes the phrasing after `field '<f>' must have `.
fn push_len_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    op: &str,
    n: u64,
    message_tail: &str,
) {
    let i = &config.indent_style;
    let access = field.read_expr();
    let name = field.name;
    let mut chk = String::new();
    chk.push_str(&format!("{i}if len({access}) {op} {n} {{\n"));
    chk.push_str(&format!(
        "{i}{i}return fmt.Errorf(\"field '{name}' must have {message_tail}\")\n"
    ));
    chk.push_str(&format!("{i}}}\n"));
    push_optional_guard(content, config, field, &chk);
}

/// The textual decimal bound for a `decimal` comparison. A `decimal` bound is
/// normally written as text (`.ge "0.00"`), but a bare numeric literal is accepted
/// and rendered as its canonical decimal text.
fn literal_as_decimal_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

/// The RFC3339 bound text for a `timestamp` comparison. Only a text literal is a
/// well-formed instant; anything else is skipped rather than emitting bad Go.
fn literal_as_timestamp_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// Emit one ordered comparison honoring the field's type. `go_op` is the Go
/// operator whose truth means the constraint is violated (e.g. `.ge` is violated
/// when the value is `<` the bound), and `desc` is the human phrasing the value
/// must satisfy. Numeric fields compare directly; `decimal`/`timestamp` fields
/// parse the bound and compare through the type's own ordering so the emitted Go
/// always compiles (never a scalar-vs-string compare).
fn emit_ordered_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
    imports: &mut ValidationImports,
) {
    let (go_op, desc) = op;
    let i = &config.indent_style;
    let access = field.read_expr();
    let name = field.name;
    match ordered_field_kind(value_type) {
        OrderedKind::Numeric => {
            let value_str = literal_value_to_go_string(value);
            let mut chk = String::new();
            chk.push_str(&format!("{i}if {access} {go_op} {value_str} {{\n"));
            chk.push_str(&format!(
                "{i}{i}return fmt.Errorf(\"field '{name}' must be {desc} {value_str}\")\n"
            ));
            chk.push_str(&format!("{i}}}\n"));
            push_optional_guard(content, config, field, &chk);
        }
        OrderedKind::Decimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let lit = go_string_lit(&text);
            // Both decimal libraries expose a sign-returning Cmp, so the same
            // `Cmp(bound) <go_op> 0` shape works for either mapping.
            let bound_expr = match config.decimal_mapping {
                DecimalMapping::Csil => format!("mustParseCsilDecimal({lit})"),
                DecimalMapping::Library => {
                    imports.decimal_lib = true;
                    format!("decimal.RequireFromString({lit})")
                }
            };
            let shown = go_escape(&text);
            let mut chk = String::new();
            chk.push_str(&format!("{i}if {access}.Cmp({bound_expr}) {go_op} 0 {{\n"));
            chk.push_str(&format!(
                "{i}{i}return fmt.Errorf(\"field '{name}' must be {desc} {shown}\")\n"
            ));
            chk.push_str(&format!("{i}}}\n"));
            push_optional_guard(content, config, field, &chk);
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            imports.time = true;
            let bound_expr = format!("mustParseTimestamp({})", go_string_lit(&text));
            // time.Time has no operators; translate the violation operator into the
            // matching Before/After/Equal expression.
            let cond = match go_op {
                "<" => format!("{access}.Before({bound_expr})"),
                ">" => format!("{access}.After({bound_expr})"),
                "<=" => format!("!{access}.After({bound_expr})"),
                ">=" => format!("!{access}.Before({bound_expr})"),
                "!=" => format!("!{access}.Equal({bound_expr})"),
                "==" => format!("{access}.Equal({bound_expr})"),
                _ => return,
            };
            let shown = go_escape(&text);
            let mut chk = String::new();
            chk.push_str(&format!("{i}if {cond} {{\n"));
            chk.push_str(&format!(
                "{i}{i}return fmt.Errorf(\"field '{name}' must be {desc} {shown}\")\n"
            ));
            chk.push_str(&format!("{i}}}\n"));
            push_optional_guard(content, config, field, &chk);
        }
    }
}

/// Whether an entry yields at least one runtime check. Encoding-only operators
/// (.bits/.and/.within/.json/.cbor/.cborseq) and `.default`/`@default` don't, so a
/// field carrying only those does not, by itself, warrant a Validate() function.
fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let meta_check = entry.metadata.iter().any(|m| match m {
        CsilFieldMetadata::Constraint(c) => constraint_is_check(c),
        _ => false,
    });
    let op_check = match &entry.value_type {
        CsilTypeExpression::Constrained { constraints, .. } => {
            constraints.iter().any(control_op_is_check)
        }
        _ => false,
    };
    meta_check || op_check
}

fn constraint_is_check(constraint: &CsilValidationConstraint) -> bool {
    // `@default` is a constructor concern, not a Validate() check; every other
    // annotation (including a `regex` Custom) produces one.
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

/// Emit a single `@`-annotation ValidationConstraint as Go inside a Validate().
fn emit_metadata_constraint(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
    imports: &mut ValidationImports,
) {
    match constraint {
        CsilValidationConstraint::MinLength(min_len) => {
            let unit = if *min_len == 1 {
                "character"
            } else {
                "characters"
            };
            let tail = format!("at least {min_len} {unit}");
            push_len_check(content, config, field, "<", *min_len, &tail);
        }
        CsilValidationConstraint::MaxLength(max_len) => {
            let unit = if *max_len == 1 {
                "character"
            } else {
                "characters"
            };
            let tail = format!("at most {max_len} {unit}");
            push_len_check(content, config, field, ">", *max_len, &tail);
        }
        CsilValidationConstraint::MinItems(min_items) => {
            let unit = if *min_items == 1 { "item" } else { "items" };
            let tail = format!("at least {min_items} {unit}");
            push_len_check(content, config, field, "<", *min_items, &tail);
        }
        CsilValidationConstraint::MaxItems(max_items) => {
            let unit = if *max_items == 1 { "item" } else { "items" };
            let tail = format!("at most {max_items} {unit}");
            push_len_check(content, config, field, ">", *max_items, &tail);
        }
        // `@min-value`/`@max-value` are the annotation form of `.ge`/`.le`; route
        // them through the shared ordered emitter so a bound on a `decimal` or
        // `timestamp` field is parsed and typed-compared rather than compared as a
        // bare scalar (which would not compile).
        CsilValidationConstraint::MinValue(min_val) => {
            emit_ordered_check(
                content,
                config,
                field,
                value_type,
                ("<", "at least"),
                min_val,
                imports,
            );
        }
        CsilValidationConstraint::MaxValue(max_val) => {
            emit_ordered_check(
                content,
                config,
                field,
                value_type,
                (">", "at most"),
                max_val,
                imports,
            );
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                imports.regexp = true;
                emit_regex_check(content, config, field, pattern);
            }
        }
    }
}

/// Emit a single `.`-control-operator. Comparison and size/regex operators become
/// runtime checks; `.default` is applied by the constructor instead; the
/// encoding-only operators (.bits/.and/.within/.json/.cbor/.cborseq) leave a doc
/// comment so their presence is visible but they never fail validation.
fn emit_control_op_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
    imports: &mut ValidationImports,
) {
    let i = &config.indent_style;
    let field_name = field.name;
    // One match, one operator list: each comparison passes its `(violation-op,
    // phrasing)` pair to the shared emitter, which turns it into a numeric, decimal,
    // or timestamp comparison by field type. This avoids the prior split between a
    // dispatch table and dead `unreachable!` arms.
    let ordered = |content: &mut String, op_pair, v, imports: &mut ValidationImports| {
        emit_ordered_check(content, config, field, value_type, op_pair, v, imports);
    };
    match op {
        CsilControlOperator::GreaterEqual(v) => ordered(content, ("<", ">="), v, imports),
        CsilControlOperator::LessEqual(v) => ordered(content, (">", "<="), v, imports),
        CsilControlOperator::GreaterThan(v) => ordered(content, ("<=", ">"), v, imports),
        CsilControlOperator::LessThan(v) => ordered(content, (">=", "<"), v, imports),
        CsilControlOperator::Equal(v) => ordered(content, ("!=", "=="), v, imports),
        CsilControlOperator::NotEqual(v) => ordered(content, ("==", "!="), v, imports),
        CsilControlOperator::Size(size) => emit_size_check(content, config, field, size),
        CsilControlOperator::Regex(pattern) => {
            imports.regexp = true;
            emit_regex_check(content, config, field, pattern);
        }
        // Applied by the constructor (New<Type>), not validated here.
        CsilControlOperator::Default(_) => {}
        CsilControlOperator::Bits(bits) => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries .bits({bits}); a bit-set encoding hint, not a runtime check\n"
            ));
        }
        CsilControlOperator::And(_) => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries .and; intersection constraint left to the consumer\n"
            ));
        }
        CsilControlOperator::Within(_) => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries .within; range membership left to the consumer\n"
            ));
        }
        CsilControlOperator::Json | CsilControlOperator::Cbor | CsilControlOperator::Cborseq => {
            content.push_str(&format!(
                "{i}// field '{field_name}' carries an embedded-encoding operator; handled at (de)serialization, not validated\n"
            ));
        }
    }
}

/// `len()`-based check shared by `.size` forms; works for strings, byte slices,
/// arrays, and maps alike.
fn emit_size_check(
    content: &mut String,
    config: &GoConfig,
    field: FieldRef,
    size: &CsilSizeConstraint,
) {
    let mut one = |op: &str, n: u64, word: &str| {
        let tail = format!("{word} {n} elements");
        push_len_check(content, config, field, op, n, &tail);
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

fn emit_regex_check(content: &mut String, config: &GoConfig, field: FieldRef, pattern: &str) {
    let i = &config.indent_style;
    let access = field.read_expr();
    let name = field.name;
    // The pattern is rendered as a backtick raw literal for MatchString, but the
    // error message is a double-quoted literal: a raw pattern like `\d+` would form
    // an invalid Go escape there, so it is escaped to stay a well-formed literal.
    let shown = go_escape(pattern);
    let mut chk = String::new();
    chk.push_str(&format!(
        "{i}matched, _ := regexp.MatchString(`{pattern}`, {access})\n"
    ));
    chk.push_str(&format!("{i}if !matched {{\n"));
    chk.push_str(&format!(
        "{i}{i}return fmt.Errorf(\"field '{name}' must match pattern '{shown}'\")\n"
    ));
    chk.push_str(&format!("{i}}}\n"));
    push_optional_guard(content, config, field, &chk);
}

fn generate_constructors(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
    timestamp_helper_defined: bool,
) -> Result<Option<String>, i32> {
    // The constructor bodies are built first so the import block (and a possible
    // local timestamp must-parser) can be derived from what the typed defaults
    // actually reference, never carrying an unused import.
    let mut body = String::new();

    for rule in &input.csil_spec.rules {
        // A record rule reaches us as either `GroupDef` or a `TypeDef` wrapping a
        // `Group`; both produce a struct, so a default on either must be applied.
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        let Some(group) = group else { continue };

        // A default arrives either as the `@default(...)` annotation or the
        // `.default(...)` control operator on the field's type; both feed the same
        // constructor assignment.
        let fields_with_defaults: Vec<_> = group
            .entries
            .iter()
            .filter_map(|entry| {
                let key = entry.key.as_ref()?;
                let value = entry_default_value(entry)?;
                Some((
                    key,
                    value,
                    &entry.value_type,
                    &entry.occurrence,
                    &entry.metadata,
                ))
            })
            .collect();

        if fields_with_defaults.is_empty() {
            continue;
        }

        // Generate godoc comment with default values listed
        body.push_str(&format!(
            "// New{} creates a {} with default values:\n",
            rule.name, rule.name
        ));
        for (key, value, _, _, _) in &fields_with_defaults {
            let field_name = go_json_name_from_key(key);
            let value_str = literal_value_to_go_string(value);
            body.push_str(&format!("//   - {field_name}: {value_str}\n"));
        }
        body.push_str(&format!("func New{}() *{} {{\n", rule.name, rule.name));
        body.push_str(&format!(
            "{}return &{} {{\n",
            config.indent_style, rule.name
        ));

        for (key, value, value_type, occurrence, metadata) in &fields_with_defaults {
            let field_name = go_field_name_from_key_with_metadata(key, metadata);
            let go_value = literal_value_to_go_value(value, value_type, occurrence, config);
            body.push_str(&format!(
                "{}{}{}: {},\n",
                config.indent_style, config.indent_style, field_name, go_value
            ));
        }

        body.push_str(&format!("{}}}\n", config.indent_style));
        body.push_str("}\n\n");
    }

    if body.is_empty() {
        return Ok(None);
    }

    // The timestamp must-parser lives in the validation file when one is emitted; a
    // constructor with a timestamp default but no Validate() must carry its own copy
    // so the package still defines the symbol exactly once.
    let needs_ts_helper = body.contains("mustParseTimestamp(") && !timestamp_helper_defined;
    if needs_ts_helper {
        body.push_str(TIMESTAMP_HELPER_GO);
    }

    // A library-mapped decimal default constructs through `decimal.RequireFromString`
    // (and an optional one names `*decimal.Decimal`); only then is shopspring imported
    // here. The default CsilDecimal mapping resolves in-package, needing no import.
    let needs_shopspring = body.contains("decimal.");
    // The `time` package is named only when this file declares the timestamp helper
    // or constructs an optional `*time.Time` default; a bare must-parse call does not.
    let needs_time = needs_ts_helper || body.contains("time.Time");

    let mut content = String::new();
    content.push_str(&format!(
        "// Package {} contains generated constructor functions.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    if needs_time || needs_shopspring {
        content.push_str("import (\n");
        if needs_time {
            content.push_str(&format!("{}\"time\"\n", config.indent_style));
        }
        if needs_shopspring {
            if needs_time {
                content.push('\n');
            }
            content.push_str(&format!(
                "{}\"github.com/shopspring/decimal\"\n",
                config.indent_style
            ));
        }
        content.push_str(")\n\n");
    }

    content.push_str(&body);

    Ok(Some(content))
}

/// The default literal for a field, honoring both constraint systems: the
/// `@default(...)` annotation (carried in metadata) and the `.default(...)`
/// control operator (carried inline on the field's type). The annotation wins if
/// both are somehow present.
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

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — that error half is the
/// Go `error` return, not part of the typed response. Without this the whole
/// union maps to the untyped `interface{}` fallback.
fn go_success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
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

/// Whether any type anywhere in the spec is the named builtin (e.g. `timestamp`
/// or `decimal`). Used to decide whether to import `time`, pull in shopspring, or
/// inject the `CsilDecimal` helper — none of which should appear when unused.
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
            CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
                type_uses_builtin(&op.input_type, builtin)
                    || type_uses_builtin(&op.output_type, builtin)
            }),
            _ => false,
        })
}

/// A push op (`-> Event`) carries a `null` input type. On a unary RPC there is
/// no request to send, so the request parameter is dropped rather than surfaced
/// as a meaningless `interface{}` the caller would have to pass `nil` for.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
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

/// The scalar Go type backing a literal-only choice (an enum), or `None` when the
/// choice mixes non-literal members (a true union, which has no single scalar form).
/// Every literal kind is checked so a numeric or boolean enum maps to its matching Go
/// scalar, not just text. An empty choice is not an enum.
fn literal_choice_scalar_go(choices: &[CsilTypeExpression]) -> Option<&'static str> {
    let all = |pred: fn(&CsilLiteralValue) -> bool| {
        !choices.is_empty()
            && choices.iter().all(|c| match c {
                CsilTypeExpression::Literal(v) => pred(v),
                _ => false,
            })
    };
    if all(|v| matches!(v, CsilLiteralValue::Text(_))) {
        Some("string")
    } else if all(|v| matches!(v, CsilLiteralValue::Integer(_))) {
        Some("int64")
    } else if all(|v| matches!(v, CsilLiteralValue::Float(_))) {
        Some("float64")
    } else if all(|v| matches!(v, CsilLiteralValue::Bool(_))) {
        Some("bool")
    } else {
        None
    }
}

/// The CSIL builtin an enum's literals back, for aliasing the enum to a scalar in the
/// codec. `None` when the choice is not a uniform literal enum. The builtin names line
/// up with what `go_enc_value`/`go_dec_func`/`scalar_alias_go_type` already handle.
fn enum_scalar_builtin(choices: &[CsilTypeExpression]) -> Option<String> {
    let kind = |v: &CsilLiteralValue| match v {
        CsilLiteralValue::Text(_) => Some("text"),
        CsilLiteralValue::Integer(_) => Some("int"),
        CsilLiteralValue::Float(_) => Some("float"),
        CsilLiteralValue::Bool(_) => Some("bool"),
        _ => None,
    };
    let first = match choices.first()? {
        CsilTypeExpression::Literal(v) => kind(v)?,
        _ => return None,
    };
    choices
        .iter()
        .all(|c| matches!(c, CsilTypeExpression::Literal(v) if kind(v) == Some(first)))
        .then(|| first.to_string())
}

fn map_csil_type_to_go(
    type_expr: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    decimal_type: &str,
) -> String {
    let base_type = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "int64",
            "uint" => "uint64",
            "float" => "float64",
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`; the lexer
            // accepts both, so every generator maps the pair identically.
            "text" | "tstr" => "string",
            "bytes" | "bstr" => "[]byte",
            "bool" => "bool",
            // CBOR tag 0, RFC3339, always UTC per the wire contract; time.Time is
            // kept in UTC so encode/decode round-trips the `Z` offset.
            "timestamp" => "time.Time",
            // CBOR tag 4 exact decimal; the concrete Go type depends on the
            // decimal_mapping option (generated CsilDecimal vs. shopspring).
            "decimal" => decimal_type,
            "nil" | "null" => "interface{}",
            _ => name,
        },
        CsilTypeExpression::Reference(name) => name,
        CsilTypeExpression::Array { element_type, .. } => {
            let element = map_csil_type_to_go(element_type, &None, decimal_type);
            return format!("[]{element}");
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let key_type = map_csil_type_to_go(key, &None, decimal_type);
            let value_type = map_csil_type_to_go(value, &None, decimal_type);
            return format!("map[{key_type}]{value_type}");
        }
        // Go has no tuple type, so a fixed-shape array becomes an anonymous
        // struct rather than `[]interface{}`, preserving the per-position types.
        CsilTypeExpression::Tuple(group) => {
            return go_tuple_struct(&group.entries, decimal_type);
        }
        CsilTypeExpression::Constrained { base_type, .. } => {
            // Unwrap constrained types and map the base type
            // Constraints like .size, .default, .regex are validation rules, not Go types
            return map_csil_type_to_go(base_type, occurrence, decimal_type);
        }
        // An enum (`ProjectStatus = "active" / "archived"`) is a closed set of scalar
        // literals: model it as its scalar Go type (`type ProjectStatus string`), which
        // round-trips through the codec, rather than `interface{}`, which the codec
        // cannot (de)serialize and which a constructor cannot assign a typed value to.
        CsilTypeExpression::Choice(choices) => {
            literal_choice_scalar_go(choices).unwrap_or("interface{}")
        }
        _ => "interface{}", // Fallback for complex types
    };

    // Handle occurrence
    match occurrence {
        Some(CsilOccurrence::Optional) => format!("*{base_type}"),
        _ => base_type.to_string(),
    }
}

/// Builds the anonymous Go struct that stands in for a CSIL tuple. Keeping it a
/// pure `entries -> String` mapping (instead of hoisting a named type) lets a
/// tuple slot in anywhere a type string is expected — top-level alias, struct
/// field, slice element, or map value — and stay type-safe. Keyed entries take
/// their key's name; positional ones fall back to `Field0`/`Field1`/….
fn go_tuple_struct(entries: &[CsilGroupEntry], decimal_type: &str) -> String {
    let fields: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let field_name = match &entry.key {
                Some(key) => go_field_name_from_key(key),
                None => format!("Field{index}"),
            };
            let field_type =
                map_csil_type_to_go(&entry.value_type, &entry.occurrence, decimal_type);
            format!("{field_name} {field_type}")
        })
        .collect();
    format!("struct {{ {} }}", fields.join("; "))
}

fn go_field_name_from_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => {
            // Convert to PascalCase for Go public fields
            pascal_case(name)
        }
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => pascal_case(name),
        _ => "Field".to_string(),
    }
}

fn go_field_name_from_key_with_metadata(
    key: &CsilGroupKey,
    metadata: &[CsilFieldMetadata],
) -> String {
    // Check for go_name custom metadata
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "go_name"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(go_name) = &param.value
        {
            return go_name.clone();
        }
    }

    // Fall back to default naming
    go_field_name_from_key(key)
}

fn get_go_type_override(metadata: &[CsilFieldMetadata]) -> Option<String> {
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "go_type"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(go_type) = &param.value
        {
            return Some(go_type.clone());
        }
    }
    None
}

fn go_json_name_from_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

fn go_method_name(name: &str) -> String {
    pascal_case(name)
}

fn pascal_case(s: &str) -> String {
    s.split(&['_', '-'][..])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

fn get_field_visibility(metadata: &[CsilFieldMetadata]) -> CsilFieldVisibility {
    for meta in metadata {
        if let CsilFieldMetadata::Visibility(vis) = meta {
            return vis.clone();
        }
    }
    CsilFieldVisibility::Bidirectional
}

fn get_field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.as_str())
        } else {
            None
        }
    })
}

/// The `@depends-on(...)` boolean condition, surfaced as a Go-comment string on
/// the field. Go has no native conditional-presence facility, so — like the
/// simple `DependsOn` form — the dependency is documentation rather than enforced
/// code; rendering it keeps the intent visible to a reader of the generated type.
fn get_depends_comment(metadata: &[CsilFieldMetadata]) -> Option<String> {
    metadata
        .iter()
        .find_map(|meta| match meta {
            CsilFieldMetadata::DependsOnExpr(condition) => {
                Some(render_depends_condition(condition))
            }
            // The parser keeps the common `@depends-on(x = "y")` (and the bare
            // presence test `@depends-on(x)`) as the simple form, not the
            // expression form; rendering it here is what actually surfaces the
            // dependency the doc comment above promises to handle.
            CsilFieldMetadata::DependsOn { field, value } => Some(match value {
                Some(value) => {
                    format!("{field} == {}", literal_value_to_go_string(value))
                }
                None => field.clone(),
            }),
            _ => None,
        })
        // A rendered text value can carry a newline; since this lands in a `//`
        // line comment, an embedded break would push the remainder onto a second,
        // uncommented line and break the file. Keep the condition on one line.
        .map(|rendered| rendered.replace(['\n', '\r'], " "))
}

fn render_depends_condition(condition: &CsilDependsCondition) -> String {
    match condition {
        CsilDependsCondition::Compare { field, op, value } => match (op, value) {
            (Some(op), Some(value)) => format!(
                "{field} {} {}",
                depends_compare_op_str(op),
                literal_value_to_go_string(value)
            ),
            // A bare field (no operator/value) is a presence test.
            _ => field.clone(),
        },
        // `&` and `|` in the source map onto Go's `&&`/`||` so the rendered
        // comment reads like the boolean expression a Go author would write.
        CsilDependsCondition::All(conditions) => join_depends_conditions(conditions, "&&"),
        CsilDependsCondition::Any(conditions) => join_depends_conditions(conditions, "||"),
    }
}

fn join_depends_conditions(conditions: &[CsilDependsCondition], separator: &str) -> String {
    conditions
        .iter()
        .map(render_depends_condition)
        .collect::<Vec<_>>()
        .join(&format!(" {separator} "))
}

fn depends_compare_op_str(op: &CsilDependsCompareOp) -> &'static str {
    match op {
        CsilDependsCompareOp::Eq => "==",
        CsilDependsCompareOp::Ne => "!=",
        CsilDependsCompareOp::Lt => "<",
        CsilDependsCompareOp::Le => "<=",
        CsilDependsCompareOp::Gt => ">",
        CsilDependsCompareOp::Ge => ">=",
    }
}

fn literal_value_to_go_string(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{s}\""),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "[]byte{}".to_string(),
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(literal_value_to_go_string).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

fn literal_value_to_go_value(
    value: &CsilLiteralValue,
    value_type: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    config: &GoConfig,
) -> String {
    let optional = matches!(occurrence, Some(CsilOccurrence::Optional));

    // A `decimal`/`timestamp` field is a typed Go value (CsilDecimal/shopspring's
    // Decimal/time.Time), never a Go string. A bare string literal assigned to such
    // a field would not compile, so the bound text is parsed into the typed value
    // via the same must-parsers the validation code uses.
    match ordered_field_kind(value_type) {
        OrderedKind::Decimal => {
            if let Some(text) = literal_as_decimal_text(value) {
                let lit = go_string_lit(&text);
                let expr = match config.decimal_mapping {
                    DecimalMapping::Csil => format!("mustParseCsilDecimal({lit})"),
                    DecimalMapping::Library => format!("decimal.RequireFromString({lit})"),
                };
                let go_type = config.decimal_go_type();
                return if optional {
                    format!("func() *{go_type} {{ v := {expr}; return &v }}()")
                } else {
                    expr
                };
            }
        }
        OrderedKind::Timestamp => {
            if let Some(text) = literal_as_timestamp_text(value) {
                let expr = format!("mustParseTimestamp({})", go_string_lit(&text));
                return if optional {
                    format!("func() *time.Time {{ v := {expr}; return &v }}()")
                } else {
                    expr
                };
            }
        }
        OrderedKind::Numeric => {}
    }

    let base_value = match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{s}\""),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "[]byte{}".to_string(),
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(literal_value_to_go_string).collect();
            format!("[]interface{{}}{{{}}}", formatted.join(", "))
        }
    };

    // An optional field is a pointer, so its default is a `*T` to a typed temporary.
    // The temporary must be the field's exact Go type, not the literal's natural type:
    // a `uint` default literal is an integer but the field is `*uint64`, and an enum
    // default is text but the field is `*ProjectStatus`. The literal is wrapped in a
    // conversion to that type (`uint64(50)`, `ProjectStatus("active")`); the non-pointer
    // path above needs no conversion since an untyped constant assigns to any matching
    // kind.
    match occurrence {
        Some(CsilOccurrence::Optional) => {
            let go_type = map_csil_type_to_go(value_type, &None, config.decimal_go_type());
            let lit = match value {
                CsilLiteralValue::Integer(i) => i.to_string(),
                CsilLiteralValue::Float(f) => f.to_string(),
                CsilLiteralValue::Text(s) => go_string_lit(s),
                CsilLiteralValue::Bool(b) => b.to_string(),
                _ => return "nil".to_string(),
            };
            format!("func() *{go_type} {{ v := {go_type}({lit}); return &v }}()")
        }
        _ => base_value,
    }
}

fn estimate_memory_usage() -> usize {
    // Simple memory usage estimate
    4096 // 4KB estimate
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
    use csilgen_common::{
        CsilPosition, CsilRule, CsilServiceDefinition, CsilServiceOperation, CsilSpecSerialized,
        GeneratorConfig,
    };
    use std::collections::HashMap;

    #[test]
    fn test_pascal_case() {
        assert_eq!(pascal_case("user_name"), "UserName");
        assert_eq!(pascal_case("api-key"), "ApiKey");
        assert_eq!(pascal_case("simple"), "Simple");
        assert_eq!(pascal_case("openbao_installed"), "OpenbaoInstalled");
        assert_eq!(pascal_case("dns_zones_created"), "DnsZonesCreated");
        assert_eq!(pascal_case("k8s_installed"), "K8sInstalled");
    }

    #[test]
    fn test_go_package_ident() {
        // Bare identifiers pass through unchanged.
        assert_eq!(go_package_ident("corndogsapi"), "corndogsapi");
        assert_eq!(go_package_ident("echoclient"), "echoclient");
        // A full module path collapses to its sanitized last segment.
        assert_eq!(
            go_package_ident("github.com/CatalystCommunity/corndogs/gen/corndogsapi"),
            "corndogsapi"
        );
        // Mixed case and disallowed characters are stripped to a legal identifier.
        assert_eq!(
            go_package_ident("github.com/org/Corn-Dogs.API"),
            "corndogsapi"
        );
        // A leading digit is illegal to start a Go identifier, so it is prefixed.
        assert_eq!(go_package_ident("github.com/org/2fa"), "_2fa");
        // A segment with nothing legal left falls back rather than emitting `package `.
        assert_eq!(go_package_ident("github.com/org/---"), "api");
        assert_eq!(go_package_ident(""), "api");
    }

    #[test]
    fn test_go_type_mapping() {
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("text".to_string()),
                &None,
                "CsilDecimal"
            ),
            "string"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("int".to_string()),
                &None,
                "CsilDecimal"
            ),
            "int64"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Reference("User".to_string()),
                &None,
                "CsilDecimal"
            ),
            "User"
        );
        // The CDDL aliases `tstr`/`bstr` map identically to `text`/`bytes`,
        // matching the rust and python generators.
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("tstr".to_string()),
                &None,
                "CsilDecimal"
            ),
            "string"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("bstr".to_string()),
                &None,
                "CsilDecimal"
            ),
            "[]byte"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("bytes".to_string()),
                &None,
                "CsilDecimal"
            ),
            "[]byte"
        );
    }

    #[test]
    fn test_timestamp_and_decimal_type_mapping() {
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("timestamp".to_string()),
                &None,
                "CsilDecimal"
            ),
            "time.Time"
        );
        // The decimal Go type is whatever the active mapping passes through.
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("decimal".to_string()),
                &None,
                "CsilDecimal"
            ),
            "CsilDecimal"
        );
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("decimal".to_string()),
                &None,
                "decimal.Decimal"
            ),
            "decimal.Decimal"
        );
    }

    #[test]
    fn test_optional_types() {
        let optional = Some(CsilOccurrence::Optional);
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Builtin("text".to_string()),
                &optional,
                "CsilDecimal"
            ),
            "*string"
        );
    }

    #[test]
    fn enum_maps_to_named_scalar_not_interface() {
        let choices = vec![
            CsilTypeExpression::Literal(CsilLiteralValue::Text("active".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("archived".to_string())),
        ];
        // A text-literal enum is a named string the codec can round-trip, not the
        // uncompilable `interface{}` it used to collapse to.
        assert_eq!(
            map_csil_type_to_go(
                &CsilTypeExpression::Choice(choices.clone()),
                &None,
                "CsilDecimal"
            ),
            "string"
        );
        assert_eq!(enum_scalar_builtin(&choices), Some("text".to_string()));
        // A mixed (non-literal) union has no scalar form and stays a union.
        let union = vec![
            CsilTypeExpression::Reference("A".to_string()),
            CsilTypeExpression::Reference("B".to_string()),
        ];
        assert_eq!(literal_choice_scalar_go(&union), None);
    }

    #[test]
    fn alias_decoder_converts_to_named_type() {
        use std::collections::{HashMap, HashSet};
        let config = GoConfig::from_options(&HashMap::new()).unwrap();
        let records = HashSet::new();
        let mut aliases = HashMap::new();
        aliases.insert(
            "MemberID".to_string(),
            CsilTypeExpression::Builtin("text".to_string()),
        );
        // A named scalar alias decodes the underlying primitive then converts to the
        // named type, so `cborDecArray` infers `[]MemberID` rather than `[]string`.
        let dec = go_dec_func(
            &CsilTypeExpression::Reference("MemberID".to_string()),
            &records,
            &aliases,
            &config,
        );
        assert!(
            dec.contains("MemberID(csilInner)"),
            "decoder must convert to the named type, got {dec}"
        );
    }

    #[test]
    fn optional_default_pointer_uses_field_type() {
        use std::collections::HashMap;
        let config = GoConfig::from_options(&HashMap::new()).unwrap();
        // A `uint` optional default is a `*uint64`, never the literal's natural `*int64`.
        let uint_default = literal_value_to_go_value(
            &CsilLiteralValue::Integer(50),
            &CsilTypeExpression::Builtin("uint".to_string()),
            &Some(CsilOccurrence::Optional),
            &config,
        );
        assert!(
            uint_default.contains("*uint64") && uint_default.contains("uint64(50)"),
            "got {uint_default}"
        );
        // An enum optional default points at the named enum type, not `*string`.
        let enum_default = literal_value_to_go_value(
            &CsilLiteralValue::Text("active".to_string()),
            &CsilTypeExpression::Reference("ProjectStatus".to_string()),
            &Some(CsilOccurrence::Optional),
            &config,
        );
        assert!(
            enum_default.contains("*ProjectStatus")
                && enum_default.contains("ProjectStatus(\"active\")"),
            "got {enum_default}"
        );
    }

    fn input_with_service(name: &str, ops: Vec<CsilServiceOperation>) -> WasmGeneratorInput {
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: name.to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: ops,
                        wire_id: None,
                    }),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "go".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "go".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "go".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    fn make_op(
        name: &str,
        input: &str,
        output: &str,
        direction: CsilServiceDirection,
    ) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference(input.to_string()),
            output_type: CsilTypeExpression::Reference(output.to_string()),
            direction,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        }
    }

    #[test]
    fn bidirectional_op_emits_inbound_method_router_and_outbound_encoder() {
        let input = input_with_service(
            "Match",
            vec![
                make_op(
                    "list-events",
                    "User",
                    "User",
                    CsilServiceDirection::Unidirectional,
                ),
                make_op("play", "User", "User", CsilServiceDirection::Bidirectional),
            ],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Codec interface emitted once.
        assert!(services.contains("type Codec interface"));

        // Unidirectional stays request/response.
        assert!(services.contains("ListEvents(ctx context.Context, req User) (User, error)"));
        // Bidirectional is a fire-and-forget inbound (no Send/Recv stream).
        assert!(services.contains("Play(ctx context.Context, msg User) error"));
        // The old Stream interface MUST NOT be emitted.
        assert!(!services.contains("PlayStream interface"));
        assert!(!services.contains("Send(User) error"));
        assert!(!services.contains("Recv() (User, error)"));

        // Router dispatches by wire method name.
        assert!(services.contains("func RouteMatchChannel(handlers Match, ctx context.Context, codec Codec, method string, data []byte) error"));
        assert!(services.contains("case \"Play\":"));
        assert!(services.contains("return handlers.Play(ctx, msg)"));

        // Outbound encoder for the bidi op.
        assert!(
            services
                .contains("func EncodeMatchPlay(codec Codec, msg User) (string, []byte, error)")
        );
        assert!(services.contains("return \"Play\", data, nil"));
    }

    #[test]
    fn reverse_op_emits_only_outbound_encoder_no_inbound_method_or_on_callback() {
        let input = input_with_service(
            "Callbacks",
            vec![make_op(
                "notify",
                "User",
                "User",
                CsilServiceDirection::Reverse,
            )],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Reverse has no server-side inbound method or On<M> callback.
        assert!(!services.contains("Notify(ctx context.Context"));
        assert!(!services.contains("OnNotify"));

        // Router exists but has no Notify case (no inbound to dispatch).
        let router_start = services.find("func RouteCallbacksChannel").unwrap();
        let router_block = &services[router_start..];
        assert!(!router_block.contains("case \"Notify\":"));

        // The server-pushed encoder is present.
        assert!(
            services.contains(
                "func EncodeCallbacksNotify(codec Codec, msg User) (string, []byte, error)"
            )
        );
    }

    #[test]
    fn unary_only_service_skips_codec_and_router() {
        let input = input_with_service(
            "Auth",
            vec![make_op(
                "login",
                "User",
                "User",
                CsilServiceDirection::Unidirectional,
            )],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        assert!(!services.contains("type Codec interface"));
        assert!(!services.contains("RouteAuthChannel"));
        assert!(!services.contains("EncodeAuthLogin"));
        // "fmt" should not be imported when no router exists.
        assert!(!services.contains("\"fmt\""));
    }

    fn unary_union_op(name: &str) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
            output_type: CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                CsilTypeExpression::Reference("ServiceError".to_string()),
            ]),
            direction: CsilServiceDirection::Unidirectional,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        }
    }

    #[test]
    fn types_drop_cbor_tags_codec_keys_by_csil_field_name() {
        // The reflection/derive payload path is gone: types carry only json/yaml
        // tags, and the generated codec — not a `cbor:` struct tag — keys the wire
        // by the CSIL field name verbatim.
        let input = group_input(
            "Task",
            vec![
                bare_entry(
                    "current_state",
                    CsilTypeExpression::Builtin("text".to_string()),
                ),
                CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("note".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                    doc_comments: Vec::new(),
                },
            ],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("`json:\"current_state\" yaml:\"current_state\"`"));
        assert!(!types.contains("cbor:"));

        let codec = super::generate_codec(&input, &config).expect("codec emitted");
        // The codec keys the map by the CSIL field name verbatim.
        assert!(codec.contains("cborText(\"current_state\")"));
        assert!(codec.contains("cborText(\"note\")"));
        // No third-party CBOR library is referenced anywhere in the codec.
        assert!(!codec.contains("fxamacker"));
    }

    #[test]
    fn typed_response_strips_service_error() {
        let input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(services.contains(
            "SubmitTask(ctx context.Context, req SubmitTaskRequest) (SubmitTaskResponse, error)"
        ));
        assert!(!services.contains("interface{}"));
    }

    /// A corndogs-shaped spec: a `Task` record (text/bytes/optional-int/map/list),
    /// `SubmitTaskRequest`, a `ServiceError`, and `CorndogsService` with one unary
    /// `submit-task: SubmitTaskRequest -> Task / ServiceError`.
    fn corndogs_input(target: &str) -> WasmGeneratorInput {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let group_rule = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
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
                // Fields typed as named map ALIASES — the regression: their codec
                // must walk the underlying map, not stub it to null. One map-of-int
                // and one map-of-record.
                bare_entry(
                    "queue_counts",
                    CsilTypeExpression::Reference("StringInt64Map".to_string()),
                ),
                bare_entry(
                    "state_counts",
                    CsilTypeExpression::Reference("QueueAndStateCountsMap".to_string()),
                ),
            ],
        );
        let counts = group_rule(
            "QueueAndStateCounts",
            vec![bare_entry(
                "count",
                CsilTypeExpression::Builtin("int".to_string()),
            )],
        );
        // Named map aliases (`X = {* text => …}`) parse to a TypeDef carrying a Map.
        let alias = |name: &str, value: CsilTypeExpression| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(text()),
                value: Box::new(value),
                occurrence: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        let str_int_map = alias(
            "StringInt64Map",
            CsilTypeExpression::Builtin("int".to_string()),
        );
        let state_map = alias(
            "QueueAndStateCountsMap",
            CsilTypeExpression::Reference("QueueAndStateCounts".to_string()),
        );
        let req = group_rule(
            "SubmitTaskRequest",
            vec![
                bare_entry("task", CsilTypeExpression::Reference("Task".to_string())),
                bare_entry("queue", text()),
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
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![str_int_map, state_map, counts, task, req, err, svc],
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
                name: "go".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "go".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    #[test]
    fn go_client_target_emits_typed_client() {
        let output = super::process_generation(corndogs_input("go-client")).expect("generation ok");
        let client = output
            .files
            .iter()
            .find(|f| f.path == "client.gen.go")
            .expect("client.gen.go emitted");
        assert!(client.content.contains("type Transport interface"));
        // The transport is a dumb byte seam now: bytes in, bytes out.
        assert!(client.content.contains(
            "Call(ctx context.Context, service string, op string, req []byte) ([]byte, error)"
        ));
        assert!(client.content.contains("type ClientError struct"));
        assert!(
            client
                .content
                .contains("func NewCorndogsClient(transport Transport) *CorndogsClient")
        );
        // Typed seam: a SubmitTaskRequest in, a Task out, codec called internally.
        assert!(client.content.contains(
            "func (c *CorndogsClient) SubmitTask(ctx context.Context, req SubmitTaskRequest) (Task, error)"
        ));
        assert!(client.content.contains(
            "csilResp, csilErr := c.transport.Call(ctx, \"corndogs\", \"SubmitTask\", EncodeSubmitTaskRequest(req))"
        ));
        assert!(client.content.contains("return DecodeTask(csilResp)"));
        // The codec ships alongside the client.
        assert!(output.files.iter().any(|f| f.path == "codec.gen.go"));
        // Server interface must not be emitted for the client target.
        assert!(!output.files.iter().any(|f| f.path == "services.gen.go"));
    }

    #[test]
    fn codec_emitted_with_typed_client() {
        let output = super::process_generation(corndogs_input("go-client")).expect("generation ok");
        let codec = output
            .files
            .iter()
            .find(|f| f.path == "codec.gen.go")
            .expect("codec.gen.go emitted");
        // Public byte wrappers and the internal canonical-map encoder are present.
        assert!(
            codec
                .content
                .contains("func EncodeSubmitTaskRequest(csilV SubmitTaskRequest) []byte")
        );
        assert!(
            codec
                .content
                .contains("func DecodeTask(csilData []byte) (Task, error)")
        );
        assert!(
            codec
                .content
                .contains("func csilEncTask(csilV Task) cborValue")
        );
        // bytes -> CBOR byte string (major type 2), via the runtime's cborBytes.
        assert!(
            codec
                .content
                .contains("cborText(\"payload\"), cborBytes(csilV.Payload)")
        );
        // A nested record reference recurses into its codec.
        assert!(codec.content.contains("csilEncTask(csilV.Task)"));
        // Optional int is a pointer: guarded on encode, deref'd into the map.
        assert!(codec.content.contains("if csilV.Priority != nil"));
        assert!(codec.content.contains("cborInt((*csilV.Priority))"));
        // Keys are laid down in canonical (encoded-key) order: within Task the len-4
        // keys `tags`/`uuid` precede longer keys, and `current_state` (len 13) is last;
        // `tags` < `uuid` on content.
        let enc_start = codec.content.find("func csilEncTask").unwrap();
        let enc = &codec.content[enc_start..];
        let pos_tags = enc.find("\"tags\"").unwrap();
        let pos_uuid = enc.find("\"uuid\"").unwrap();
        let pos_state = enc.find("\"current_state\"").unwrap();
        assert!(
            pos_tags < pos_uuid && pos_uuid < pos_state,
            "fields not in canonical key order"
        );
    }

    #[test]
    fn go_server_alias_and_typesonly() {
        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-server".to_string();
        let output = super::process_generation(input).expect("generation ok");
        assert!(output.files.iter().any(|f| f.path == "services.gen.go"));
        assert!(!output.files.iter().any(|f| f.path == "client.gen.go"));

        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-typesonly".to_string();
        let output = super::process_generation(input).expect("generation ok");
        // This spec has no type rules, so the service surface is simply absent.
        assert!(!output.files.iter().any(|f| f.path == "services.gen.go"));
        assert!(!output.files.iter().any(|f| f.path == "client.gen.go"));
    }

    #[test]
    fn unknown_go_subtarget_errors() {
        let mut input = input_with_service("CorndogsService", vec![unary_union_op("SubmitTask")]);
        input.config.target = "go-bogus".to_string();
        assert!(super::process_generation(input).is_err());
    }

    fn group_input(
        type_name: &str,
        entries: Vec<CsilGroupEntry>,
        options: HashMap<String, serde_json::Value>,
    ) -> WasmGeneratorInput {
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: type_name.to_string(),
                    rule_type: CsilRuleType::GroupDef(csilgen_common::CsilGroupExpression {
                        entries,
                    }),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "go".to_string(),
                output_dir: "/tmp".to_string(),
                options,
            },
            generator_metadata: GeneratorMetadata {
                name: "go".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "go".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    fn string_opts(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    fn bare_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type,
            occurrence: None,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    fn constrained_entry(
        name: &str,
        base: &str,
        constraints: Vec<CsilControlOperator>,
    ) -> CsilGroupEntry {
        bare_entry(
            name,
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin(base.to_string())),
                constraints,
            },
        )
    }

    #[test]
    fn timestamp_maps_to_time_and_imports_time() {
        let input = group_input(
            "Event",
            vec![bare_entry(
                "created_at",
                CsilTypeExpression::Builtin("timestamp".to_string()),
            )],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("CreatedAt time.Time"));
        assert!(types.contains("import (\n\t\"time\"\n)"));
    }

    #[test]
    fn decimal_csil_mode_emits_self_contained_helper() {
        let input = group_input(
            "Money",
            vec![bare_entry(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
            )],
            HashMap::new(),
        );
        let output = super::process_generation(input).expect("generation ok");

        let types = output
            .files
            .iter()
            .find(|f| f.path == "types.gen.go")
            .expect("types emitted");
        assert!(types.content.contains("Amount CsilDecimal"));
        // Default mode must not pull in shopspring anywhere.
        assert!(!types.content.contains("shopspring"));

        let helper = output
            .files
            .iter()
            .find(|f| f.path == "csil_decimal.gen.go")
            .expect("CsilDecimal helper emitted");
        assert!(helper.content.contains("type CsilDecimal struct"));
        // The helper no longer owns the wire: no fxamacker, no Marshal/UnmarshalCBOR.
        assert!(!helper.content.contains("MarshalCBOR"));
        assert!(!helper.content.contains("fxamacker"));
        // Interop bridge present, but no hard dependency on shopspring.
        assert!(helper.content.contains("func ParseCsilDecimal"));
        assert!(
            helper
                .content
                .contains("func (d CsilDecimal) String() string")
        );
        // The bridge is documented, but shopspring is never imported.
        assert!(!helper.content.contains("\"github.com/shopspring/decimal\""));

        // The generated codec owns the CBOR tag-4 decimal wire form directly.
        let codec = output
            .files
            .iter()
            .find(|f| f.path == "codec.gen.go")
            .expect("codec.gen.go emitted");
        assert!(
            codec
                .content
                .contains("func csilEncDecimal(d CsilDecimal) cborValue")
        );
        assert!(codec.content.contains(
            "cborTag{num: 4, inner: cborArray{cborInt(d.Exponent), csilEncBigInt(d.mantissa())}}"
        ));
        assert!(codec.content.contains("\"math/big\""));
        assert!(!codec.content.contains("fxamacker"));
    }

    #[test]
    fn decimal_library_mode_uses_shopspring_and_no_helper() {
        let input = group_input(
            "Money",
            vec![bare_entry(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
            )],
            string_opts(&[("decimal_mapping", "library")]),
        );
        let output = super::process_generation(input).expect("generation ok");

        let types = output
            .files
            .iter()
            .find(|f| f.path == "types.gen.go")
            .expect("types emitted");
        assert!(types.content.contains("Amount decimal.Decimal"));
        assert!(types.content.contains("\"github.com/shopspring/decimal\""));
        // The library type stands alone; no generated helper.
        assert!(!output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
    }

    #[test]
    fn csil_decimal_helper_absent_when_decimal_unused() {
        let input = group_input(
            "Plain",
            vec![bare_entry(
                "name",
                CsilTypeExpression::Builtin("text".to_string()),
            )],
            HashMap::new(),
        );
        let output = super::process_generation(input).expect("generation ok");
        assert!(!output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
        let types = output
            .files
            .iter()
            .find(|f| f.path == "types.gen.go")
            .expect("types emitted");
        assert!(!types.content.contains("\"time\""));
    }

    #[test]
    fn unknown_decimal_mapping_is_hard_error() {
        let input = group_input(
            "Money",
            vec![bare_entry(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
            )],
            string_opts(&[("decimal_mapping", "bogus")]),
        );
        assert!(super::process_generation(input).is_err());
    }

    #[test]
    fn control_operators_emit_validation_checks() {
        let entries = vec![
            constrained_entry(
                "username",
                "text",
                vec![
                    CsilControlOperator::Size(CsilSizeConstraint::Range { min: 3, max: 20 }),
                    CsilControlOperator::Regex("^[a-z]+$".to_string()),
                ],
            ),
            constrained_entry(
                "age",
                "int",
                vec![
                    CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(18)),
                    CsilControlOperator::LessEqual(CsilLiteralValue::Integer(120)),
                ],
            ),
            // Encoding-only operator: documented, never a check, never an error.
            constrained_entry("blob", "bytes", vec![CsilControlOperator::Cbor]),
        ];
        let input = group_input("Account", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        assert!(validation.contains("func (v *Account) Validate() error"));
        assert!(validation.contains("if len(v.Username) < 3 {"));
        assert!(validation.contains("if len(v.Username) > 20 {"));
        assert!(validation.contains("regexp.MatchString(`^[a-z]+$`, v.Username)"));
        // regexp is imported only because a pattern check landed.
        assert!(validation.contains("\"regexp\""));
        assert!(validation.contains("if v.Age < 18 {"));
        assert!(validation.contains("if v.Age > 120 {"));
        assert!(validation.contains("// field 'Blob' carries an embedded-encoding operator"));
    }

    #[test]
    fn both_constraint_systems_coexist_in_validate() {
        let entries = vec![
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("name".to_string())),
                value_type: CsilTypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::MinLength(2),
                )],
                doc_comments: Vec::new(),
            },
            constrained_entry(
                "count",
                "int",
                vec![CsilControlOperator::GreaterThan(CsilLiteralValue::Integer(
                    0,
                ))],
            ),
        ];
        let input = group_input("Mix", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        // The `@`-annotation and `.`-control-operator both land in one Validate().
        assert!(validation.contains("if len(v.Name) < 2 {"));
        assert!(validation.contains("if v.Count <= 0 {"));
        // No regex here, so regexp must not be imported.
        assert!(!validation.contains("\"regexp\""));
    }

    #[test]
    fn validation_skipped_when_only_encoding_operators() {
        let input = group_input(
            "Blobby",
            vec![constrained_entry(
                "raw",
                "bytes",
                vec![CsilControlOperator::Cborseq],
            )],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        // An encoding-only operator yields no runtime check, so no Validate() file.
        assert!(
            super::generate_validation(&input, &config, &mut Vec::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn default_control_operator_feeds_constructor() {
        let input = group_input(
            "Config",
            vec![constrained_entry(
                "retries",
                "int",
                vec![CsilControlOperator::Default(CsilLiteralValue::Integer(3))],
            )],
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let ctors = super::generate_constructors(&input, &config, &mut Vec::new(), false)
            .unwrap()
            .expect("constructors emitted");
        assert!(ctors.contains("func NewConfig() *Config"));
        assert!(ctors.contains("Retries: 3,"));
    }

    fn decimal_and_timestamp_bound_entries() -> Vec<CsilGroupEntry> {
        vec![
            constrained_entry(
                "balance",
                "decimal",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "0.00".to_string(),
                ))],
            ),
            constrained_entry(
                "created_at",
                "timestamp",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            ),
        ]
    }

    #[test]
    fn decimal_and_timestamp_bounds_parse_not_bare_compare_csil_mode() {
        let input = group_input(
            "User",
            decimal_and_timestamp_bound_entries(),
            HashMap::new(),
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        assert!(validation.contains("func (v *User) Validate() error"));

        // The decimal bound is parsed and compared through Cmp, never `v.Balance < "0.00"`.
        assert!(validation.contains("if v.Balance.Cmp(mustParseCsilDecimal(\"0.00\")) < 0 {"));
        assert!(!validation.contains("v.Balance < \"0.00\""));

        // The timestamp bound is parsed via RFC3339 and compared with Before.
        assert!(
            validation
                .contains("if v.CreatedAt.Before(mustParseTimestamp(\"1970-01-01T00:00:00Z\")) {")
        );
        assert!(!validation.contains("v.CreatedAt < \"1970-01-01T00:00:00Z\""));
        assert!(validation.contains("func mustParseTimestamp(s string) time.Time"));

        // time is imported because a timestamp comparison landed; the default
        // decimal mapping never references shopspring.
        assert!(validation.contains("\"time\""));
        assert!(!validation.contains("shopspring"));
        assert!(!validation.contains("decimal.RequireFromString"));
    }

    #[test]
    fn decimal_bound_uses_shopspring_in_library_mode() {
        let input = group_input(
            "User",
            decimal_and_timestamp_bound_entries(),
            string_opts(&[("decimal_mapping", "library")]),
        );
        let output = super::process_generation(input).expect("generation ok");
        let validation = output
            .files
            .iter()
            .find(|f| f.path == "validation.gen.go")
            .expect("validation emitted");

        // Library mode compares through shopspring's RequireFromString/Cmp and must
        // import the package in the validation file itself.
        assert!(
            validation
                .content
                .contains("if v.Balance.Cmp(decimal.RequireFromString(\"0.00\")) < 0 {")
        );
        assert!(
            validation
                .content
                .contains("\"github.com/shopspring/decimal\"")
        );
        assert!(!validation.content.contains("mustParseCsilDecimal"));
        // No CsilDecimal helper file in library mode.
        assert!(!output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
    }

    #[test]
    fn csil_decimal_helper_carries_cmp_and_must_parse() {
        // The Cmp method and must-parser the validation file relies on live in the
        // generated helper, so a decimal Validate() compiles against the same package.
        let input = group_input(
            "User",
            decimal_and_timestamp_bound_entries(),
            HashMap::new(),
        );
        let output = super::process_generation(input).expect("generation ok");
        let helper = output
            .files
            .iter()
            .find(|f| f.path == "csil_decimal.gen.go")
            .expect("CsilDecimal helper emitted");
        assert!(
            helper
                .content
                .contains("func (d CsilDecimal) Cmp(other CsilDecimal) int")
        );
        assert!(
            helper
                .content
                .contains("func mustParseCsilDecimal(s string) CsilDecimal")
        );
    }

    #[test]
    fn min_value_annotation_on_decimal_field_parses_bound() {
        // `@min-value` is the annotation form; it must get the same typed-compare
        // treatment as `.ge` so it does not emit a bare scalar comparison.
        let entry = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("balance".to_string())),
            value_type: CsilTypeExpression::Builtin("decimal".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinValue(CsilLiteralValue::Text("0.00".to_string())),
            )],
            doc_comments: Vec::new(),
        };
        let input = group_input("Wallet", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        assert!(validation.contains("if v.Balance.Cmp(mustParseCsilDecimal(\"0.00\")) < 0 {"));
        assert!(validation.contains("must be at least 0.00"));
        assert!(!validation.contains("v.Balance < \"0.00\""));
    }

    #[test]
    fn bound_with_embedded_quote_stays_a_valid_literal() {
        // A pathological bound must never break the surrounding Go string literal;
        // the embedded quote is escaped in both the parse argument and the message.
        let entry = constrained_entry(
            "label",
            "timestamp",
            vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                "a\"b".to_string(),
            ))],
        );
        let input = group_input("Weird", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        assert!(validation.contains("mustParseTimestamp(\"a\\\"b\")"));
        assert!(validation.contains("must be >= a\\\"b"));
    }

    fn optional_constrained_entry(
        name: &str,
        base: &str,
        constraints: Vec<CsilControlOperator>,
    ) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin(base.to_string())),
                constraints,
            },
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn regex_message_escapes_pattern_so_go_literal_stays_valid() {
        // A pattern with a backslash escape (`\d+`) must not be spliced raw into the
        // double-quoted error message: `\d` is an invalid Go escape and would not
        // compile. The MatchString call keeps the raw backtick form.
        let entry = constrained_entry(
            "code",
            "text",
            vec![CsilControlOperator::Regex(r"\d+".to_string())],
        );
        let input = group_input("Ticket", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        // MatchString still uses the raw backtick literal verbatim.
        assert!(validation.contains("regexp.MatchString(`\\d+`, v.Code)"));
        // The message escapes the backslash so the double-quoted literal is valid.
        assert!(validation.contains("must match pattern '\\\\d+'"));
        // The invalid single-backslash form must never appear in the message.
        assert!(!validation.contains("must match pattern '\\d+'"));
    }

    #[test]
    fn optional_fields_are_nil_guarded_and_dereferenced() {
        // Optional fields are Go pointers; dereferencing a nil one in Validate()
        // would panic. Every check must sit behind a nil guard and read through a
        // deref so a missing optional is simply skipped.
        let entries = vec![
            optional_constrained_entry(
                "balance",
                "decimal",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "0.00".to_string(),
                ))],
            ),
            optional_constrained_entry(
                "created_at",
                "timestamp",
                vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            ),
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("name".to_string())),
                value_type: CsilTypeExpression::Builtin("text".to_string()),
                occurrence: Some(CsilOccurrence::Optional),
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::MinLength(2),
                )],
                doc_comments: Vec::new(),
            },
        ];
        let input = group_input("Account", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");

        // Decimal: guarded, and the pointer is dereferenced for the Cmp.
        assert!(validation.contains("if v.Balance != nil {"));
        assert!(validation.contains("if (*v.Balance).Cmp(mustParseCsilDecimal(\"0.00\")) < 0 {"));
        // Timestamp: guarded, dereferenced for Before.
        assert!(validation.contains("if v.CreatedAt != nil {"));
        assert!(
            validation.contains(
                "if (*v.CreatedAt).Before(mustParseTimestamp(\"1970-01-01T00:00:00Z\")) {"
            )
        );
        // String length: guarded, dereferenced for len.
        assert!(validation.contains("if v.Name != nil {"));
        assert!(validation.contains("if len((*v.Name)) < 2 {"));
    }

    #[test]
    fn typed_defaults_construct_decimal_and_timestamp_values() {
        // A `decimal`/`timestamp` default must build the typed Go value, never a bare
        // string literal assigned to a CsilDecimal/time.Time field (a compile error).
        let entries = vec![
            constrained_entry(
                "balance",
                "decimal",
                vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                    "0.00".to_string(),
                ))],
            ),
            constrained_entry(
                "created_at",
                "timestamp",
                vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            ),
        ];
        let input = group_input("Wallet", entries, HashMap::new());
        let output = super::process_generation(input).expect("generation ok");
        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");

        assert!(
            ctors
                .content
                .contains("Balance: mustParseCsilDecimal(\"0.00\"),")
        );
        assert!(
            ctors
                .content
                .contains("CreatedAt: mustParseTimestamp(\"1970-01-01T00:00:00Z\"),")
        );
        // No Validate() lands here (defaults are not checks), so the constructor file
        // carries its own copy of the timestamp must-parser and imports time.
        assert!(
            ctors
                .content
                .contains("func mustParseTimestamp(s string) time.Time")
        );
        assert!(ctors.content.contains("\"time\""));
        // The CsilDecimal must-parser is provided by the helper file, in-package.
        assert!(output.files.iter().any(|f| f.path == "csil_decimal.gen.go"));
    }

    #[test]
    fn timestamp_default_does_not_redeclare_helper_when_validation_defines_it() {
        // When a timestamp field has both a bound (Validate() defines the must-parser)
        // and a default (constructor references it), the helper is defined once.
        let entry = constrained_entry(
            "created_at",
            "timestamp",
            vec![
                CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                )),
                CsilControlOperator::Default(CsilLiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                )),
            ],
        );
        let input = group_input("Event", vec![entry], HashMap::new());
        let output = super::process_generation(input).expect("generation ok");

        let validation = output
            .files
            .iter()
            .find(|f| f.path == "validation.gen.go")
            .expect("validation emitted");
        assert!(
            validation
                .content
                .contains("func mustParseTimestamp(s string) time.Time")
        );

        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");
        // The constructor references the must-parser but must not redeclare it.
        assert!(
            ctors
                .content
                .contains("CreatedAt: mustParseTimestamp(\"1970-01-01T00:00:00Z\"),")
        );
        assert!(!ctors.content.contains("func mustParseTimestamp"));
    }

    #[test]
    fn library_decimal_default_uses_shopspring_and_imports_it() {
        let entry = constrained_entry(
            "balance",
            "decimal",
            vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "0.00".to_string(),
            ))],
        );
        let input = group_input(
            "Wallet",
            vec![entry],
            string_opts(&[("decimal_mapping", "library")]),
        );
        let output = super::process_generation(input).expect("generation ok");
        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");
        assert!(
            ctors
                .content
                .contains("Balance: decimal.RequireFromString(\"0.00\"),")
        );
        assert!(ctors.content.contains("\"github.com/shopspring/decimal\""));
    }

    #[test]
    fn typedef_group_record_gets_constructor_for_defaults() {
        // A record authored as a `TypeDef` wrapping a `Group` must apply defaults just
        // like a `GroupDef`; the constructor path handles both rule shapes.
        let group = csilgen_common::CsilGroupExpression {
            entries: vec![constrained_entry(
                "retries",
                "int",
                vec![CsilControlOperator::Default(CsilLiteralValue::Integer(3))],
            )],
        };
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: "Config".to_string(),
                    rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(group)),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "go".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "go".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "go".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        };
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let ctors = super::generate_constructors(&input, &config, &mut Vec::new(), false)
            .unwrap()
            .expect("constructors emitted");
        assert!(ctors.contains("func NewConfig() *Config"));
        assert!(ctors.contains("Retries: 3,"));
    }

    #[test]
    fn decimal_integer_bound_and_default_render_as_decimal_text() {
        // A `decimal` bound/default written as a bare integer literal is rendered to
        // its decimal string and parsed, not only handled when it arrives as text.
        let entry = constrained_entry(
            "balance",
            "decimal",
            vec![
                CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(5)),
                CsilControlOperator::Default(CsilLiteralValue::Integer(0)),
            ],
        );
        let input = group_input("Wallet", vec![entry], HashMap::new());
        let output = super::process_generation(input).expect("generation ok");

        let validation = output
            .files
            .iter()
            .find(|f| f.path == "validation.gen.go")
            .expect("validation emitted");
        assert!(
            validation
                .content
                .contains("if v.Balance.Cmp(mustParseCsilDecimal(\"5\")) < 0 {")
        );

        let ctors = output
            .files
            .iter()
            .find(|f| f.path == "constructors.gen.go")
            .expect("constructors emitted");
        assert!(
            ctors
                .content
                .contains("Balance: mustParseCsilDecimal(\"0\"),")
        );
    }

    #[test]
    fn equality_operators_still_emit_after_match_collapse() {
        // Collapsing the comparison dispatch must not drop any operator: `.eq`/`.ne`
        // still produce their checks.
        let entries = vec![
            constrained_entry(
                "exact",
                "int",
                vec![CsilControlOperator::Equal(CsilLiteralValue::Integer(7))],
            ),
            constrained_entry(
                "forbidden",
                "int",
                vec![CsilControlOperator::NotEqual(CsilLiteralValue::Integer(13))],
            ),
        ];
        let input = group_input("Limits", entries, HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let validation = super::generate_validation(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("validation emitted");
        assert!(validation.contains("if v.Exact != 7 {"));
        assert!(validation.contains("if v.Forbidden == 13 {"));
    }

    fn tuple_entry(
        key: Option<&str>,
        value_type: CsilTypeExpression,
        occurrence: Option<CsilOccurrence>,
    ) -> CsilGroupEntry {
        CsilGroupEntry {
            key: key.map(|k| CsilGroupKey::Bare(k.to_string())),
            value_type,
            occurrence,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn positional_tuple_maps_to_anonymous_struct() {
        // `[text, ?int, bool]` has no keys, so entries become Field0/Field1/Field2;
        // the optional entry keeps its pointer mapping inside the struct.
        let tuple = CsilTypeExpression::Tuple(csilgen_common::CsilGroupExpression {
            entries: vec![
                tuple_entry(None, CsilTypeExpression::Builtin("text".to_string()), None),
                tuple_entry(
                    None,
                    CsilTypeExpression::Builtin("int".to_string()),
                    Some(CsilOccurrence::Optional),
                ),
                tuple_entry(None, CsilTypeExpression::Builtin("bool".to_string()), None),
            ],
        });
        assert_eq!(
            map_csil_type_to_go(&tuple, &None, "CsilDecimal"),
            "struct { Field0 string; Field1 *int64; Field2 bool }"
        );
    }

    #[test]
    fn keyed_tuple_uses_keys_for_field_names() {
        // `[tag: text, value: any]` names fields after its keys.
        let tuple = CsilTypeExpression::Tuple(csilgen_common::CsilGroupExpression {
            entries: vec![
                tuple_entry(
                    Some("tag"),
                    CsilTypeExpression::Builtin("text".to_string()),
                    None,
                ),
                tuple_entry(
                    Some("value"),
                    CsilTypeExpression::Builtin("any".to_string()),
                    None,
                ),
            ],
        });
        assert_eq!(
            map_csil_type_to_go(&tuple, &None, "CsilDecimal"),
            "struct { Tag string; Value any }"
        );
    }

    #[test]
    fn tuple_typedef_emits_named_struct() {
        // A top-level tuple alias resolves to a named Go struct, so it stays
        // type-safe rather than collapsing to interface{}.
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: "MixedArray".to_string(),
                    rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Tuple(
                        csilgen_common::CsilGroupExpression {
                            entries: vec![
                                tuple_entry(
                                    None,
                                    CsilTypeExpression::Builtin("text".to_string()),
                                    None,
                                ),
                                tuple_entry(
                                    None,
                                    CsilTypeExpression::Builtin("int".to_string()),
                                    None,
                                ),
                            ],
                        },
                    )),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "go".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "go".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "go".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        };
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("type MixedArray struct { Field0 string; Field1 int64 }"));
    }

    #[test]
    fn tuple_carrying_timestamp_pulls_time_import() {
        // A tuple entry typed `timestamp` must count toward the `time` import the
        // same way an array or group entry would.
        let tuple = CsilTypeExpression::Tuple(csilgen_common::CsilGroupExpression {
            entries: vec![tuple_entry(
                Some("at"),
                CsilTypeExpression::Builtin("timestamp".to_string()),
                None,
            )],
        });
        assert!(type_uses_builtin(&tuple, "timestamp"));
        assert!(!type_uses_builtin(&tuple, "decimal"));
    }

    #[test]
    fn depends_on_expr_renders_boolean_tree() {
        // All -> &&, Any -> ||, with comparison and presence leaves.
        let condition = CsilDependsCondition::Any(vec![
            CsilDependsCondition::All(vec![
                CsilDependsCondition::Compare {
                    field: "account_type".to_string(),
                    op: Some(CsilDependsCompareOp::Eq),
                    value: Some(CsilLiteralValue::Text("enterprise".to_string())),
                },
                CsilDependsCondition::Compare {
                    field: "seats".to_string(),
                    op: Some(CsilDependsCompareOp::Gt),
                    value: Some(CsilLiteralValue::Integer(5)),
                },
            ]),
            // A bare field is a presence test, no operator.
            CsilDependsCondition::Compare {
                field: "override_flag".to_string(),
                op: None,
                value: None,
            },
        ]);
        assert_eq!(
            render_depends_condition(&condition),
            "account_type == \"enterprise\" && seats > 5 || override_flag"
        );
    }

    #[test]
    fn depends_on_expr_emits_field_comment() {
        // The dependency survives generation as a Go comment on the field.
        let entry = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("state".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::DependsOnExpr(
                CsilDependsCondition::Compare {
                    field: "country".to_string(),
                    op: Some(CsilDependsCompareOp::Ne),
                    value: Some(CsilLiteralValue::Text("US".to_string())),
                },
            )],
            doc_comments: Vec::new(),
        };
        let input = group_input("ShippingForm", vec![entry], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("// depends-on: country != \"US\""));
    }

    #[test]
    fn reverse_op_with_null_input_emits_encoder_without_request_param() {
        // `op: <- Event` yields a null input on a Reverse op; it must produce a
        // server-push encoder keyed on the output type and never a request param.
        let push_op = CsilServiceOperation {
            name: "user-joined".to_string(),
            input_type: CsilTypeExpression::Builtin("null".to_string()),
            output_type: CsilTypeExpression::Reference("UserJoinedEvent".to_string()),
            direction: CsilServiceDirection::Reverse,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        };
        let input = input_with_service("ChatService", vec![push_op]);
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Push-only op rides the encoder surface, typed by the event it sends.
        assert!(
            services.contains("func EncodeChatServiceUserJoined(codec Codec, msg UserJoinedEvent)")
        );
        // No inbound interface method and no bogus request parameter for a push op.
        assert!(!services.contains("UserJoined(ctx context.Context, req"));
        assert!(!services.contains("UserJoined(ctx context.Context, msg"));
    }

    #[test]
    fn simple_depends_on_emits_field_comment() {
        // The parser keeps `@depends-on(x = "y")` as the simple `DependsOn` form;
        // both a string comparison and a boolean comparison must surface as a Go
        // comment rather than being silently dropped.
        let text_dep = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("region".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::DependsOn {
                field: "country".to_string(),
                value: Some(CsilLiteralValue::Text("US".to_string())),
            }],
            doc_comments: Vec::new(),
        };
        let bool_dep = CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("tax_id".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::DependsOn {
                field: "is_business".to_string(),
                value: Some(CsilLiteralValue::Bool(true)),
            }],
            doc_comments: Vec::new(),
        };
        let input = group_input("Address", vec![text_dep, bool_dep], HashMap::new());
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let types = super::generate_types(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("types emitted");
        assert!(types.contains("// depends-on: country == \"US\""));
        assert!(types.contains("// depends-on: is_business == true"));
    }

    #[test]
    fn unidirectional_op_with_null_input_omits_request_param() {
        // `op: -> Event` carries a null input on a unary op; neither the client
        // method nor the interface method should surface a meaningless request
        // parameter the caller would have to pass `nil` for.
        let push_op = CsilServiceOperation {
            name: "ping".to_string(),
            input_type: CsilTypeExpression::Builtin("null".to_string()),
            output_type: CsilTypeExpression::Reference("Pong".to_string()),
            direction: CsilServiceDirection::Unidirectional,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        };
        let mut input = input_with_service("HealthService", vec![push_op]);
        // The typed client decodes the response through the codec, so the success
        // type must be a record; add a `Pong` so the method is emitted.
        input.csil_spec.rules.push(CsilRule {
            name: "Pong".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry(
                    "ok",
                    CsilTypeExpression::Builtin("bool".to_string()),
                )],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        let config = GoConfig::from_options(&input.config.options).unwrap();

        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(services.contains("Ping(ctx context.Context) (Pong, error)"));
        assert!(!services.contains("Ping(ctx context.Context, req"));

        let client = super::generate_client(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("client emitted");
        assert!(client.contains("func (c *HealthClient) Ping(ctx context.Context) (Pong, error)"));
        assert!(!client.contains("Ping(ctx context.Context, req"));
        // A null input carries no body: the transport gets a nil payload, and the
        // response bytes decode through the codec.
        assert!(client.contains("c.transport.Call(ctx, \"health\", \"Ping\", nil)"));
        assert!(client.contains("return DecodePong(csilResp)"));
    }

    fn wire_id_input() -> WasmGeneratorInput {
        let mut place = make_op(
            "place-order",
            "Order",
            "Receipt",
            CsilServiceDirection::Unidirectional,
        );
        place.wire_id = Some(7);
        let cancel = make_op(
            "cancel-order",
            "Order",
            "Receipt",
            CsilServiceDirection::Unidirectional,
        );
        let mut input = input_with_service("OrderService", vec![place, cancel]);
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[0].rule_type {
            service.wire_id = Some(3);
        }
        input
    }

    #[test]
    fn wire_ids_emitted_when_present() {
        let input = wire_id_input();
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(
            services.contains("const OrderServiceServiceWireID uint64 = 3"),
            "expected service ordinal const, got:\n{services}"
        );
        assert!(
            services.contains("const OrderServiceOpPlaceOrderWireID uint64 = 7"),
            "expected operation ordinal const, got:\n{services}"
        );
        // Operation without a wire-id contributes no const.
        assert!(
            !services.contains("CancelOrderWireID"),
            "operation without wire-id must not emit a const"
        );
    }

    #[test]
    fn wire_id_op_named_service_does_not_collide() {
        let mut place = make_op(
            "service",
            "Order",
            "Receipt",
            CsilServiceDirection::Unidirectional,
        );
        place.wire_id = Some(7);
        let mut input = input_with_service("OrderService", vec![place]);
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[0].rule_type {
            service.wire_id = Some(3);
        }
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        // Op `service` becomes OrderServiceOpServiceWireID, distinct from the
        // service const OrderServiceServiceWireID, so Go won't redeclare a name.
        assert!(
            services.contains("const OrderServiceServiceWireID uint64 = 3"),
            "expected service ordinal const, got:\n{services}"
        );
        assert!(
            services.contains("const OrderServiceOpServiceWireID uint64 = 7"),
            "expected distinct op ordinal const, got:\n{services}"
        );
    }

    #[test]
    fn wire_ids_absent_when_unset() {
        let input = input_with_service(
            "OrderService",
            vec![make_op(
                "place-order",
                "Order",
                "Receipt",
                CsilServiceDirection::Unidirectional,
            )],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        assert!(
            !services.contains("WireID"),
            "no wire-id output when service has no wire-id, got:\n{services}"
        );
    }

    // Build a channel (bidirectional) service carrying `@wire-id` ordinals so the
    // compact-router twin has something to dispatch on.
    fn wire_id_channel_input() -> WasmGeneratorInput {
        let mut play = make_op("play", "User", "User", CsilServiceDirection::Bidirectional);
        play.wire_id = Some(5);
        let mut input = input_with_service("Match", vec![play]);
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[0].rule_type {
            service.wire_id = Some(1);
        }
        input
    }

    #[test]
    fn compact_router_emitted_for_wire_id_channel_service() {
        let input = wire_id_channel_input();
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Verbose router stays byte-identical alongside the compact twin.
        assert!(
            services.contains("func RouteMatchChannel(handlers Match, ctx context.Context, codec Codec, method string, data []byte) error"),
            "verbose router expected, got:\n{services}"
        );
        // Compact twin dispatches on the operation ordinal, not the wire name.
        assert!(
            services.contains("func RouteMatchChannelCompact(handlers Match, ctx context.Context, codec Codec, op uint64, data []byte) error"),
            "compact router expected, got:\n{services}"
        );
        assert!(
            services.contains("case 5:"),
            "compact router matches the op ordinal, got:\n{services}"
        );
        assert!(
            services.contains("return handlers.Play(ctx, msg)"),
            "compact router dispatches to the handler, got:\n{services}"
        );
        assert!(
            services.contains("unknown channel ordinal %d"),
            "compact router has an ordinal fallthrough, got:\n{services}"
        );
    }

    #[test]
    fn compact_router_absent_without_wire_id() {
        let input = input_with_service(
            "Match",
            vec![make_op(
                "play",
                "User",
                "User",
                CsilServiceDirection::Bidirectional,
            )],
        );
        let config = GoConfig::from_options(&input.config.options).unwrap();
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");
        // The verbose router survives; the compact twin must not appear.
        assert!(
            services.contains("func RouteMatchChannel("),
            "verbose router expected, got:\n{services}"
        );
        assert!(
            !services.contains("Compact"),
            "no compact router without wire-ids, got:\n{services}"
        );
    }

    /// Compile the generated codec + typed client and round-trip a corndogs request
    /// through a loopback transport with `go run`. Skips cleanly when no Go toolchain
    /// is on PATH; with one present, this is the real proof the output is usable.
    #[test]
    fn codec_round_trips_through_go() {
        let probe = std::process::Command::new("go").arg("version").output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no go toolchain on PATH");
            return;
        }

        let output = super::process_generation(corndogs_input("go-client")).expect("generation ok");

        let dir = std::env::temp_dir().join(format!("csilgen-go-codec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let api_dir = dir.join("api");
        std::fs::create_dir_all(&api_dir).unwrap();
        for file in &output.files {
            std::fs::write(api_dir.join(&file.path), &file.content).unwrap();
        }
        // A go.mod pinning go 1.18 (the generics floor the codec needs) plus a driver
        // main wiring a loopback transport that decodes the request and re-encodes its
        // task as the response.
        std::fs::write(dir.join("go.mod"), "module csilroundtrip\n\ngo 1.18\n").unwrap();
        std::fs::write(dir.join("main.go"), GO_CODEC_DRIVER).unwrap();

        let run = std::process::Command::new("go")
            .arg("run")
            .arg(".")
            .current_dir(&dir)
            // Keep the build hermetic: never fetch a toolchain or hit the module proxy
            // (the generated code has no third-party deps), and confine the cache.
            .env("GOTOOLCHAIN", "local")
            .env("GOFLAGS", "-mod=mod")
            .env("GOPROXY", "off")
            .env("GO111MODULE", "on")
            .env("GOCACHE", dir.join(".gocache"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "go run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const GO_CODEC_DRIVER: &str = r#"package main

import (
	"bytes"
	"context"
	"fmt"
	"os"

	"csilroundtrip/api"
)

// loopback is a "server" on the far side of the dumb byte seam: it decodes the
// typed request, then encodes its task as the typed response, exercising both
// decode and encode across the transport boundary.
type loopback struct{}

func (loopback) Call(ctx context.Context, service, op string, req []byte) ([]byte, error) {
	if service != "corndogs" || op != "SubmitTask" {
		return nil, fmt.Errorf("unexpected route %s/%s", service, op)
	}
	in, err := api.DecodeSubmitTaskRequest(req)
	if err != nil {
		return nil, err
	}
	return api.EncodeTask(in.Task), nil
}

func check(cond bool, msg string) {
	if !cond {
		fmt.Println("FAIL:", msg)
		os.Exit(1)
	}
}

func must(err error) {
	if err != nil {
		fmt.Println("ERR:", err)
		os.Exit(1)
	}
}

func main() {
	prio := int64(7)
	task := api.Task{
		Uuid:         "u-123",
		CurrentState: "PENDING",
		Payload:      []byte{0xde, 0xad, 0xbe},
		Priority:     &prio,
		Labels:       map[string]int64{"a": 1, "b": 2},
		Tags:         []string{"x", "y"},
		QueueCounts:  api.StringInt64Map{"q1": 3, "q2": 1},
		StateCounts:  api.QueueAndStateCountsMap{"q1": {Count: 5}},
	}
	req := api.SubmitTaskRequest{Task: task, Queue: "default"}

	// Direct codec round-trip through the nested record.
	back, err := api.DecodeSubmitTaskRequest(api.EncodeSubmitTaskRequest(req))
	must(err)
	check(back.Task.Uuid == "u-123", "uuid")
	check(bytes.Equal(back.Task.Payload, []byte{0xde, 0xad, 0xbe}), "payload")
	check(back.Task.Priority != nil && *back.Task.Priority == 7, "priority")
	check(len(back.Task.Labels) == 2 && back.Task.Labels["a"] == 1 && back.Task.Labels["b"] == 2, "labels")
	check(len(back.Task.Tags) == 2 && back.Task.Tags[0] == "x" && back.Task.Tags[1] == "y", "tags")
	check(back.Queue == "default", "queue")
	// Named map aliases must round-trip their entries, not drop them (the regression).
	check(len(back.Task.QueueCounts) == 2 && back.Task.QueueCounts["q1"] == 3 && back.Task.QueueCounts["q2"] == 1, "queue_counts map alias")
	check(len(back.Task.StateCounts) == 1 && back.Task.StateCounts["q1"].Count == 5, "state_counts map-of-record alias")

	// An absent optional must round-trip to nil, not a zero value.
	task2 := task
	task2.Priority = nil
	back2, err := api.DecodeSubmitTaskRequest(api.EncodeSubmitTaskRequest(api.SubmitTaskRequest{Task: task2, Queue: "q"}))
	must(err)
	check(back2.Task.Priority == nil, "absent optional nil")

	// Typed client round-trip over the loopback carrier.
	client := api.NewCorndogsClient(loopback{})
	resp, err := client.SubmitTask(context.Background(), req)
	must(err)
	check(resp.Uuid == "u-123", "client uuid")
	check(bytes.Equal(resp.Payload, []byte{0xde, 0xad, 0xbe}), "client payload")
	check(resp.Priority != nil && *resp.Priority == 7, "client priority")
	check(len(resp.Tags) == 2 && resp.Tags[1] == "y", "client tags")

	fmt.Println("ok")
}
"#;
}
