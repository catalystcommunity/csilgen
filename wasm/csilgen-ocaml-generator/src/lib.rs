//! OCaml code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target ocaml` from `csilgen_ocaml_generator.wasm`.
//! Emits idiomatic OCaml *source* — records and Capitalized-constructor variants
//! behind a `types.mli` interface, services as modules with verbose + compact
//! routers, and a transport seam — but never the wire bytes (those live in
//! `transports/ocaml/`).

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSpecSerialized,
    CsilTypeExpression, GeneratedFile, GenerationStats, GeneratorCapability, GeneratorConfig,
    GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "ocaml-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "OCaml code generator".to_string(),
        target: "ocaml".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
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

    let files = generate_ocaml(&input.csil_spec, &input.config)
        .map_err(|_| error_codes::GENERATION_ERROR)?;

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
        warnings: Vec::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// Sub-target surface
// ---------------------------------------------------------------------------

/// Which surface a sub-target emits. `ocaml`/`ocaml-server` emit handler records
/// and routers; `ocaml-client` emits transport-agnostic call functions;
/// `ocaml-typesonly` emits only the type declarations.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

fn resolve_surface(target: &str) -> Result<Surface, i32> {
    match target {
        "ocaml" | "ocaml-server" => Ok(Surface::Server),
        "ocaml-client" => Ok(Surface::Client),
        "ocaml-typesonly" => Ok(Surface::TypesOnly),
        _ => Err(error_codes::GENERATION_ERROR),
    }
}

fn generate_ocaml(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<Vec<GeneratedFile>, i32> {
    let surface = resolve_surface(config.target.as_str())?;
    let mut files = Vec::new();

    // Types are shared by every surface (the client/server reference them), so the
    // typesonly surface is simply "stop after types".
    let (types_ml, types_mli) = generate_types(spec);
    if !types_ml.trim().is_empty() {
        files.push(GeneratedFile {
            path: "types.ml".to_string(),
            content: types_ml,
        });
        files.push(GeneratedFile {
            path: "types.mli".to_string(),
            content: types_mli,
        });
    }

    // Per-type CBOR (de)serializers make the generated records usable over the wire
    // without a hand-written codec; the typed client below calls them.
    if let Some(codec) = generate_codec(spec) {
        files.push(GeneratedFile {
            path: "codec.ml".to_string(),
            content: codec,
        });
    }

    let has_services = spec
        .rules
        .iter()
        .any(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)));

    // A package's `genquickstart.md` demonstrates both the calling side (the RPC and
    // Datagrams sections, over `Client`) and the handling side (the Events section,
    // over `Services`), so a package must carry both surfaces for its own quickstart
    // to compile — regardless of which surface was requested. A flat (non-package)
    // build stays byte-identical: it emits only the requested surface.
    let pkg_mode = package_requested(config);
    let want_client =
        matches!(surface, Surface::Client) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    let want_server =
        matches!(surface, Surface::Server) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    if has_services {
        if want_client {
            files.push(GeneratedFile {
                path: "client.ml".to_string(),
                content: generate_client(spec),
            });
        }
        if want_server {
            files.push(GeneratedFile {
                path: "services.ml".to_string(),
                content: generate_services(spec),
            });
        }
    }

    // Package mode is orthogonal to the surface: it relocates whatever the surface
    // produced into a `lib/` directory and adds the dune/opam scaffolding so the
    // output directory is itself a buildable, publishable package. The default
    // (flat) output is left byte-identical when the option is absent.
    if package_requested(config) {
        let pkg = package_name(spec, config);
        let version = package_version(config);
        return Ok(wrap_as_package(
            spec,
            files,
            &pkg,
            &version,
            emit_readme_enabled(config),
            wanted_transports(config),
        ));
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// Self-contained package mode
// ---------------------------------------------------------------------------

/// Whether `config.options["emit_packages"]` requests an OCaml package. The option
/// is a JSON array shared across generators, so it is parsed defensively: a missing
/// key, a non-array value, or non-string elements all mean "not requested" rather
/// than an error, and a list that names other languages but not `"ocaml"` leaves
/// the OCaml output untouched.
fn package_requested(config: &GeneratorConfig) -> bool {
    config
        .options
        .get("emit_packages")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().filter_map(|e| e.as_str()).any(|s| s == "ocaml"))
}

/// The opam/dune package and library name. Taken from `package_name` when the
/// option is a non-empty string, else derived from the first service's name, else
/// the `"csilgen_client"` default. Always sanitized to a valid OCaml library name
/// because dune rejects a `name`/`public_name` that is not a lowercase,
/// underscore-joined identifier beginning with a letter.
fn package_name(spec: &CsilSpecSerialized, config: &GeneratorConfig) -> String {
    let configured = config
        .options
        .get("package_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let base = match configured {
        // A path-style `package_name` (e.g. a Go module path) is the cross-ecosystem
        // source of truth; opam wants only its tail. See `package_name_last_segment`.
        Some(name) => csilgen_common::package_name_last_segment(name).to_string(),
        None => derive_package_name(spec),
    };
    sanitize_lib_name(&base)
}

/// A package name derived from the spec when none is configured: the first declared
/// service's name (so a single-service spec yields a recognizable package), falling
/// back to the conventional default for a service-less spec.
fn derive_package_name(spec: &CsilSpecSerialized) -> String {
    spec.rules
        .iter()
        .find_map(|r| match &r.rule_type {
            CsilRuleType::ServiceDef(_) => Some(r.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "csilgen_client".to_string())
}

/// Coerce an arbitrary name into a valid OCaml library / opam package name:
/// snake_case (lowercase, underscore-joined), stripped of any non-alphanumeric
/// character, and forced to begin with a letter (a leading digit or empty result is
/// prefixed) so dune accepts it as both a `public_name` and an `.opam` basename.
fn sanitize_lib_name(name: &str) -> String {
    let snake = name.to_case(Case::Snake);
    let cleaned: String = snake
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let needs_prefix = cleaned
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphabetic());
    if needs_prefix {
        format!("csil_{cleaned}")
    } else {
        cleaned
    }
}

/// The package version: the `package_version` option when a non-empty string, else
/// the conventional initial `0.1.0`.
fn package_version(config: &GeneratorConfig) -> String {
    config
        .options
        .get("package_version")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("0.1.0")
        .to_string()
}

/// Whether the package README should be emitted. Only an explicit `emit_readme: false`
/// opts out; any other value (absent, non-bool, or `true`) keeps the README.
fn emit_readme_enabled(config: &GeneratorConfig) -> bool {
    config.options.get("emit_readme").and_then(|v| v.as_bool()) != Some(false)
}

/// Relocate the generated modules into `lib/` and prepend the dune/opam scaffolding
/// that turns the output directory into a standalone package, plus a copy-paste
/// `genquickstart.md` Quickstart. The generated codec is self-contained, so the emitted
/// library declares no third-party dependencies.
fn wrap_as_package(
    spec: &CsilSpecSerialized,
    files: Vec<GeneratedFile>,
    pkg: &str,
    version: &str,
    emit_readme: bool,
    transports: (bool, bool, bool),
) -> Vec<GeneratedFile> {
    let mut out = Vec::with_capacity(files.len() + 4);
    out.push(GeneratedFile {
        path: "dune-project".to_string(),
        content: dune_project_file(pkg),
    });
    out.push(GeneratedFile {
        path: format!("{pkg}.opam"),
        content: opam_file(pkg, version),
    });
    // The README is opt-out: an explicit `emit_readme: false` suppresses it, while an
    // absent, non-bool, or `true` value keeps the default emission.
    if emit_readme {
        out.push(GeneratedFile {
            path: "genquickstart.md".to_string(),
            content: readme(spec, pkg, transports),
        });
    }
    out.push(GeneratedFile {
        path: "lib/dune".to_string(),
        content: lib_dune_file(pkg),
    });
    for file in files {
        out.push(GeneratedFile {
            path: format!("lib/{}", file.path),
            content: file.content,
        });
    }
    out
}

fn dune_project_file(pkg: &str) -> String {
    // The `.opam` file is emitted directly (not generated by dune), so the project
    // only declares the lang and name; the package surface comes from the opam file.
    format!("(lang dune 3.0)\n(name {pkg})\n")
}

/// A minimal but valid opam 2.0 package file. Its basename is the package name, which
/// is the package dune resolves the library's `public_name` against; the consumer
/// edits the placeholder metadata before publishing.
fn opam_file(pkg: &str, version: &str) -> String {
    format!(
        "opam-version: \"2.0\"\n\
         version: \"{version}\"\n\
         synopsis: \"Generated CSIL bindings ({pkg})\"\n\
         description: \"OCaml types and CBOR codec generated by csilgen.\"\n\
         maintainer: \"csilgen\"\n\
         authors: \"csilgen\"\n\
         license: \"Apache-2.0\"\n\
         homepage: \"https://github.com/catalystcommunity/csilgen\"\n\
         bug-reports: \"https://github.com/catalystcommunity/csilgen/issues\"\n\
         depends: [\n  \"ocaml\"\n  \"dune\" {{>= \"3.0\"}}\n]\n\
         build: [\n  [\"dune\" \"build\" \"-p\" name \"-j\" jobs]\n]\n"
    )
}

/// The library `dune` stanza. The generated modules are the only sources in `lib/`,
/// so dune's default module discovery covers them; the `public_name` makes the
/// library installable under the package name.
fn lib_dune_file(pkg: &str) -> String {
    format!("(library\n (name {pkg})\n (public_name {pkg}))\n")
}

// ---------------------------------------------------------------------------
// Package README + CSIL-RPC Quickstart
// ---------------------------------------------------------------------------

/// Which transport sections the consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or empty
/// value means "all three". Sections always render in a fixed order.
fn wanted_transports(config: &GeneratorConfig) -> (bool, bool, bool) {
    let listed = match config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
            let has = |t: &str| names.contains(&t);
            // An array that names none of the known transports (all unknown, or empty)
            // falls back to all three rather than an empty doc.
            if has("rpc") || has("events") || has("datagrams") {
                Some((has("rpc"), has("events"), has("datagrams")))
            } else {
                None
            }
        }
        _ => None,
    };
    listed.unwrap_or((true, true, true))
}

/// The package genquickstart: a transport-by-transport Quickstart over the official
/// `csilgen-transport` library. The generated codec owns CBOR (de)serialization and the
/// library owns the envelope/framing/lifecycle; you supply only a *carrier* that moves
/// bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events over TLS,
/// CSIL-Datagrams over UDP) is a complete, copy-paste example built on the library.
fn readme(spec: &CsilSpecSerialized, pkg: &str, transports: (bool, bool, bool)) -> String {
    // dune wraps the library under a single Capitalized module of the library name, so
    // the generated `Types`/`Codec`/`Client`/`Services` are reached as `<Root>.Types`,
    // etc.; the transport library is `Csilgen_transport`.
    let root = capitalize(pkg);
    let mut out = format!(
        "# {pkg}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the official `csilgen-transport` library owns the\n\
         envelope, framing, and connection lifecycle. You supply only a *carrier* that\n\
         moves bytes, so the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or\n\
         raw UDP unchanged.\n\n\
         ## Consume\n\n\
         This is a dune/opam package. Depend on it, the transport library, and the\n\
         stdlib `unix` library (the example carriers' socket I/O) from your\n\
         executable's `dune`:\n\n\
         ```\n\
         (executable\n \
         (name main)\n \
         (libraries {pkg} csilgen-transport unix))\n\
         ```\n\n\
         <!-- TODO: `csilgen-transport` is not yet published — vendor it or pin a git\n\
              source until it lands in an opam repository, then `opam install {pkg}`. -->\n\n"
    );
    let (rpc, events, datagrams) = transports;
    let unary = first_unary_example(spec);
    let channel = first_channel_example(spec);
    if rpc {
        out.push_str(&rpc_section(&root, unary.as_ref()));
    }
    if events {
        out.push_str(&events_section(&root, channel.as_ref()));
    }
    if datagrams {
        out.push_str(&datagrams_section(&root, unary.as_ref()));
    }
    out
}

/// The pieces a unary (`->`) example needs: the service module (under `Client`), the
/// call function, a compiling sample request `(literal, type-name)` (None for a
/// request-less op), the request/response codec type names (so the datagram section can
/// name `encode_<t>_bytes`/`decode_<t>_bytes`), and the op's datagram ordinal.
struct UnaryExample {
    service_module: String,
    fn_name: String,
    sample: Option<(String, String)>,
    req_codec: Option<String>,
    res_codec: Option<String>,
    op_ord: i64,
}

/// The first service (in declared order) with a unary op whose success type — and, when
/// present, request type — is a record the generated codec covers, so the example can
/// call the clean typed client form. `None` for a serviceless / non-record-op spec.
fn first_unary_example(spec: &CsilSpecSerialized) -> Option<UnaryExample> {
    let records = codec_record_names(spec);
    for rule in &spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let res_codec = op_codec_type(&success_type(&op.output_type), &records);
            if res_codec.is_none() {
                continue;
            }
            let null_in = op_input_is_null(&op.input_type);
            let (sample, req_codec) = if null_in {
                (None, None)
            } else {
                // The request must be a record the codec covers, so the typed client
                // method takes the value directly (no consumer-supplied closures).
                let CsilTypeExpression::Reference(name) = &op.input_type else {
                    continue;
                };
                let group = ocaml_find_record(spec, name)?;
                (
                    Some((ocaml_record_literal(spec, group), ocaml_type_name(name))),
                    op_codec_type(&op.input_type, &records),
                )
            };
            return Some(UnaryExample {
                service_module: ocaml_module_name(&rule.name),
                fn_name: ocaml_ident(&op.name),
                sample,
                req_codec,
                res_codec,
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.map(|id| id as i64).unwrap_or(1),
            });
        }
    }
    None
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `Client` transport seam that
/// builds/parses the envelope with the library's `Rpc.encode_request`/`Rpc.decode_response`
/// (never hand-rolled) and POSTs it to `{base}/csil/v1/rpc` over the stdlib `unix`
/// library. The typed client decodes the success payload; a non-zero transport status
/// (`Rpc.as_transport_error`) and the `ServiceError` application arm are surfaced
/// distinctly.
fn rpc_section(root: &str, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`Rpc.encode_request`/\n\
         `Rpc.decode_response`); you bring a carrier that moves bytes. The `unix`-socket\n\
         HTTP carrier below is just one example — swap it for any HTTP client (it\n\
         implements the generated `Client` byte seam).\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```ocaml\n");
    out.push_str(&format!("open {root}\nopen Csilgen_transport\n\n"));
    out.push_str(RPC_CARRIER_OCAML);
    out.push('\n');
    out.push_str("let () =\n");
    out.push_str("  let client = make_rpc_client \"http://localhost:5080\" in\n");
    match &ex.sample {
        Some((literal, type_name)) => {
            // The binding's type annotation lets OCaml resolve the record's labels by
            // type-directed disambiguation without the consumer opening `Types`.
            out.push_str(&format!("  let req : Types.{type_name} = {literal} in\n"));
            out.push_str(&format!(
                "  match Client.{}.{} client req with\n",
                ex.service_module, ex.fn_name
            ));
        }
        None => {
            out.push_str(&format!(
                "  match Client.{}.{} client with\n",
                ex.service_module, ex.fn_name
            ));
        }
    }
    out.push_str("  | Ok _resp -> print_endline \"ok: received typed response\"\n");
    out.push_str("  | Error e -> prerr_endline e\n");
    out.push_str("```\n\n");
    out
}

/// The record a type reference names, if any — both `Name = { .. }` (`TypeDef(Group)`)
/// and a bare group rule (`GroupDef`) are records.
fn ocaml_find_record<'a>(
    spec: &'a CsilSpecSerialized,
    name: &str,
) -> Option<&'a CsilGroupExpression> {
    spec.rules
        .iter()
        .filter(|r| r.name == name)
        .find_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        })
}

/// `{ field = <sample>; ... }` over a record's required (non-optional) named fields,
/// keyed by the OCaml labels the generated types use.
fn ocaml_record_literal(spec: &CsilSpecSerialized, group: &CsilGroupExpression) -> String {
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| match &e.key {
            Some(CsilGroupKey::Bare(k))
            | Some(CsilGroupKey::Literal(CsilLiteralValue::Text(k))) => Some(format!(
                "{} = {}",
                ocaml_ident(k),
                ocaml_sample(spec, &e.value_type)
            )),
            _ => None,
        })
        .collect();
    if fields.is_empty() {
        // An empty record maps to `unit`, whose literal is `()`.
        "()".to_string()
    } else {
        format!("{{ {} }}", fields.join("; "))
    }
}

/// A compiling OCaml literal for `ty`: real values for scalars/collections and nested
/// records (required fields only), and a `failwith` escape (typed `'a`, so it fits any
/// field) for shapes a generic sample cannot fabricate.
fn ocaml_sample(spec: &CsilSpecSerialized, ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "float" => "0.0".to_string(),
            "bytes" | "bstr" => "(Bytes.of_string \"\")".to_string(),
            "timestamp" => "\"1970-01-01T00:00:00Z\"".to_string(),
            "decimal" => "\"0\"".to_string(),
            "int" | "uint" => "0L".to_string(),
            "nil" | "null" => "()".to_string(),
            _ => "(failwith \"TODO: provide a value\")".to_string(),
        },
        CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. } => "[]".to_string(),
        CsilTypeExpression::Reference(name) => match ocaml_find_record(spec, name) {
            Some(group) => ocaml_record_literal(spec, group),
            None => "(failwith \"TODO: provide a value\")".to_string(),
        },
        _ => "(failwith \"TODO: provide a value\")".to_string(),
    }
}

/// The HTTP carrier body — spec-independent, so a constant. It encodes the request with
/// the library's `Rpc.encode_request` (never hand-rolled), POSTs it to
/// `{base}/csil/v1/rpc` over the stdlib `unix` library, and decodes the reply with
/// `Rpc.decode_response`. `Rpc.as_transport_error` surfaces a non-zero transport status;
/// the typed `ServiceError` arm (a status-0 `variant`) is surfaced separately so the
/// generated client decodes success only.
const RPC_CARRIER_OCAML: &str = r#"(* The library owns the CSIL-RPC envelope ([Rpc]); this carrier owns only the HTTP
   transport. HTTP is a minimal blocking POST over the stdlib [unix] library (bundled
   with the OCaml compiler) — one example carrier; swap it for any HTTP client. *)

(* Minimal blocking HTTP/1.1 POST; returns (status_code, body_bytes). *)
let http_post ~(base_url : string) ~(path : string) ~(body : bytes) :
    (int * bytes, string) result =
  let stripped =
    if String.length base_url >= 7 && String.sub base_url 0 7 = "http://" then
      String.sub base_url 7 (String.length base_url - 7)
    else base_url
  in
  let host, port =
    match String.index_opt stripped ':' with
    | Some i ->
      ( String.sub stripped 0 i,
        int_of_string (String.sub stripped (i + 1) (String.length stripped - i - 1)) )
    | None -> (stripped, 80)
  in
  match Unix.gethostbyname host with
  | exception Not_found -> Error ("csil-rpc: cannot resolve host " ^ host)
  | he ->
    let sock = Unix.socket Unix.PF_INET Unix.SOCK_STREAM 0 in
    Fun.protect
      ~finally:(fun () -> try Unix.close sock with _ -> ())
      (fun () ->
        Unix.connect sock (Unix.ADDR_INET (he.Unix.h_addr_list.(0), port));
        let oc = Unix.out_channel_of_descr sock in
        let ic = Unix.in_channel_of_descr sock in
        Printf.fprintf oc
          "POST %s HTTP/1.1\r\nHost: %s\r\nContent-Type: application/cbor\r\n\
           Content-Length: %d\r\nConnection: close\r\n\r\n"
          path host (Bytes.length body);
        output_bytes oc body;
        flush oc;
        let status =
          match String.split_on_char ' ' (input_line ic) with
          | _ :: code :: _ -> ( try int_of_string code with _ -> 0)
          | _ -> 0
        in
        (* Skip headers (input_line strips '\n', leaving a lone '\r' on the blank line),
           then read the body to EOF since we asked for Connection: close. *)
        let rec skip () =
          match input_line ic with "" | "\r" -> () | _ -> skip ()
        in
        skip ();
        let buf = Buffer.create 256 in
        (try
           while true do
             Buffer.add_channel buf ic 1
           done
         with End_of_file -> ());
        Ok (status, Buffer.to_bytes buf))

(* The generated client's transport seam. *)
let csil_rpc_call base_url ~service ~op ~(payload : bytes) : (bytes, string) result =
  (* Encode the request with the library's RPC envelope (never hand-rolled). *)
  let req = Rpc.new_request service op payload in
  match http_post ~base_url ~path:"/csil/v1/rpc" ~body:(Rpc.encode_request req) with
  | Error _ as e -> e
  | Ok (http_status, _) when http_status <> 200 ->
    Error (Printf.sprintf "csil-rpc: http %d" http_status)
  | Ok (_, resp_bytes) -> (
    match Rpc.decode_response resp_bytes with
    | Error _ -> Error "csil-rpc: bad response envelope"
    | Ok resp -> (
      (* A non-zero transport status is a transport failure: no typed payload. *)
      match Rpc.as_transport_error resp with
      | Some _ -> Error "csil-rpc: non-zero transport status"
      | None -> (
        (* A typed ServiceError arm rides as a status-0 [variant] — an application
           error distinct from a transport failure. Surface it so the client decodes
           success only. *)
        match resp.Rpc.variant with
        | Some "ServiceError" -> Error "csil-rpc: ServiceError"
        | _ -> Ok resp.Rpc.payload)))

(* Build the typed client over the carrier; change the base URL to your server. *)
let make_rpc_client base_url =
  Client.make_client ~call:(fun ~service ~op ~payload ->
      csil_rpc_call base_url ~service ~op ~payload)
"#;

/// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as the
/// library's `Carrier.stream_carrier` (CSIL length-prefix framing), performs the
/// `$hello`/`$hello-ack` handshake, sends one outbound event via the generated codec,
/// and runs a recv loop that decodes each frame to an `Events.event`, answers `$ping`
/// with `$pong`, and dispatches typed events to the generated `Services.<S>.route`. When
/// the spec has no record channel op the dispatch wiring is replaced with a note (the
/// handshake + heartbeat still apply to any connection).
fn events_section(root: &str, ch: Option<&ChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library\n\
         owns the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and\n\
         length-prefix framing; the generated router dispatches typed events. The TLS\n\
         carrier below is just one example — a WebSocket/QUIC carrier drops in unchanged.\n\n",
    );
    out.push_str("```ocaml\n");
    out.push_str(&format!("open {root}\nopen Csilgen_transport\n\n"));
    out.push_str(EVENTS_CARRIER_OCAML);
    out.push('\n');
    match ch {
        Some(ch) => out.push_str(&events_session(ch)),
        None => out.push_str(EVENTS_NO_CHANNEL_SESSION_OCAML),
    }
    out.push_str("```\n\n");
    out
}

/// The TLS frame-carrier adapter — spec-independent, so a constant. It opens a TCP
/// stream and hands the channels to the library's `Carrier.stream_carrier`, which owns
/// the length-prefix framing. Wrapping the channels in TLS (via the `tls`/`ssl` opam
/// lib) is a one-line swap the comment marks; the session logic is unchanged.
const EVENTS_CARRIER_OCAML: &str = r#"(* One example carrier: a byte stream framed with CSIL's 4-byte length prefix. The
   library's [stream_carrier] owns framing; this only opens the socket. For real TLS,
   wrap [ic]/[oc] with the [tls] (or [ssl]) opam library at the marked point — the
   carrier and the session logic below ride any channel pair unchanged. *)
let connect_frame_carrier host port : Carrier.frame_carrier =
  let he = Unix.gethostbyname host in
  let addr = Unix.ADDR_INET (he.Unix.h_addr_list.(0), port) in
  let ic, oc = Unix.open_connection addr in
  (* TLS swap point: [let ic, oc = Tls_unix.wrap ic oc in] (or the [ssl] equivalent). *)
  Carrier.stream_carrier ic oc
"#;

/// The channel session body for an Events connection that has a record `<->` op: the
/// handshake, one outbound event via the generated codec, the generated handler record,
/// and the recv loop that heartbeats and dispatches into the generated router.
fn events_session(ch: &ChannelExample) -> String {
    format!(
        r#"let () =
  let carrier = connect_frame_carrier "localhost" 7443 in

  (* $hello / $hello-ack handshake. The peer's $hello-ack pins the wire profile. *)
  let hello : Events.hello =
    {{ versions = [ 1L ]; profiles = [ "verbose" ]; hello_service = Some "{wire_service}"; hello_auth = None }}
  in
  ignore (carrier.Carrier.send_frame (Events.encode_hello hello));
  let profile =
    match carrier.Carrier.recv_frame () with
    | Ok (Some frame) -> (
      match Events.decode_hello_ack frame with
      | Ok (ack : Events.hello_ack) -> (
        match Events.parse_profile ack.ack_profile with Some p -> p | None -> Events.Verbose)
      | Error _ -> failwith "csil-events: bad hello-ack")
    | _ -> failwith "csil-events: no hello-ack"
  in

  (* Send one outbound event via the generated codec, framed as a typed Event. *)
  let value : Types.{out_type} = {out_sample} in
  let outbound = Events.new_verbose_event (Some "{wire_service}") "{wire_op}" (Codec.encode_{out_codec}_bytes value) in
  (match Events.encode_event outbound profile with
   | Ok frame -> ignore (carrier.Carrier.send_frame frame)
   | Error _ -> ());

  (* The generated handler record: the router dispatches inbound events to it. *)
  let handler : Services.{service_module}.handler = {handler_record} in

  (* Recv loop: decode each frame to an Event, answer $ping with $pong (heartbeat), and
     dispatch the rest into the generated router. *)
  let rec loop () =
    match carrier.Carrier.recv_frame () with
    | Ok (Some frame) -> (
      match Events.decode_event frame profile with
      | Ok (ev : Events.event) ->
        (match ev.event with
         | Some name when name = Events.ping_name -> (
           match Events.decode_heartbeat ev.payload with
           | Ok hb ->
             let pong = Events.new_verbose_event None Events.pong_name (Events.encode_heartbeat hb) in
             (match Events.encode_event pong profile with
              | Ok b -> ignore (carrier.Carrier.send_frame b)
              | Error _ -> ())
           | Error _ -> ())
         | Some name -> ignore (Services.{service_module}.route handler ~op:name ~payload:ev.payload)
         | None -> ());
        loop ()
      | Error _ -> ())
    | _ -> ()
  in
  loop ()
"#,
        wire_service = ch.wire_service,
        out_type = ch.out_type,
        out_sample = ch.out_sample,
        out_codec = ch.out_codec,
        wire_op = ch.wire_op,
        service_module = ch.service_module,
        handler_record = ch.handler_record,
    )
}

/// The Events session body when the spec declares no record channel op: the handshake
/// and heartbeat still apply, so they are shown, with a note where the dispatch would go.
const EVENTS_NO_CHANNEL_SESSION_OCAML: &str = r#"let () =
  let carrier = connect_frame_carrier "localhost" 7443 in

  (* $hello / $hello-ack handshake. The peer's $hello-ack pins the wire profile. *)
  let hello : Events.hello =
    { versions = [ 1L ]; profiles = [ "verbose" ]; hello_service = None; hello_auth = None }
  in
  ignore (carrier.Carrier.send_frame (Events.encode_hello hello));
  let profile =
    match carrier.Carrier.recv_frame () with
    | Ok (Some frame) -> (
      match Events.decode_hello_ack frame with
      | Ok (ack : Events.hello_ack) -> (
        match Events.parse_profile ack.ack_profile with Some p -> p | None -> Events.Verbose)
      | Error _ -> failwith "csil-events: bad hello-ack")
    | _ -> failwith "csil-events: no hello-ack"
  in

  (* Recv loop: answer $ping with $pong. This build exposes no generated channel router
     (it is emitted by the `ocaml`/`ocaml-server` target for record `<->`/`<-` ops), so
     there is no typed dispatch to wire here. *)
  let rec loop () =
    match carrier.Carrier.recv_frame () with
    | Ok (Some frame) -> (
      match Events.decode_event frame profile with
      | Ok (ev : Events.event) ->
        (match ev.event with
         | Some name when name = Events.ping_name -> (
           match Events.decode_heartbeat ev.payload with
           | Ok hb ->
             let pong = Events.new_verbose_event None Events.pong_name (Events.encode_heartbeat hb) in
             (match Events.encode_event pong profile with
              | Ok b -> ignore (carrier.Carrier.send_frame b)
              | Error _ -> ())
           | Error _ -> ())
         | _ -> ());
        loop ()
      | Error _ -> ())
    | _ -> ()
  in
  loop ()
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in
/// the library's `Datagram`, and send it fire-and-forget over the library's UDP carrier.
/// The recv path `Datagrams.decode_datagram`s an inbound datagram and decodes its payload
/// with the generated codec into the RESPONSE type — there is NO synchronous response.
fn datagrams_section(root: &str, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram`\n\
         envelope (and a UDP carrier); you bring a datagram carrier. The UDP carrier\n\
         below is one example — a WebRTC unreliable channel or QUIC datagrams drop in\n\
         unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some((sample, req_type)), Some(req_codec), Some(res_codec)) =
        (&ex.sample, &ex.req_codec, &ex.res_codec)
    else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```ocaml\n");
    out.push_str(&format!("open {root}\nopen Csilgen_transport\n\n"));
    out.push_str(DATAGRAMS_CARRIER_OCAML);
    out.push('\n');
    out.push_str(&format!(
        r#"let () =
  let carrier = connect_datagram_carrier "localhost" 9000 in

  (* The operation's datagram ordinal — its @wire-id, or a channel-agreed number. *)
  let op_ord = {op_ord}L in

  (* Fire-and-forget: encode the [->] request via the generated codec, wrap it in the
     library's Datagram, and send it. seq 0 marks an unsequenced datagram. *)
  let req : Types.{req_type} = {sample} in
  let dg = Datagrams.new_datagram op_ord 0L (Codec.encode_{req_codec}_bytes req) in
  ignore (carrier.Carrier.send_datagram (Datagrams.encode_datagram dg));

  (* Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is NO
     synchronous response; the caller must tolerate loss and reordering and handle a
     reply whenever (if ever) it shows up. *)
  match carrier.Carrier.recv_datagram () with
  | Ok (Some bytes) -> (
    match Datagrams.decode_datagram bytes with
    | Ok (dg : Datagrams.datagram) ->
      let resp = Codec.decode_{res_codec}_bytes dg.payload in
      ignore resp;
      print_endline "late response"
    | Error _ -> ())
  | _ -> ()
```

"#,
        op_ord = ex.op_ord,
        req_type = req_type,
        sample = sample,
        req_codec = req_codec,
        res_codec = res_codec,
    ));
    out
}

/// The UDP datagram-carrier adapter — spec-independent, so a constant. It opens a
/// connected UDP socket and wraps it with the library's `Udp.udp_datagram_carrier`; the
/// carrier never waits for or correlates a reply.
const DATAGRAMS_CARRIER_OCAML: &str = r#"(* One example carrier: UDP via the library's [Udp.udp_datagram_carrier] over a
   connected [unix] socket. Datagrams are unreliable and unordered, so the carrier never
   waits for or correlates a reply. The library owns the Datagram envelope. *)
let connect_datagram_carrier host port : Carrier.datagram_carrier =
  let he = Unix.gethostbyname host in
  let addr = Unix.ADDR_INET (he.Unix.h_addr_list.(0), port) in
  let sock = Unix.socket Unix.PF_INET Unix.SOCK_DGRAM 0 in
  Unix.connect sock addr;
  Udp.udp_datagram_carrier sock
"#;

/// The pieces the Events session needs: the generated service module (under `Services`),
/// the wire service/op strings, the channel op's input/output codec type names, the
/// outbound sample literal + type, and the full handler-record literal (every non-reverse
/// op field, with the channel op's field wired to decode + dispatch).
struct ChannelExample {
    service_module: String,
    wire_service: String,
    wire_op: String,
    out_codec: String,
    out_type: String,
    out_sample: String,
    handler_record: String,
}

/// The first service (in declared order) with a `<->` op whose input and success output
/// are both records (so the generated router + handler + per-type codec helpers exist).
/// `None` when no service has a usable channel op — the Events section then shows the
/// handshake/heartbeat without dispatch wiring.
fn first_channel_example(spec: &CsilSpecSerialized) -> Option<ChannelExample> {
    let records = codec_record_names(spec);
    for rule in &spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let (Some(in_codec), Some(out_codec)) = (
                op_codec_type(&op.input_type, &records),
                op_codec_type(&success, &records),
            ) else {
                continue;
            };
            let CsilTypeExpression::Reference(out_name) = &success else {
                continue;
            };
            let out_group = ocaml_find_record(spec, out_name)?;
            return Some(ChannelExample {
                service_module: ocaml_module_name(&rule.name),
                wire_service: wire_service_name(&rule.name),
                wire_op: wire_op_name(&op.name),
                out_codec,
                out_type: ocaml_type_name(out_name),
                out_sample: ocaml_record_literal(spec, out_group),
                handler_record: ocaml_handler_record(spec, service, &op.name, &in_codec),
            });
        }
    }
    None
}

/// The full `{ field = fn; ... }` handler-record literal the generated `route` takes:
/// one field per non-reverse op. The chosen channel op's field decodes its payload with
/// the generated codec and logs; the other inbound ops get inert stubs so the record is
/// complete (a unary stub returns a transport error, a fire-and-forget channel stub
/// returns unit).
fn ocaml_handler_record(
    spec: &CsilSpecSerialized,
    service: &CsilServiceDefinition,
    channel_op: &str,
    in_codec: &str,
) -> String {
    let mut fields: Vec<String> = Vec::new();
    for op in &service.operations {
        if matches!(op.direction, CsilServiceDirection::Reverse) {
            continue;
        }
        let field = ocaml_ident(&op.name);
        if op.name == channel_op {
            fields.push(format!(
                "{field} = (fun payload -> let msg = Codec.decode_{in_codec}_bytes payload in ignore msg; print_endline \"event {wire}\")",
                wire = wire_op_name(&op.name)
            ));
        } else {
            match op.direction {
                CsilServiceDirection::Unidirectional => fields.push(format!(
                    "{field} = (fun _payload -> Services.transport_error ~status:0L ~message:\"unhandled in this events demo\")"
                )),
                CsilServiceDirection::Bidirectional => {
                    fields.push(format!("{field} = (fun _payload -> ())"))
                }
                CsilServiceDirection::Reverse => {}
            }
        }
    }
    let _ = spec;
    format!("{{ {} }}", fields.join("; "))
}

// ---------------------------------------------------------------------------
// Identifier mapping
// ---------------------------------------------------------------------------

/// OCaml keywords that cannot be used as a value/type identifier; a clashing CSIL
/// name gets a trailing `_` so the emitted source stays valid.
const OCAML_KEYWORDS: &[&str] = &[
    "and",
    "as",
    "assert",
    "asr",
    "begin",
    "class",
    "constraint",
    "do",
    "done",
    "downto",
    "else",
    "end",
    "exception",
    "external",
    "false",
    "for",
    "fun",
    "function",
    "functor",
    "if",
    "in",
    "include",
    "inherit",
    "initializer",
    "land",
    "lazy",
    "let",
    "lor",
    "lsl",
    "lsr",
    "lxor",
    "match",
    "method",
    "mod",
    "module",
    "mutable",
    "new",
    "nonrec",
    "object",
    "of",
    "open",
    "or",
    "private",
    "rec",
    "sig",
    "struct",
    "then",
    "to",
    "true",
    "try",
    "type",
    "val",
    "virtual",
    "when",
    "while",
    "with",
];

fn is_keyword(s: &str) -> bool {
    OCAML_KEYWORDS.contains(&s)
}

/// A lowercase OCaml value / record-label / type identifier. CSIL field names are
/// snake_case already, kebab operation names become snake_case, and a name that
/// collides with a keyword or starts with a digit is made legal.
fn ocaml_ident(name: &str) -> String {
    let snake = name.to_case(Case::Snake);
    let snake = if snake.is_empty() {
        "field".to_string()
    } else {
        snake
    };
    let first = snake.chars().next().unwrap();
    let snake = if first.is_ascii_digit() {
        format!("v_{snake}")
    } else {
        snake
    };
    if is_keyword(&snake) {
        format!("{snake}_")
    } else {
        snake
    }
}

/// A type name (lowercase snake_case): OCaml type names are lowercase, so a
/// PascalCase CSIL type maps to snake_case, e.g. `DepositClaimRequest` →
/// `deposit_claim_request`.
fn ocaml_type_name(name: &str) -> String {
    ocaml_ident(name)
}

/// A Capitalized OCaml variant constructor, e.g. `not-found` → `Not_found`,
/// `DepositClaimResponse` → `Deposit_claim_response`. A leading digit (a literal
/// like `"404"` used as an enum member) is prefixed so the constructor is legal.
fn ocaml_ctor_name(name: &str) -> String {
    let snake = name.to_case(Case::Snake);
    let snake = if snake.is_empty() {
        "arm".to_string()
    } else {
        snake
    };
    let snake = if snake.chars().next().unwrap().is_ascii_digit() {
        format!("v_{snake}")
    } else {
        snake
    };
    capitalize(&snake)
}

/// A Capitalized OCaml module name for a service, e.g. `attestation-service` →
/// `Attestation_service`.
fn ocaml_module_name(name: &str) -> String {
    capitalize(&name.to_case(Case::Snake))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// The wire `service` string: strip a trailing `Service` suffix and **lowercase**,
/// per `docs/cbor-wire-contract.md` (`CorndogsService` → `"corndogs"`), so an OCaml
/// client reaches the same endpoint as the Go/Python/Rust peers.
fn wire_service_name(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
        .to_lowercase()
}

/// The wire `op` string: the operation name PascalCased with the simple rule
/// (capitalize after `_`/`-`, leave the rest), matching the other generators
/// (`submit-task` → `"SubmitTask"`).
fn wire_op_name(name: &str) -> String {
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

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// The OCaml type for a CSIL type expression. CSIL integers map to `int64` to
/// dodge OCaml's 63-bit native `int` (a `u64`/large wire-id would silently lose
/// its high bit as a native `int`).
fn map_type(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => "int64".to_string(),
            "float" => "float".to_string(),
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`.
            "text" | "tstr" => "string".to_string(),
            "bytes" | "bstr" => "bytes".to_string(),
            "bool" => "bool".to_string(),
            // CBOR tag 0 RFC3339 text and CBOR tag 4 exact decimal: OCaml's stdlib
            // has neither a datetime nor a decimal type, so both are carried as
            // their exact wire text and documented at the field.
            "timestamp" => "string".to_string(),
            "decimal" => "string".to_string(),
            "nil" | "null" => "unit".to_string(),
            "any" => "Csilgen_transport.Cbor.t".to_string(),
            other => ocaml_type_name(other),
        },
        CsilTypeExpression::Reference(name) => ocaml_type_name(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("{} list", map_type(element_type))
        }
        // OCaml maps are association lists in the generated surface so the codec
        // can preserve canonical key order without a comparator functor.
        CsilTypeExpression::Map { key, value, .. } => {
            format!("({} * {}) list", map_type(key), map_type(value))
        }
        CsilTypeExpression::Tuple(group) => {
            if group.entries.is_empty() {
                "unit".to_string()
            } else {
                let parts: Vec<String> = group
                    .entries
                    .iter()
                    .map(|e| map_type(&e.value_type))
                    .collect();
                format!("({})", parts.join(" * "))
            }
        }
        CsilTypeExpression::Constrained { base_type, .. } => map_type(base_type),
        // A choice that is not a named rule collapses to the opaque CBOR value; a
        // named choice gets its own variant type via `generate_type_choice`.
        CsilTypeExpression::Choice(_) => "Csilgen_transport.Cbor.t".to_string(),
        _ => "Csilgen_transport.Cbor.t".to_string(),
    }
}

/// The OCaml type for a field, wrapping optionals as `t option`. No extra parens
/// are needed: the postfix `list`/`option`/`t` constructors are left-associative
/// (`string list option` already means `(string list) option`), and the only
/// compound bases `map_type` can produce — tuples and map assoc-lists — are
/// already parenthesized at their source.
fn map_field_type(entry: &CsilGroupEntry) -> String {
    let base = map_type(&entry.value_type);
    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
        format!("{base} option")
    } else {
        base
    }
}

/// The OCaml record label for a group entry, or `None` for a keyless / typed-key
/// entry that has no stable field name.
fn entry_field_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(ocaml_ident(name)),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => Some(ocaml_ident(name)),
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(ocaml_ident(name))
            }
            _ => None,
        },
    }
}

/// A constructor name and optional payload type for one arm of a (named) type
/// choice. A text literal becomes a nullary constructor named after the literal
/// (the string-enum case); a reference/builtin carries its mapped type.
fn choice_ctor(type_expr: &CsilTypeExpression) -> (String, Option<String>) {
    match type_expr {
        CsilTypeExpression::Reference(name) => (ocaml_ctor_name(name), Some(ocaml_type_name(name))),
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "nil" | "null" => (capitalize(&ocaml_ident(name)), None),
            _ => (ocaml_ctor_name(name), Some(map_type(type_expr))),
        },
        CsilTypeExpression::Literal(CsilLiteralValue::Text(text)) => (ocaml_ctor_name(text), None),
        _ => ("Other".to_string(), Some(map_type(type_expr))),
    }
}

/// Disambiguate a constructor name against the ones already emitted in this
/// variant (two literals can snake to the same identifier, and the opaque
/// fallback is always `Other`). The first use keeps the bare name; later ones get
/// a numeric suffix so the variant stays a set of distinct constructors.
fn unique_ctor(seen: &mut Vec<String>, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while seen.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    seen.push(candidate.clone());
    candidate
}

// ---------------------------------------------------------------------------
// Types file
// ---------------------------------------------------------------------------

/// Build `types.ml` and `types.mli` together so the implementation and its
/// interface always agree on the same declarations (the `.mli` is the
/// abstraction boundary the research mandates).
fn generate_types(spec: &CsilSpecSerialized) -> (String, String) {
    let header = "(* Code generated by csilgen; DO NOT EDIT. *)\n\n";
    let mut decls: Vec<String> = Vec::new();

    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::TypeDef(type_expr) => match type_expr {
                CsilTypeExpression::Group(group) => {
                    decls.push(generate_record(&rule.name, group));
                }
                CsilTypeExpression::Choice(choices) => {
                    decls.push(generate_type_choice(&rule.name, choices));
                }
                _ => {
                    decls.push(format!(
                        "type {} = {}",
                        ocaml_type_name(&rule.name),
                        map_type(type_expr)
                    ));
                }
            },
            CsilRuleType::GroupDef(group) => {
                decls.push(generate_record(&rule.name, group));
            }
            CsilRuleType::TypeChoice(choices) => {
                decls.push(generate_type_choice(&rule.name, choices));
            }
            CsilRuleType::GroupChoice(groups) => {
                decls.push(generate_group_choice(&rule.name, groups));
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    if decls.is_empty() {
        return (String::new(), String::new());
    }

    // OCaml record labels share one module-level namespace, so distinct request /
    // response records that each legitimately carry e.g. a `username` field would
    // trip warning 30 (and fail under dune's warnings-as-errors). Field uses are
    // type-directed-disambiguated, so the collision is harmless here; silence the
    // warning for the generated type surface rather than mangling field names.
    let preamble = format!("{header}[@@@warning \"-30\"]\n\n");

    // Successive type declarations after the first are joined with `and` so they
    // form one mutually-recursive `type` group — generated records reference each
    // other regardless of source order, so a non-recursive `type` would fail to
    // resolve a forward reference.
    let joined = join_type_decls(&decls);
    let ml = format!("{preamble}{joined}\n");
    // The interface re-exposes exactly the same declarations; with no functions to
    // hide, the `.mli` simply publishes the type surface.
    let mli = format!("{preamble}{joined}\n");
    (ml, mli)
}

/// Join `type ...` declarations into a single `type ... and ... and ...` group.
/// The formatter packs adjacent single-line clauses with no blank between them and
/// surrounds a multi-line (record) clause with a blank line; matching that keeps
/// the generated `types.ml` stable under `ocamlformat`.
fn join_type_decls(decls: &[String]) -> String {
    let mut out = String::new();
    for (i, decl) in decls.iter().enumerate() {
        if i > 0 {
            let either_multiline = decls[i - 1].contains('\n') || decl.contains('\n');
            out.push_str(if either_multiline { "\n\n" } else { "\n" });
            // Each subsequent decl already begins with `type `; swap it for `and `.
            let body = decl.strip_prefix("type ").unwrap_or(decl);
            out.push_str("and ");
            out.push_str(body);
        } else {
            out.push_str(decl);
        }
    }
    out
}

fn generate_record(name: &str, group: &CsilGroupExpression) -> String {
    let type_name = ocaml_type_name(name);
    if group.entries.is_empty() {
        // An empty group has no fields; OCaml has no empty record, so it is a unit
        // alias which still round-trips as an empty CBOR map.
        return format!("type {type_name} = unit");
    }

    // Single-line fast path: the formatter collapses a comment-free record that
    // fits the 80-column margin onto one line, so emit that shape directly to keep
    // the generated source format-stable. `collect()` yields `None` if any entry
    // is skipped (no field name) or carries a doc comment — either forces the
    // multi-line form below.
    let simple: Option<Vec<String>> = group
        .entries
        .iter()
        .map(|entry| match entry_field_name(entry) {
            Some(field) if field_doc(entry).is_empty() => {
                Some(format!("{field} : {}", map_field_type(entry)))
            }
            _ => None,
        })
        .collect();
    if let Some(parts) = simple {
        let single = format!("type {type_name} = {{ {} }}", parts.join("; "));
        if single.len() <= MARGIN {
            return single;
        }
    }

    let mut lines = vec![format!("type {type_name} = {{")];
    for entry in &group.entries {
        let Some(field) = entry_field_name(entry) else {
            lines.push("  (* group-spread entry skipped (no field name) *)".to_string());
            continue;
        };
        for doc in field_doc(entry) {
            lines.push(format!("  (* {doc} *)"));
        }
        lines.push(format!("  {field} : {};", map_field_type(entry)));
    }
    lines.push("}".to_string());
    lines.join("\n")
}

/// The formatter's line-width margin; a declaration whose one-line form fits is
/// emitted on a single line so `ocamlformat` leaves the generated source untouched.
const MARGIN: usize = 80;

/// Render a variant type as a single line when its arms fit the margin (the
/// formatter's choice for a short sum), else one `| Arm` per line.
fn render_variant(type_name: &str, arms: &[String]) -> String {
    let single = format!("type {type_name} = {}", arms.join(" | "));
    if single.len() <= MARGIN {
        return single;
    }
    let mut lines = vec![format!("type {type_name} =")];
    for arm in arms {
        lines.push(format!("  | {arm}"));
    }
    lines.join("\n")
}

fn generate_type_choice(name: &str, choices: &[CsilTypeExpression]) -> String {
    let type_name = ocaml_type_name(name);

    // A CSIL string enum — every arm a text literal, optionally led by a single
    // `text`/`tstr` base — is the idiomatic OCaml variant: one nullary
    // Capitalized constructor per literal. A leading base means any string is
    // valid on the wire, so an extra `Other of string` arm keeps an unknown value
    // round-trippable rather than collapsing the whole type to an opaque blob.
    let literals: Vec<&str> = choices
        .iter()
        .filter_map(|c| match c {
            CsilTypeExpression::Literal(CsilLiteralValue::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let non_literals: Vec<&CsilTypeExpression> = choices
        .iter()
        .filter(|c| !matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Text(_))))
        .collect();
    let open_base = matches!(
        non_literals.as_slice(),
        [CsilTypeExpression::Builtin(n)] if n == "text" || n == "tstr"
    );
    if !literals.is_empty() && (non_literals.is_empty() || open_base) {
        let mut seen: Vec<String> = Vec::new();
        let mut arms: Vec<String> = Vec::new();
        for lit in &literals {
            arms.push(unique_ctor(&mut seen, &ocaml_ctor_name(lit)));
        }
        if open_base {
            arms.push(format!("{} of string", unique_ctor(&mut seen, "Other")));
        }
        return render_variant(&type_name, &arms);
    }

    // Otherwise a union of (named) alternatives: one constructor per arm carrying
    // its mapped payload, with any name clash disambiguated.
    let mut seen: Vec<String> = Vec::new();
    let arms: Vec<String> = choices
        .iter()
        .map(|choice| {
            let (base, payload) = choice_ctor(choice);
            let ctor = unique_ctor(&mut seen, &base);
            match payload {
                Some(ty) => format!("{ctor} of {ty}"),
                None => ctor,
            }
        })
        .collect();
    render_variant(&type_name, &arms)
}

/// A group choice (`A // B`) becomes a variant whose arms each carry an inline
/// record-shaped tuple; here each alternative is exposed as a constructor wrapping
/// a reference to its own generated record is not possible (the groups are
/// anonymous), so each arm carries the opaque CBOR value with a documented index.
fn generate_group_choice(name: &str, groups: &[CsilGroupExpression]) -> String {
    let type_name = ocaml_type_name(name);
    let arms: Vec<String> = (1..=groups.len())
        .map(|n| format!("Variant_{n} of Csilgen_transport.Cbor.t"))
        .collect();
    render_variant(&type_name, &arms)
}

/// Field documentation: the `@description` text and a wire-form note for the
/// core types whose OCaml mapping is lossy text (`timestamp`/`decimal`).
fn field_doc(entry: &CsilGroupEntry) -> Vec<String> {
    let mut out = Vec::new();
    for meta in &entry.metadata {
        if let csilgen_common::CsilFieldMetadata::Description(desc) = meta {
            out.push(desc.clone());
        }
    }
    if type_uses_builtin(&entry.value_type, "timestamp") {
        out.push("wire: CBOR tag 0 RFC3339 UTC timestamp text".to_string());
    }
    if type_uses_builtin(&entry.value_type, "decimal") {
        out.push("wire: CBOR tag 4 exact decimal text".to_string());
    }
    out
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

// ---------------------------------------------------------------------------
// Codec (codec.ml)
// ---------------------------------------------------------------------------

/// The CBOR encoding of a text key (major type 3 head + bytes); comparing these
/// byte vectors lexicographically is RFC 8949 §4.2.1 key ordering, computed once at
/// generation time so the emitted map is canonical without a runtime sort.
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

/// One codec field: its OCaml record label, the verbatim CBOR wire key, its value
/// type, and whether it is optional.
struct CodecField<'a> {
    label: String,
    wire: String,
    key_bytes: Vec<u8>,
    value_type: &'a CsilTypeExpression,
    optional: bool,
}

/// The verbatim wire key for an entry (the raw bare/text-literal name), or `None`
/// for a keyless/typed-key entry — kept in lockstep with `entry_field_name`.
fn entry_wire_key(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(name.clone()),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => Some(name.clone()),
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(name.clone())
            }
            _ => None,
        },
    }
}

fn codec_fields(group: &CsilGroupExpression) -> Vec<CodecField<'_>> {
    let mut fields: Vec<CodecField> = group
        .entries
        .iter()
        .filter_map(|entry| {
            let label = entry_field_name(entry)?;
            let wire = entry_wire_key(entry)?;
            Some(CodecField {
                key_bytes: cbor_text_key_bytes(&wire),
                label,
                wire,
                value_type: &entry.value_type,
                optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
            })
        })
        .collect();
    fields.sort_by(|a, b| a.key_bytes.cmp(&b.key_bytes));
    fields
}

/// The record rules that get a generated codec, keyed by OCaml type name. Only
/// records (a CBOR map) are covered; a field referencing a non-record type emits a
/// `failwith` placeholder so the codec still type-checks.
fn codec_record_names(spec: &CsilSpecSerialized) -> std::collections::HashSet<String> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(ocaml_type_name(&r.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(ocaml_type_name(&r.name)),
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through, keyed by OCaml type
/// name: a `TypeDef` whose target is a map / array / scalar / reference / tuple
/// (NOT a record group or a choice, which have their own handling). A field
/// referencing one has no codec of its own, so it must encode/decode as the
/// underlying type rather than the `failwith` stub a bare non-record reference
/// yields — otherwise a `StringInt64Map = {* text => int}` field silently drops its
/// data. The named alias is a transparent abbreviation (`type string_int64_map =
/// (string * int64) list`), so the underlying map/array/scalar encoder operates on
/// the very same value the field already holds.
fn codec_aliases(
    spec: &CsilSpecSerialized,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((ocaml_type_name(&r.name), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// An OCaml expression encoding `expr` (a value of OCaml type for `ty`) to a
/// `Cbor.t`. Unsupported shapes raise at runtime via `failwith` (and never on the
/// corndogs/round-trip path), keeping the generated module well-typed.
fn enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            enc_value(base_type, expr, records, aliases)
        }
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => format!("(Cbor.int64 {expr})"),
            "float" => format!("(Cbor.Float {expr})"),
            "text" | "tstr" => format!("(Cbor.Text {expr})"),
            "bytes" | "bstr" => format!("(Cbor.Bytes {expr})"),
            "bool" => format!("(Cbor.Bool {expr})"),
            "timestamp" => format!("(Cbor.Tag (0, Cbor.Text {expr}))"),
            "nil" | "null" => format!("(ignore {expr}; Cbor.Null)"),
            other => format!("(failwith \"csilgen: no codec for builtin {other}\")"),
        },
        CsilTypeExpression::Reference(name) => {
            let tn = ocaml_type_name(name);
            if records.contains(&tn) {
                format!("(encode_{tn} {expr})")
            } else if let Some(underlying) = aliases.get(&tn) {
                // A transparent alias has no codec of its own; encode it as its
                // underlying type. The named OCaml abbreviation is structurally the
                // underlying type, so the field's value flows through unchanged.
                enc_value(underlying, expr, records, aliases)
            } else {
                format!("(failwith \"csilgen: no codec for type {tn}\")")
            }
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = enc_value(element_type, "csil_e", records, aliases);
            format!("(Cbor.Array (List.map (fun csil_e -> {inner}) {expr}))")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let ek = enc_value(key, "csil_k", records, aliases);
            let ev = enc_value(value, "csil_v", records, aliases);
            format!("(Cbor.Map (List.map (fun (csil_k, csil_v) -> ({ek}, {ev})) {expr}))")
        }
        _ => "(failwith \"csilgen: no codec for this field shape\")".to_string(),
    }
}

/// An OCaml expression decoding `expr` (a `Cbor.t`) into a value of the OCaml type
/// for `ty`.
fn dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            dec_value(base_type, expr, records, aliases)
        }
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => format!("(Cbor.to_i64 {expr})"),
            "float" => format!("(Cbor.to_float {expr})"),
            "text" | "tstr" => format!("(Cbor.to_text {expr})"),
            "bytes" | "bstr" => format!("(Cbor.to_bytes {expr})"),
            "bool" => format!("(Cbor.to_bool {expr})"),
            "timestamp" => format!(
                "(match {expr} with Cbor.Tag (0, Cbor.Text csil_s) -> csil_s | _ -> failwith \"csilgen: bad timestamp\")"
            ),
            "nil" | "null" => format!("(ignore {expr}; ())"),
            other => format!("(failwith \"csilgen: no codec for builtin {other}\")"),
        },
        CsilTypeExpression::Reference(name) => {
            let tn = ocaml_type_name(name);
            if records.contains(&tn) {
                format!("(decode_{tn} {expr})")
            } else if let Some(underlying) = aliases.get(&tn) {
                // A transparent alias decodes as its underlying type; the value the
                // map/array/scalar decoder returns is the named abbreviation's value.
                dec_value(underlying, expr, records, aliases)
            } else {
                format!("(failwith \"csilgen: no codec for type {tn}\")")
            }
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = dec_value(element_type, "csil_e", records, aliases);
            format!(
                "(match {expr} with Cbor.Array csil_xs -> List.map (fun csil_e -> {inner}) csil_xs | _ -> failwith \"csilgen: expected array\")"
            )
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let dk = dec_value(key, "csil_k", records, aliases);
            let dv = dec_value(value, "csil_v", records, aliases);
            format!(
                "(match {expr} with Cbor.Map csil_kvs -> List.map (fun (csil_k, csil_v) -> ({dk}, {dv})) csil_kvs | _ -> failwith \"csilgen: expected map\")"
            )
        }
        _ => "(failwith \"csilgen: no codec for this field shape\")".to_string(),
    }
}

/// Emit `encode_<tn>` / `decode_<tn>` clause bodies for one record (joined into the
/// mutually-recursive `let rec … and …` groups by the caller).
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> (String, String) {
    let tn = ocaml_type_name(name);
    if group.entries.is_empty() {
        // The empty group is a `unit` alias; it round-trips as an empty CBOR map.
        let enc = format!("encode_{tn} (_v : {tn}) : Cbor.t = Cbor.Map []");
        let dec = format!(
            "decode_{tn} (csil_c : Cbor.t) : {tn} =\n  match csil_c with Cbor.Map _ -> () | _ -> failwith \"csilgen: expected map for {tn}\""
        );
        return (enc, dec);
    }
    let fields = codec_fields(group);

    let mut enc = String::new();
    enc.push_str(&format!("encode_{tn} (v : {tn}) : Cbor.t =\n"));
    enc.push_str("  Cbor.Map\n    (List.filter_map\n       (fun x -> x)\n       [\n");
    for f in &fields {
        if f.optional {
            let inner = enc_value(f.value_type, "csil_x", records, aliases);
            enc.push_str(&format!(
                "         (match v.{} with Some csil_x -> Some (Cbor.Text \"{}\", {inner}) | None -> None);\n",
                f.label, f.wire
            ));
        } else {
            let inner = enc_value(f.value_type, &format!("v.{}", f.label), records, aliases);
            enc.push_str(&format!(
                "         Some (Cbor.Text \"{}\", {inner});\n",
                f.wire
            ));
        }
    }
    enc.push_str("       ])");

    let mut dec = String::new();
    dec.push_str(&format!("decode_{tn} (csil_c : Cbor.t) : {tn} =\n"));
    dec.push_str("  match csil_c with\n  | Cbor.Map csil_kvs ->\n");
    dec.push_str("      let csil_field k = List.assoc_opt (Cbor.Text k) csil_kvs in\n");
    dec.push_str("      let csil_req k =\n        match csil_field k with Some v -> v | None -> failwith (\"csilgen: missing field \" ^ k)\n      in\n");
    dec.push_str("      ignore csil_req;\n");
    dec.push_str("      {\n");
    for f in &fields {
        if f.optional {
            let inner = dec_value(f.value_type, "csil_v", records, aliases);
            dec.push_str(&format!(
                "        {} = (match csil_field \"{}\" with Some csil_v -> Some {inner} | None -> None);\n",
                f.label, f.wire
            ));
        } else {
            let inner = dec_value(
                f.value_type,
                &format!("(csil_req \"{}\")", f.wire),
                records,
                aliases,
            );
            dec.push_str(&format!("        {} = {inner};\n", f.label));
        }
    }
    dec.push_str("      }\n");
    dec.push_str(&format!(
        "  | _ -> failwith \"csilgen: expected map for {tn}\""
    ));

    (enc, dec)
}

/// Build `codec.ml`: a self-contained canonical-CBOR module plus per-record
/// `encode_<t>`/`decode_<t>` and the `encode_<t>_bytes`/`decode_<t>_bytes` wrappers
/// the typed client calls. `None` when the spec declares no record types.
fn generate_codec(spec: &CsilSpecSerialized) -> Option<String> {
    let records = codec_record_names(spec);
    if records.is_empty() {
        return None;
    }
    let aliases = codec_aliases(spec);
    let mut enc_clauses: Vec<String> = Vec::new();
    let mut dec_clauses: Vec<String> = Vec::new();
    let mut wrappers = String::new();
    for rule in &spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            let (enc, dec) = emit_record_codec(&rule.name, group, &records, &aliases);
            enc_clauses.push(enc);
            dec_clauses.push(dec);
            let tn = ocaml_type_name(&rule.name);
            wrappers.push_str(&format!(
                "let encode_{tn}_bytes (v : {tn}) : bytes = Cbor.encode (encode_{tn} v)\n"
            ));
            wrappers.push_str(&format!(
                "let decode_{tn}_bytes (b : bytes) : {tn} =\n  match Cbor.decode b with Ok c -> decode_{tn} c | Error e -> failwith e\n\n"
            ));
        }
    }

    let mut out = String::new();
    out.push_str("(* Generated CBOR (de)serializers for the CSIL value types. *)\n");
    out.push_str("(* Code generated by csilgen; DO NOT EDIT. *)\n\n");
    // Distinct request/response records may share a label (e.g. `queue`); the uses
    // here are type-directed-disambiguated, so silence warning 30 as `types.ml` does.
    out.push_str("[@@@warning \"-30\"]\n\n");
    out.push_str("open Types\n");
    out.push_str(CODEC_RUNTIME_OCAML);
    out.push('\n');
    out.push_str("let rec ");
    out.push_str(&enc_clauses.join("\n\nand "));
    out.push_str("\n\n");
    out.push_str("let rec ");
    out.push_str(&dec_clauses.join("\n\nand "));
    out.push_str("\n\n");
    out.push_str(&wrappers);
    Some(format!("{}\n", out.trim_end()))
}

// ---------------------------------------------------------------------------
// Operation helpers
// ---------------------------------------------------------------------------

/// A push op (`-> Event`) carries a `null` input type: there is no request to
/// send, so the generated call/handler takes no request value.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Whether a choice arm is the conventional error half of a `Response / …Error`
/// union — the canonical `ServiceError` or any reference whose name ends in
/// `Error`/`_error`. The success type drops it so the client's typed reply is the
/// response record, not an opaque union; the error is surfaced via the transport.
fn is_error_arm(type_expr: &CsilTypeExpression) -> bool {
    matches!(
        type_expr,
        CsilTypeExpression::Reference(name)
            if name == "ServiceError" || name.ends_with("Error") || name.ends_with("_error")
    )
}

/// Reduce an operation output to its success type by dropping a top-level error
/// arm — the error half is surfaced as the `result`'s `Error`.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !is_error_arm(c))
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

/// The builtins that map to an OCaml primitive (or the opaque CBOR value), i.e.
/// everything that does NOT resolve to a generated `Types` declaration.
fn is_primitive_builtin(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "uint"
            | "float"
            | "text"
            | "tstr"
            | "bytes"
            | "bstr"
            | "bool"
            | "timestamp"
            | "decimal"
            | "nil"
            | "null"
            | "any"
    )
}

/// Whether a type expression resolves (anywhere within it) to a name declared in
/// the generated `Types` module — a reference, or a non-primitive builtin.
fn expr_uses_named_type(type_expr: &CsilTypeExpression) -> bool {
    match type_expr {
        CsilTypeExpression::Reference(_) => true,
        CsilTypeExpression::Builtin(name) => !is_primitive_builtin(name),
        CsilTypeExpression::Array { element_type, .. } => expr_uses_named_type(element_type),
        CsilTypeExpression::Map { key, value, .. } => {
            expr_uses_named_type(key) || expr_uses_named_type(value)
        }
        CsilTypeExpression::Tuple(group) => group
            .entries
            .iter()
            .any(|e| expr_uses_named_type(&e.value_type)),
        CsilTypeExpression::Constrained { base_type, .. } => expr_uses_named_type(base_type),
        CsilTypeExpression::Choice(choices) => choices.iter().any(expr_uses_named_type),
        _ => false,
    }
}

/// Whether the emitted client will reference any `Types` declaration, so the
/// `open Types` is actually used (a unary op's request or its success reply).
fn client_uses_types(spec: &CsilSpecSerialized) -> bool {
    spec.rules.iter().any(|rule| match &rule.rule_type {
        CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
            matches!(op.direction, CsilServiceDirection::Unidirectional)
                && ((!op_input_is_null(&op.input_type) && expr_uses_named_type(&op.input_type))
                    || expr_uses_named_type(&success_type(&op.output_type)))
        }),
        _ => false,
    })
}

// ---------------------------------------------------------------------------
// Client surface
// ---------------------------------------------------------------------------

/// `client.ml`: one module per service exposing typed call functions. A call
/// takes the transport client plus per-operation `encode`/`decode` closures (the
/// codec is the consumer's — the generator never owns serialization) and returns a
/// `result`.
fn generate_client(spec: &CsilSpecSerialized) -> String {
    let records = codec_record_names(spec);
    let mut out = String::new();
    out.push_str("(* Code generated by csilgen; DO NOT EDIT. *)\n\n");
    // `open Types` only when a typed call actually names a generated type — an
    // unused `open` is a hard error under dune's default warnings-as-errors.
    if client_uses_types(spec) {
        out.push_str("open Types\n\n");
    }
    out.push_str(CLIENT_PRELUDE);

    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            out.push_str(&emit_client_module(&rule.name, service, &records));
        }
    }
    // A single trailing newline (the formatter's end-of-file convention).
    format!("{}\n", out.trim_end())
}

const CLIENT_PRELUDE: &str = "\
(* The transport seam is supplied by the consumer: [call] performs the RPC named
   by (service, op) over its carrier and returns the raw reply payload, or an
   error string. The generated client owns serialization (it encodes the typed
   request and decodes the typed reply via [Codec]); the carrier only moves bytes. *)
type client = {
  call : service:string -> op:string -> payload:bytes -> (bytes, string) result;
}

let make_client ~call = { call }

";

/// The `Codec.<fn>_<type>_bytes` base name for an operation input/output type, when
/// it is a reference to a record the codec covers. `None` for a non-record type, for
/// which the client falls back to the consumer-supplied codec closures.
fn op_codec_type(
    type_expr: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
) -> Option<String> {
    if let CsilTypeExpression::Reference(name) = type_expr {
        let tn = ocaml_type_name(name);
        if records.contains(&tn) {
            return Some(tn);
        }
    }
    None
}

fn emit_client_module(
    name: &str,
    service: &CsilServiceDefinition,
    records: &std::collections::HashSet<String>,
) -> String {
    let module = ocaml_module_name(name);
    let wire_service = wire_service_name(name);
    let mut out = String::new();
    out.push_str(&format!("module {module} = struct\n"));
    out.push_str(&emit_wire_ids(service));

    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the server router surface.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            out.push_str(&format!(
                "  (* channel operation {} is not part of the RPC client *)\n",
                op.name
            ));
            continue;
        }
        let fn_name = ocaml_ident(&op.name);
        let out_type = map_type(&success_type(&op.output_type));
        let null_input = op_input_is_null(&op.input_type);
        let resp_codec = op_codec_type(&success_type(&op.output_type), records);
        let req_codec = op_codec_type(&op.input_type, records);

        // The typed seam: when both ends are records the codec covers, the generated
        // method serializes/deserializes itself. Otherwise it keeps the explicit
        // encode/decode closures so an exotic payload stays callable.
        let decode_call = match &resp_codec {
            Some(tn) => format!("Codec.decode_{tn}_bytes payload"),
            None => "decode_response payload".to_string(),
        };
        match (null_input, &req_codec, &resp_codec) {
            (true, _, _) => {
                if resp_codec.is_some() {
                    out.push_str(&format!(
                        "  let {fn_name} (c : client) : ({out_type}, string) result =\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "  let {fn_name} (c : client)\n      ~(decode_response : bytes -> {out_type}) :\n      ({out_type}, string) result =\n"
                    ));
                }
                out.push_str(&format!(
                    "    match c.call ~service:\"{wire_service}\" ~op:\"{}\" ~payload:Bytes.empty with\n",
                    wire_op_name(&op.name)
                ));
            }
            (false, Some(req_tn), _) => {
                let in_type = map_type(&op.input_type);
                if resp_codec.is_some() {
                    out.push_str(&format!(
                        "  let {fn_name} (c : client) (req : {in_type}) : ({out_type}, string) result =\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "  let {fn_name} (c : client)\n      ~(decode_response : bytes -> {out_type})\n      (req : {in_type}) : ({out_type}, string) result =\n"
                    ));
                }
                out.push_str(&format!(
                    "    match c.call ~service:\"{wire_service}\" ~op:\"{}\" ~payload:(Codec.encode_{req_tn}_bytes req) with\n",
                    wire_op_name(&op.name)
                ));
            }
            (false, None, _) => {
                let in_type = map_type(&op.input_type);
                let decode_param = if resp_codec.is_some() {
                    String::new()
                } else {
                    format!("\n      ~(decode_response : bytes -> {out_type})")
                };
                out.push_str(&format!(
                    "  let {fn_name} (c : client)\n      ~(encode_request : {in_type} -> bytes){decode_param}\n      (req : {in_type}) : ({out_type}, string) result =\n"
                ));
                out.push_str(&format!(
                    "    match c.call ~service:\"{wire_service}\" ~op:\"{}\" ~payload:(encode_request req) with\n",
                    wire_op_name(&op.name)
                ));
            }
        }
        out.push_str(&format!("    | Ok payload -> Ok ({decode_call})\n"));
        out.push_str("    | Error _ as e -> e\n\n");
    }

    // No blank line directly before `end` (the formatter's struct convention).
    format!("{}\nend\n\n", out.trim_end())
}

// ---------------------------------------------------------------------------
// Server surface
// ---------------------------------------------------------------------------

/// `services.ml`: one module per service with a handler record (one function per
/// operation), a verbose router (dispatch by op name) and — for wire-id-bearing
/// services — a compact router (dispatch by `@wire-id` ordinal).
fn generate_services(spec: &CsilSpecSerialized) -> String {
    let mut out = String::new();
    out.push_str("(* Code generated by csilgen; DO NOT EDIT. *)\n\n");
    // The server surface is codec-agnostic: handlers receive opaque payload bytes
    // and decode them with the consumer's codec, so it never names a [Types]
    // declaration (an `open Types` here would be an unused-open warning).
    out.push_str(SERVER_PRELUDE);

    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            out.push_str(&emit_service_module(&rule.name, service));
        }
    }
    // A single trailing newline (the formatter's end-of-file convention).
    format!("{}\n", out.trim_end())
}

const SERVER_PRELUDE: &str = "\
(* A handler returns either a typed reply (the [variant] names the chosen output
   arm and [payload] is its encoded bytes) or a transport-level failure. *)
type outcome =
  | Reply of { variant : string; payload : bytes }
  | Transport_error of { status : int64; message : string }

let reply ~variant ~payload = Reply { variant; payload }
let transport_error ~status ~message = Transport_error { status; message }

";

fn emit_service_module(name: &str, service: &CsilServiceDefinition) -> String {
    let module = ocaml_module_name(name);
    let mut out = String::new();
    out.push_str(&format!("module {module} = struct\n"));
    out.push_str(&emit_wire_ids(service));

    // Handler record: one field per inbound operation. The field receives the
    // opaque request payload bytes; the handler decodes them with the generated
    // [Types] (the codec is the consumer's, never the generator's). A unary op
    // returns an [outcome]; a fire-and-forget channel op returns [unit]; a
    // push-only (reverse) op has no inbound handler. The typed request shape for
    // each op is documented alongside its field.
    out.push_str("  type handler = {\n");
    for op in &service.operations {
        if matches!(op.direction, CsilServiceDirection::Reverse) {
            continue;
        }
        let field = ocaml_ident(&op.name);
        let in_ty = if op_input_is_null(&op.input_type) {
            "unit".to_string()
        } else {
            map_type(&op.input_type)
        };
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                out.push_str(&format!(
                    "    (* request payload decodes to: {in_ty} *)\n    {field} : bytes -> outcome;\n"
                ));
            }
            CsilServiceDirection::Bidirectional => {
                out.push_str(&format!(
                    "    (* channel message decodes to: {in_ty} *)\n    {field} : bytes -> unit;\n"
                ));
            }
            CsilServiceDirection::Reverse => {}
        }
    }
    out.push_str("  }\n\n");

    out.push_str(&emit_router_verbose(service));
    if service.wire_id.is_some() {
        out.push_str(&emit_router_compact(service));
    }

    // No blank line directly before `end` (the formatter's struct convention).
    format!("{}\nend\n\n", out.trim_end())
}

/// Emit `@wire-id` ordinal bindings (as `int64`, the safe width for a possibly
/// large ordinal). Emits nothing for a wire-id-free service so its output stays
/// byte-identical.
fn emit_wire_ids(service: &CsilServiceDefinition) -> String {
    let Some(service_id) = service.wire_id else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str("  (* Wire-id ordinals (transport compact profiles). *)\n");
    out.push_str(&format!("  let service_wire_id = {service_id}L\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            out.push_str(&format!(
                "  let op_{}_wire_id = {op_id}L\n",
                ocaml_ident(&op.name)
            ));
        }
    }
    out.push('\n');
    out
}

/// The verbose router dispatches one decoded request by its operation name to the
/// matching handler field. Request/reply decoding is the consumer's codec, passed
/// as `decode_<op>` closures so the router stays codec-agnostic.
fn emit_router_verbose(service: &CsilServiceDefinition) -> String {
    let mut out = String::new();
    out.push_str(
        "  (* Verbose router: dispatch one request by operation name to its handler\n     field, which decodes the opaque payload with the generated [Types]. *)\n",
    );
    // The return type is inferred as [outcome]; an explicit annotation would
    // overflow the formatter margin and read as Rust/Go ceremony, not OCaml.
    out.push_str("  let route (h : handler) ~(op : string) ~(payload : bytes) =\n");
    out.push_str("    match op with\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            continue;
        }
        let field = ocaml_ident(&op.name);
        if op_input_is_null(&op.input_type) {
            out.push_str(&format!(
                "    | \"{}\" -> ignore payload; h.{field} Bytes.empty\n",
                wire_op_name(&op.name)
            ));
        } else {
            out.push_str(&format!(
                "    | \"{}\" -> h.{field} payload\n",
                wire_op_name(&op.name)
            ));
        }
    }
    out.push_str("    | other -> transport_error ~status:2L ~message:(\"unknown op: \" ^ other)\n");
    out.push('\n');
    out
}

/// The compact-profile router: dispatch by the operation's `@wire-id` ordinal
/// (`int64`) rather than its name. Emitted only for wire-id-bearing services.
fn emit_router_compact(service: &CsilServiceDefinition) -> String {
    let mut out = String::new();
    out.push_str(
        "  (* Compact router: dispatch one request by its @wire-id ordinal. The\n     verbose twin is [route]; the host calls whichever the negotiated profile\n     selected. *)\n",
    );
    out.push_str("  let route_compact (h : handler) ~(op_ord : int64) ~(payload : bytes) =\n");
    out.push_str("    match op_ord with\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            continue;
        }
        let Some(op_id) = op.wire_id else {
            continue;
        };
        let field = ocaml_ident(&op.name);
        if op_input_is_null(&op.input_type) {
            out.push_str(&format!(
                "    | {op_id}L -> ignore payload; h.{field} Bytes.empty\n"
            ));
        } else {
            out.push_str(&format!("    | {op_id}L -> h.{field} payload\n"));
        }
    }
    // Pre-wrapped to the formatter's layout (the one-line form exceeds the margin).
    out.push_str(
        "    | other ->\n        transport_error ~status:2L\n          ~message:(Printf.sprintf \"unknown op ordinal: %Ld\" other)\n",
    );
    out.push('\n');
    out
}

/// The self-contained canonical-CBOR module the generated codecs build on. Its
/// `Cbor.t` carries the bool/float/null items a payload may hold (the transport's
/// envelope codec does not), so the generated output stays standalone.
const CODEC_RUNTIME_OCAML: &str = r#"
module Cbor = struct
  type t =
    | Uint of int64
    | Nint of int64 (* the logical (negative) value; encodes as CBOR major type 1 *)
    | Bool of bool
    | Float of float
    | Null
    | Text of string
    | Bytes of bytes
    | Array of t list
    | Map of (t * t) list
    | Tag of int * t

  let int64 (n : int64) : t = if Int64.compare n 0L >= 0 then Uint n else Nint n

  let add_head buf major (u : int64) =
    let mt = major lsl 5 in
    let byte shift = Char.chr (Int64.to_int (Int64.logand (Int64.shift_right_logical u shift) 0xffL)) in
    if Int64.unsigned_compare u 24L < 0 then
      Buffer.add_char buf (Char.chr (mt lor Int64.to_int u))
    else if Int64.unsigned_compare u 0x100L < 0 then begin
      Buffer.add_char buf (Char.chr (mt lor 24));
      Buffer.add_char buf (byte 0)
    end
    else if Int64.unsigned_compare u 0x10000L < 0 then begin
      Buffer.add_char buf (Char.chr (mt lor 25));
      Buffer.add_char buf (byte 8);
      Buffer.add_char buf (byte 0)
    end
    else if Int64.unsigned_compare u 0x100000000L < 0 then begin
      Buffer.add_char buf (Char.chr (mt lor 26));
      for i = 3 downto 0 do Buffer.add_char buf (byte (i * 8)) done
    end
    else begin
      Buffer.add_char buf (Char.chr (mt lor 27));
      for i = 7 downto 0 do Buffer.add_char buf (byte (i * 8)) done
    end

  let rec enc buf = function
    | Uint n -> add_head buf 0 n
    | Nint n -> add_head buf 1 (Int64.sub (Int64.neg n) 1L)
    | Bool b -> Buffer.add_char buf (if b then '\xf5' else '\xf4')
    | Null -> Buffer.add_char buf '\xf6'
    | Float f ->
      Buffer.add_char buf '\xfb';
      let bits = Int64.bits_of_float f in
      for i = 7 downto 0 do
        Buffer.add_char buf (Char.chr (Int64.to_int (Int64.logand (Int64.shift_right_logical bits (i * 8)) 0xffL)))
      done
    | Text s -> add_head buf 3 (Int64.of_int (String.length s)); Buffer.add_string buf s
    | Bytes b -> add_head buf 2 (Int64.of_int (Bytes.length b)); Buffer.add_bytes buf b
    | Array xs -> add_head buf 4 (Int64.of_int (List.length xs)); List.iter (enc buf) xs
    | Map kvs ->
      add_head buf 5 (Int64.of_int (List.length kvs));
      List.iter (fun (k, v) -> enc buf k; enc buf v) kvs
    | Tag (t, v) -> add_head buf 6 (Int64.of_int t); enc buf v

  let encode (v : t) : bytes =
    let buf = Buffer.create 64 in
    enc buf v;
    Buffer.to_bytes buf

  let decode (b : bytes) : (t, string) result =
    let len = Bytes.length b in
    let byte i = Char.code (Bytes.get b i) in
    let read_arg pos low =
      if low < 24 then (Int64.of_int low, pos + 1)
      else if low = 24 then (Int64.of_int (byte (pos + 1)), pos + 2)
      else if low = 25 then (Int64.of_int ((byte (pos + 1) lsl 8) lor byte (pos + 2)), pos + 3)
      else if low = 26 then begin
        let v = ref 0L in
        for i = 1 to 4 do v := Int64.logor (Int64.shift_left !v 8) (Int64.of_int (byte (pos + i))) done;
        (!v, pos + 5)
      end
      else if low = 27 then begin
        let v = ref 0L in
        for i = 1 to 8 do v := Int64.logor (Int64.shift_left !v 8) (Int64.of_int (byte (pos + i))) done;
        (!v, pos + 9)
      end
      else failwith "csilgen: bad head"
    in
    let rec dec pos =
      let ib = byte pos in
      let major = ib lsr 5 and low = ib land 0x1f in
      if major = 7 then
        match low with
        | 20 -> (Bool false, pos + 1)
        | 21 -> (Bool true, pos + 1)
        | 22 | 23 -> (Null, pos + 1)
        | 26 ->
          let arg, p = read_arg pos low in
          (Float (Int32.float_of_bits (Int64.to_int32 arg)), p)
        | 27 ->
          let arg, p = read_arg pos low in
          (Float (Int64.float_of_bits arg), p)
        | _ -> failwith "csilgen: unsupported simple value"
      else begin
        let arg, p = read_arg pos low in
        match major with
        | 0 -> (Uint arg, p)
        | 1 -> (Nint (Int64.sub (Int64.neg arg) 1L), p)
        | 2 -> let n = Int64.to_int arg in (Bytes (Bytes.sub b p n), p + n)
        | 3 -> let n = Int64.to_int arg in (Text (Bytes.sub_string b p n), p + n)
        | 4 ->
          let n = Int64.to_int arg in
          let rec loop i pos acc =
            if i = 0 then (List.rev acc, pos)
            else let v, np = dec pos in loop (i - 1) np (v :: acc)
          in
          let items, np = loop n p [] in
          (Array items, np)
        | 5 ->
          let n = Int64.to_int arg in
          let rec loop i pos acc =
            if i = 0 then (List.rev acc, pos)
            else
              let k, p1 = dec pos in
              let v, p2 = dec p1 in
              loop (i - 1) p2 ((k, v) :: acc)
          in
          let kvs, np = loop n p [] in
          (Map kvs, np)
        | 6 -> let inner, np = dec p in (Tag (Int64.to_int arg, inner), np)
        | _ -> failwith "csilgen: bad major"
      end
    in
    try
      let v, np = dec 0 in
      if np <> len then Error "csilgen: trailing bytes" else Ok v
    with
    | Failure m -> Error m
    | _ -> Error "csilgen: malformed cbor"

  let to_i64 = function Uint n | Nint n -> n | _ -> failwith "csilgen: expected int"
  let to_text = function Text s -> s | _ -> failwith "csilgen: expected text"
  let to_bytes = function Bytes b -> b | _ -> failwith "csilgen: expected bytes"
  let to_bool = function Bool b -> b | _ -> failwith "csilgen: expected bool"
  let to_float = function
    | Float f -> f
    | Uint n | Nint n -> Int64.to_float n
    | _ -> failwith "csilgen: expected float"
end
"#;

#[cfg(test)]
mod tests;
