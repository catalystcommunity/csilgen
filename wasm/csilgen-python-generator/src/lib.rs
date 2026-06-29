//! Python code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target python` from `csilgen_python_generator.wasm`.

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression,
    CsilValidationConstraint, CsilgenError, GeneratedFile, GeneratedFiles, GenerationStats,
    GeneratorCapability, GeneratorConfig, GeneratorMetadata, GeneratorWarning, Result,
    WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::{BTreeSet, HashMap, HashSet};

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "python-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Python code generator".to_string(),
        target: "python".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
    };
    write_json_to_wasm(&metadata) as *const u8
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
        Ok(output) => write_json_to_wasm(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn write_json_to_wasm<T: serde::Serialize>(value: &T) -> *mut u8 {
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

fn process_generation(
    input_ptr: *const u8,
    input_len: usize,
) -> std::result::Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let bytes = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let s = std::str::from_utf8(bytes).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(s).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = generate_python_code_from_serialized(&input.csil_spec, &input.config)
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
        warnings: Vec::<GeneratorWarning>::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

fn csil_literal_to_python_str(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Text(text) => format!("\"{text}\""),
        CsilLiteralValue::Integer(num) => num.to_string(),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Null => "None".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            format!("b\"{}\"", String::from_utf8_lossy(bytes))
        }
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(csil_literal_to_python_str).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

/// Whether an operation input is the empty `null`/`nil` type. A push-only op
/// (`op: <- Event`) carries no request payload, so its client/handler method
/// must take no `req`/`msg` parameter and its router must not decode a body.
fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Resolve the base builtin type name of a (possibly `.`-constrained) field type
/// so a comparison bound can be constructed as the matching Python value.
fn base_builtin_name(type_expr: &CsilTypeExpression) -> Option<&str> {
    match type_expr {
        CsilTypeExpression::Builtin(name) => Some(name.as_str()),
        CsilTypeExpression::Constrained { base_type, .. } => base_builtin_name(base_type),
        _ => None,
    }
}

/// The Python expression a comparison/min-max bound must compare against. A
/// `decimal` field's in-memory value is a `Decimal` and a `timestamp` field's is
/// a tz-aware `datetime`, so a raw `str` bound (an exact decimal text or RFC3339
/// text on the wire) would raise `TypeError` at comparison time. The bound is
/// therefore built as `Decimal(...)` or `datetime.fromisoformat(...)` (with a
/// trailing `Z` normalized to `+00:00` so it parses as tz-aware UTC). Numeric and
/// other field types keep their native literal.
fn python_bound_expr(value: &CsilLiteralValue, value_type: &CsilTypeExpression) -> String {
    let literal = csil_literal_to_python_str(value);
    match base_builtin_name(value_type) {
        // An integer bound on a `decimal` field (the core guarantees only an
        // Integer literal or a well-formed decimal Text here) is rendered through
        // its decimal string so it constructs the same exact value a text bound
        // does — `Decimal("0")`, never the lossy/float-prone `Decimal(0)`.
        Some("decimal") => match value {
            CsilLiteralValue::Integer(n) => format!("Decimal(\"{n}\")"),
            _ => format!("Decimal({literal})"),
        },
        Some("timestamp") => {
            format!("datetime.fromisoformat({literal}.replace(\"Z\", \"+00:00\"))")
        }
        _ => literal,
    }
}

/// Walk a type expression marking which stdlib/typing imports the spec needs:
/// `datetime` for `timestamp`, `decimal` for `decimal`, `re` for any `.regex`
/// operator, and `typing.Tuple` for any fixed-shape `Tuple`. Nested forms
/// (arrays/maps/groups/tuples/choices/`.and`/`.within`) are followed so a
/// `decimal` buried inside `[* decimal]` or a `[text, decimal]` tuple still
/// surfaces the import.
fn scan_special_types(
    type_expr: &CsilTypeExpression,
    needs_datetime: &mut bool,
    needs_decimal: &mut bool,
    needs_re: &mut bool,
    needs_tuple: &mut bool,
) {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "timestamp" => *needs_datetime = true,
            "decimal" => *needs_decimal = true,
            _ => {}
        },
        CsilTypeExpression::Array { element_type, .. } => {
            scan_special_types(
                element_type,
                needs_datetime,
                needs_decimal,
                needs_re,
                needs_tuple,
            );
        }
        CsilTypeExpression::Map { key, value, .. } => {
            scan_special_types(key, needs_datetime, needs_decimal, needs_re, needs_tuple);
            scan_special_types(value, needs_datetime, needs_decimal, needs_re, needs_tuple);
        }
        CsilTypeExpression::Group(group) => {
            for entry in &group.entries {
                scan_special_types(
                    &entry.value_type,
                    needs_datetime,
                    needs_decimal,
                    needs_re,
                    needs_tuple,
                );
            }
        }
        // A `Tuple` renders as `typing.Tuple[...]`, so it both pulls the import
        // and may carry nested special types in its positional entries.
        CsilTypeExpression::Tuple(group) => {
            *needs_tuple = true;
            for entry in &group.entries {
                scan_special_types(
                    &entry.value_type,
                    needs_datetime,
                    needs_decimal,
                    needs_re,
                    needs_tuple,
                );
            }
        }
        CsilTypeExpression::Choice(choices) => {
            for c in choices {
                scan_special_types(c, needs_datetime, needs_decimal, needs_re, needs_tuple);
            }
        }
        CsilTypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            scan_special_types(
                base_type,
                needs_datetime,
                needs_decimal,
                needs_re,
                needs_tuple,
            );
            for op in constraints {
                match op {
                    CsilControlOperator::Regex(_) => *needs_re = true,
                    CsilControlOperator::And(t) | CsilControlOperator::Within(t) => {
                        scan_special_types(t, needs_datetime, needs_decimal, needs_re, needs_tuple);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// The control operators attached to a `.`-constrained field, or an empty slice
/// for any other type. Lets the field/validation emitters honor the `.`-system
/// (Size/Regex/comparisons/Default/…) the same way `@`-annotations are honored.
fn control_operators(type_expr: &CsilTypeExpression) -> &[CsilControlOperator] {
    match type_expr {
        CsilTypeExpression::Constrained { constraints, .. } => constraints,
        _ => &[],
    }
}

/// Whether a dataclass field declaration carries a default value: an optional
/// field defaults to `None` and any field with a `.default` operator pins that
/// value. Python forbids a non-default field after a defaulted one, so the
/// emitter uses this to float defaulted fields to the end (see
/// `generate_group_def`).
fn dataclass_field_has_default(entry: &CsilGroupEntry) -> bool {
    let is_optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
    let has_explicit_default = control_operators(&entry.value_type)
        .iter()
        .any(|op| matches!(op, CsilControlOperator::Default(_)));
    is_optional || has_explicit_default
}

/// The Python attribute name for a group entry, or `None` when no stable name
/// can be derived. A keyed entry uses its key. A keyless group-spread entry
/// (`r = { g, b: bool }`) has no key, so the referenced/builtin type's own name
/// is used: this keeps the emitted field constructible and round-trippable.
/// The previous hardcoded `field` fallback produced a *required* `field: G`
/// attribute that `to_dict`/`from_dict` then skipped, so the class could not be
/// rebuilt from its own `from_dict` output (`TypeError: missing argument`). By
/// funnelling every emitter through this single helper, the field declaration,
/// `__init__`, `to_dict`, `from_dict`, and the validators all agree on the same
/// name — or all skip the entry together when no name exists (e.g. a typed key).
fn entry_field_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(name.to_case(Case::Snake)),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
            Some(name.to_case(Case::Snake))
        }
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(name.to_case(Case::Snake))
            }
            _ => None,
        },
    }
}

/// Human-readable notes for the encoding-only / structural operators that have
/// no runtime check in Python (`.json`/`.cbor`/`.cborseq`/`.bits`/`.and`/
/// `.within`). They document the wire intent without altering the type or
/// emitting a guard, so they never cause a regression or a spurious error.
fn encoding_only_notes(type_expr: &CsilTypeExpression) -> Vec<String> {
    control_operators(type_expr)
        .iter()
        .filter_map(|op| match op {
            CsilControlOperator::Json => Some("json-encoded".to_string()),
            CsilControlOperator::Cbor => Some("cbor-encoded".to_string()),
            CsilControlOperator::Cborseq => Some("cbor-sequence-encoded".to_string()),
            CsilControlOperator::Bits(name) => Some(format!("bit field from {name}")),
            CsilControlOperator::And(_) => {
                Some("intersection (.and) — enforced by the wire type".to_string())
            }
            CsilControlOperator::Within(_) => {
                Some("subset (.within) — enforced by the wire type".to_string())
            }
            _ => None,
        })
        .collect()
}

/// A safely-escaped double-quoted Python string literal for arbitrary text. A
/// bare `r"..."` raw literal breaks when the text contains a `"` or ends in a
/// backslash (e.g. a regex pattern), so escaping the metacharacters into a normal
/// literal is the only form that round-trips every pattern/message. Backslashes
/// are doubled so a regex escape like `\d` survives as a literal backslash-d.
fn python_string_literal(s: &str) -> String {
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

/// Render one validation guard: when `condition` holds the value is invalid, so
/// raise `ValueError(message)`. The message becomes a fully-escaped Python string
/// literal so an embedded quote or trailing backslash can't break the generated
/// source.
fn emit_validation_guard(condition: &str, message: &str) -> String {
    let literal = python_string_literal(message);
    format!("        if {condition}:\n            raise ValueError({literal})\n")
}

/// Generate Python dataclasses from serialized CDDL specification
pub fn generate_python_code_from_serialized(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<GeneratedFiles> {
    let mut generator = PythonGenerator::new(config);
    generator.generate(spec)
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — the error half is
/// raised by the transport, not part of the returned value.
fn python_success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
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

/// PascalCase an operation name for the wire, using the same simple rule the
/// TypeScript/Go/Rust clients use so all four agree on the method string.
fn wire_method_name(name: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            cap = true;
        } else if cap {
            out.extend(ch.to_uppercase());
            cap = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The async/sync surface a client module is emitted as. `client_style` selects it
/// from the CSIL options block; `Both` (the default) ships the blocking client plus
/// a distinct async twin so a consumer gets both in one package.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClientStyle {
    Sync,
    Async,
    Both,
}

/// Read & validate the `client_style` option. Absent defaults to `Both` — a
/// deliberate, user-requested default so every consumer gets their existing
/// blocking client PLUS an async twin. An unrecognized value is a hard error
/// (mirroring how `decimal_mapping` rejects a typo) so a mistake never silently
/// degrades to a different surface.
fn parse_client_style(config: &GeneratorConfig) -> Result<ClientStyle> {
    match config.options.get("client_style") {
        None => Ok(ClientStyle::Both),
        Some(v) => match v.as_str() {
            Some("sync") => Ok(ClientStyle::Sync),
            Some("async") => Ok(ClientStyle::Async),
            Some("both") => Ok(ClientStyle::Both),
            _ => Err(CsilgenError::GenerationError(format!(
                "client_style must be \"sync\", \"async\", or \"both\", got {v:?}"
            ))),
        },
    }
}

/// Per-file shape that turns an emitted client async and keeps an async twin's
/// public symbols distinct from the sync client's. `marker` is empty for a
/// stand-alone client (sync, or the async drop-in at the canonical filename) and
/// `"Async"` for the twin in `Both` mode, where both clients share one package and
/// would otherwise collide on `Transport`/`<Base>Client`.
#[derive(Clone, Copy)]
struct ClientShape {
    is_async: bool,
    marker: &'static str,
}

impl ClientShape {
    /// `async def` vs `def` for the client method and the transport seam.
    fn def_kw(&self) -> &'static str {
        if self.is_async { "async def" } else { "def" }
    }

    /// `await ` before the transport seam call, empty for the sync client. Only the
    /// seam is awaited; the codec is pure CPU work and stays synchronous.
    fn await_kw(&self) -> &'static str {
        if self.is_async { "await " } else { "" }
    }

    /// The transport Protocol name (`Transport`, or `AsyncTransport` for the twin).
    fn transport_name(&self) -> String {
        format!("{}Transport", self.marker)
    }

    /// A per-service client class name (`FooClient`, or `FooAsyncClient` twin).
    fn client_class_name(&self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }
}

/// Python code generator implementation
/// Resolved coordinates for self-contained-package emission. `dist_name` is the PEP
/// 621 distribution name (what `pip install` resolves), while `import_name` is the
/// on-disk package directory the modules live under and the name `import` expects.
struct PythonPackage {
    dist_name: String,
    import_name: String,
    version: String,
}

/// A distribution name may legally contain `-`/`.` (e.g. `csilgen-client`), but an
/// importable package directory must be a valid Python identifier. We map every
/// character that is not ASCII-alphanumeric or `_` to `_`, then guard the leading
/// character so the result is always importable.
fn sanitize_python_import_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("csilgen_client");
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Escapes a value for a TOML basic string. Only `\` and `"` can break the literals
/// we emit; package names/versions never legitimately contain control characters.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Renders a setuptools-backed `pyproject.toml`. Package discovery is pinned to the
/// single generated import package so the build needs no auto-discovery heuristics
/// and the artifact stays dependency-free (no third-party runtime requirements).
fn render_pyproject(pkg: &PythonPackage) -> String {
    let dist = toml_escape(&pkg.dist_name);
    let version = toml_escape(&pkg.version);
    let import_name = toml_escape(&pkg.import_name);
    format!(
        "[build-system]\n\
         requires = [\"setuptools>=61.0\"]\n\
         build-backend = \"setuptools.build_meta\"\n\
         \n\
         [project]\n\
         name = \"{dist}\"\n\
         version = \"{version}\"\n\
         requires-python = \">=3.9\"\n\
         description = \"Generated CSIL client package\"\n\
         dependencies = []\n\
         \n\
         [tool.setuptools]\n\
         packages = [\"{import_name}\"]\n"
    )
}

/// Which transport sections a consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or
/// empty value means "all three". The three sections always render in a fixed order so
/// the document reads the same regardless of how the subset was written.
fn wanted_transports(options: &HashMap<String, serde_json::Value>) -> (bool, bool, bool) {
    let listed = match options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: BTreeSet<&str> = items.iter().filter_map(|v| v.as_str()).collect();
            // An array naming none of the known transports (all unknown, or empty)
            // falls back to all three rather than an empty document.
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
/// `csilgen_transport` library. The generated codec owns CBOR (de)serialization and
/// the library owns the envelope/framing/lifecycle; the consumer supplies only a
/// *carrier* that moves bytes, so the same typed surface rides HTTP, TLS, a WebSocket,
/// QUIC, or raw UDP unchanged. Each requested section (CSIL-RPC over HTTP, CSIL-Events
/// over TLS, CSIL-Datagrams over UDP) is a complete, copy-paste example on the library.
fn readme(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
    pkg: &PythonPackage,
    options: &HashMap<String, serde_json::Value>,
) -> String {
    let dist = &pkg.dist_name;
    let import = &pkg.import_name;
    let mut out = format!(
        "# {dist}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the `csilgen_transport` library owns the envelope,\n\
         framing, and connection lifecycle. You supply only a *carrier* that moves\n\
         bytes, so the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw\n\
         UDP unchanged.\n\n\
         ## Install\n\n\
         ```sh\n\
         pip install {dist} csilgen-transport\n\
         ```\n\n\
         <!-- TODO: csilgen-transport is not yet published; vendor it or install from\n\
         the repo until it ships on PyPI. -->\n\n"
    );

    let (rpc, events, datagrams) = wanted_transports(options);
    let unary = first_unary_example(spec, records);
    let channel = first_channel_example(spec, records);
    if rpc {
        out.push_str(&rpc_section(import, unary.as_ref()));
    }
    if events {
        out.push_str(&events_section(import, channel.as_ref()));
    }
    if datagrams {
        out.push_str(&datagrams_section(import, unary.as_ref()));
    }
    out
}

/// CSIL-RPC over HTTP: a carrier implementing the generated byte seam that encodes the
/// request with the library's `RpcRequest` and decodes its `RpcResponse` (never
/// hand-rolled), POSTing to `{base_url}/csil/v1/rpc` with the stdlib `urllib`. A
/// non-zero transport status (via `into_transport_error`) and the typed `ServiceError`
/// arm are surfaced distinctly. Then the typed client calls the first `->` op.
fn rpc_section(import: &str, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`);\n\
         you bring a carrier that moves bytes. The HTTP carrier below is just one\n\
         example — swap `urllib` for any client (it implements the generated byte seam).\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    let mut names = vec![ex.client_class.clone()];
    names.extend(ex.used_types.iter().cloned());
    out.push_str("```python\n");
    out.push_str("from csilgen_transport.rpc import RpcRequest, RpcResponse\n");
    out.push_str(&format!("from {import} import {}\n\n", names.join(", ")));
    out.push_str(RPC_CARRIER_PYTHON);
    out.push_str("\n\ndef main() -> None:\n");
    out.push_str(&format!(
        "    client = {}(HttpRpcCarrier(\"http://localhost:5080\"))\n",
        ex.client_class
    ));
    if ex.has_request {
        out.push_str(&format!("    resp = client.{}({})\n", ex.method, ex.sample));
    } else {
        out.push_str(&format!("    resp = client.{}()\n", ex.method));
    }
    out.push_str("    print(resp)\n\n\n");
    out.push_str("if __name__ == \"__main__\":\n    main()\n");
    out.push_str("```\n\n");
    out
}

/// The HTTP carrier body — spec-independent, so a constant. It builds the request
/// envelope with the library's `RpcRequest`, POSTs it to `{base_url}/csil/v1/rpc` with
/// blocking `urllib`, and returns the success payload bytes the typed client decodes.
/// `RpcResponse.decode(...).into_transport_error()` raises on a non-zero transport
/// status; the typed `ServiceError` arm (a status-0 variant) is surfaced separately.
const RPC_CARRIER_PYTHON: &str = r#"# One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope;
# the carrier owns only the transport. Swap `urllib` for any HTTP client.
import urllib.error
import urllib.request


class HttpRpcCarrier:
    """The dumb byte carrier the generated client calls: it owns only the CSIL-RPC
    envelope + blocking HTTP, never your types (structurally the generated Transport)."""

    RPC_PATH = "/csil/v1/rpc"

    def __init__(self, base_url: str, timeout: float = 30.0):
        self._url = base_url.rstrip("/") + self.RPC_PATH
        self._timeout = timeout

    def call(self, service: str, op: str, req: bytes) -> bytes:
        # The library owns the envelope; pass the already-encoded request bytes.
        envelope = RpcRequest(service, op, payload=req).encode()
        http = urllib.request.Request(
            self._url,
            data=envelope,
            headers={"content-type": "application/cbor", "accept": "application/cbor"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(http, timeout=self._timeout) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as exc:
            raise RuntimeError(f"csil-rpc {service}/{op}: http {exc.code}") from None

        # into_transport_error() raises StatusError for any non-zero transport status.
        env = RpcResponse.decode(raw).into_transport_error()
        # A typed application error rides as a status-0 `ServiceError` variant — distinct
        # from a transport failure. Surface it so the typed client decodes success only.
        if env.variant == "ServiceError":
            raise RuntimeError(f"csil-rpc {service}/{op}: ServiceError")
        return env.payload
"#;

/// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as the
/// library's `StreamCarrier` (CSIL length-prefix framing), performs the
/// `$hello`/`$hello-ack` handshake, sends one outbound event via the generated
/// `encode_<service>_<op>`, and runs a recv loop that decodes each frame to an `Event`,
/// answers `$ping` with `$pong`, and dispatches typed events to the generated
/// `route_<service>_channel`. When the spec has no channel ops the dispatch wiring is
/// replaced with a note (the handshake + heartbeat still apply to any connection).
fn events_section(import: &str, ch: Option<&ChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library\n\
         owns the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and\n\
         framing; the generated router dispatches typed events. The TLS carrier below is\n\
         just one example — a WebSocket/WebTransport/QUIC carrier drops in unchanged.\n\n",
    );
    out.push_str("```python\n");
    out.push_str("import socket\nimport ssl\n\n");
    out.push_str("from csilgen_transport import VERSION\n");
    out.push_str(
        "from csilgen_transport.carrier import StreamCarrier\n\
         from csilgen_transport.events import Event, Hello, HelloAck, Heartbeat, Profile, control\n",
    );
    match ch {
        Some(ch) => {
            out.push_str(&format!(
                "from {import} import {route}, {encode}, {handlers}, {inbound}, {outbound}\n\n",
                route = ch.route_fn,
                encode = ch.encode_fn,
                handlers = ch.handler_class,
                inbound = ch.inbound_type,
                outbound = ch.outbound_type,
            ));
        }
        None => out.push('\n'),
    }
    out.push_str(EVENTS_CARRIER_PYTHON);
    out.push('\n');
    match ch {
        Some(ch) => out.push_str(&events_session(ch)),
        None => out.push_str(EVENTS_NO_CHANNEL_SESSION_PYTHON),
    }
    out.push_str("```\n\n");
    out
}

/// The TLS `StreamCarrier` adapter + the user-supplied `Codec` — spec-independent. The
/// carrier wraps a TLS socket's read/write file object with the library's canonical
/// 4-byte length-prefix framing; the codec bridges the library's byte seam to this
/// package's generated per-type `to_cbor`/`from_cbor`.
const EVENTS_CARRIER_PYTHON: &str = r#"# One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
def open_tls_carrier(host: str, port: int) -> StreamCarrier:
    raw = socket.create_connection((host, port))
    ctx = ssl.create_default_context()
    tls = ctx.wrap_socket(raw, server_hostname=host)
    # StreamCarrier owns the length-prefix framing over any read/write/flush stream.
    return StreamCarrier(tls.makefile("rwb"))


class GenCodec:
    """Bridges the library's byte seam to the generated per-type codec: encode a
    dataclass via its `to_cbor`, decode bytes via the target type's `from_cbor`."""

    def encode(self, value) -> bytes:
        return value.to_cbor()

    def decode(self, data: bytes, target_type: type):
        return target_type.from_cbor(data)
"#;

/// The channel session body for an Events connection that has a `<->` op: the
/// handshake, one outbound event via the generated encoder, and the recv loop that
/// heartbeats and dispatches inbound frames into the generated router.
fn events_session(ch: &ChannelExample) -> String {
    format!(
        r#"
# A duck-typed handler implementing {handlers}' channel methods. route_{service}_...
# dispatches each decoded inbound message here (inbound {inbound}).
class ChannelHandlers:
    def {method}(self, msg: {inbound}, ctx: dict) -> None:
        print("event {method}", msg)


def session() -> None:
    carrier = open_tls_carrier("localhost", 7443)

    # $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the
    # wire profile for the connection's lifetime.
    carrier.send_frame(Hello([VERSION], ["verbose"], "{service}").encode())
    ack = carrier.recv_frame()
    if ack is None:
        raise RuntimeError("connection closed during handshake")
    profile = Profile(HelloAck.decode(ack).profile)

    codec = GenCodec()
    handlers = ChannelHandlers()

    # Send one outbound event via the generated encoder (outbound {outbound}).
    method, body = {encode}(codec, {outbound_sample})
    carrier.send_frame(Event.verbose("{service}", method, body).encode(profile))

    # Recv loop: decode each frame to an Event, answer $ping with $pong, dispatch the
    # rest to the generated router.
    while True:
        frame = carrier.recv_frame()
        if frame is None:
            break
        ev = Event.decode(frame, profile)
        if ev.event == control.PING_NAME:
            ping = Heartbeat.decode(ev.payload)
            carrier.send_frame(
                Event.verbose("{service}", control.PONG_NAME, Heartbeat(ping.nonce).encode()).encode(profile)
            )
            continue
        {route}(handlers, codec, ev.event, ev.payload, {{}})


if __name__ == "__main__":
    session()
"#,
        service = ch.service_wire,
        inbound = ch.inbound_type,
        outbound = ch.outbound_type,
        outbound_sample = ch.outbound_sample,
        handlers = ch.handler_class,
        encode = ch.encode_fn,
        route = ch.route_fn,
        method = ch.method,
    )
}

/// The Events session body when the spec declares no channel ops: the handshake and
/// heartbeat still apply, so they are shown, with a note where the dispatch would go.
const EVENTS_NO_CHANNEL_SESSION_PYTHON: &str = r#"
def session() -> None:
    carrier = open_tls_carrier("localhost", 7443)

    # $hello / $hello-ack handshake (control plane).
    carrier.send_frame(Hello([VERSION], ["verbose"]).encode())
    ack = carrier.recv_frame()
    if ack is None:
        raise RuntimeError("connection closed during handshake")
    profile = Profile(HelloAck.decode(ack).profile)

    # Recv loop: answer $ping with $pong. This package declares no <->/<- operations,
    # so there is no generated channel router to dispatch typed events into.
    while True:
        frame = carrier.recv_frame()
        if frame is None:
            break
        ev = Event.decode(frame, profile)
        if ev.event == control.PING_NAME:
            ping = Heartbeat.decode(ev.payload)
            carrier.send_frame(
                Event.verbose(None, control.PONG_NAME, Heartbeat(ping.nonce).encode()).encode(profile)
            )


if __name__ == "__main__":
    session()
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in
/// the library's `Datagram`, and `send_datagram` it fire-and-forget. The recv path
/// `Datagram.decode`s an inbound datagram and decodes its payload with the generated
/// codec — there is NO synchronous response.
fn datagrams_section(import: &str, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram`\n\
         envelope; you bring a datagram carrier. The UDP carrier below is one example —\n\
         a WebRTC unreliable DataChannel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no record `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_type), Some(res_type)) = (&ex.req_type, &ex.res_type) else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```python\n");
    out.push_str("import socket\n\n");
    out.push_str(
        "from csilgen_transport.carrier import UdpDatagramCarrier\n\
         from csilgen_transport.datagrams import Datagram\n",
    );
    out.push_str(&format!("from {import} import {req_type}, {res_type}\n\n"));
    out.push_str(&format!(
        "# The operation's datagram ordinal — its @wire-id, or a channel-agreed number.\nOP_ORD = {}\n\n",
        ex.op_ord
    ));
    out.push_str(DATAGRAMS_CARRIER_PYTHON);
    out.push_str(&format!(
        r#"

def main() -> None:
    carrier = open_udp_carrier("localhost", 9000)

    # Fire-and-forget: encode the `->` request via the generated codec and send it.
    # seq 0 marks an unsequenced datagram.
    req = {sample}
    carrier.send_datagram(Datagram(OP_ORD, 0, req.to_cbor()).encode())

    # Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is
    # NO synchronous response; the caller must tolerate loss and reordering and handle
    # a reply whenever (if ever) it shows up.
    inbound = carrier.recv_datagram()
    if inbound is not None:
        dg = Datagram.decode(inbound)
        resp = {res_type}.from_cbor(dg.payload)
        print("late response", resp)


if __name__ == "__main__":
    main()
```

"#,
        sample = ex.sample,
        res_type = res_type,
    ));
    out
}

/// The UDP `UdpDatagramCarrier` adapter — spec-independent. It connects a UDP socket so
/// `send_datagram` writes one packet and `recv_datagram` reads the next; the carrier
/// never waits for or correlates a reply.
const DATAGRAMS_CARRIER_PYTHON: &str = r#"# One example carrier: UDP via the stdlib socket. Datagrams are unreliable and
# unordered, so the carrier never waits for or correlates a reply.
def open_udp_carrier(host: str, port: int) -> UdpDatagramCarrier:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.connect((host, port))
    return UdpDatagramCarrier(sock)
"#;

/// The pieces the RPC + datagram examples need: which client class + method to call, a
/// constructible sample request literal (empty when the op takes no request), the
/// record type names the literal constructs (so the snippet imports them), the
/// request/response record class names (for the datagram codec), and the op's ordinal.
struct UnaryExample {
    client_class: String,
    method: String,
    has_request: bool,
    sample: String,
    used_types: Vec<String>,
    req_type: Option<String>,
    res_type: Option<String>,
    op_ord: u32,
}

/// The first service (in spec order, matching how the client iterates) whose first
/// emittable unidirectional op the typed client actually exposes — i.e. a record (or
/// null) request and a record success type, the same filter `generate_client_class`
/// applies. `None` for a serviceless / non-record-payload spec.
fn first_unary_example(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
) -> Option<UnaryExample> {
    for rule in &spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        let service_class = rule.name.to_case(Case::Pascal);
        let base = service_class
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .unwrap_or(&service_class)
            .to_string();
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = python_success_type(&op.output_type);
            let resp_record = is_record_ref(&success, records);
            let req_ok = is_null_input(&op.input_type) || is_record_ref(&op.input_type, records);
            if !resp_record || !req_ok {
                continue;
            }
            let has_request = !is_null_input(&op.input_type);
            let mut used = BTreeSet::new();
            let sample = if has_request {
                python_sample(spec, &op.input_type, &mut used)
            } else {
                String::new()
            };
            return Some(UnaryExample {
                client_class: format!("{base}Client"),
                method: op.name.to_case(Case::Snake),
                has_request,
                sample,
                used_types: used.into_iter().collect(),
                // The datagram payload needs a record request; `None` for a null-input
                // op so the datagram section notes the non-record payload instead.
                req_type: has_request
                    .then(|| ref_class_name(&op.input_type))
                    .flatten(),
                res_type: ref_class_name(&success),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.map(|id| id as u32).unwrap_or(1),
            });
        }
    }
    None
}

/// The pieces the Events session needs: the generated channel router + handler class +
/// outbound encoder names, the inbound (router-decoded input) and outbound (encoder
/// output) record class names, the handler method name, the wire service, and a
/// constructible literal for the outbound record.
struct ChannelExample {
    service_wire: String,
    route_fn: String,
    handler_class: String,
    encode_fn: String,
    method: String,
    inbound_type: String,
    outbound_type: String,
    outbound_sample: String,
}

/// The first service (in spec order) with a `<->` op whose input and output are both
/// records, so the generated router, handler ABC, and outbound encoder all exist with
/// codec-backed (de)serialization. `None` when no service has a usable channel op — the
/// Events section then shows the handshake/heartbeat without dispatch wiring.
fn first_channel_example(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
) -> Option<ChannelExample> {
    for rule in &spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        let service_class = rule.name.to_case(Case::Pascal);
        let base = service_class
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .unwrap_or(&service_class)
            .to_string();
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            // The router decodes the input; the encoder encodes the output. Require both
            // to be records so a compiling, codec-backed sample exists.
            if !is_record_ref(&op.input_type, records) || !is_record_ref(&op.output_type, records) {
                continue;
            }
            let (Some(inbound_type), Some(outbound_type)) = (
                ref_class_name(&op.input_type),
                ref_class_name(&op.output_type),
            ) else {
                continue;
            };
            let snake = rule.name.to_case(Case::Snake);
            let mut used = BTreeSet::new();
            let outbound_sample = python_sample(spec, &op.output_type, &mut used);
            return Some(ChannelExample {
                service_wire: base.to_lowercase(),
                route_fn: format!("route_{snake}_channel"),
                handler_class: format!("{service_class}Handlers"),
                encode_fn: format!("encode_{snake}_{}", op.name.to_case(Case::Snake)),
                method: op.name.to_case(Case::Snake),
                inbound_type,
                outbound_type,
                outbound_sample,
            });
        }
    }
    None
}

/// The Pascal-case class name a record type reference names, if it is a reference. The
/// datagram codec and channel example name records by their generated dataclass.
fn ref_class_name(ty: &CsilTypeExpression) -> Option<String> {
    match ty {
        CsilTypeExpression::Reference(name) => Some(name.to_case(Case::Pascal)),
        _ => None,
    }
}

/// A constructible Python literal for `ty`: real values for scalars/collections and a
/// nested-record constructor (required fields only), and a `None` escape for shapes a
/// generic sample can't fabricate (timestamp, decimal, choices, tuples) so the user
/// only fills those in. Every constructed record name is recorded in `used` so the
/// Quickstart imports it.
fn python_sample(
    spec: &CsilSpecSerialized,
    ty: &CsilTypeExpression,
    used: &mut BTreeSet<String>,
) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" | "string" => "\"example\"".to_string(),
            "bool" | "boolean" => "False".to_string(),
            "bytes" | "bstr" => "b\"\"".to_string(),
            "float" | "float16" | "float32" | "float64" | "double" => "0.0".to_string(),
            "int" | "uint" | "nint" | "integer" | "number" | "int8" | "int16" | "int32"
            | "int64" | "uint8" | "uint16" | "uint32" | "uint64" => "0".to_string(),
            _ => "None".to_string(),
        },
        CsilTypeExpression::Array { .. } => "[]".to_string(),
        CsilTypeExpression::Map { .. } => "{}".to_string(),
        CsilTypeExpression::Constrained { base_type, .. } => python_sample(spec, base_type, used),
        CsilTypeExpression::Reference(name) => match find_record_group(spec, name) {
            Some(group) => record_literal(spec, name, group, used),
            None => "None".to_string(),
        },
        _ => "None".to_string(),
    }
}

/// `Name(field=<sample>, ...)` over a record's required fields, keyed by the snake_case
/// names the generated dataclass uses.
fn record_literal(
    spec: &CsilSpecSerialized,
    name: &str,
    group: &CsilGroupExpression,
    used: &mut BTreeSet<String>,
) -> String {
    let class = name.to_case(Case::Pascal);
    used.insert(class.clone());
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            entry_field_name(e)
                .map(|fname| format!("{fname}={}", python_sample(spec, &e.value_type, used)))
        })
        .collect();
    format!("{class}({})", fields.join(", "))
}

/// The record a type reference names, if any. A `Name = { ... }` rule parses as
/// `TypeDef(Group(..))`, while a bare group rule is `GroupDef(..)`; both are records.
fn find_record_group<'a>(
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

struct PythonGenerator {
    #[allow(dead_code)]
    config: GeneratorConfig,
    use_pydantic: bool,
    generated_types: HashSet<String>,
    imports: HashSet<String>,
}

impl PythonGenerator {
    fn new(config: &GeneratorConfig) -> Self {
        let use_pydantic = config
            .options
            .get("use_pydantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Self {
            config: config.clone(),
            use_pydantic,
            generated_types: HashSet::new(),
            imports: HashSet::new(),
        }
    }

    fn generate(&mut self, spec: &CsilSpecSerialized) -> Result<GeneratedFiles> {
        // Validate-early (same idiom as `ts_bidirectional_transport`): Python always
        // maps `decimal` to the stdlib `decimal.Decimal`, so "csil" and "library"
        // are both honored as no-ops, but an unrecognized value is a hard error so a
        // typo never silently degrades to the default.
        if let Some(value) = self.config.options.get("decimal_mapping") {
            match value.as_str() {
                Some("csil") | Some("library") => {}
                _ => {
                    return Err(CsilgenError::GenerationError(format!(
                        "Unknown decimal_mapping {value:?}. Supported: \"csil\", \"library\" (Python always uses decimal.Decimal)"
                    )));
                }
            }
        }

        // Validate-early, like `decimal_mapping` above: a bad `client_style` fails
        // the whole run before any file is emitted, regardless of the requested
        // sub-target. Absent value defaults to `Both`.
        let client_style = parse_client_style(&self.config)?;

        // Dispatch on target: the base `python` (and explicit `python-server`)
        // target emits server-side handler ABCs; `python-client` emits
        // transport-agnostic clients; `python-typesonly` emits the dataclasses
        // alone. An unrecognized sub-target is an error, not a silent fall-through.
        enum Surface {
            Server,
            Client,
            TypesOnly,
        }
        let surface = match self.config.target.as_str() {
            "python" | "python-server" => Surface::Server,
            "python-client" => Surface::Client,
            "python-typesonly" => Surface::TypesOnly,
            other => {
                return Err(CsilgenError::GenerationError(format!(
                    "Unknown python sub-target '{other}'. Supported: python, python-server, python-client, python-typesonly"
                )));
            }
        };

        // A package's `genquickstart.md` exercises the calling side (RPC + Datagrams, over
        // the typed client) AND the handling side (Events, over the generated channel
        // router), so a publishable package must carry BOTH surfaces for its own quickstart
        // to compile — regardless of which sub-target was requested. This mirrors the OCaml
        // generator, which emits both `client.ml` and `services.ml` in package mode. A flat
        // (non-package) build stays byte-identical: it emits only the requested surface.
        let pkg_mode = self.resolve_package_layout().is_some();
        let want_client = matches!(surface, Surface::Client)
            || (pkg_mode && !matches!(surface, Surface::TypesOnly));
        let want_server = matches!(surface, Surface::Server)
            || (pkg_mode && !matches!(surface, Surface::TypesOnly));

        let mut files = Vec::new();

        // The codec covers every record (dataclass); the typed client below calls a
        // record's generated `to_cbor`/`from_cbor`, so compute the record set once.
        let records = python_record_names(spec);

        self.setup_imports();
        self.collect_special_imports(spec);

        let mut types_code = String::new();
        let mut services_code = String::new();

        // Detect channel ops once so the services prelude (Codec) is emitted
        // exactly once at the top of the services file, not per-service.
        let has_channel_ops = spec.rules.iter().any(|r| {
            matches!(&r.rule_type, CsilRuleType::ServiceDef(def)
                if Self::service_has_channel_ops(def))
        });

        let mut prelude_emitted = false;

        for rule in &spec.rules {
            match &rule.rule_type {
                CsilRuleType::TypeDef(type_expr) => {
                    types_code.push_str(&self.generate_type_def(&rule.name, type_expr)?);
                }
                CsilRuleType::GroupDef(group_expr) => {
                    types_code.push_str(&self.generate_group_def(&rule.name, group_expr)?);
                }
                CsilRuleType::TypeChoice(choices) => {
                    types_code.push_str(&self.generate_type_choice(&rule.name, choices)?);
                }
                CsilRuleType::GroupChoice(choices) => {
                    types_code.push_str(&self.generate_group_choice(&rule.name, choices)?);
                }
                // The server surface accumulates handler ABCs + routers into one buffer;
                // the client surface (built after this loop from `client_style`) may emit
                // two files, so it is not accumulated here. In package mode both `want_*`
                // are set so the package carries both.
                CsilRuleType::ServiceDef(service) => {
                    if want_server {
                        if !prelude_emitted {
                            services_code
                                .push_str(&Self::generate_services_prelude(has_channel_ops));
                            prelude_emitted = true;
                        }
                        services_code
                            .push_str(&self.generate_service_artifacts(&rule.name, service)?);
                        if let Some(wire_ids) = Self::generate_wire_ids(&rule.name, service) {
                            services_code.push_str(&wire_ids);
                        }
                    }
                }
            }
        }

        if !types_code.is_empty() {
            let types_file = self.generate_types_file(types_code)?;
            files.push(types_file);
        }

        // The codec rides alongside the types for every surface (the records'
        // (de)serializers), so a typesonly consumer still gets usable wire codecs.
        let has_codec = if let Some(codec_file) = generate_codec_file(spec, &records) {
            files.push(codec_file);
            true
        } else {
            false
        };

        // Server-side handlers ride the single accumulated buffer (one file).
        if !services_code.is_empty() {
            let module_file = self.generate_module_file(services_code, false, has_codec)?;
            files.push(module_file);
        }

        // Client surface: `Sync`/`Async` each emit one file at the canonical
        // `client.py` (the async one is a drop-in with identical symbol names);
        // `Both` (default) emits the sync client plus an async twin at
        // `client_async.py` whose public symbols carry an `Async` marker so the two
        // coexist in one package. A spec with no services yields no client file.
        if want_client {
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
            let mut push_client = |this: &Self, path: &str, shape: ClientShape| -> Result<()> {
                let body = this.generate_client_body(spec, &records, shape)?;
                if !body.is_empty() {
                    files.push(this.generate_client_module_file(path, body, has_codec)?);
                }
                Ok(())
            };
            match client_style {
                ClientStyle::Sync => push_client(self, "client.py", sync)?,
                ClientStyle::Async => push_client(self, "client.py", async_drop_in)?,
                ClientStyle::Both => {
                    push_client(self, "client.py", sync)?;
                    push_client(self, "client_async.py", async_twin)?;
                }
            }
        }

        if !files.is_empty() {
            let init_file = self.generate_init_file(&files, spec)?;
            files.push(init_file);
        }

        // Self-contained-package mode is purely additive: the per-module files and
        // their relative imports are identical to the default layout, so it is
        // enough to relocate them under one import-package directory and drop a
        // `pyproject.toml` beside it. The non-package layout is left byte-for-byte
        // unchanged when `emit_packages` does not opt Python in.
        if let Some(pkg) = self.resolve_package_layout() {
            // The README sits at the distribution root (next to `pyproject.toml`, which
            // references it), not under the import-package directory, so it is rendered
            // before the modules are relocated and pushed afterward at the root.
            let readme = readme(spec, &records, &pkg, &self.config.options);
            for file in &mut files {
                file.path = format!("{}/{}", pkg.import_name, file.path);
            }
            files.push(GeneratedFile {
                path: "pyproject.toml".to_string(),
                content: render_pyproject(&pkg),
            });
            // The README is opt-out: only an explicit `emit_readme: false` suppresses
            // it, so a missing or non-bool value (and `true`) keeps it default-on.
            if self.wants_readme() {
                files.push(GeneratedFile {
                    path: "genquickstart.md".to_string(),
                    content: readme,
                });
            }
        }

        Ok(files)
    }

    /// Whether to emit the package `genquickstart.md`. Default true; only an explicit
    /// `emit_readme: false` suppresses it, so a missing or non-bool value (and `true`)
    /// leaves the README in place.
    fn wants_readme(&self) -> bool {
        self.config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false)
    }

    /// Returns the package coordinates only when `emit_packages` opts Python in.
    /// Parsing is deliberately tolerant: a missing key, a non-array value, or an
    /// array without `"python"` all mean "not in package mode" rather than an error,
    /// so an unrelated `emit_packages` payload never destabilizes Python output.
    fn resolve_package_layout(&self) -> Option<PythonPackage> {
        let opts_in = self
            .config
            .options
            .get("emit_packages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|e| e.as_str() == Some("python")))
            .unwrap_or(false);
        if !opts_in {
            return None;
        }

        let dist_name = self
            .config
            .options
            .get("package_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            // A path-style `package_name` is the cross-ecosystem source of truth; the
            // PyPI dist name wants only its tail. See `package_name_last_segment`.
            .map(csilgen_common::package_name_last_segment)
            .unwrap_or("csilgen_client")
            .to_string();

        let version = self
            .config
            .options
            .get("package_version")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("0.1.0")
            .to_string();

        let import_name = sanitize_python_import_name(&dist_name);

        Some(PythonPackage {
            dist_name,
            import_name,
            version,
        })
    }

    fn setup_imports(&mut self) {
        self.imports
            .insert("from typing import Optional, List, Dict, Any, Union".to_string());
        self.imports.insert("import json".to_string());

        if self.use_pydantic {
            self.imports
                .insert("from pydantic import BaseModel, Field, validator".to_string());
        } else {
            self.imports
                .insert("from dataclasses import dataclass, field".to_string());
        }
    }

    /// `timestamp`, `decimal`, and regex constraints each pull a stdlib import
    /// that is only emitted when the spec actually uses the feature, so a spec
    /// of plain scalars never imports `datetime`/`decimal`/`re`. `re` is only
    /// needed by the dataclass path's `re.match` checks, so it is skipped under
    /// pydantic (which encodes patterns in `Field` config, not generated code).
    fn collect_special_imports(&mut self, spec: &CsilSpecSerialized) {
        let mut needs_datetime = false;
        let mut needs_decimal = false;
        let mut needs_re = false;
        let mut needs_tuple = false;
        for rule in &spec.rules {
            match &rule.rule_type {
                CsilRuleType::TypeDef(t) => {
                    scan_special_types(
                        t,
                        &mut needs_datetime,
                        &mut needs_decimal,
                        &mut needs_re,
                        &mut needs_tuple,
                    );
                }
                CsilRuleType::GroupDef(g) => {
                    for entry in &g.entries {
                        scan_special_types(
                            &entry.value_type,
                            &mut needs_datetime,
                            &mut needs_decimal,
                            &mut needs_re,
                            &mut needs_tuple,
                        );
                    }
                }
                CsilRuleType::TypeChoice(cs) => {
                    for c in cs {
                        scan_special_types(
                            c,
                            &mut needs_datetime,
                            &mut needs_decimal,
                            &mut needs_re,
                            &mut needs_tuple,
                        );
                    }
                }
                CsilRuleType::GroupChoice(gs) => {
                    for g in gs {
                        for entry in &g.entries {
                            scan_special_types(
                                &entry.value_type,
                                &mut needs_datetime,
                                &mut needs_decimal,
                                &mut needs_re,
                                &mut needs_tuple,
                            );
                        }
                    }
                }
                CsilRuleType::ServiceDef(def) => {
                    for op in &def.operations {
                        scan_special_types(
                            &op.input_type,
                            &mut needs_datetime,
                            &mut needs_decimal,
                            &mut needs_re,
                            &mut needs_tuple,
                        );
                        scan_special_types(
                            &op.output_type,
                            &mut needs_datetime,
                            &mut needs_decimal,
                            &mut needs_re,
                            &mut needs_tuple,
                        );
                    }
                }
            }
        }

        if needs_datetime {
            self.imports
                .insert("from datetime import datetime".to_string());
        }
        if needs_decimal {
            self.imports
                .insert("from decimal import Decimal".to_string());
        }
        if needs_re && !self.use_pydantic {
            self.imports.insert("import re".to_string());
        }
        // `Tuple` is only imported when a fixed-shape tuple is actually present,
        // so a spec without tuples never carries an unused `typing.Tuple` import.
        if needs_tuple {
            self.imports.insert("from typing import Tuple".to_string());
        }
    }

    fn generate_type_def(&mut self, name: &str, type_expr: &CsilTypeExpression) -> Result<String> {
        // `Name = { ... }` parses to a TypeDef carrying a Group expression. Emit a
        // real dataclass for it (as the Rust/Go generators do) instead of a bare
        // `Dict[str, Any]` alias, so records keep field-level typing. Named scalar
        // and map aliases stay aliases via the fallthrough below.
        if let CsilTypeExpression::Group(group) = type_expr {
            return self.generate_group_def(name, group);
        }

        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        let python_type = self.map_type_expression(type_expr)?;

        Ok(format!("{class_name} = {python_type}\n\n"))
    }

    fn generate_group_def(&mut self, name: &str, group: &CsilGroupExpression) -> Result<String> {
        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        let mut code = String::new();

        if self.use_pydantic {
            code.push_str(&format!("class {class_name}(BaseModel):\n"));
        } else {
            code.push_str("@dataclass\n");
            code.push_str(&format!("class {class_name}:\n"));
        }

        if group.entries.is_empty() {
            code.push_str("    pass\n");
        } else {
            // A dataclass rejects a non-default field declared after a defaulted
            // one (`TypeError` at import), so defaulted fields are floated to the
            // end with a stable partition. The CBOR wire is keyed by field name,
            // not declaration order, so this reordering is invisible on the wire.
            // Pydantic has no such ordering rule, so its fields stay in spec order.
            let ordered: Vec<&CsilGroupEntry> = if self.use_pydantic {
                group.entries.iter().collect()
            } else {
                let (defaulted, required): (Vec<_>, Vec<_>) = group
                    .entries
                    .iter()
                    .partition(|entry| dataclass_field_has_default(entry));
                required.into_iter().chain(defaulted).collect()
            };
            for entry in ordered {
                code.push_str(&self.generate_field(entry)?);
            }

            if !self.use_pydantic {
                code.push_str(&self.generate_serialization_methods(&class_name, &group.entries)?);
                code.push_str(&self.generate_validation_methods(&class_name, &group.entries)?);
            } else {
                code.push_str(&self.generate_pydantic_validators(&class_name, &group.entries)?);
            }
        }

        code.push('\n');
        Ok(code)
    }

    fn generate_field(&self, entry: &CsilGroupEntry) -> Result<String> {
        // An entry with no derivable name (e.g. a typed key) is skipped entirely
        // rather than given a placeholder name, because the serialization and
        // validation emitters likewise skip it — emitting a required field here
        // would leave an attribute that `from_dict` never populates.
        let field_name = match entry_field_name(entry) {
            Some(name) => name,
            None => {
                return Ok(String::from(
                    "    # group-spread entry skipped (no field name)\n",
                ));
            }
        };

        let python_type = self.map_type_expression(&entry.value_type)?;
        let is_optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));

        let field_type = if is_optional {
            format!("Optional[{python_type}]")
        } else {
            python_type
        };

        let mut field_definition = String::new();

        if let Some(description) = self.get_field_description(&entry.metadata) {
            field_definition.push_str(&format!("    # {description}\n"));
        }

        // Encoding-only operators (`.json`/`.cbor`/`.cborseq`/`.bits`/`.and`/
        // `.within`) describe the wire form, not an in-memory invariant, so they
        // surface as a field comment rather than a type change or a check.
        for note in encoding_only_notes(&entry.value_type) {
            field_definition.push_str(&format!("    # wire constraint: {note}\n"));
        }

        if self.use_pydantic {
            let field_config = self.generate_pydantic_field_config(entry)?;
            if field_config.is_empty() {
                field_definition.push_str(&format!("    {field_name}: {field_type}\n"));
            } else {
                field_definition.push_str(&format!(
                    "    {field_name}: {field_type} = Field({field_config})\n"
                ));
            }
        } else {
            // A `.default` operator pins the dataclass default; otherwise an
            // optional field defaults to `None` and a required one has no default.
            let explicit_default =
                control_operators(&entry.value_type)
                    .iter()
                    .find_map(|op| match op {
                        // A `decimal`/`timestamp` default must be the typed value
                        // (`Decimal(...)`/`datetime(...)`), not the raw str, or the
                        // field defaults to a str of the wrong type.
                        CsilControlOperator::Default(value) => {
                            Some(python_bound_expr(value, &entry.value_type))
                        }
                        _ => None,
                    });
            let default_value = match explicit_default {
                Some(rendered) => format!(" = {rendered}"),
                None if is_optional => " = None".to_string(),
                None => String::new(),
            };
            field_definition.push_str(&format!("    {field_name}: {field_type}{default_value}\n"));
        }

        Ok(field_definition)
    }

    fn generate_pydantic_field_config(&self, entry: &CsilGroupEntry) -> Result<String> {
        // A duplicated `Field(...)` kwarg is a `SyntaxError`, and the same bound
        // can arrive from both constraint systems (e.g. `@min-value` and `.ge`),
        // so each kwarg name is emitted at most once — first writer wins. The two
        // systems agree on the value (both typed via `python_bound_expr`), so the
        // dropped duplicate is genuinely redundant.
        let mut config_parts: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push_kwarg = |key: &str, value: String| {
            if seen.insert(key.to_string()) {
                config_parts.push(format!("{key}={value}"));
            }
        };

        if let Some(description) = self.get_field_description(&entry.metadata) {
            push_kwarg("description", python_string_literal(&description));
        }

        for metadata in &entry.metadata {
            match metadata {
                CsilFieldMetadata::Constraint(constraint) => match constraint {
                    CsilValidationConstraint::MinLength(min) => {
                        push_kwarg("min_length", min.to_string());
                    }
                    CsilValidationConstraint::MaxLength(max) => {
                        push_kwarg("max_length", max.to_string());
                    }
                    CsilValidationConstraint::MinItems(min) => {
                        push_kwarg("min_items", min.to_string());
                    }
                    CsilValidationConstraint::MaxItems(max) => {
                        push_kwarg("max_items", max.to_string());
                    }
                    // `MinValue`/`MaxValue` become pydantic's inclusive numeric
                    // bounds, mirroring the dataclass path's `>=`/`<=` guards. The
                    // bound is typed (Decimal/datetime for decimal/timestamp) so
                    // pydantic compares like-with-like instead of against a `str`.
                    CsilValidationConstraint::MinValue(value) => {
                        push_kwarg("ge", python_bound_expr(value, &entry.value_type));
                    }
                    CsilValidationConstraint::MaxValue(value) => {
                        push_kwarg("le", python_bound_expr(value, &entry.value_type));
                    }
                    CsilValidationConstraint::Custom { .. } => {}
                },
                CsilFieldMetadata::Custom { name, parameters } if name == "pydantic" => {
                    for param in parameters {
                        if let Some(param_name) = &param.name {
                            match &param.value {
                                CsilLiteralValue::Text(value) => {
                                    push_kwarg(param_name, python_string_literal(value));
                                }
                                CsilLiteralValue::Bool(value) => {
                                    push_kwarg(param_name, value.to_string());
                                }
                                CsilLiteralValue::Integer(value) => {
                                    push_kwarg(param_name, value.to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Honor the `.`-operator system in pydantic too: numeric bounds become
        // ge/le/gt/lt, `.size` becomes min/max_length, `.default` becomes the
        // field default. Encoding/structural operators have no pydantic kwarg.
        for op in control_operators(&entry.value_type) {
            match op {
                CsilControlOperator::GreaterEqual(value) => {
                    push_kwarg("ge", python_bound_expr(value, &entry.value_type));
                }
                CsilControlOperator::LessEqual(value) => {
                    push_kwarg("le", python_bound_expr(value, &entry.value_type));
                }
                CsilControlOperator::GreaterThan(value) => {
                    push_kwarg("gt", python_bound_expr(value, &entry.value_type));
                }
                CsilControlOperator::LessThan(value) => {
                    push_kwarg("lt", python_bound_expr(value, &entry.value_type));
                }
                CsilControlOperator::Default(value) => {
                    push_kwarg("default", python_bound_expr(value, &entry.value_type));
                }
                CsilControlOperator::Size(CsilSizeConstraint::Min(n)) => {
                    push_kwarg("min_length", n.to_string());
                }
                CsilControlOperator::Size(CsilSizeConstraint::Max(n)) => {
                    push_kwarg("max_length", n.to_string());
                }
                CsilControlOperator::Size(CsilSizeConstraint::Exact(n)) => {
                    push_kwarg("min_length", n.to_string());
                    push_kwarg("max_length", n.to_string());
                }
                CsilControlOperator::Size(CsilSizeConstraint::Range { min, max }) => {
                    push_kwarg("min_length", min.to_string());
                    push_kwarg("max_length", max.to_string());
                }
                _ => {}
            }
        }

        Ok(config_parts.join(", "))
    }

    fn get_field_description(&self, metadata: &[CsilFieldMetadata]) -> Option<String> {
        metadata.iter().find_map(|m| match m {
            CsilFieldMetadata::Description(desc) => Some(desc.clone()),
            _ => None,
        })
    }

    fn generate_serialization_methods(
        &self,
        class_name: &str,
        entries: &[CsilGroupEntry],
    ) -> Result<String> {
        let mut code = String::new();

        code.push_str("    def to_dict(self) -> Dict[str, Any]:\n");
        code.push_str("        \"\"\"Convert to dictionary for JSON serialization.\"\"\"\n");
        code.push_str("        result = {}\n");

        for entry in entries {
            let field_name = match entry_field_name(entry) {
                Some(name) => name,
                None => continue,
            };

            let visibility = self.get_field_visibility(&entry.metadata);

            match visibility {
                Some(CsilFieldVisibility::ReceiveOnly) => {
                    continue;
                }
                _ => {
                    code.push_str(&format!("        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"));
                    code.push_str(&format!(
                        "            result['{field_name}'] = self.{field_name}\n"
                    ));
                }
            }
        }

        code.push_str("        return result\n\n");

        code.push_str("    @classmethod\n");
        code.push_str(&format!(
            "    def from_dict(cls, data: Dict[str, Any]) -> '{class_name}':\n"
        ));
        code.push_str("        \"\"\"Create instance from dictionary.\"\"\"\n");

        let mut field_assignments = Vec::new();
        for entry in entries {
            let field_name = match entry_field_name(entry) {
                Some(name) => name,
                None => continue,
            };

            let visibility = self.get_field_visibility(&entry.metadata);

            match visibility {
                Some(CsilFieldVisibility::SendOnly) => {
                    continue;
                }
                _ => {
                    field_assignments.push(format!("{field_name}=data.get('{field_name}')"));
                }
            }
        }

        code.push_str(&format!(
            "        return cls({})\n\n",
            field_assignments.join(", ")
        ));

        code.push_str("    def to_json(self) -> str:\n");
        code.push_str("        \"\"\"Convert to JSON string.\"\"\"\n");
        code.push_str("        return json.dumps(self.to_dict())\n\n");

        code.push_str("    @classmethod\n");
        code.push_str(&format!(
            "    def from_json(cls, json_str: str) -> '{class_name}':\n"
        ));
        code.push_str("        \"\"\"Create instance from JSON string.\"\"\"\n");
        code.push_str("        return cls.from_dict(json.loads(json_str))\n\n");

        Ok(code)
    }

    fn generate_validation_methods(
        &self,
        _class_name: &str,
        entries: &[CsilGroupEntry],
    ) -> Result<String> {
        // The validate() body collects guards from both constraint systems —
        // `@`-annotations (ValidationConstraint) and `.`-operators
        // (ControlOperator) — alongside the existing field-dependency checks.
        // The method (and the `__post_init__` that calls it) is only emitted when
        // there is at least one guard, so unconstrained dataclasses stay bare.
        let mut body = String::new();

        for entry in entries {
            let field_name = match entry_field_name(entry) {
                Some(name) => name,
                None => continue,
            };

            for metadata in &entry.metadata {
                if let CsilFieldMetadata::DependsOn { field, value } = metadata {
                    body.push_str(&self.dependency_guard(&field_name, field, value));
                }
                if let CsilFieldMetadata::DependsOnExpr(condition) = metadata {
                    body.push_str(&Self::depends_expr_guard(&field_name, condition));
                }
                if let CsilFieldMetadata::Constraint(constraint) = metadata {
                    body.push_str(&Self::annotation_guard(
                        &field_name,
                        constraint,
                        &entry.value_type,
                    ));
                }
            }

            for op in control_operators(&entry.value_type) {
                body.push_str(&Self::control_operator_guard(
                    &field_name,
                    op,
                    &entry.value_type,
                ));
            }
        }

        if body.is_empty() {
            return Ok(String::new());
        }

        let mut code = String::new();
        code.push_str("    def validate(self) -> bool:\n");
        code.push_str("        \"\"\"Validate field dependencies and constraints.\"\"\"\n");
        code.push_str(&body);
        code.push_str("        return True\n\n");
        code.push_str("    def __post_init__(self):\n");
        code.push_str("        \"\"\"Validate object after initialization.\"\"\"\n");
        code.push_str("        self.validate()\n\n");

        Ok(code)
    }

    /// One `@depends_on` guard, preserving the original presence/equality shape.
    fn dependency_guard(
        &self,
        field_name: &str,
        depends_on_field: &str,
        depends_on_value: &Option<CsilLiteralValue>,
    ) -> String {
        let dep_field_name = depends_on_field.to_case(Case::Snake);
        let mut out = String::new();
        match depends_on_value {
            Some(value) => {
                let value_str = csil_literal_to_python_str(value);
                // The message embeds `value_str`, which for a text value carries
                // its own quotes; building it as an escaped literal keeps the
                // generated `raise` a syntactically valid statement.
                let message =
                    format!("Field '{field_name}' requires '{dep_field_name}' to be {value_str}");
                let literal = python_string_literal(&message);
                out.push_str(&format!(
                    "        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"
                ));
                out.push_str(&format!(
                    "            if not (hasattr(self, '{dep_field_name}') and self.{dep_field_name} == {value_str}):\n"
                ));
                out.push_str(&format!("                raise ValueError({literal})\n"));
            }
            None => {
                let message =
                    format!("Field '{field_name}' requires '{dep_field_name}' to be present");
                let literal = python_string_literal(&message);
                out.push_str(&format!(
                    "        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"
                ));
                out.push_str(&format!(
                    "            if not (hasattr(self, '{dep_field_name}') and self.{dep_field_name} is not None):\n"
                ));
                out.push_str(&format!("                raise ValueError({literal})\n"));
            }
        }
        out
    }

    /// Render a `@depends-on(...)` boolean condition tree to a Python boolean
    /// expression. `All` joins with `and`, `Any` with `or` (each parenthesized so
    /// precedence survives nesting), and a `Compare` becomes either a presence
    /// check (no operator) or `<access> <op> <value>`. `access` maps a referenced
    /// peer field name to the expression that reads it — `self.<field>` in a
    /// dataclass, `values.get('<field>')` inside a pydantic validator.
    fn render_depends_condition(
        condition: &CsilDependsCondition,
        access: &dyn Fn(&str) -> String,
    ) -> String {
        match condition {
            CsilDependsCondition::Compare { field, op, value } => {
                let lhs = access(field);
                match (op, value) {
                    (Some(compare_op), Some(literal)) => {
                        let py_op = match compare_op {
                            CsilDependsCompareOp::Eq => "==",
                            CsilDependsCompareOp::Ne => "!=",
                            CsilDependsCompareOp::Lt => "<",
                            CsilDependsCompareOp::Le => "<=",
                            CsilDependsCompareOp::Gt => ">",
                            CsilDependsCompareOp::Ge => ">=",
                        };
                        let rhs = csil_literal_to_python_str(literal);
                        format!("{lhs} {py_op} {rhs}")
                    }
                    // No operator (presence) — or an operator with no value, which
                    // can only be satisfied by the field being present.
                    _ => format!("{lhs} is not None"),
                }
            }
            // An empty `All` is vacuously true and an empty `Any` vacuously false,
            // so the field is unconditionally allowed / forbidden respectively.
            CsilDependsCondition::All(parts) => {
                if parts.is_empty() {
                    "True".to_string()
                } else {
                    let rendered: Vec<String> = parts
                        .iter()
                        .map(|part| Self::render_depends_condition(part, access))
                        .collect();
                    format!("({})", rendered.join(" and "))
                }
            }
            CsilDependsCondition::Any(parts) => {
                if parts.is_empty() {
                    "False".to_string()
                } else {
                    let rendered: Vec<String> = parts
                        .iter()
                        .map(|part| Self::render_depends_condition(part, access))
                        .collect();
                    format!("({})", rendered.join(" or "))
                }
            }
        }
    }

    /// One boolean `@depends-on` guard for the dataclass path: when this field is
    /// present its condition tree must hold, otherwise the value is invalid. Peer
    /// fields are read via `self.<field>`, mirroring the simple `dependency_guard`.
    fn depends_expr_guard(field_name: &str, condition: &CsilDependsCondition) -> String {
        let expr = Self::render_depends_condition(condition, &|field| {
            format!("self.{}", field.to_case(Case::Snake))
        });
        let message = format!("Field '{field_name}' requires {expr}");
        let literal = python_string_literal(&message);
        let mut out = String::new();
        out.push_str(&format!("        if self.{field_name} is not None:\n"));
        out.push_str(&format!("            if not ({expr}):\n"));
        out.push_str(&format!("                raise ValueError({literal})\n"));
        out
    }

    /// A guard for one `@`-annotation constraint. Length/items checks guard on
    /// `is not None` so they no-op on absent optionals; numeric bounds compare
    /// directly while `decimal`/`timestamp` bounds are reconstructed as the
    /// matching Python value (see `python_bound_expr`) so the comparison is
    /// type-correct. `Custom` is advisory only and surfaces as a comment, never a
    /// hard check.
    fn annotation_guard(
        field_name: &str,
        constraint: &CsilValidationConstraint,
        value_type: &CsilTypeExpression,
    ) -> String {
        match constraint {
            CsilValidationConstraint::MinLength(min) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) < {min}"),
                &format!("Field '{field_name}' must have length >= {min}"),
            ),
            CsilValidationConstraint::MaxLength(max) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) > {max}"),
                &format!("Field '{field_name}' must have length <= {max}"),
            ),
            CsilValidationConstraint::MinItems(min) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) < {min}"),
                &format!("Field '{field_name}' must have at least {min} items"),
            ),
            CsilValidationConstraint::MaxItems(max) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) > {max}"),
                &format!("Field '{field_name}' must have at most {max} items"),
            ),
            CsilValidationConstraint::MinValue(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} < {bound}"),
                    &format!("Field '{field_name}' must be >= {v}"),
                )
            }
            CsilValidationConstraint::MaxValue(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} > {bound}"),
                    &format!("Field '{field_name}' must be <= {v}"),
                )
            }
            CsilValidationConstraint::Custom { name, .. } => {
                format!(
                    "        # custom constraint '{name}' on '{field_name}' is advisory; enforce in application code\n"
                )
            }
        }
    }

    /// A guard for one `.`-control operator. Comparison operators map to their
    /// negation (a value violating `.ge 3` is one that is `< 3`); `.size` reuses
    /// the length checks. `.default` is realized on the field declaration and the
    /// encoding/structural operators are documented on the field, so both are
    /// no-ops here.
    fn control_operator_guard(
        field_name: &str,
        op: &CsilControlOperator,
        value_type: &CsilTypeExpression,
    ) -> String {
        match op {
            CsilControlOperator::Size(size) => Self::size_guard(field_name, size),
            CsilControlOperator::Regex(pattern) => {
                // A bare `r"<pattern>"` breaks on an embedded `"` or a trailing
                // backslash; a fully-escaped literal round-trips every pattern.
                let pattern_literal = python_string_literal(pattern);
                emit_validation_guard(
                    &format!(
                        "self.{field_name} is not None and not re.match({pattern_literal}, self.{field_name})"
                    ),
                    &format!("Field '{field_name}' must match pattern {pattern}"),
                )
            }
            CsilControlOperator::GreaterEqual(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} < {bound}"),
                    &format!("Field '{field_name}' must be >= {v}"),
                )
            }
            CsilControlOperator::LessEqual(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} > {bound}"),
                    &format!("Field '{field_name}' must be <= {v}"),
                )
            }
            CsilControlOperator::GreaterThan(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} <= {bound}"),
                    &format!("Field '{field_name}' must be > {v}"),
                )
            }
            CsilControlOperator::LessThan(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} >= {bound}"),
                    &format!("Field '{field_name}' must be < {v}"),
                )
            }
            CsilControlOperator::Equal(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} != {bound}"),
                    &format!("Field '{field_name}' must equal {v}"),
                )
            }
            CsilControlOperator::NotEqual(value) => {
                let v = csil_literal_to_python_str(value);
                let bound = python_bound_expr(value, value_type);
                emit_validation_guard(
                    &format!("self.{field_name} is not None and self.{field_name} == {bound}"),
                    &format!("Field '{field_name}' must not equal {v}"),
                )
            }
            // `.default` -> field declaration; encoding/structural -> field doc.
            CsilControlOperator::Default(_)
            | CsilControlOperator::Bits(_)
            | CsilControlOperator::And(_)
            | CsilControlOperator::Within(_)
            | CsilControlOperator::Json
            | CsilControlOperator::Cbor
            | CsilControlOperator::Cborseq => String::new(),
        }
    }

    /// The length guard(s) for a `.size` operator: exact, range, min, or max.
    fn size_guard(field_name: &str, size: &CsilSizeConstraint) -> String {
        match size {
            CsilSizeConstraint::Exact(n) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) != {n}"),
                &format!("Field '{field_name}' must have length {n}"),
            ),
            CsilSizeConstraint::Min(n) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) < {n}"),
                &format!("Field '{field_name}' must have length >= {n}"),
            ),
            CsilSizeConstraint::Max(n) => emit_validation_guard(
                &format!("self.{field_name} is not None and len(self.{field_name}) > {n}"),
                &format!("Field '{field_name}' must have length <= {n}"),
            ),
            CsilSizeConstraint::Range { min, max } => {
                let mut out = emit_validation_guard(
                    &format!("self.{field_name} is not None and len(self.{field_name}) < {min}"),
                    &format!("Field '{field_name}' must have length >= {min}"),
                );
                out.push_str(&emit_validation_guard(
                    &format!("self.{field_name} is not None and len(self.{field_name}) > {max}"),
                    &format!("Field '{field_name}' must have length <= {max}"),
                ));
                out
            }
        }
    }

    fn generate_pydantic_validators(
        &self,
        _class_name: &str,
        entries: &[CsilGroupEntry],
    ) -> Result<String> {
        let mut code = String::new();

        let dependencies: Vec<_> = entries
            .iter()
            .filter_map(|entry| {
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::DependsOn { field, value } = metadata {
                        let field_name = entry_field_name(entry)?;
                        return Some((field_name, field.clone(), value.clone()));
                    }
                }
                None
            })
            .collect();

        for (field_name, depends_on_field, depends_on_value) in &dependencies {
            let dep_field_name = depends_on_field.to_case(Case::Snake);

            code.push_str(&format!("    @validator('{field_name}')\n"));
            code.push_str(&format!("    def validate_{field_name}(cls, v, values):\n"));
            code.push_str(&format!(
                "        \"\"\"Validate {field_name} field dependencies.\"\"\"\n"
            ));

            match depends_on_value {
                Some(value) => {
                    let value_str = csil_literal_to_python_str(value);
                    // `value_str` carries its own quotes for text values, so the
                    // message is built as an escaped literal to stay valid Python.
                    let message = format!(
                        "Field '{field_name}' requires '{dep_field_name}' to be {value_str}"
                    );
                    let literal = python_string_literal(&message);

                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!(
                        "            if '{dep_field_name}' not in values or values['{dep_field_name}'] != {value_str}:\n"
                    ));
                    code.push_str(&format!("                raise ValueError({literal})\n"));
                }
                None => {
                    let message =
                        format!("Field '{field_name}' requires '{dep_field_name}' to be present");
                    let literal = python_string_literal(&message);
                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!(
                        "            if '{dep_field_name}' not in values or values['{dep_field_name}'] is None:\n"
                    ));
                    code.push_str(&format!("                raise ValueError({literal})\n"));
                }
            }

            code.push_str("        return v\n\n");
        }

        // Boolean `@depends-on(...)` expressions get one validator per field that,
        // when the field is present, asserts its condition tree. Pydantic v1
        // exposes already-validated peers in `values`, so peer fields are read
        // through `values.get(...)` rather than `self`.
        for entry in entries {
            let field_name = match entry_field_name(entry) {
                Some(name) => name,
                None => continue,
            };
            for metadata in &entry.metadata {
                if let CsilFieldMetadata::DependsOnExpr(condition) = metadata {
                    let expr = Self::render_depends_condition(condition, &|field| {
                        format!("values.get('{}')", field.to_case(Case::Snake))
                    });
                    let message = format!("Field '{field_name}' requires {expr}");
                    let literal = python_string_literal(&message);
                    code.push_str(&format!("    @validator('{field_name}')\n"));
                    code.push_str(&format!(
                        "    def validate_{field_name}_depends(cls, v, values):\n"
                    ));
                    code.push_str(&format!(
                        "        \"\"\"Validate {field_name} dependency condition.\"\"\"\n"
                    ));
                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!("            if not ({expr}):\n"));
                    code.push_str(&format!("                raise ValueError({literal})\n"));
                    code.push_str("        return v\n\n");
                }
            }
        }

        Ok(code)
    }

    fn get_field_visibility(&self, metadata: &[CsilFieldMetadata]) -> Option<CsilFieldVisibility> {
        metadata.iter().find_map(|m| match m {
            CsilFieldMetadata::Visibility(vis) => Some(vis.clone()),
            _ => None,
        })
    }

    fn generate_type_choice(
        &mut self,
        name: &str,
        choices: &[CsilTypeExpression],
    ) -> Result<String> {
        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        let choice_types: Result<Vec<String>> = choices
            .iter()
            .map(|choice| self.map_type_expression(choice))
            .collect();
        let choice_types = choice_types?;

        Ok(format!(
            "{} = Union[{}]\n\n",
            class_name,
            choice_types.join(", ")
        ))
    }

    fn generate_group_choice(
        &mut self,
        name: &str,
        choices: &[CsilGroupExpression],
    ) -> Result<String> {
        let mut code = String::new();

        for (i, choice) in choices.iter().enumerate() {
            let choice_name = format!("{name}Choice{}", i + 1);
            code.push_str(&self.generate_group_def(&choice_name, choice)?);
        }

        let choice_names: Vec<String> = (0..choices.len())
            .map(|i| format!("{name}Choice{}", i + 1))
            .collect();

        let class_name = name.to_case(Case::Pascal);
        self.generated_types.insert(class_name.clone());

        code.push_str(&format!(
            "{} = Union[{}]\n\n",
            class_name,
            choice_names.join(", ")
        ));

        Ok(code)
    }

    fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
        def.operations
            .iter()
            .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
    }

    /// Once-per-file preamble for the services module: `ServiceError`
    /// exception, plus a `Codec` Protocol when any service has channel ops.
    /// Imports needed for these definitions live inline so the file's existing
    /// imports block (assembled from `self.imports`) isn't affected.
    fn generate_services_prelude(has_channel_ops: bool) -> String {
        let mut out = String::new();
        out.push_str("from abc import ABC, abstractmethod\n");
        if has_channel_ops {
            out.push_str("from typing import Protocol, Any, Tuple\n");
        }
        out.push('\n');
        out.push_str("class ServiceError(Exception):\n");
        out.push_str(
            "    \"\"\"Transport-level error thrown by service routers and handlers.\"\"\"\n",
        );
        out.push_str("    def __init__(self, code: int, message: str):\n");
        out.push_str("        self.code = code\n");
        out.push_str("        self.message = message\n");
        out.push_str("        super().__init__(f\"service error {code}: {message}\")\n\n");

        if has_channel_ops {
            out.push_str("class Codec(Protocol):\n");
            out.push_str(
                "    \"\"\"User-supplied (de)serialization for channel messages.\n\n\
                 \x20   The generator is codec-agnostic; the implementer wires this to CBOR,\n\
                 \x20   JSON, or anything else its protocol expects.\n\
                 \x20   \"\"\"\n",
            );
            out.push_str("    def encode(self, value: Any) -> bytes: ...\n");
            out.push_str("    def decode(self, data: bytes, target_type: type) -> Any: ...\n\n");
        }
        out
    }

    /// Once-per-file preamble for the client module: the `ServiceError`
    /// exception the transport raises, and the `Transport` Protocol every client
    /// delegates to. The generator never owns the wire (CBOR-over-HTTP etc.).
    fn generate_client_prelude(shape: ClientShape) -> String {
        let transport = shape.transport_name();
        let def_kw = shape.def_kw();
        let mut out = String::new();
        out.push_str("from typing import Protocol\n\n");
        out.push_str("class ServiceError(Exception):\n");
        out.push_str(
            "    \"\"\"Structured error a service returns; raised by the transport.\"\"\"\n",
        );
        out.push_str("    def __init__(self, code: int, message: str):\n");
        out.push_str("        self.code = code\n");
        out.push_str("        self.message = message\n");
        out.push_str("        super().__init__(f\"service error {code}: {message}\")\n\n");
        out.push_str(&format!("class {transport}(Protocol):\n"));
        out.push_str(
            "    \"\"\"Caller-supplied byte carrier. It performs the call named by\n\
             \x20   (service, method) with the already-encoded request bytes and returns the\n\
             \x20   response bytes, or raises ServiceError. The generated client owns\n\
             \x20   (de)serialization; the carrier only moves bytes.\n\
             \x20   \"\"\"\n",
        );
        // An `async def` seam annotated `-> bytes` is a coroutine the client
        // `await`s for the bytes; the codec it then feeds them to stays synchronous.
        out.push_str(&format!(
            "    {def_kw} call(self, service: str, method: str, req: bytes) -> bytes: ...\n\n"
        ));
        out
    }

    /// Emit a `<SERVICE>_WIRE_IDS` dict exposing the `@wire-id(N)` ordinals so a
    /// host can reference them instead of hardcoding. Purely additive: returns
    /// `None` unless the service carries a wire-id, so wire-id-free output is
    /// byte-identical.
    fn generate_wire_ids(name: &str, service: &CsilServiceDefinition) -> Option<String> {
        let service_id = service.wire_id?;
        let const_name = format!("{}_WIRE_IDS", name.to_case(Case::ScreamingSnake));
        let mut out = format!("{const_name}: dict[str, object] = {{\n");
        out.push_str(&format!("    \"service\": {service_id},\n"));
        // Operations nest under `"ops"` so an op named `service` keys into
        // `["ops"]["service"]` and can never overwrite the `"service"` ordinal.
        out.push_str("    \"ops\": {\n");
        for op in &service.operations {
            if let Some(op_id) = op.wire_id {
                out.push_str(&format!("        \"{}\": {op_id},\n", op.name));
            }
        }
        out.push_str("    },\n");
        out.push_str("}\n\n");
        Some(out)
    }

    /// Emit a typed client class for one service: one method per unary operation
    /// that serializes the typed request to CBOR, hands the bytes to the `Transport`,
    /// and deserializes the typed success response. The carrier only moves bytes.
    fn generate_client_class(
        &self,
        name: &str,
        service: &CsilServiceDefinition,
        records: &HashSet<String>,
        shape: ClientShape,
    ) -> Result<String> {
        let service_class = name.to_case(Case::Pascal);
        let base = service_class
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .unwrap_or(&service_class);
        let client_class = shape.client_class_name(base);
        let transport = shape.transport_name();
        let wire_service = base.to_lowercase();

        let mut out = String::new();
        out.push_str(&format!("class {client_class}:\n"));
        out.push_str(&format!(
            "    \"\"\"Typed client for the {name} service.\"\"\"\n"
        ));
        out.push_str(&format!(
            "    def __init__(self, transport: {transport}):\n"
        ));
        out.push_str("        self._transport = transport\n");

        for op in &service.operations {
            // Only unary request/response ops belong on the RPC client; channel
            // ops ride the router/encoder surface emitted by the base target.
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                out.push_str(&format!(
                    "\n    # channel operation {} is not part of the RPC client\n",
                    op.name
                ));
                continue;
            }
            // The typed-codec path needs a record success type (the response carries
            // `from_cbor`) and a record-or-null request (the request carries
            // `to_cbor`). Anything else is skipped with a note rather than emitting a
            // call that can't (de)serialize itself.
            let success = python_success_type(&op.output_type);
            let resp_record = is_record_ref(&success, records);
            let req_ok = is_null_input(&op.input_type) || is_record_ref(&op.input_type, records);
            if !resp_record || !req_ok {
                out.push_str(&format!(
                    "\n    # operation {} has a non-record payload; (de)serialize it manually\n",
                    op.name
                ));
                continue;
            }
            let method_name = op.name.to_case(Case::Snake);
            // The wire method must agree byte-for-byte with the other language
            // clients, which all PascalCase the op name with the same simple
            // rule — convert_case would diverge on acronyms, so avoid it here.
            let wire_method = wire_method_name(&op.name);
            // A `null`-input op carries no request body, so the method takes no `req`
            // parameter and sends empty bytes as the payload.
            let has_input = !is_null_input(&op.input_type);
            let output_type = self.map_type_expression(&success)?;
            let def_kw = shape.def_kw();
            out.push('\n');
            if has_input {
                let input_type = self.map_type_expression(&op.input_type)?;
                out.push_str(&format!(
                    "    {def_kw} {method_name}(self, req: {input_type}) -> {output_type}:\n"
                ));
            } else {
                out.push_str(&format!(
                    "    {def_kw} {method_name}(self) -> {output_type}:\n"
                ));
            }
            if op.doc_comments.is_empty() {
                out.push_str(&format!("        \"\"\"{}\"\"\"\n", op.name));
            } else {
                out.push_str("        \"\"\"");
                for (i, line) in op.doc_comments.iter().enumerate() {
                    if i > 0 {
                        out.push_str("\n        ");
                    }
                    out.push_str(line);
                }
                out.push_str("\"\"\"\n");
            }
            let payload = if has_input { "req.to_cbor()" } else { "b\"\"" };
            // Only the transport seam is awaited; `from_cbor` is the synchronous
            // codec decoding the bytes the seam yields.
            let await_kw = shape.await_kw();
            out.push_str(&format!(
                "        return {output_type}.from_cbor({await_kw}self._transport.call(\"{wire_service}\", \"{wire_method}\", {payload}))\n"
            ));
        }
        out.push('\n');
        Ok(out)
    }

    /// Build one client module body for a sync/async `shape`: the prelude
    /// (`ServiceError` + the transport Protocol) followed by every service's typed
    /// client class. The `<SERVICE>_WIRE_IDS` table is data, not a sync/async
    /// symbol, so it is emitted only for the marker-free shape — that keeps the
    /// async twin from redefining a constant the sync sibling already exports into
    /// the shared package barrel.
    fn generate_client_body(
        &self,
        spec: &CsilSpecSerialized,
        records: &HashSet<String>,
        shape: ClientShape,
    ) -> Result<String> {
        let mut body = String::new();
        let mut prelude_emitted = false;
        for rule in &spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                if !prelude_emitted {
                    body.push_str(&Self::generate_client_prelude(shape));
                    prelude_emitted = true;
                }
                body.push_str(&self.generate_client_class(&rule.name, service, records, shape)?);
                if shape.marker.is_empty()
                    && let Some(wire_ids) = Self::generate_wire_ids(&rule.name, service)
                {
                    body.push_str(&wire_ids);
                }
            }
        }
        Ok(body)
    }

    /// Wrap a client body in the module banner + imports at the given filename. The
    /// sync `client.py` output is byte-identical to the pre-async layout; the async
    /// twin reuses the same wrapper so both files share one import shape.
    fn generate_client_module_file(
        &self,
        path: &str,
        body_code: String,
        has_codec: bool,
    ) -> Result<GeneratedFile> {
        let mut content = String::new();
        content.push_str("# Generated service clients from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n\n");

        for import in &self.imports {
            content.push_str(import);
            content.push('\n');
        }
        content.push_str("from .types import *\n");
        // The client calls each record's `to_cbor`/`from_cbor`, which the codec
        // module binds onto the dataclasses on import, so the client imports it.
        if has_codec {
            content.push_str("from .codec import *\n");
        }

        content.push_str("\n\n");
        content.push_str(&body_code);

        Ok(GeneratedFile {
            path: path.to_string(),
            content,
        })
    }

    /// Emit the server-side handler ABC plus, when channel ops exist, a
    /// `route_<service>_channel` dispatcher and per-op outbound encoders.
    /// Reverse ops contribute only the outbound encoder (server pushes only).
    fn generate_service_artifacts(
        &self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String> {
        let service_class = name.to_case(Case::Pascal);
        let handler_class = format!("{service_class}Handlers");
        let mut out = String::new();

        // Server-side handlers ABC: unidirectional ops return Output; <->
        // inbound is fire-and-forget. Reverse has no server inbound here.
        out.push_str(&format!("class {handler_class}(ABC):\n"));
        out.push_str(&format!(
            "    \"\"\"Server-side handlers for {name} service operations.\"\"\"\n"
        ));
        let server_inbound: Vec<&CsilServiceOperation> = service
            .operations
            .iter()
            .filter(|op| {
                matches!(
                    op.direction,
                    CsilServiceDirection::Unidirectional | CsilServiceDirection::Bidirectional
                )
            })
            .collect();
        if server_inbound.is_empty() {
            // ABC must have a body; reverse-only services have nothing here.
            out.push_str("    pass\n");
        } else {
            for op in &server_inbound {
                let method_name = op.name.to_case(Case::Snake);
                // A `null`-input inbound op has no payload, so the handler takes
                // only `ctx` — no `req`/`msg` parameter to bind a missing body.
                let input_param = if is_null_input(&op.input_type) {
                    String::new()
                } else {
                    let input_type = self.map_type_expression(&op.input_type)?;
                    match op.direction {
                        CsilServiceDirection::Bidirectional => format!("msg: {input_type}, "),
                        _ => format!("req: {input_type}, "),
                    }
                };
                out.push('\n');
                out.push_str("    @abstractmethod\n");
                match op.direction {
                    CsilServiceDirection::Unidirectional => {
                        let output_type = self.map_type_expression(&op.output_type)?;
                        out.push_str(&format!(
                            "    def {method_name}(self, {input_param}ctx: dict) -> {output_type}:\n"
                        ));
                    }
                    CsilServiceDirection::Bidirectional => {
                        // Fire-and-forget channel inbound: the implementer's
                        // connection plumbing pulls a frame, the router decodes
                        // it, and this method handles it.
                        out.push_str(&format!(
                            "    def {method_name}(self, {input_param}ctx: dict) -> None:\n"
                        ));
                    }
                    CsilServiceDirection::Reverse => unreachable!(),
                }
                if op.doc_comments.is_empty() {
                    out.push_str(&format!("        \"\"\"{}\"\"\"\n", op.name));
                } else {
                    out.push_str("        \"\"\"");
                    for (i, line) in op.doc_comments.iter().enumerate() {
                        if i > 0 {
                            out.push_str("\n        ");
                        }
                        out.push_str(line);
                    }
                    out.push_str("\"\"\"\n");
                }
                out.push_str("        ...\n");
            }
        }
        out.push('\n');

        if Self::service_has_channel_ops(service) {
            // Channel router: only <-> dispatches inbound on the server side.
            let route_fn = format!("route_{}_channel", name.to_case(Case::Snake));
            let bidi_ops: Vec<&CsilServiceOperation> = service
                .operations
                .iter()
                .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
                .collect();

            out.push_str(&format!(
                "def {route_fn}(handlers: {handler_class}, codec: Codec, method: str, data: bytes, ctx: dict) -> None:\n"
            ));
            out.push_str(&format!(
                "    \"\"\"Decode one inbound channel frame for {name} and dispatch.\n\n\
                 \x20   The implementer feeds frames pulled off its connection here; this\n\
                 \x20   function never touches the wire.\n\
                 \x20   \"\"\"\n"
            ));
            if bidi_ops.is_empty() {
                // A reverse-only service still gets a router so consumers can
                // always call it, but any incoming method is a protocol error.
                out.push_str("    raise ServiceError(404, f\"unknown channel {method}\")\n\n");
            } else {
                for op in &bidi_ops {
                    let wire = Self::wire_method(&op.name);
                    let method_name = op.name.to_case(Case::Snake);
                    out.push_str(&format!("    if method == \"{wire}\":\n"));
                    // A `null`-input channel op carries no body to decode, so the
                    // router dispatches with `ctx` alone.
                    if is_null_input(&op.input_type) {
                        out.push_str(&format!("        handlers.{method_name}(ctx)\n"));
                    } else {
                        let input_type = self.map_type_expression(&op.input_type)?;
                        out.push_str(&format!("        msg = codec.decode(data, {input_type})\n"));
                        out.push_str(&format!("        handlers.{method_name}(msg, ctx)\n"));
                    }
                    out.push_str("        return\n");
                }
                out.push_str("    raise ServiceError(404, f\"unknown channel {method}\")\n\n");
            }

            // Compact-profile twin, emitted only for wire-id-bearing services so
            // wire-id-free specs stay byte-identical. It dispatches on the
            // operation ordinal instead of the wire method name; the profile is
            // negotiated on the wire (never declared in CSIL), so a host keeps
            // both routers and calls whichever the peer selected.
            if service.wire_id.is_some() {
                let route_fn = format!("route_{}_channel_compact", name.to_case(Case::Snake));
                out.push_str(&format!(
                    "def {route_fn}(handlers: {handler_class}, codec: Codec, op: int, data: bytes, ctx: dict) -> None:\n"
                ));
                out.push_str(&format!(
                    "    \"\"\"Decode one inbound channel frame for {name} by its @wire-id\n\n\
                     \x20   ordinal (compact transport profile) and dispatch. The verbose-profile\n\
                     \x20   twin is route_{}_channel; the host calls whichever matches the\n\
                     \x20   profile negotiated on the wire.\n\
                     \x20   \"\"\"\n",
                    name.to_case(Case::Snake)
                ));
                for op in &bidi_ops {
                    // The all-or-nothing wire-id rule (enforced by the validator)
                    // means a bidirectional op on a wire-id-bearing service always
                    // has an ordinal.
                    let Some(op_id) = op.wire_id else {
                        continue;
                    };
                    let method_name = op.name.to_case(Case::Snake);
                    out.push_str(&format!("    if op == {op_id}:\n"));
                    if is_null_input(&op.input_type) {
                        out.push_str(&format!("        handlers.{method_name}(ctx)\n"));
                    } else {
                        let input_type = self.map_type_expression(&op.input_type)?;
                        out.push_str(&format!("        msg = codec.decode(data, {input_type})\n"));
                        out.push_str(&format!("        handlers.{method_name}(msg, ctx)\n"));
                    }
                    out.push_str("        return\n");
                }
                out.push_str("    raise ServiceError(404, f\"unknown channel ordinal {op}\")\n\n");
            }

            // Outbound encoders for <-> and <- (server pushes Output to client).
            for op in &service.operations {
                if !matches!(
                    op.direction,
                    CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
                ) {
                    continue;
                }
                let method_name = op.name.to_case(Case::Snake);
                let output_type = self.map_type_expression(&op.output_type)?;
                let wire = Self::wire_method(&op.name);
                let fn_name = format!("encode_{}_{}", name.to_case(Case::Snake), method_name);
                out.push_str(&format!(
                    "def {fn_name}(codec: Codec, msg: {output_type}) -> Tuple[str, bytes]:\n"
                ));
                out.push_str(&format!(
                    "    \"\"\"Encode a `{wire}` message the server pushes to a peer.\n\n\
                     \x20   Returns (method, bytes) for the implementer to frame on its connection.\n\
                     \x20   \"\"\"\n"
                ));
                out.push_str(&format!("    return (\"{wire}\", codec.encode(msg))\n\n"));
            }
        }

        Ok(out)
    }

    /// PascalCase wire method name — same convention as TS/Rust/Go so a CBOR
    /// or JSON frame keyed by method is routable across all generated targets.
    fn wire_method(s: &str) -> String {
        let mut out = String::new();
        let mut cap = true;
        for ch in s.chars() {
            if ch == '-' || ch == '_' {
                cap = true;
            } else if cap {
                out.push(ch.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn map_type_expression(&self, type_expr: &CsilTypeExpression) -> Result<String> {
        match type_expr {
            CsilTypeExpression::Builtin(name) => self.map_builtin_type(name),
            CsilTypeExpression::Reference(name) => Ok(name.to_case(Case::Pascal)),
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let element = self.map_type_expression(element_type)?;
                match occurrence {
                    Some(CsilOccurrence::Optional) => Ok(format!("Optional[List[{element}]]")),
                    _ => Ok(format!("List[{element}]")),
                }
            }
            CsilTypeExpression::Map {
                key,
                value,
                occurrence,
            } => {
                let key_type = self.map_type_expression(key)?;
                let value_type = self.map_type_expression(value)?;
                match occurrence {
                    Some(CsilOccurrence::Optional) => {
                        Ok(format!("Optional[Dict[{key_type}, {value_type}]]"))
                    }
                    _ => Ok(format!("Dict[{key_type}, {value_type}]")),
                }
            }
            CsilTypeExpression::Group(_group) => Ok("Dict[str, Any]".to_string()),
            // A fixed-shape array maps to a positional `Tuple[...]`. Any key on a
            // keyed entry (`[tag: text, value: any]`) is positional metadata on
            // the wire, so only the entry value types matter for the Python type.
            // An optional entry keeps its position but becomes `Optional[...]`.
            CsilTypeExpression::Tuple(group) => {
                if group.entries.is_empty() {
                    return Ok("Tuple".to_string());
                }
                let parts: Result<Vec<String>> = group
                    .entries
                    .iter()
                    .map(|entry| {
                        let mapped = self.map_type_expression(&entry.value_type)?;
                        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                            Ok(format!("Optional[{mapped}]"))
                        } else {
                            Ok(mapped)
                        }
                    })
                    .collect();
                Ok(format!("Tuple[{}]", parts?.join(", ")))
            }
            CsilTypeExpression::Choice(choices) => {
                let choice_types: Result<Vec<String>> = choices
                    .iter()
                    .map(|choice| self.map_type_expression(choice))
                    .collect();
                let choice_types = choice_types?;
                Ok(format!("Union[{}]", choice_types.join(", ")))
            }
            CsilTypeExpression::Literal(literal) => match literal {
                CsilLiteralValue::Integer(_) => Ok("int".to_string()),
                CsilLiteralValue::Float(_) => Ok("float".to_string()),
                CsilLiteralValue::Text(_) => Ok("str".to_string()),
                CsilLiteralValue::Bytes(_) => Ok("bytes".to_string()),
                CsilLiteralValue::Bool(_) => Ok("bool".to_string()),
                CsilLiteralValue::Null => Ok("None".to_string()),
                CsilLiteralValue::Array(_) => Ok("List[Any]".to_string()),
            },
            CsilTypeExpression::Range { .. } => Ok("int".to_string()),
            CsilTypeExpression::Socket(_) => Ok("Any".to_string()),
            CsilTypeExpression::Plug(_) => Ok("Any".to_string()),
            CsilTypeExpression::Constrained { base_type, .. } => {
                // For constrained types, use the base type
                self.map_type_expression(base_type)
            }
        }
    }

    fn map_builtin_type(&self, builtin: &str) -> Result<String> {
        let python_type = match builtin {
            "int" | "uint" | "nint" => "int",
            "float" | "double" | "float16" | "float32" | "float64" => "float",
            "text" | "tstr" => "str",
            "bytes" | "bstr" => "bytes",
            "bool" | "true" | "false" => "bool",
            "undefined" => "None",
            // tag-0 RFC3339 timestamp: a tz-aware UTC `datetime`. The `datetime`
            // import is added by `collect_special_imports` only when used.
            "timestamp" => "datetime",
            // tag-4 exact decimal: Python has an exact base-10 type in the
            // stdlib, so it always maps to `decimal.Decimal` and emits no
            // `CsilDecimal` helper — the `decimal_mapping` option is a no-op here.
            "decimal" => "Decimal",
            "null" | "nil" => "None",
            "any" => "Any",
            _ => {
                return Err(CsilgenError::GenerationError(format!(
                    "Unknown builtin type: {builtin}"
                )));
            }
        };
        Ok(python_type.to_string())
    }

    fn generate_types_file(&self, types_code: String) -> Result<GeneratedFile> {
        let mut content = String::new();

        content.push_str("# Generated types from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n");
        // The wire contract requires `timestamp` to be tag-0 RFC3339 in UTC, so
        // the in-memory `datetime` must be tz-aware UTC before encoding.
        if self.imports.contains("from datetime import datetime") {
            content.push_str(
                "# NOTE: `timestamp` fields are tz-aware `datetime` values in UTC (CBOR tag 0).\n",
            );
        }
        content.push('\n');

        for import in &self.imports {
            content.push_str(import);
            content.push('\n');
        }

        content.push_str("\n\n");
        content.push_str(&types_code);

        Ok(GeneratedFile {
            path: "types.py".to_string(),
            content,
        })
    }

    fn generate_module_file(
        &self,
        body_code: String,
        want_client: bool,
        has_codec: bool,
    ) -> Result<GeneratedFile> {
        let (path, banner) = if want_client {
            (
                "client.py",
                "# Generated service clients from CSIL specification\n",
            )
        } else {
            (
                "services.py",
                "# Generated service handlers from CSIL specification\n",
            )
        };

        let mut content = String::new();
        content.push_str(banner);
        content.push_str("# Do not edit this file manually\n\n");

        for import in &self.imports {
            content.push_str(import);
            content.push('\n');
        }
        content.push_str("from .types import *\n");
        // The client calls each record's `to_cbor`/`from_cbor`, which the codec module
        // binds onto the dataclasses on import, so the client must import the codec.
        if want_client && has_codec {
            content.push_str("from .codec import *\n");
        }

        content.push_str("\n\n");
        content.push_str(&body_code);

        Ok(GeneratedFile {
            path: path.to_string(),
            content,
        })
    }

    fn generate_init_file(
        &self,
        files: &[GeneratedFile],
        spec: &CsilSpecSerialized,
    ) -> Result<GeneratedFile> {
        let mut content = String::new();

        content.push_str("# Generated package init from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n\n");

        let mut exports = Vec::new();

        for file in files {
            if file.path == "types.py" {
                content.push_str("from .types import *\n");
                exports.push("types");
            } else if file.path == "codec.py" {
                content.push_str("from .codec import *\n");
                exports.push("codec");
            } else if file.path == "services.py" {
                content.push_str("from .services import *\n");
                exports.push("services");
            } else if file.path == "client.py" {
                content.push_str("from .client import *\n");
                exports.push("client");
            } else if file.path == "client_async.py" {
                // The async twin's symbols carry an `Async` marker (`AsyncTransport`,
                // `<Base>AsyncClient`), so star-importing it beside the sync client
                // adds the async surface without shadowing the blocking one.
                content.push_str("from .client_async import *\n");
                exports.push("client_async");
            }
        }

        // services.py / client.py emit a framework `ServiceError(Exception)`; if the
        // spec ALSO declares a `ServiceError` record, the wildcard imports above bind
        // the exception at the package root and shadow the codec-bearing data type.
        // Re-export the dataclass last so `from <pkg> import ServiceError` resolves to
        // the record (the exception stays reachable via `.services` for internal use).
        if exports.contains(&"types")
            && spec.rules.iter().any(|r| {
                r.name.to_case(Case::Pascal) == "ServiceError"
                    && matches!(
                        r.rule_type,
                        CsilRuleType::GroupDef(_)
                            | CsilRuleType::TypeDef(CsilTypeExpression::Group(_))
                    )
            })
        {
            content.push_str(
                "\n# A spec-defined `ServiceError` record shadows the framework exception.\nfrom .types import ServiceError\n",
            );
        }

        if !exports.is_empty() {
            content.push_str(&format!(
                "\n__all__ = [{}]\n",
                exports
                    .iter()
                    .map(|e| format!("\"{e}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        Ok(GeneratedFile {
            path: "__init__.py".to_string(),
            content,
        })
    }
}

// ---------------------------------------------------------------------------
// Codec (codec.py)
// ---------------------------------------------------------------------------
//
// C/Zig/OCaml/Dart/Swift have no reflection-driven CBOR ecosystem, so their
// generators emit a per-type codec. Python *could* lean on a third-party CBOR
// library, but a self-contained codec keeps the generated package dependency-free
// and pins the exact wire bytes (canonical map key order, byte strings as major
// type 2, tag-0 timestamps, tag-4 decimals) the cross-language contract requires.

/// The function-name suffix for a record's generated codec helpers. The CSIL rule
/// name snake-cases the same way the dataclass field/attribute names do, so a
/// reference to a record resolves to the same `_encode_<suffix>_value` everywhere.
fn record_suffix(name: &str) -> String {
    name.to_case(Case::Snake)
}

/// The set of record rule suffixes — the rules whose CBOR form is a map and which
/// therefore get a generated `to_cbor`/`from_cbor`. A `Name = { ... }` TypeDef is a
/// record just like a bare GroupDef.
fn python_record_names(spec: &CsilSpecSerialized) -> HashSet<String> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(record_suffix(&r.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(record_suffix(&r.name)),
            _ => None,
        })
        .collect()
}

/// Whether a type expression is a reference to a record the codec covers, so the
/// client can call the record's own `to_cbor`/`from_cbor` rather than the
/// value-tree helpers.
fn is_record_ref(type_expr: &CsilTypeExpression, records: &HashSet<String>) -> bool {
    matches!(type_expr, CsilTypeExpression::Reference(name) if records.contains(&record_suffix(name)))
}

/// The verbatim CBOR map key for an entry (the raw bare/text-literal name, or the
/// referenced type's name for a keyless spread), or `None` for a typed-key entry —
/// kept in lockstep with `entry_field_name` so the attribute and the wire key agree.
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

/// The CBOR encoding of a text key (major type 3 head + bytes). Comparing these byte
/// vectors lexicographically is RFC 8949 §4.2.1 canonical key ordering, computed once
/// at generation time so the emitted map is canonical without a runtime sort.
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

fn unwrap_constrained(type_expr: &CsilTypeExpression) -> &CsilTypeExpression {
    match type_expr {
        CsilTypeExpression::Constrained { base_type, .. } => base_type,
        other => other,
    }
}

/// Whether a value of this type is its own CBOR value-tree node already (the encode
/// and decode are the identity), so a `[..]`/`{..}` comprehension would be pointless.
/// Scalars are identity; `timestamp`/`decimal` (tagged) and record references are not.
fn is_identity_type(
    type_expr: &CsilTypeExpression,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
) -> bool {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Builtin(name) => matches!(
            name.as_str(),
            "int"
                | "uint"
                | "nint"
                | "float"
                | "double"
                | "float16"
                | "float32"
                | "float64"
                | "text"
                | "tstr"
                | "bytes"
                | "bstr"
                | "bool"
                | "any"
        ),
        // A reference to a generated record always needs a transform. A transparent
        // alias is identity only when its underlying type is — `StringInt64Map =
        // dict[str, int]` stays identity, but `M = {* text => SomeRecord}` does not, so
        // an alias-typed field (or a container of one) still recurses into the record.
        CsilTypeExpression::Reference(name) => {
            let suffix = record_suffix(name);
            if records.contains(&suffix) {
                false
            } else if let Some(underlying) = aliases.get(&suffix) {
                is_identity_type(underlying, records, aliases)
            } else {
                true
            }
        }
        CsilTypeExpression::Array { element_type, .. } => {
            is_identity_type(element_type, records, aliases)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            is_identity_type(key, records, aliases) && is_identity_type(value, records, aliases)
        }
        _ => false,
    }
}

/// A Python expression building the CBOR value tree for `expr` (a typed value). The
/// value tree is native Python (int/float/bool/None/str/bytes/list/dict) plus
/// `CborTag`, so a scalar field is the identity and only tagged/record/container
/// shapes carry a transform.
fn py_enc_value(
    type_expr: &CsilTypeExpression,
    expr: &str,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    unions: &HashSet<String>,
) -> String {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "timestamp" => format!("CborTag(0, _csil_ts_to_text({expr}))"),
            "decimal" => format!("CborTag(4, _csil_decimal_to_pair({expr}))"),
            "null" | "nil" | "undefined" => "None".to_string(),
            _ => expr.to_string(),
        },
        CsilTypeExpression::Reference(name) => {
            let suffix = record_suffix(name);
            // Records and unions both have a generated `_encode_<suffix>_value` helper
            // (a record map codec, or a union tagged-sum codec).
            if records.contains(&suffix) || unions.contains(&suffix) {
                format!("_encode_{suffix}_value({expr})")
            } else if let Some(underlying) = aliases.get(&suffix) {
                // A transparent alias has no codec of its own; encode its underlying
                // type. A scalar/structural alias resolves to the identity, but a
                // map/array-of-record alias recurses into the record helper rather
                // than passing the dataclass instances through raw (the regression).
                py_enc_value(underlying, expr, records, aliases, unions)
            } else {
                expr.to_string()
            }
        }
        CsilTypeExpression::Array { element_type, .. } => {
            if is_identity_type(element_type, records, aliases) {
                expr.to_string()
            } else {
                let inner = py_enc_value(element_type, "csil_e", records, aliases, unions);
                format!("[{inner} for csil_e in {expr}]")
            }
        }
        CsilTypeExpression::Map { key, value, .. } => {
            if is_identity_type(key, records, aliases) && is_identity_type(value, records, aliases)
            {
                expr.to_string()
            } else {
                let ek = py_enc_value(key, "csil_k", records, aliases, unions);
                let ev = py_enc_value(value, "csil_v", records, aliases, unions);
                format!("{{{ek}: {ev} for csil_k, csil_v in {expr}.items()}}")
            }
        }
        // A fixed-shape tuple encodes positionally into a list; an absent optional
        // element stays None (encoded as null) so the array length is fixed.
        CsilTypeExpression::Tuple(group) => {
            let mut parts = Vec::with_capacity(group.entries.len());
            for (i, entry) in group.entries.iter().enumerate() {
                let elem = format!("{expr}[{i}]");
                let enc = py_enc_value(&entry.value_type, &elem, records, aliases, unions);
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    parts.push(format!("(None if {elem} is None else {enc})"));
                } else {
                    parts.push(enc);
                }
            }
            format!("[{}]", parts.join(", "))
        }
        // An opaque value is carried as its value tree.
        _ => expr.to_string(),
    }
}

/// A Python expression reconstructing the typed value from `expr` (a CBOR value tree
/// node). The inverse of `py_enc_value`.
fn py_dec_value(
    type_expr: &CsilTypeExpression,
    expr: &str,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    unions: &HashSet<String>,
) -> String {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "timestamp" => format!("_csil_ts_from_tree({expr})"),
            "decimal" => format!("_csil_decimal_from_tree({expr})"),
            "null" | "nil" | "undefined" => "None".to_string(),
            _ => expr.to_string(),
        },
        CsilTypeExpression::Reference(name) => {
            let suffix = record_suffix(name);
            if records.contains(&suffix) || unions.contains(&suffix) {
                format!("_decode_{suffix}_value({expr})")
            } else if let Some(underlying) = aliases.get(&suffix) {
                // The inverse of the encode: resolve a transparent alias to its
                // underlying type so a map/array-of-record alias reconstructs the
                // record rather than leaving raw value-tree dicts in place.
                py_dec_value(underlying, expr, records, aliases, unions)
            } else {
                expr.to_string()
            }
        }
        CsilTypeExpression::Array { element_type, .. } => {
            if is_identity_type(element_type, records, aliases) {
                expr.to_string()
            } else {
                let inner = py_dec_value(element_type, "csil_e", records, aliases, unions);
                format!("[{inner} for csil_e in {expr}]")
            }
        }
        CsilTypeExpression::Map { key, value, .. } => {
            if is_identity_type(key, records, aliases) && is_identity_type(value, records, aliases)
            {
                expr.to_string()
            } else {
                let dk = py_dec_value(key, "csil_k", records, aliases, unions);
                let dv = py_dec_value(value, "csil_v", records, aliases, unions);
                format!("{{{dk}: {dv} for csil_k, csil_v in {expr}.items()}}")
            }
        }
        // Reconstruct a fixed-shape tuple positionally from the decoded array.
        CsilTypeExpression::Tuple(group) => {
            let mut parts = Vec::with_capacity(group.entries.len());
            for (i, entry) in group.entries.iter().enumerate() {
                let elem = format!("{expr}[{i}]");
                let dec = py_dec_value(&entry.value_type, &elem, records, aliases, unions);
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    parts.push(format!("(None if {elem} is None else {dec})"));
                } else {
                    parts.push(dec);
                }
            }
            // A 1-tuple needs the trailing comma to stay a tuple.
            let trailing = if parts.len() == 1 { "," } else { "" };
            format!("({}{trailing})", parts.join(", "))
        }
        _ => expr.to_string(),
    }
}

/// One codec field: its dataclass attribute name, the verbatim wire key, the
/// canonical-order sort key, its value type, and whether it is optional.
struct PyCodecField<'a> {
    attr: String,
    wire: String,
    key_bytes: Vec<u8>,
    value_type: &'a CsilTypeExpression,
    optional: bool,
}

fn py_codec_fields(group: &CsilGroupExpression) -> Vec<PyCodecField<'_>> {
    group
        .entries
        .iter()
        .filter_map(|entry| {
            let attr = entry_field_name(entry)?;
            let wire = entry_wire_key(entry)?;
            Some(PyCodecField {
                key_bytes: cbor_text_key_bytes(&wire),
                attr,
                wire,
                value_type: &entry.value_type,
                optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
            })
        })
        .collect()
}

/// Emit the value-tree encoder/decoder pair for one record plus the `to_cbor` /
/// `from_cbor` methods bound onto its dataclass. The encoder inserts map entries in
/// canonical key order (computed at generation time); the decoder reads each field by
/// its wire key, defaulting an absent optional to `None`.
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    unions: &HashSet<String>,
) -> String {
    let class_name = name.to_case(Case::Pascal);
    let suffix = record_suffix(name);
    let fields = py_codec_fields(group);

    let mut out = String::new();

    // Encoder: build the wire map in canonical key order.
    out.push_str(&format!(
        "def _encode_{suffix}_value(v: \"{class_name}\") -> Dict[Any, Any]:\n"
    ));
    out.push_str("    csil_m: Dict[Any, Any] = {}\n");
    let mut encode_fields: Vec<&PyCodecField> = fields.iter().collect();
    encode_fields.sort_by(|a, b| a.key_bytes.cmp(&b.key_bytes));
    for field in &encode_fields {
        if field.optional {
            let enc = py_enc_value(field.value_type, "csil_x", records, aliases, unions);
            out.push_str(&format!("    csil_x = v.{}\n", field.attr));
            out.push_str("    if csil_x is not None:\n");
            out.push_str(&format!("        csil_m[\"{}\"] = {enc}\n", field.wire));
        } else {
            let enc = py_enc_value(
                field.value_type,
                &format!("v.{}", field.attr),
                records,
                aliases,
                unions,
            );
            out.push_str(&format!("    csil_m[\"{}\"] = {enc}\n", field.wire));
        }
    }
    out.push_str("    return csil_m\n\n");

    // Decoder: read by wire key in declaration order, then construct the dataclass.
    out.push_str(&format!(
        "def _decode_{suffix}_value(tree: Any) -> \"{class_name}\":\n"
    ));
    if fields.is_empty() {
        out.push_str(&format!("    return {class_name}()\n\n\n"));
    } else {
        out.push_str(&format!("    return {class_name}(\n"));
        // The decode keeps declaration order; only the encode is canonically sorted.
        for field in &fields {
            if field.optional {
                let dec = py_dec_value(
                    field.value_type,
                    &format!("tree[\"{}\"]", field.wire),
                    records,
                    aliases,
                    unions,
                );
                out.push_str(&format!(
                    "        {}=(None if tree.get(\"{}\") is None else {dec}),\n",
                    field.attr, field.wire
                ));
            } else {
                let dec = py_dec_value(
                    field.value_type,
                    &format!("tree[\"{}\"]", field.wire),
                    records,
                    aliases,
                    unions,
                );
                out.push_str(&format!("        {}={dec},\n", field.attr));
            }
        }
        out.push_str("    )\n\n\n");
    }

    // Bind the byte-level entry points onto the dataclass so the typed client can call
    // `req.to_cbor()` / `Type.from_cbor(bytes)` directly.
    out.push_str(&format!("def _{suffix}_to_cbor(self) -> bytes:\n"));
    out.push_str(&format!(
        "    return cbor_encode(_encode_{suffix}_value(self))\n\n\n"
    ));
    out.push_str(&format!(
        "def _{suffix}_from_cbor(data: bytes) -> \"{class_name}\":\n"
    ));
    out.push_str(&format!(
        "    return _decode_{suffix}_value(cbor_decode(data))\n\n\n"
    ));
    out.push_str(&format!("{class_name}.to_cbor = _{suffix}_to_cbor\n"));
    out.push_str(&format!(
        "{class_name}.from_cbor = staticmethod(_{suffix}_from_cbor)\n\n"
    ));

    out
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference / tuple (NOT a record group or a choice, which
/// have their own handling), keyed the same way records are so a `Reference` resolves
/// identically. A field referencing one must encode as the underlying type rather than
/// passing the value through raw — which drops the records inside a map/array alias.
fn codec_aliases(spec: &CsilSpecSerialized) -> HashMap<String, CsilTypeExpression> {
    spec.rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((record_suffix(&rule.name), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Named non-literal type-choices (unions) and their variant types, in declaration
/// order. Literal-only choices are enums (encoded bare); a choice with a `null`
/// variant is an optional. Both are excluded.
fn python_union_defs(spec: &CsilSpecSerialized) -> Vec<(String, Vec<CsilTypeExpression>)> {
    spec.rules
        .iter()
        .filter_map(|rule| {
            let choices = match &rule.rule_type {
                CsilRuleType::TypeChoice(c) => c,
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(c)) => c,
                _ => return None,
            };
            let all_literal = choices
                .iter()
                .all(|c| matches!(c, CsilTypeExpression::Literal(_)));
            let has_null = choices
                .iter()
                .any(|c| matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Null)));
            if all_literal || has_null {
                return None;
            }
            Some((record_suffix(&rule.name), choices.clone()))
        })
        .collect()
}

/// The Python `isinstance` type a union variant dispatches on when encoding (the
/// runtime value carries no tag, so the variant index is recovered from its type).
fn py_isinstance_type(variant: &CsilTypeExpression, records: &HashSet<String>) -> String {
    match unwrap_constrained(variant) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "bool" => "bool".to_string(),
            "int" | "uint" | "nint" => "int".to_string(),
            "float" | "float16" | "float32" | "float64" | "double" => "float".to_string(),
            "text" | "tstr" => "str".to_string(),
            "bytes" | "bstr" => "(bytes, bytearray)".to_string(),
            "timestamp" => "datetime".to_string(),
            "decimal" => "Decimal".to_string(),
            _ => "object".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&record_suffix(name)) => {
            name.to_case(Case::Pascal)
        }
        CsilTypeExpression::Array { .. } => "list".to_string(),
        CsilTypeExpression::Map { .. } => "dict".to_string(),
        _ => "object".to_string(),
    }
}

/// Emit the tagged-sum codec helpers for a union: `_encode_<u>_value` dispatches on
/// the Python runtime type to find the variant index and emits `[index, value]`;
/// `_decode_<u>_value` reads the index and reconstructs that variant. `bool` is
/// checked before `int` (Python's `bool` is an `int` subclass).
fn emit_union_codec(
    name: &str,
    variants: &[CsilTypeExpression],
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    unions: &HashSet<String>,
) -> String {
    let suffix = record_suffix(name);
    let mut out = String::new();

    // Order so a `bool` variant is tested before any `int` variant.
    let mut order: Vec<usize> = (0..variants.len()).collect();
    order.sort_by_key(|&i| usize::from(py_isinstance_type(&variants[i], records) != "bool"));

    out.push_str(&format!("def _encode_{suffix}_value(csil_v):\n"));
    for &i in &order {
        let ty = py_isinstance_type(&variants[i], records);
        let enc = py_enc_value(&variants[i], "csil_v", records, aliases, unions);
        out.push_str(&format!(
            "    if isinstance(csil_v, {ty}):\n        return [{i}, {enc}]\n"
        ));
    }
    out.push_str(&format!(
        "    raise ValueError(\"csil cbor: value does not match any {name} variant\")\n\n\n"
    ));

    out.push_str(&format!("def _decode_{suffix}_value(csil_tree):\n"));
    out.push_str("    if not isinstance(csil_tree, (list, tuple)) or len(csil_tree) != 2:\n");
    out.push_str(&format!(
        "        raise ValueError(\"csil cbor: {name} union expects a 2-element array\")\n"
    ));
    out.push_str("    csil_idx = csil_tree[0]\n    csil_val = csil_tree[1]\n");
    for (i, variant) in variants.iter().enumerate() {
        let dec = py_dec_value(variant, "csil_val", records, aliases, unions);
        out.push_str(&format!("    if csil_idx == {i}:\n        return {dec}\n"));
    }
    out.push_str(&format!(
        "    raise ValueError(\"csil cbor: unknown {name} variant\")\n\n\n"
    ));
    out
}

/// Build `codec.py`: the self-contained canonical-CBOR runtime plus per-record
/// value-tree (de)serializers and the `to_cbor`/`from_cbor` methods bound onto each
/// dataclass. `None` when the spec declares no record types.
fn generate_codec_file(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
) -> Option<GeneratedFile> {
    if records.is_empty() {
        return None;
    }

    let aliases = codec_aliases(spec);

    // Only pull `datetime`/`decimal` when a record field actually uses the tagged
    // core type, so a plain-scalar spec's codec stays import-light.
    let mut needs_datetime = false;
    let mut needs_decimal = false;
    let mut needs_re = false;
    let mut needs_tuple = false;
    let union_defs = python_union_defs(spec);
    let unions: HashSet<String> = union_defs.iter().map(|(n, _)| n.clone()).collect();
    let mut body = String::new();
    for rule in &spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            for entry in &group.entries {
                scan_special_types(
                    &entry.value_type,
                    &mut needs_datetime,
                    &mut needs_decimal,
                    &mut needs_re,
                    &mut needs_tuple,
                );
            }
            body.push_str(&emit_record_codec(
                &rule.name, group, records, &aliases, &unions,
            ));
        }
    }
    // Tagged-sum codec helpers for unions referenced by record fields.
    for (name, variants) in &union_defs {
        for variant in variants {
            scan_special_types(
                variant,
                &mut needs_datetime,
                &mut needs_decimal,
                &mut needs_re,
                &mut needs_tuple,
            );
        }
        body.push_str(&emit_union_codec(
            name, variants, records, &aliases, &unions,
        ));
    }

    let mut content = String::new();
    content.push_str("# Generated CBOR codec from CSIL specification\n");
    content.push_str("# Do not edit this file manually\n\n");
    content.push_str("import struct\n");
    content.push_str("from typing import Any, Dict\n");
    if needs_datetime {
        content.push_str("from datetime import datetime, timezone\n");
    }
    if needs_decimal {
        content.push_str("from decimal import Decimal\n");
    }
    // The records are patched in place, so the type classes must be in scope.
    content.push_str("from .types import *\n\n\n");
    content.push_str(CBOR_RUNTIME_PYTHON);
    content.push_str("\n\n");
    content.push_str(body.trim_end());
    content.push('\n');

    Some(GeneratedFile {
        path: "codec.py".to_string(),
        content,
    })
}

/// The self-contained canonical-CBOR (RFC 8949 subset) value model and codec the
/// generated per-record helpers build on. The value tree is native Python plus
/// `CborTag`, so a record encodes as a map keyed by the verbatim CSIL field names and
/// `bytes` ride as a CBOR byte string (major type 2) rather than an array of ints.
const CBOR_RUNTIME_PYTHON: &str = r#"class CborTag:
    """A CBOR tagged value (major type 6): a tag number wrapping a value tree."""

    __slots__ = ("tag", "value")

    def __init__(self, tag: int, value: Any) -> None:
        self.tag = tag
        self.value = value

    def __eq__(self, other: Any) -> bool:
        return (
            isinstance(other, CborTag)
            and self.tag == other.tag
            and self.value == other.value
        )

    def __repr__(self) -> str:
        return f"CborTag({self.tag!r}, {self.value!r})"


def _csil_head(major: int, n: int, out: bytearray) -> None:
    mt = major << 5
    if n < 24:
        out.append(mt | n)
    elif n < 0x100:
        out.append(mt | 24)
        out.append(n)
    elif n < 0x10000:
        out.append(mt | 25)
        out += n.to_bytes(2, "big")
    elif n < 0x100000000:
        out.append(mt | 26)
        out += n.to_bytes(4, "big")
    else:
        out.append(mt | 27)
        out += n.to_bytes(8, "big")


def _csil_enc(v: Any, out: bytearray) -> None:
    # bool is a subclass of int, so it is matched before the int branch.
    if v is None:
        out.append(0xF6)
    elif v is True:
        out.append(0xF5)
    elif v is False:
        out.append(0xF4)
    elif isinstance(v, int):
        if v >= 0:
            _csil_head(0, v, out)
        else:
            _csil_head(1, -1 - v, out)
    elif isinstance(v, float):
        out.append(0xFB)
        out += struct.pack(">d", v)
    elif isinstance(v, str):
        data = v.encode("utf-8")
        _csil_head(3, len(data), out)
        out += data
    elif isinstance(v, (bytes, bytearray)):
        _csil_head(2, len(v), out)
        out += bytes(v)
    elif isinstance(v, CborTag):
        _csil_head(6, v.tag, out)
        _csil_enc(v.value, out)
    elif isinstance(v, dict):
        _csil_head(5, len(v), out)
        for key, val in v.items():
            _csil_enc(key, out)
            _csil_enc(val, out)
    elif isinstance(v, (list, tuple)):
        _csil_head(4, len(v), out)
        for item in v:
            _csil_enc(item, out)
    else:
        raise ValueError(f"csilgen: cannot encode value of type {type(v)!r}")


def cbor_encode(value: Any) -> bytes:
    """Encode a CBOR value tree to canonical CBOR bytes."""
    out = bytearray()
    _csil_enc(value, out)
    return bytes(out)


def _csil_read_arg(b: bytes, pos: int, low: int):
    if low < 24:
        return low, pos + 1
    if low == 24:
        return b[pos + 1], pos + 2
    if low == 25:
        return int.from_bytes(b[pos + 1 : pos + 3], "big"), pos + 3
    if low == 26:
        return int.from_bytes(b[pos + 1 : pos + 5], "big"), pos + 5
    if low == 27:
        return int.from_bytes(b[pos + 1 : pos + 9], "big"), pos + 9
    raise ValueError("csilgen: bad head")


def _csil_dec(b: bytes, pos: int):
    ib = b[pos]
    major = ib >> 5
    low = ib & 0x1F
    if major == 7:
        if low == 20:
            return False, pos + 1
        if low == 21:
            return True, pos + 1
        if low in (22, 23):
            return None, pos + 1
        if low == 26:
            return struct.unpack(">f", b[pos + 1 : pos + 5])[0], pos + 5
        if low == 27:
            return struct.unpack(">d", b[pos + 1 : pos + 9])[0], pos + 9
        raise ValueError("csilgen: unsupported simple value")
    arg, pos = _csil_read_arg(b, pos, low)
    if major == 0:
        return arg, pos
    if major == 1:
        return -1 - arg, pos
    if major == 2:
        return bytes(b[pos : pos + arg]), pos + arg
    if major == 3:
        return b[pos : pos + arg].decode("utf-8"), pos + arg
    if major == 4:
        items = []
        for _ in range(arg):
            item, pos = _csil_dec(b, pos)
            items.append(item)
        return items, pos
    if major == 5:
        result: Dict[Any, Any] = {}
        for _ in range(arg):
            key, pos = _csil_dec(b, pos)
            val, pos = _csil_dec(b, pos)
            result[key] = val
        return result, pos
    if major == 6:
        inner, pos = _csil_dec(b, pos)
        return CborTag(arg, inner), pos
    raise ValueError("csilgen: bad major type")


def cbor_decode(data: bytes) -> Any:
    """Decode canonical CBOR bytes into a value tree."""
    value, pos = _csil_dec(data, 0)
    if pos != len(data):
        raise ValueError("csilgen: trailing bytes")
    return value


def _csil_ts_to_text(dt: Any) -> str:
    # The contract pins tag-0 timestamps to RFC3339 UTC with a `Z` offset.
    text = dt.astimezone(timezone.utc).isoformat()
    return text.replace("+00:00", "Z")


def _csil_ts_from_tree(node: Any) -> Any:
    text = node.value if isinstance(node, CborTag) else node
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def _csil_decimal_to_pair(d: Any) -> list:
    # tag-4 decimal fraction: [exponent, mantissa], value = mantissa * 10**exponent.
    sign, digits, exp = d.as_tuple()
    mant = 0
    for digit in digits:
        mant = mant * 10 + digit
    if sign:
        mant = -mant
    return [exp, mant]


def _csil_decimal_from_tree(node: Any) -> Any:
    exp, mant = node.value
    return Decimal(mant).scaleb(exp)"#;

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{CsilRule, CsilRuleType, CsilSpecSerialized};
    use std::collections::HashMap;

    fn create_test_config(use_pydantic: bool) -> GeneratorConfig {
        let mut options = HashMap::new();
        options.insert(
            "use_pydantic".to_string(),
            serde_json::Value::Bool(use_pydantic),
        );

        GeneratorConfig {
            target: "python".to_string(),
            output_dir: "/tmp/test".to_string(),
            options,
        }
    }

    fn create_test_position() -> csilgen_common::CsilPosition {
        csilgen_common::CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    #[test]
    fn test_all_cddl_numeric_builtins_map() {
        // nint and the sized floats are valid CDDL builtins; they must map, not error.
        let generator = PythonGenerator::new(&create_test_config(false));
        for (builtin, expected) in [
            ("nint", "int"),
            ("float16", "float"),
            ("float32", "float"),
            ("float64", "float"),
        ] {
            assert_eq!(generator.map_builtin_type(builtin).unwrap(), expected);
        }
    }

    #[test]
    fn test_generate_simple_dataclass() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("email".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        assert_eq!(result.len(), 3); // types.py, codec.py, and __init__.py

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("@dataclass"));
        assert!(types_file.content.contains("class User:"));
        assert!(types_file.content.contains("name: str"));
        assert!(types_file.content.contains("email: Optional[str] = None"));
        assert!(types_file.content.contains("def to_dict"));
        assert!(types_file.content.contains("def from_dict"));
    }

    #[test]
    fn test_generate_pydantic_model() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            CsilFieldMetadata::Description("User's full name".to_string()),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(1)),
                        ],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        };

        let config = create_test_config(true);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(
            types_file
                .content
                .contains("from pydantic import BaseModel")
        );
        assert!(types_file.content.contains("class User(BaseModel):"));
        assert!(types_file.content.contains("name: str = Field"));
        assert!(
            types_file
                .content
                .contains("description=\"User's full name\"")
        );
        assert!(types_file.content.contains("min_length=1"));
    }

    #[test]
    fn unidirectional_service_emits_handlers_abc_no_router() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "UserService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "create_user".to_string(),
                        input_type: CsilTypeExpression::Builtin("text".to_string()),
                        output_type: CsilTypeExpression::Builtin("text".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                        wire_id: None,
                    }],
                    wire_id: None,
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        let services_file = result.iter().find(|f| f.path == "services.py").unwrap();
        let content = &services_file.content;

        // ServiceError exception always emitted alongside any service.
        assert!(content.contains("class ServiceError(Exception):"));
        // No Codec when there are no channel ops.
        assert!(!content.contains("class Codec(Protocol):"));

        // Server-side handlers ABC; reverse/bidi-free service has only the
        // unary ABC method, no channel router, no encoders.
        assert!(content.contains("class UserServiceHandlers(ABC):"));
        assert!(content.contains("def create_user(self, req: str, ctx: dict) -> str:"));
        assert!(!content.contains("route_user_service_channel"));
        assert!(!content.contains("encode_user_service_create_user"));

        // The legacy Client/Server/dispatch shape must NOT reappear.
        assert!(!content.contains("UserServiceClient"));
        assert!(!content.contains("UserServiceServer"));
        assert!(!content.contains("def dispatch(self, operation: str"));
    }

    #[test]
    fn bidirectional_op_emits_channel_inbound_router_and_outbound_encoder() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Match".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![
                        CsilServiceOperation {
                            name: "list_events".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        },
                        CsilServiceOperation {
                            name: "play".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Bidirectional,
                            position: create_test_position(),
                            doc_comments: vec!["Open a play channel.".to_string()],
                            wire_id: None,
                        },
                    ],
                    wire_id: None,
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;

        // Codec protocol emitted exactly once at the top of the services file.
        assert!(content.contains("class Codec(Protocol):"));
        assert_eq!(content.matches("class Codec(Protocol):").count(), 1);

        // Handlers ABC contains both unidirectional (returns Output) and
        // bidirectional inbound (fire-and-forget, returns None).
        assert!(content.contains("class MatchHandlers(ABC):"));
        assert!(content.contains("def list_events(self, req: str, ctx: dict) -> str:"));
        assert!(content.contains("def play(self, msg: str, ctx: dict) -> None:"));
        // Doc comment surfaces as the method docstring.
        assert!(content.contains("\"\"\"Open a play channel.\"\"\""));

        // Router routes inbound by wire-method name (PascalCase, matches
        // TS/Rust/Go so frames are cross-language compatible).
        assert!(content.contains(
            "def route_match_channel(handlers: MatchHandlers, codec: Codec, method: str, data: bytes, ctx: dict) -> None:"
        ));
        assert!(content.contains("if method == \"Play\":"));
        assert!(content.contains("msg = codec.decode(data, str)"));
        assert!(content.contains("handlers.play(msg, ctx)"));
        assert!(content.contains("raise ServiceError(404, f\"unknown channel {method}\")"));

        // Outbound encoder for the bidirectional op (server pushes Output).
        assert!(
            content.contains("def encode_match_play(codec: Codec, msg: str) -> Tuple[str, bytes]:")
        );
        assert!(content.contains("return (\"Play\", codec.encode(msg))"));
    }

    #[test]
    fn reverse_op_emits_only_outbound_encoder_no_handler_no_router_case() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Callbacks".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "notify".to_string(),
                        input_type: CsilTypeExpression::Builtin("text".to_string()),
                        output_type: CsilTypeExpression::Builtin("text".to_string()),
                        direction: CsilServiceDirection::Reverse,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                        wire_id: None,
                    }],
                    wire_id: None,
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;

        // Reverse-only service: ABC body is `pass` (no inbound methods).
        assert!(content.contains("class CallbacksHandlers(ABC):"));
        assert!(content.contains("    pass\n"));
        // No inbound method named `notify` on the server side.
        assert!(!content.contains("def notify(self, "));

        // Router still exists for API consistency but has no `Notify` case.
        assert!(content.contains("def route_callbacks_channel("));
        let router_start = content.find("def route_callbacks_channel(").unwrap();
        let router_body = &content[router_start..];
        assert!(!router_body.contains("if method == \"Notify\":"));

        // The server-pushed encoder is present.
        assert!(
            content.contains(
                "def encode_callbacks_notify(codec: Codec, msg: str) -> Tuple[str, bytes]:"
            )
        );
        assert!(content.contains("return (\"Notify\", codec.encode(msg))"));
    }

    #[test]
    fn test_field_visibility_handling() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Message".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("content".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::Bidirectional,
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("timestamp".to_string())),
                            value_type: CsilTypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 2,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        // The to_dict method should exclude receive-only fields
        assert!(types_file.content.contains("def to_dict"));
        // The from_dict method should include receive-only fields
        assert!(types_file.content.contains("def from_dict"));
    }

    #[test]
    fn test_field_dependencies() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "ConditionalData".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("type".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("extra_data".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::DependsOn {
                                field: "type".to_string(),
                                value: Some(CsilLiteralValue::Text("advanced".to_string())),
                            }],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("def validate(self)"));
        // The embedded text value's quotes are backslash-escaped so the emitted
        // `raise ValueError(...)` is a valid Python statement.
        assert!(
            types_file
                .content
                .contains("Field 'extra_data' requires 'type' to be \\\"advanced\\\"")
        );
        assert!(types_file.content.contains("def __post_init__(self)"));
    }

    #[test]
    fn test_type_mappings() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "TypeTest".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("numbers".to_string())),
                            value_type: CsilTypeExpression::Array {
                                element_type: Box::new(CsilTypeExpression::Builtin(
                                    "int".to_string(),
                                )),
                                occurrence: None,
                            },
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("mapping".to_string())),
                            value_type: CsilTypeExpression::Map {
                                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                                occurrence: None,
                            },
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("numbers: List[int]"));
        assert!(types_file.content.contains("mapping: Dict[str, int]"));
    }

    #[test]
    fn test_union_types() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "StringOrNumber".to_string(),
                rule_type: CsilRuleType::TypeChoice(vec![
                    CsilTypeExpression::Builtin("text".to_string()),
                    CsilTypeExpression::Builtin("int".to_string()),
                ]),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(
            types_file
                .content
                .contains("StringOrNumber = Union[str, int]")
        );
    }

    #[test]
    fn test_python_naming_conventions() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "test-class".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("field-name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("class TestClass:"));
        assert!(types_file.content.contains("field_name: str"));
    }

    #[test]
    fn test_empty_spec() {
        let spec = CsilSpecSerialized {
            rules: vec![],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn test_init_file_generation() {
        let spec = CsilSpecSerialized {
            rules: vec![
                CsilRule {
                    name: "User".to_string(),
                    rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                },
                CsilRule {
                    name: "UserService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![],
                        wire_id: None,
                    }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                },
            ],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        // Should have types.py, codec.py, services.py, and __init__.py — `User` is an
        // (empty) record, so the codec rides alongside the types.
        assert_eq!(result.len(), 4);

        let init_file = result.iter().find(|f| f.path == "__init__.py").unwrap();
        assert!(init_file.content.contains("from .types import *"));
        assert!(init_file.content.contains("from .codec import *"));
        assert!(init_file.content.contains("from .services import *"));
        assert!(
            init_file
                .content
                .contains("__all__ = [\"types\", \"codec\", \"services\"]")
        );
    }

    #[test]
    fn test_typedef_group_emits_dataclass_not_dict_alias() {
        // `Task = { ... }` parses to a TypeDef carrying a Group; it must become a
        // real dataclass, not a bare `Dict[str, Any]` alias.
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Task".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("uuid".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("payload".to_string())),
                            value_type: CsilTypeExpression::Builtin("bytes".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("@dataclass"));
        assert!(types_file.content.contains("class Task:"));
        assert!(types_file.content.contains("uuid: str"));
        assert!(types_file.content.contains("payload: bytes"));
        assert!(!types_file.content.contains("Task = Dict[str, Any]"));
    }

    fn record_rule(name: &str, fields: Vec<(&str, CsilTypeExpression)>) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: fields
                    .into_iter()
                    .map(|(key, value_type)| CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare(key.to_string())),
                        value_type,
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    })
                    .collect(),
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }
    }

    fn service_spec_with_union_op() -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![
                // The request/response must be real records so the typed client can
                // call their generated `to_cbor`/`from_cbor`.
                record_rule(
                    "SubmitTaskRequest",
                    vec![("queue", CsilTypeExpression::Builtin("text".to_string()))],
                ),
                record_rule(
                    "SubmitTaskResponse",
                    vec![("uuid", CsilTypeExpression::Builtin("text".to_string()))],
                ),
                CsilRule {
                    name: "CorndogsService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![CsilServiceOperation {
                            name: "SubmitTask".to_string(),
                            input_type: CsilTypeExpression::Reference(
                                "SubmitTaskRequest".to_string(),
                            ),
                            output_type: CsilTypeExpression::Choice(vec![
                                CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                                CsilTypeExpression::Reference("ServiceError".to_string()),
                            ]),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        }],
                        wire_id: None,
                    }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                },
            ],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    #[test]
    fn test_python_client_target_emits_typed_client() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();

        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        let client = result
            .iter()
            .find(|f| f.path == "client.py")
            .expect("client.py emitted");
        assert!(client.content.contains("class Transport(Protocol):"));
        // The carrier seam is a dumb byte mover: req bytes in, response bytes out.
        assert!(
            client
                .content
                .contains("def call(self, service: str, method: str, req: bytes) -> bytes: ...")
        );
        assert!(client.content.contains("class CorndogsClient:"));
        // The client imports the codec so the records carry to_cbor/from_cbor.
        assert!(client.content.contains("from .codec import *"));
        // Success type is stripped from the `/ ServiceError` union; the typed client
        // serializes the request and deserializes the response over the byte seam.
        assert!(
            client
                .content
                .contains("def submit_task(self, req: SubmitTaskRequest) -> SubmitTaskResponse:")
        );
        assert!(client.content.contains(
            "return SubmitTaskResponse.from_cbor(self._transport.call(\"corndogs\", \"SubmitTask\", req.to_cbor()))"
        ));
        // The old object-passing seam must not reappear.
        assert!(!client.content.contains("\"SubmitTask\", req)"));
        // The server handler surface must not be emitted for the client target.
        assert!(!result.iter().any(|f| f.path == "services.py"));
        // The codec rides alongside the client.
        assert!(result.iter().any(|f| f.path == "codec.py"));
    }

    /// Default `client_style` (`both`) ships the unchanged sync client PLUS an async
    /// twin at `client_async.py` whose every public symbol carries an `Async` marker
    /// so the two coexist in one package barrel without colliding.
    #[test]
    fn async_twin_emitted_by_default_with_marked_symbols() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();

        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        // Sync client at the canonical path is unchanged: blocking `def`, plain names.
        let sync = result
            .iter()
            .find(|f| f.path == "client.py")
            .expect("client.py emitted");
        assert!(sync.content.contains("class Transport(Protocol):"));
        assert!(sync.content.contains("class CorndogsClient:"));
        assert!(
            sync.content
                .contains("def submit_task(self, req: SubmitTaskRequest) -> SubmitTaskResponse:")
        );

        // Async twin at a separate path: marked symbols, `async def`, awaited seam.
        let twin = result
            .iter()
            .find(|f| f.path == "client_async.py")
            .expect("client_async.py emitted");
        assert!(twin.content.contains("class AsyncTransport(Protocol):"));
        assert!(
            twin.content.contains(
                "async def call(self, service: str, method: str, req: bytes) -> bytes: ..."
            )
        );
        assert!(twin.content.contains("class CorndogsAsyncClient:"));
        assert!(
            twin.content
                .contains("def __init__(self, transport: AsyncTransport):")
        );
        assert!(twin.content.contains(
            "async def submit_task(self, req: SubmitTaskRequest) -> SubmitTaskResponse:"
        ));
        // Only the seam is awaited; the codec `from_cbor` stays synchronous.
        assert!(twin.content.contains(
            "return SubmitTaskResponse.from_cbor(await self._transport.call(\"corndogs\", \"SubmitTask\", req.to_cbor()))"
        ));
        // The twin must not redefine the sync names that would shadow on import.
        assert!(!twin.content.contains("class Transport(Protocol):"));
        assert!(!twin.content.contains("class CorndogsClient:"));

        // The package barrel registers the twin alongside the sync client.
        let init = result
            .iter()
            .find(|f| f.path == "__init__.py")
            .expect("__init__.py emitted");
        assert!(init.content.contains("from .client import *"));
        assert!(init.content.contains("from .client_async import *"));
    }

    /// `client_style = "async"` is a DROP-IN: the async client lives at the canonical
    /// `client.py` with the canonical symbol names (just async), and no twin appears.
    #[test]
    fn client_style_async_is_drop_in_at_canonical_path() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        config
            .options
            .insert("client_style".to_string(), serde_json::json!("async"));

        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        // No separate twin file in drop-in mode.
        assert!(!result.iter().any(|f| f.path == "client_async.py"));

        let client = result
            .iter()
            .find(|f| f.path == "client.py")
            .expect("client.py emitted");
        // Canonical names, but async.
        assert!(client.content.contains("class Transport(Protocol):"));
        assert!(client.content.contains("class CorndogsClient:"));
        assert!(
            client.content.contains(
                "async def call(self, service: str, method: str, req: bytes) -> bytes: ..."
            )
        );
        assert!(client.content.contains(
            "async def submit_task(self, req: SubmitTaskRequest) -> SubmitTaskResponse:"
        ));
        assert!(client.content.contains(
            "return SubmitTaskResponse.from_cbor(await self._transport.call(\"corndogs\", \"SubmitTask\", req.to_cbor()))"
        ));
        // No async-marked symbols in drop-in mode.
        assert!(!client.content.contains("AsyncTransport"));
        assert!(!client.content.contains("CorndogsAsyncClient"));
    }

    /// `client_style = "sync"` suppresses the twin: today's output verbatim.
    #[test]
    fn client_style_sync_suppresses_the_twin() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        config
            .options
            .insert("client_style".to_string(), serde_json::json!("sync"));

        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        assert!(!result.iter().any(|f| f.path == "client_async.py"));
        let client = result
            .iter()
            .find(|f| f.path == "client.py")
            .expect("client.py emitted");
        assert!(client.content.contains("class CorndogsClient:"));
        // No async surface at all in sync mode.
        assert!(!client.content.contains("async def"));
        assert!(!client.content.contains("await self._transport"));
        assert!(!client.content.contains("Async"));
    }

    /// An unrecognized `client_style` fails the whole run with an error that names
    /// the option — validated early, before any file is emitted.
    #[test]
    fn client_style_invalid_value_is_rejected() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        config
            .options
            .insert("client_style".to_string(), serde_json::json!("blocking"));

        let err = generate_python_code_from_serialized(&spec, &config)
            .expect_err("invalid client_style must fail generation");
        assert!(
            err.to_string().contains("client_style"),
            "error must mention client_style, got: {err}"
        );
    }

    #[test]
    fn test_python_server_alias_and_typesonly() {
        let spec = service_spec_with_union_op();

        let mut config = create_test_config(false);
        config.target = "python-server".to_string();
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        assert!(result.iter().any(|f| f.path == "services.py"));
        assert!(!result.iter().any(|f| f.path == "client.py"));

        let mut config = create_test_config(false);
        config.target = "python-typesonly".to_string();
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        assert!(!result.iter().any(|f| f.path == "services.py"));
        assert!(!result.iter().any(|f| f.path == "client.py"));
    }

    #[test]
    fn test_unknown_python_subtarget_errors() {
        let spec = service_spec_with_union_op();
        let mut config = create_test_config(false);
        config.target = "python-bogus".to_string();
        assert!(generate_python_code_from_serialized(&spec, &config).is_err());
    }

    /// Build a one-field dataclass spec whose single field carries the given
    /// type and metadata, so constraint/type tests stay terse.
    fn one_field_spec(
        field: &str,
        value_type: CsilTypeExpression,
        metadata: Vec<CsilFieldMetadata>,
    ) -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Sample".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare(field.to_string())),
                        value_type,
                        occurrence: None,
                        metadata,
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        }
    }

    fn types_content(spec: &CsilSpecSerialized, config: &GeneratorConfig) -> String {
        let result = generate_python_code_from_serialized(spec, config).unwrap();
        result
            .iter()
            .find(|f| f.path == "types.py")
            .unwrap()
            .content
            .clone()
    }

    #[test]
    fn timestamp_maps_to_tz_aware_datetime_with_import() {
        let spec = one_field_spec(
            "created_at",
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("from datetime import datetime"));
        assert!(content.contains("created_at: datetime"));
        // UTC documentation is emitted whenever timestamps are present.
        assert!(content.contains("tz-aware") && content.contains("UTC"));
    }

    #[test]
    fn decimal_and_timestamp_bounds_are_typed_not_bare_strings() {
        // user = { balance: decimal .ge "0.00",
        //          created_at: timestamp .ge "1970-01-01T00:00:00Z" }
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("balance".to_string())),
                            value_type: CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Builtin(
                                    "decimal".to_string(),
                                )),
                                constraints: vec![CsilControlOperator::GreaterEqual(
                                    CsilLiteralValue::Text("0.00".to_string()),
                                )],
                            },
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("created_at".to_string())),
                            value_type: CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Builtin(
                                    "timestamp".to_string(),
                                )),
                                constraints: vec![CsilControlOperator::GreaterEqual(
                                    CsilLiteralValue::Text("1970-01-01T00:00:00Z".to_string()),
                                )],
                            },
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let content = types_content(&spec, &create_test_config(false));

        // The bounds must be reconstructed as the field's Python type, not
        // compared against a bare `str` (which raises `TypeError` at runtime).
        assert!(
            content.contains("self.balance < Decimal(\"0.00\")"),
            "decimal bound must be a Decimal(...), got:\n{content}"
        );
        assert!(
            content.contains(
                "self.created_at < datetime.fromisoformat(\"1970-01-01T00:00:00Z\".replace(\"Z\", \"+00:00\"))"
            ),
            "timestamp bound must be a datetime.fromisoformat(...), got:\n{content}"
        );
        // A bare string comparison is exactly the bug being fixed.
        assert!(!content.contains("self.balance < \"0.00\""));
        assert!(!content.contains("self.created_at < \"1970-01-01T00:00:00Z\""));
        // The constructors require their imports.
        assert!(content.contains("from decimal import Decimal"));
        assert!(content.contains("from datetime import datetime"));
    }

    #[test]
    fn decimal_always_maps_to_stdlib_decimal_no_helper() {
        let spec = one_field_spec(
            "amount",
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("from decimal import Decimal"));
        assert!(content.contains("amount: Decimal"));
        // Python never emits the CsilDecimal helper other targets generate.
        assert!(!content.contains("CsilDecimal"));
    }

    #[test]
    fn decimal_mapping_library_and_csil_both_yield_decimal() {
        for mapping in ["library", "csil"] {
            let spec = one_field_spec(
                "amount",
                CsilTypeExpression::Builtin("decimal".to_string()),
                vec![],
            );
            let mut config = create_test_config(false);
            config
                .options
                .insert("decimal_mapping".to_string(), mapping.into());
            let content = types_content(&spec, &config);
            assert!(content.contains("amount: Decimal"));
            assert!(!content.contains("CsilDecimal"));
        }
    }

    #[test]
    fn decimal_mapping_unknown_value_is_hard_error() {
        let spec = one_field_spec(
            "amount",
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![],
        );
        let mut config = create_test_config(false);
        config
            .options
            .insert("decimal_mapping".to_string(), "bogus".into());
        assert!(generate_python_code_from_serialized(&spec, &config).is_err());
    }

    #[test]
    fn no_special_imports_when_unused() {
        let spec = one_field_spec(
            "name",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(!content.contains("from datetime import datetime"));
        assert!(!content.contains("from decimal import Decimal"));
        assert!(!content.contains("import re"));
    }

    #[test]
    fn annotation_min_max_value_emit_numeric_guards() {
        let spec = one_field_spec(
            "age",
            CsilTypeExpression::Builtin("int".to_string()),
            vec![
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinValue(
                    CsilLiteralValue::Integer(0),
                )),
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxValue(
                    CsilLiteralValue::Integer(120),
                )),
            ],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("def validate(self)"));
        assert!(content.contains("def __post_init__(self)"));
        assert!(content.contains("self.age is not None and self.age < 0"));
        assert!(content.contains("self.age is not None and self.age > 120"));
    }

    #[test]
    fn control_operator_comparisons_emit_guards() {
        let spec = one_field_spec(
            "qty",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                constraints: vec![
                    CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(1)),
                    CsilControlOperator::LessThan(CsilLiteralValue::Integer(10)),
                    CsilControlOperator::NotEqual(CsilLiteralValue::Integer(5)),
                ],
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        // Base type is unwrapped for the annotation.
        assert!(content.contains("qty: int"));
        assert!(content.contains("self.qty is not None and self.qty < 1"));
        assert!(content.contains("self.qty is not None and self.qty >= 10"));
        assert!(content.contains("self.qty is not None and self.qty == 5"));
    }

    #[test]
    fn control_operator_size_and_regex_emit_guards_and_re_import() {
        let spec = one_field_spec(
            "code",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                constraints: vec![
                    CsilControlOperator::Size(CsilSizeConstraint::Range { min: 2, max: 8 }),
                    CsilControlOperator::Regex("^[A-Z]+$".to_string()),
                ],
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("import re"));
        assert!(content.contains("len(self.code) < 2"));
        assert!(content.contains("len(self.code) > 8"));
        assert!(content.contains("not re.match(\"^[A-Z]+$\", self.code)"));
    }

    #[test]
    fn control_operator_default_sets_field_default_not_guard() {
        let spec = one_field_spec(
            "limit",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Integer(50))],
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("limit: int = 50"));
        // A lone `.default` is no invariant, so no validate() is emitted.
        assert!(!content.contains("def validate(self)"));
    }

    #[test]
    fn encoding_only_operators_documented_no_guard_no_error() {
        let spec = one_field_spec(
            "blob",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("bytes".to_string())),
                constraints: vec![CsilControlOperator::Cbor],
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("# wire constraint: cbor-encoded"));
        assert!(content.contains("blob: bytes"));
        assert!(!content.contains("def validate(self)"));
    }

    #[test]
    fn pydantic_completes_min_max_value() {
        let spec = one_field_spec(
            "age",
            CsilTypeExpression::Builtin("int".to_string()),
            vec![
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinValue(
                    CsilLiteralValue::Integer(0),
                )),
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxValue(
                    CsilLiteralValue::Integer(120),
                )),
            ],
        );
        let content = types_content(&spec, &create_test_config(true));
        assert!(content.contains("age: int = Field("));
        assert!(content.contains("ge=0"));
        assert!(content.contains("le=120"));
    }

    #[test]
    fn required_field_after_optional_is_reordered_before_defaulted() {
        // record = { nickname: text ?, id: text }  — spec order puts the
        // defaulted optional before the required field, which a dataclass rejects
        // at import (`non-default argument follows default argument`). The emitter
        // must float the required field ahead of the defaulted one.
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Account".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("nickname".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("id".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let content = types_content(&spec, &create_test_config(false));
        let required_at = content.find("id: str").expect("required field emitted");
        let defaulted_at = content
            .find("nickname: Optional[str] = None")
            .expect("defaulted field emitted");
        assert!(
            required_at < defaulted_at,
            "required field must precede the defaulted one, got:\n{content}"
        );
    }

    #[test]
    fn explicit_default_field_floats_after_required() {
        // A `.default` field is defaulted too, so a later required field must
        // still be reordered ahead of it.
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Paging".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("limit".to_string())),
                            value_type: CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                                constraints: vec![CsilControlOperator::Default(
                                    CsilLiteralValue::Integer(50),
                                )],
                            },
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("cursor".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let content = types_content(&spec, &create_test_config(false));
        let required_at = content.find("cursor: str").expect("required field emitted");
        let defaulted_at = content.find("limit: int = 50").expect("defaulted emitted");
        assert!(
            required_at < defaulted_at,
            "required field must precede the `.default` field, got:\n{content}"
        );
    }

    #[test]
    fn decimal_default_is_typed_not_bare_string() {
        // `balance: decimal .default "0.00"` must default to `Decimal("0.00")`, not
        // the str "0.00" (which would give the field the wrong type).
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Wallet".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("balance".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin("decimal".to_string())),
                            constraints: vec![CsilControlOperator::Default(
                                CsilLiteralValue::Text("0.00".to_string()),
                            )],
                        },
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let content = types_content(&spec, &create_test_config(false));
        assert!(
            content.contains("balance: Decimal = Decimal(\"0.00\")"),
            "decimal default must be typed, got:\n{content}"
        );
    }

    #[test]
    fn pydantic_decimal_and_timestamp_bounds_are_typed() {
        // Under pydantic, a decimal/timestamp bound must construct a Decimal /
        // datetime — a bare `str` raises when pydantic compares it to the field.
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Money".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("balance".to_string())),
                            value_type: CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Builtin(
                                    "decimal".to_string(),
                                )),
                                constraints: vec![CsilControlOperator::GreaterEqual(
                                    CsilLiteralValue::Text("0.00".to_string()),
                                )],
                            },
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("created_at".to_string())),
                            value_type: CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Builtin(
                                    "timestamp".to_string(),
                                )),
                                constraints: vec![CsilControlOperator::GreaterEqual(
                                    CsilLiteralValue::Text("1970-01-01T00:00:00Z".to_string()),
                                )],
                            },
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };

        let content = types_content(&spec, &create_test_config(true));
        assert!(
            content.contains("ge=Decimal(\"0.00\")"),
            "decimal pydantic bound must be a Decimal(...), got:\n{content}"
        );
        assert!(
            content.contains(
                "ge=datetime.fromisoformat(\"1970-01-01T00:00:00Z\".replace(\"Z\", \"+00:00\"))"
            ),
            "timestamp pydantic bound must be a datetime(...), got:\n{content}"
        );
        // The string form being replaced is exactly the bug.
        assert!(!content.contains("ge=\"0.00\""));
    }

    #[test]
    fn pydantic_bound_from_both_systems_emits_kwarg_once() {
        // The same lower bound supplied by both `@min-value` and `.ge` must not
        // produce `Field(ge=1, ge=1)` (a `SyntaxError`).
        let spec = one_field_spec(
            "age",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                constraints: vec![CsilControlOperator::GreaterEqual(
                    CsilLiteralValue::Integer(1),
                )],
            },
            vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(1)),
            )],
        );
        let content = types_content(&spec, &create_test_config(true));
        assert!(content.contains("ge=1"));
        assert_eq!(
            content.matches("ge=").count(),
            1,
            "ge must be emitted exactly once, got:\n{content}"
        );
    }

    #[test]
    fn regex_pattern_with_double_quote_is_escaped() {
        // A pattern containing a `"` would break a bare `r"..."` literal; the
        // emitter must escape it into a normal Python string literal.
        let spec = one_field_spec(
            "label",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                constraints: vec![CsilControlOperator::Regex("^\"[a-z]+\"$".to_string())],
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(
            content.contains("re.match(\"^\\\"[a-z]+\\\"$\", self.label)"),
            "double-quote pattern must be escaped, got:\n{content}"
        );
        // The fragile raw-string form must not be used.
        assert!(!content.contains("re.match(r\""));
    }

    #[test]
    fn decimal_integer_bound_renders_as_quoted_decimal() {
        // An Integer bound on a `decimal` field must build `Decimal("0")` (its
        // decimal string), matching how a text bound constructs the value.
        let spec = one_field_spec(
            "amount",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("decimal".to_string())),
                constraints: vec![CsilControlOperator::GreaterEqual(
                    CsilLiteralValue::Integer(0),
                )],
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(
            content.contains("self.amount < Decimal(\"0\")"),
            "integer decimal bound must be Decimal(\"0\"), got:\n{content}"
        );
        // Never the bare-int form, which compares against an int, not a Decimal.
        assert!(!content.contains("Decimal(0)"));
    }

    #[test]
    fn timestamp_decimal_imports_surface_from_nested_types() {
        let spec = one_field_spec(
            "stamps",
            CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("timestamp".to_string())),
                occurrence: None,
            },
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("from datetime import datetime"));
        assert!(content.contains("stamps: List[datetime]"));
    }

    /// A keyed/positional tuple group reaching the generator.
    fn tuple_group(
        entries: Vec<(Option<&str>, CsilTypeExpression, Option<CsilOccurrence>)>,
    ) -> CsilGroupExpression {
        CsilGroupExpression {
            entries: entries
                .into_iter()
                .map(|(key, value_type, occurrence)| CsilGroupEntry {
                    key: key.map(|k| CsilGroupKey::Bare(k.to_string())),
                    value_type,
                    occurrence,
                    metadata: Vec::new(),
                    doc_comments: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn tuple_type_maps_to_typing_tuple_with_import() {
        // mixed = [text, int, bool]  ->  Tuple[str, int, bool]
        let spec = one_field_spec(
            "mixed",
            CsilTypeExpression::Tuple(tuple_group(vec![
                (None, CsilTypeExpression::Builtin("text".to_string()), None),
                (None, CsilTypeExpression::Builtin("int".to_string()), None),
                (None, CsilTypeExpression::Builtin("bool".to_string()), None),
            ])),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("from typing import Tuple"));
        assert!(
            content.contains("mixed: Tuple[str, int, bool]"),
            "expected positional Tuple, got:\n{content}"
        );
    }

    #[test]
    fn keyed_tuple_uses_value_types_optional_position_wrapped() {
        // tagged = [tag: text, value: ?any]  ->  Tuple[str, Optional[Any]]
        let spec = one_field_spec(
            "tagged",
            CsilTypeExpression::Tuple(tuple_group(vec![
                (
                    Some("tag"),
                    CsilTypeExpression::Builtin("text".to_string()),
                    None,
                ),
                (
                    Some("value"),
                    CsilTypeExpression::Builtin("any".to_string()),
                    Some(CsilOccurrence::Optional),
                ),
            ])),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(
            content.contains("tagged: Tuple[str, Optional[Any]]"),
            "keys are positional metadata; only value types matter, got:\n{content}"
        );
    }

    #[test]
    fn tuple_surfaces_nested_special_imports() {
        // [text, decimal] must still pull in `decimal` and `Tuple`.
        let spec = one_field_spec(
            "row",
            CsilTypeExpression::Tuple(tuple_group(vec![
                (None, CsilTypeExpression::Builtin("text".to_string()), None),
                (
                    None,
                    CsilTypeExpression::Builtin("decimal".to_string()),
                    None,
                ),
            ])),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("from typing import Tuple"));
        assert!(content.contains("from decimal import Decimal"));
        assert!(content.contains("row: Tuple[str, Decimal]"));
    }

    #[test]
    fn no_tuple_import_when_unused() {
        let spec = one_field_spec(
            "name",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(!content.contains("from typing import Tuple"));
    }

    #[test]
    fn boolean_depends_on_renders_condition_tree_guard() {
        // @depends-on(country = "US" | country = "CA") state?: text
        let condition = CsilDependsCondition::Any(vec![
            CsilDependsCondition::Compare {
                field: "country".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("US".to_string())),
            },
            CsilDependsCondition::Compare {
                field: "country".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("CA".to_string())),
            },
        ]);
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "ShippingForm".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("country".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("state".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::DependsOnExpr(condition)],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        };
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("def validate(self)"));
        assert!(content.contains("if self.state is not None:"));
        // `|` becomes an `or` over parenthesized equality compares.
        assert!(
            content.contains("if not ((self.country == \"US\" or self.country == \"CA\")):"),
            "expected an OR condition tree, got:\n{content}"
        );
    }

    #[test]
    fn boolean_depends_on_presence_and_nested_compare() {
        // @depends-on(registration_type = "group" & group_size > 5)
        let condition = CsilDependsCondition::All(vec![
            CsilDependsCondition::Compare {
                field: "registration_type".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("group".to_string())),
            },
            CsilDependsCondition::Compare {
                field: "group_size".to_string(),
                op: Some(CsilDependsCompareOp::Gt),
                value: Some(CsilLiteralValue::Integer(5)),
            },
        ]);
        let spec = one_field_spec(
            "group_discount_code",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilFieldMetadata::DependsOnExpr(condition)],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(
            content.contains(
                "if not ((self.registration_type == \"group\" and self.group_size > 5)):"
            ),
            "expected an AND tree with comparison, got:\n{content}"
        );

        // A bare presence dependency (no operator) becomes an `is not None` check.
        let presence = CsilDependsCondition::Compare {
            field: "parent".to_string(),
            op: None,
            value: None,
        };
        let spec = one_field_spec(
            "child",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilFieldMetadata::DependsOnExpr(presence)],
        );
        let content = types_content(&spec, &create_test_config(false));
        assert!(content.contains("if not (self.parent is not None):"));
    }

    #[test]
    fn keyless_group_spread_field_is_wired_into_from_dict() {
        // R = { g, b: bool } — `g` is a keyless group-spread referencing type G.
        // The generated class must be constructible from its own from_dict
        // output, so the spread entry has to be a properly-named field that
        // round-trips, not the old hardcoded `field` placeholder that left a
        // required attribute from_dict never populated.
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "R".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: None,
                            value_type: CsilTypeExpression::Reference("g".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("b".to_string())),
                            value_type: CsilTypeExpression::Builtin("bool".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };
        let content = types_content(&spec, &create_test_config(false));

        // The spread field is named after the referenced type, not the old
        // `field` placeholder.
        assert!(
            content.contains("g: G"),
            "expected named spread field, got:\n{content}"
        );
        assert!(
            !content.contains("    field:"),
            "unexpected unconstructible placeholder field, got:\n{content}"
        );
        // Every required attribute the class declares is populated by from_dict,
        // so `R.from_dict(R(...).to_dict())` cannot raise on a missing argument.
        assert!(
            content.contains("g=data.get('g')"),
            "spread field missing from from_dict, got:\n{content}"
        );
        assert!(
            content.contains("b=data.get('b')"),
            "keyed field missing from from_dict, got:\n{content}"
        );
        // to_dict must also serialize the spread field so the round-trip carries
        // its value back into from_dict.
        assert!(
            content.contains("result['g'] = self.g"),
            "spread field missing from to_dict, got:\n{content}"
        );
    }

    #[test]
    fn both_depends_on_variants_render_on_one_spec() {
        // The parser keeps `@depends-on(x = "y")` as the simple DependsOn and
        // only promotes boolean forms (`!=`/`<`/`&`/`|`/...) to DependsOnExpr, so
        // a spec can carry both. Neither must be silently dropped.
        let bool_condition = CsilDependsCondition::Compare {
            field: "tier".to_string(),
            op: Some(CsilDependsCompareOp::Ne),
            value: Some(CsilLiteralValue::Text("free".to_string())),
        };
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Account".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("tier".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("coupon".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::DependsOn {
                                field: "tier".to_string(),
                                value: Some(CsilLiteralValue::Text("paid".to_string())),
                            }],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("seats".to_string())),
                            value_type: CsilTypeExpression::Builtin("int".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::DependsOnExpr(bool_condition)],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 2,
        };

        // Dataclass path: both the simple equality guard and the boolean `!=`
        // guard are present.
        let dataclass = types_content(&spec, &create_test_config(false));
        assert!(
            dataclass.contains("Field 'coupon' requires 'tier' to be"),
            "simple depends-on dropped from dataclass, got:\n{dataclass}"
        );
        assert!(
            dataclass.contains("if not (self.tier != \"free\"):"),
            "boolean depends-on dropped from dataclass, got:\n{dataclass}"
        );

        // Pydantic path: both validators are emitted too.
        let pydantic = types_content(&spec, &create_test_config(true));
        assert!(
            pydantic.contains("def validate_coupon(cls, v, values):"),
            "simple depends-on dropped from pydantic, got:\n{pydantic}"
        );
        assert!(
            pydantic.contains("def validate_seats_depends(cls, v, values):"),
            "boolean depends-on dropped from pydantic, got:\n{pydantic}"
        );
        assert!(
            pydantic.contains("values.get('tier') != \"free\""),
            "boolean condition missing from pydantic, got:\n{pydantic}"
        );
    }

    #[test]
    fn null_input_op_emits_no_request_param() {
        // A push-only reverse op pairs with a unary op that has a null input,
        // exercising the client/server null-input paths without a bogus `req`.
        fn null_input_service() -> CsilSpecSerialized {
            CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: "PingService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![CsilServiceOperation {
                            name: "heartbeat".to_string(),
                            input_type: CsilTypeExpression::Builtin("null".to_string()),
                            output_type: CsilTypeExpression::Builtin("bool".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        }],
                        wire_id: None,
                    }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            }
        }

        // Server handler ABC: no `req` parameter, only `ctx`.
        let server =
            generate_python_code_from_serialized(&null_input_service(), &create_test_config(false))
                .unwrap();
        let services = server
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content
            .clone();
        assert!(
            services.contains("def heartbeat(self, ctx: dict) -> bool:"),
            "null-input handler must take no req, got:\n{services}"
        );
        assert!(!services.contains("req: None"));

        // Client method: heartbeat returns a scalar `bool`, which has no codec, so the
        // typed-codec client skips it with a note rather than emitting a call that
        // cannot deserialize itself.
        let mut client_config = create_test_config(false);
        client_config.target = "python-client".to_string();
        let client =
            generate_python_code_from_serialized(&null_input_service(), &client_config).unwrap();
        let client_src = client
            .iter()
            .find(|f| f.path == "client.py")
            .unwrap()
            .content
            .clone();
        assert!(
            client_src.contains("# operation heartbeat has a non-record payload"),
            "non-record op must be skipped with a note, got:\n{client_src}"
        );
        assert!(!client_src.contains("def heartbeat(self)"));
    }

    /// A null-input op with a *record* success type does get a typed client method:
    /// no `req` parameter, empty payload bytes, response deserialized via `from_cbor`.
    #[test]
    fn null_input_record_output_op_emits_typed_method() {
        let spec = CsilSpecSerialized {
            rules: vec![
                record_rule(
                    "Pong",
                    vec![("at", CsilTypeExpression::Builtin("text".to_string()))],
                ),
                CsilRule {
                    name: "PingService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![CsilServiceOperation {
                            name: "ping".to_string(),
                            input_type: CsilTypeExpression::Builtin("null".to_string()),
                            output_type: CsilTypeExpression::Reference("Pong".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        }],
                        wire_id: None,
                    }),
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                },
            ],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();
        let client = result.iter().find(|f| f.path == "client.py").unwrap();
        assert!(client.content.contains("def ping(self) -> Pong:"));
        assert!(
            client
                .content
                .contains("return Pong.from_cbor(self._transport.call(\"ping\", \"Ping\", b\"\"))")
        );
    }

    fn wire_id_service(service_wire: Option<u64>, op_wire: Option<u64>) -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "OrderService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![
                        CsilServiceOperation {
                            name: "place-order".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: op_wire,
                        },
                        CsilServiceOperation {
                            name: "cancel-order".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                            wire_id: None,
                        },
                    ],
                    wire_id: service_wire,
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    #[test]
    fn wire_ids_emitted_when_present() {
        let spec = wire_id_service(Some(3), Some(7));
        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;
        assert!(
            content.contains("ORDER_SERVICE_WIRE_IDS: dict[str, object] = {"),
            "expected wire-ids dict, got:\n{content}"
        );
        assert!(
            content.contains("\"service\": 3,"),
            "expected service ordinal, got:\n{content}"
        );
        assert!(
            content.contains("\"ops\": {"),
            "expected nested ops dict, got:\n{content}"
        );
        assert!(
            content.contains("\"place-order\": 7,"),
            "expected operation ordinal, got:\n{content}"
        );
        // Operation without a wire-id contributes no entry.
        assert!(
            !content.contains("\"cancel-order\":"),
            "operation without wire-id must not appear, got:\n{content}"
        );
    }

    #[test]
    fn wire_ids_op_named_service_does_not_collide() {
        let mut spec = wire_id_service(Some(3), Some(7));
        if let CsilRuleType::ServiceDef(service) = &mut spec.rules[0].rule_type {
            service.operations[0].name = "service".to_string();
        }
        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;
        // The op named `service` nests under `"ops"`, so the top-level
        // `"service"` ordinal key is never overwritten.
        assert!(
            content.contains("\"service\": 3,"),
            "expected service ordinal, got:\n{content}"
        );
        assert!(
            content.contains("\"ops\": {"),
            "expected nested ops dict, got:\n{content}"
        );
        assert!(
            content.contains("\"service\": 7,"),
            "expected nested op ordinal, got:\n{content}"
        );
    }

    #[test]
    fn wire_ids_absent_when_unset() {
        let spec = wire_id_service(None, None);
        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;
        assert!(
            !content.contains("WIRE_IDS"),
            "no wire-id output when service has no wire-id, got:\n{content}"
        );
    }

    // Build a channel (bidirectional) service carrying `@wire-id` ordinals so the
    // compact-router twin has something to dispatch on.
    fn wire_id_channel_service(
        service_wire: Option<u64>,
        op_wire: Option<u64>,
    ) -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Match".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "play".to_string(),
                        input_type: CsilTypeExpression::Builtin("text".to_string()),
                        output_type: CsilTypeExpression::Builtin("text".to_string()),
                        direction: CsilServiceDirection::Bidirectional,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                        wire_id: op_wire,
                    }],
                    wire_id: service_wire,
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    #[test]
    fn compact_router_emitted_for_wire_id_channel_service() {
        let spec = wire_id_channel_service(Some(1), Some(5));
        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;

        // Verbose router stays byte-identical alongside the compact twin.
        assert!(
            content.contains(
                "def route_match_channel(handlers: MatchHandlers, codec: Codec, method: str, data: bytes, ctx: dict) -> None:"
            ),
            "verbose router expected, got:\n{content}"
        );
        // Compact twin dispatches on the operation ordinal, not the wire name.
        assert!(
            content.contains(
                "def route_match_channel_compact(handlers: MatchHandlers, codec: Codec, op: int, data: bytes, ctx: dict) -> None:"
            ),
            "compact router expected, got:\n{content}"
        );
        assert!(
            content.contains("if op == 5:"),
            "compact router matches the op ordinal, got:\n{content}"
        );
        assert!(
            content.contains("handlers.play(msg, ctx)"),
            "compact router dispatches to the handler, got:\n{content}"
        );
        assert!(
            content.contains("raise ServiceError(404, f\"unknown channel ordinal {op}\")"),
            "compact router has an ordinal fallthrough, got:\n{content}"
        );
    }

    #[test]
    fn compact_router_absent_without_wire_id() {
        let spec = wire_id_channel_service(None, None);
        let result =
            generate_python_code_from_serialized(&spec, &create_test_config(false)).unwrap();
        let content = &result
            .iter()
            .find(|f| f.path == "services.py")
            .unwrap()
            .content;
        // The verbose router survives; the compact twin must not appear.
        assert!(
            content.contains("def route_match_channel("),
            "verbose router expected, got:\n{content}"
        );
        assert!(
            !content.contains("_compact"),
            "no compact router without wire-ids, got:\n{content}"
        );
    }

    // --- codec ------------------------------------------------------------------

    fn opt_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type,
            occurrence: Some(CsilOccurrence::Optional),
            metadata: Vec::new(),
            doc_comments: Vec::new(),
        }
    }

    fn bare(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type,
            occurrence: None,
            metadata: Vec::new(),
            doc_comments: Vec::new(),
        }
    }

    fn group_rule_entries(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }
    }

    /// A corndogs-shaped spec: text/bytes/optional-int/map/list fields, a nested
    /// record, and a `submit-task: SubmitTaskRequest -> Task / ServiceError` op.
    fn corndogs_spec() -> CsilSpecSerialized {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let task = group_rule_entries(
            "Task",
            vec![
                bare("uuid", text()),
                bare("current_state", text()),
                bare("payload", CsilTypeExpression::Builtin("bytes".to_string())),
                opt_entry("priority", CsilTypeExpression::Builtin("int".to_string())),
                bare(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(text()),
                        value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                        occurrence: None,
                    },
                ),
                bare(
                    "tags",
                    CsilTypeExpression::Array {
                        element_type: Box::new(text()),
                        occurrence: None,
                    },
                ),
            ],
        );
        // A named map alias (`StringInt64Map = {* text => int}`) and a map-of-record
        // alias (`TaskMap = {* text => Task}`): both are transparent `TypeDef`s the
        // codec must resolve through, or the field's entries are dropped (the
        // regression). `TaskMap` exercises the record-recursion path specifically.
        let string_int_map = CsilRule {
            name: "StringInt64Map".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(text()),
                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        };
        let task_map = CsilRule {
            name: "TaskMap".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(text()),
                value: Box::new(CsilTypeExpression::Reference("Task".to_string())),
                occurrence: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        };
        let req = group_rule_entries(
            "SubmitTaskRequest",
            vec![
                bare("task", CsilTypeExpression::Reference("Task".to_string())),
                bare("queue", text()),
                bare(
                    "counts",
                    CsilTypeExpression::Reference("StringInt64Map".to_string()),
                ),
                bare(
                    "by_id",
                    CsilTypeExpression::Reference("TaskMap".to_string()),
                ),
            ],
        );
        let err = group_rule_entries(
            "ServiceError",
            vec![
                bare("code", CsilTypeExpression::Builtin("int".to_string())),
                bare("message", text()),
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
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        };
        CsilSpecSerialized {
            rules: vec![task, string_int_map, task_map, req, err, svc],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    fn codec_content(spec: &CsilSpecSerialized) -> String {
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        let result = generate_python_code_from_serialized(spec, &config).unwrap();
        result
            .iter()
            .find(|f| f.path == "codec.py")
            .expect("codec.py emitted")
            .content
            .clone()
    }

    #[test]
    fn codec_emits_self_contained_runtime_and_per_record_entry_points() {
        let codec = codec_content(&corndogs_spec());
        // The self-contained value model + codec.
        assert!(codec.contains("class CborTag:"));
        assert!(codec.contains("def cbor_encode(value: Any) -> bytes:"));
        assert!(codec.contains("def cbor_decode(data: bytes) -> Any:"));
        // bytes ride as a CBOR byte string (major type 2), not an int array.
        assert!(codec.contains("_csil_head(2, len(v), out)"));
        // Per-record byte entry points bound onto the dataclasses.
        assert!(codec.contains("Task.to_cbor = _task_to_cbor"));
        assert!(codec.contains("Task.from_cbor = staticmethod(_task_from_cbor)"));
        assert!(codec.contains("def _encode_submit_task_request_value(v: \"SubmitTaskRequest\")"));
        // A nested record recurses through the record helper, not a raw passthrough.
        assert!(codec.contains("csil_m[\"task\"] = _encode_task_value(v.task)"));
        // The codec imports the dataclasses to patch them.
        assert!(codec.contains("from .types import *"));
    }

    #[test]
    fn codec_orders_map_keys_canonically() {
        let codec = codec_content(&corndogs_spec());
        let body = codec.split("def _encode_task_value").nth(1).unwrap();
        // RFC 8949 §4.2.1: shorter keys first, then lexicographic. Among length-4
        // keys `tags` precedes `uuid`; `current_state` (len 13) is last.
        let pos_tags = body.find("\"tags\"").unwrap();
        let pos_uuid = body.find("\"uuid\"").unwrap();
        let pos_state = body.find("\"current_state\"").unwrap();
        assert!(
            pos_tags < pos_uuid && pos_uuid < pos_state,
            "map keys must be canonically ordered, got:\n{body}"
        );
    }

    #[test]
    fn codec_omits_absent_optional_and_keeps_bytes_field() {
        let codec = codec_content(&corndogs_spec());
        // An optional field is conditionally inserted (absent → omitted from the map).
        assert!(codec.contains("if csil_x is not None:"));
        assert!(codec.contains("csil_m[\"priority\"] = csil_x"));
        // A missing optional decodes to None.
        assert!(
            codec.contains(
                "priority=(None if tree.get(\"priority\") is None else tree[\"priority\"])"
            )
        );
        // `payload` stays a Python `bytes` (scalar identity in the value tree).
        assert!(codec.contains("csil_m[\"payload\"] = v.payload"));
    }

    /// Generate the corndogs `python-client` package, round-trip a Task through both
    /// `to_cbor`/`from_cbor` and the typed client over a loopback transport, and run it
    /// with `python3`. Skips cleanly when python3 is not on PATH so the suite stays
    /// portable.
    #[test]
    fn codec_round_trips_through_python() {
        let have = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok();
        if !have {
            eprintln!("skipping: no python3 on PATH");
            return;
        }

        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        let files = generate_python_code_from_serialized(&corndogs_spec(), &config).unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-python-codec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pkg = dir.join("csil_gen_pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        for f in &files {
            std::fs::write(pkg.join(&f.path), &f.content).unwrap();
        }
        std::fs::write(dir.join("driver.py"), CODEC_DRIVER_PYTHON).unwrap();

        let run = std::process::Command::new("python3")
            .arg("driver.py")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "python round-trip failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const CODEC_DRIVER_PYTHON: &str = r#"import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from csil_gen_pkg.types import Task, SubmitTaskRequest
from csil_gen_pkg.codec import *  # binds to_cbor/from_cbor onto the dataclasses
from csil_gen_pkg.client import CorndogsClient


def make_task(priority):
    return Task(
        uuid="u-123",
        current_state="PENDING",
        payload=b"\xde\xad\xbe",
        priority=priority,
        labels={"a": 1, "b": 2},
        tags=["x", "y"],
    )


def make_req(priority, queue="default"):
    # `counts` is a named scalar-map alias; `by_id` a map-of-record alias. Both must
    # survive the round-trip with their entries intact.
    return SubmitTaskRequest(
        task=make_task(priority),
        queue=queue,
        counts={"pending": 3, "done": 9},
        by_id={"first": make_task(1), "second": make_task(2)},
    )


# Direct codec round-trip through the nested record.
req = make_req(7)
back = SubmitTaskRequest.from_cbor(req.to_cbor())
assert back.task.uuid == "u-123"
assert back.task.current_state == "PENDING"
assert back.task.payload == b"\xde\xad\xbe"
assert back.task.priority == 7
assert back.task.labels == {"a": 1, "b": 2}
assert back.task.tags == ["x", "y"]
assert back.queue == "default"

# The named map alias keeps its entries (the regression dropped these).
assert back.counts == {"pending": 3, "done": 9}

# The map-of-record alias reconstructs each value as a Task, not a raw dict.
assert set(back.by_id.keys()) == {"first", "second"}
assert isinstance(back.by_id["first"], Task)
assert back.by_id["first"].uuid == "u-123"
assert back.by_id["first"].priority == 1
assert back.by_id["second"].priority == 2
assert back.by_id["first"].labels == {"a": 1, "b": 2}

# An absent optional must round-trip to None.
back2 = SubmitTaskRequest.from_cbor(make_req(None, "q").to_cbor())
assert back2.task.priority is None


# Typed client over a loopback carrier: decode the request, encode its task back.
class Loopback:
    def call(self, service, method, req):
        assert service == "corndogs"
        assert method == "SubmitTask"
        decoded = SubmitTaskRequest.from_cbor(req)
        assert decoded.counts == {"pending": 3, "done": 9}
        assert decoded.by_id["second"].priority == 2
        return decoded.task.to_cbor()


result = CorndogsClient(Loopback()).submit_task(make_req(7))
assert result.uuid == "u-123"
assert result.payload == b"\xde\xad\xbe"
assert result.priority == 7
assert result.labels == {"a": 1, "b": 2}
assert result.tags == ["x", "y"]

print("ok")
"#;

    /// Generate the default (`both`) `python-client` package and drive the ASYNC twin
    /// through `asyncio.run(...)` over an async loopback transport: the typed request
    /// is awaited across the seam and the typed response survives the round-trip.
    /// Skips cleanly when python3 is absent, mirroring the sync round-trip test.
    #[test]
    fn async_client_round_trips_through_python() {
        let have = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok();
        if !have {
            eprintln!("skipping: no python3 on PATH");
            return;
        }

        // No `client_style` option → default `both`, so both client.py and the async
        // twin client_async.py are emitted; the driver exercises the twin.
        let mut config = create_test_config(false);
        config.target = "python-client".to_string();
        let files = generate_python_code_from_serialized(&corndogs_spec(), &config).unwrap();
        assert!(files.iter().any(|f| f.path == "client_async.py"));

        let dir = std::env::temp_dir().join(format!("csilgen-python-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pkg = dir.join("csil_gen_pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        for f in &files {
            std::fs::write(pkg.join(&f.path), &f.content).unwrap();
        }
        std::fs::write(dir.join("driver.py"), ASYNC_CLIENT_DRIVER_PYTHON).unwrap();

        let run = std::process::Command::new("python3")
            .arg("driver.py")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "python async round-trip failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const ASYNC_CLIENT_DRIVER_PYTHON: &str = r#"import asyncio
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from csil_gen_pkg.types import Task, SubmitTaskRequest
from csil_gen_pkg.codec import *  # binds to_cbor/from_cbor onto the dataclasses
from csil_gen_pkg.client_async import CorndogsAsyncClient


def make_task(priority):
    return Task(
        uuid="u-123",
        current_state="PENDING",
        payload=b"\xde\xad\xbe",
        priority=priority,
        labels={"a": 1, "b": 2},
        tags=["x", "y"],
    )


def make_req(priority, queue="default"):
    return SubmitTaskRequest(
        task=make_task(priority),
        queue=queue,
        counts={"pending": 3, "done": 9},
        by_id={"first": make_task(1), "second": make_task(2)},
    )


# Async loopback carrier: the seam is a coroutine the generated client awaits. It
# decodes the request and encodes its task back, after yielding to the event loop so
# the await is a real suspension point, not a synchronous shortcut.
class AsyncLoopback:
    async def call(self, service, method, req):
        assert service == "corndogs"
        assert method == "SubmitTask"
        await asyncio.sleep(0)
        decoded = SubmitTaskRequest.from_cbor(req)
        assert decoded.counts == {"pending": 3, "done": 9}
        assert decoded.by_id["second"].priority == 2
        return decoded.task.to_cbor()


async def main():
    result = await CorndogsAsyncClient(AsyncLoopback()).submit_task(make_req(7))
    assert result.uuid == "u-123"
    assert result.payload == b"\xde\xad\xbe"
    assert result.priority == 7
    assert result.labels == {"a": 1, "b": 2}
    assert result.tags == ["x", "y"]


asyncio.run(main())
print("ok")
"#;

    /// `emit_packages` must be the sole trigger: absent or not listing Python leaves
    /// the default flat layout untouched; listing Python adds exactly a `pyproject.toml`
    /// carrying the requested distribution name and version.
    #[test]
    fn pyproject_emitted_iff_emit_packages_includes_python() {
        // No emit_packages at all → no pyproject, flat layout preserved.
        let base = create_test_config(false);
        let plain = generate_python_code_from_serialized(&corndogs_spec(), &base).unwrap();
        assert!(plain.iter().all(|f| f.path != "pyproject.toml"));
        assert!(plain.iter().any(|f| f.path == "types.py"));

        // emit_packages present but Python not listed → still no pyproject.
        let mut other = create_test_config(false);
        other.options.insert(
            "emit_packages".to_string(),
            serde_json::json!(["go", "rust"]),
        );
        let other_files = generate_python_code_from_serialized(&corndogs_spec(), &other).unwrap();
        assert!(other_files.iter().all(|f| f.path != "pyproject.toml"));
        assert!(other_files.iter().any(|f| f.path == "types.py"));

        // Python listed → pyproject emitted with the configured coordinates, and the
        // modules relocate under the import-package directory.
        let mut pkg = create_test_config(false);
        pkg.target = "python-client".to_string();
        pkg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        pkg.options.insert(
            "package_name".to_string(),
            serde_json::json!("acme-corndogs"),
        );
        pkg.options
            .insert("package_version".to_string(), serde_json::json!("2.3.4"));
        let files = generate_python_code_from_serialized(&corndogs_spec(), &pkg).unwrap();

        let toml = files
            .iter()
            .find(|f| f.path == "pyproject.toml")
            .expect("pyproject.toml emitted in package mode");
        assert!(toml.content.contains("name = \"acme-corndogs\""));
        assert!(toml.content.contains("version = \"2.3.4\""));
        // The `-` is illegal in an import name, so discovery points at the sanitized dir.
        assert!(toml.content.contains("packages = [\"acme_corndogs\"]"));
        assert!(toml.content.contains("requires-python"));

        // Every module now lives under the import package; only the dist-root metadata
        // files (`pyproject.toml`, `genquickstart.md`) are left beside it.
        for f in &files {
            if f.path == "pyproject.toml" || f.path == "genquickstart.md" {
                continue;
            }
            assert!(
                f.path.starts_with("acme_corndogs/"),
                "expected {} under import package dir",
                f.path
            );
        }
        assert!(files.iter().any(|f| f.path == "acme_corndogs/__init__.py"));
        // The README rides at the dist root alongside the pyproject in package mode.
        let readme = files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .expect("genquickstart.md emitted at dist root in package mode");
        assert!(readme.content.contains("# acme-corndogs"));
        assert!(readme.content.contains("## CSIL-RPC (HTTP)"));
    }

    /// Defaults apply when the coordinate options are omitted entirely.
    #[test]
    fn package_mode_defaults_distribution_and_version() {
        let mut cfg = create_test_config(false);
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        let files = generate_python_code_from_serialized(&corndogs_spec(), &cfg).unwrap();
        let toml = files
            .iter()
            .find(|f| f.path == "pyproject.toml")
            .expect("pyproject emitted");
        assert!(toml.content.contains("name = \"csilgen_client\""));
        assert!(toml.content.contains("version = \"0.1.0\""));
        assert!(files.iter().any(|f| f.path == "csilgen_client/__init__.py"));
    }

    /// In package mode the README is emitted by default, and an explicit
    /// `emit_readme: false` suppresses only the README — `pyproject.toml` and the
    /// relocated modules are unaffected.
    #[test]
    fn emit_readme_false_suppresses_only_readme_in_package_mode() {
        let mut cfg = create_test_config(false);
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));

        // Default: README present alongside the pyproject.
        let files = generate_python_code_from_serialized(&corndogs_spec(), &cfg).unwrap();
        assert!(
            files.iter().any(|f| f.path == "genquickstart.md"),
            "README must be emitted by default in package mode"
        );

        // Explicit opt-out: README gone, pyproject and modules still present.
        cfg.options
            .insert("emit_readme".to_string(), serde_json::json!(false));
        let files = generate_python_code_from_serialized(&corndogs_spec(), &cfg).unwrap();
        assert!(
            !files.iter().any(|f| f.path == "genquickstart.md"),
            "emit_readme: false must suppress the README"
        );
        assert!(
            files.iter().any(|f| f.path == "pyproject.toml"),
            "emit_readme: false must leave pyproject.toml untouched"
        );
        assert!(
            files.iter().any(|f| f.path == "csilgen_client/__init__.py"),
            "emit_readme: false must leave the relocated modules untouched"
        );
    }

    /// Generate a real `python-client` package into a temp dir, then prove the artifact
    /// is publishable-shaped: its `pyproject.toml` parses as TOML and the package imports
    /// cleanly with the output dir on `sys.path`. Skips when python3 is unavailable.
    #[test]
    fn package_mode_pyproject_parses_and_imports() {
        let have = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok();
        if !have {
            eprintln!("skipping: no python3 on PATH");
            return;
        }

        let mut cfg = create_test_config(false);
        cfg.target = "python-client".to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options.insert(
            "package_name".to_string(),
            serde_json::json!("corndogs-sdk"),
        );
        cfg.options
            .insert("package_version".to_string(), serde_json::json!("1.2.3"));
        let files = generate_python_code_from_serialized(&corndogs_spec(), &cfg).unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-python-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in &files {
            let dest = dir.join(&f.path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, &f.content).unwrap();
        }

        // (a) The pyproject must parse as TOML and (b) the package must import with the
        // output dir on the path. `tomllib` is stdlib from 3.11; fall back to a plain
        // existence check on older interpreters so the test never spuriously fails.
        let checker = r#"import os, sys
root = os.path.dirname(os.path.abspath(__file__))
try:
    import tomllib
    with open(os.path.join(root, "pyproject.toml"), "rb") as fh:
        data = tomllib.load(fh)
    assert data["project"]["name"] == "corndogs-sdk", data
    assert data["project"]["version"] == "1.2.3", data
    assert data["tool"]["setuptools"]["packages"] == ["corndogs_sdk"], data
except ModuleNotFoundError:
    assert os.path.exists(os.path.join(root, "pyproject.toml"))
sys.path.insert(0, root)
import corndogs_sdk
import corndogs_sdk.types
import corndogs_sdk.client
print("ok")
"#;
        std::fs::write(dir.join("check.py"), checker).unwrap();

        let run = std::process::Command::new("python3")
            .arg("check.py")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "package validation failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A minimal ping/pong spec whose op echoes its request type as its response, so a
    /// hermetic echo carrier round-trips a typed value end to end.
    fn pingpong_spec() -> CsilSpecSerialized {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let int = || CsilTypeExpression::Builtin("int".to_string());
        let ping = group_rule_entries("Ping", vec![bare("message", text()), bare("count", int())]);
        let svc = CsilRule {
            name: "PingService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "ping".to_string(),
                    input_type: CsilTypeExpression::Reference("Ping".to_string()),
                    output_type: CsilTypeExpression::Reference("Ping".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        };
        CsilSpecSerialized {
            rules: vec![ping, svc],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    /// The 3-transport verification spec: an `Echo` service with a `->` op (`ping`) and a
    /// record-typed `<->` op (`pulse`), both over `Ping`/`Pong` records, so the codec,
    /// client, and channel router/encoder all render against real ops.
    fn transports_spec() -> CsilSpecSerialized {
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let ping = group_rule_entries("Ping", vec![bare("msg", text())]);
        let pong = group_rule_entries("Pong", vec![bare("msg", text())]);
        let op = |name: &str, dir: CsilServiceDirection| CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference("Ping".to_string()),
            output_type: CsilTypeExpression::Reference("Pong".to_string()),
            direction: dir,
            position: create_test_position(),
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
            position: create_test_position(),
            doc_comments: Vec::new(),
        };
        CsilSpecSerialized {
            rules: vec![ping, pong, svc],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    /// Generate a package for `spec` at `target` with the genquickstart README, returning
    /// its `genquickstart.md` content. RPC + datagram examples come from the `python`
    /// (client) surface; the Events router lives on the `python-server` surface.
    fn transports_readme(spec: &CsilSpecSerialized, target: &str) -> String {
        let mut cfg = create_test_config(false);
        cfg.target = target.to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options
            .insert("package_name".to_string(), serde_json::json!("echo-sdk"));
        let files = generate_python_code_from_serialized(spec, &cfg).unwrap();
        files
            .into_iter()
            .find(|f| f.path == "genquickstart.md")
            .expect("genquickstart.md emitted")
            .content
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

    /// The `python` code block under `heading` (the section's first fenced block).
    fn section_block(md: &str, heading: &str) -> String {
        let sec = section(md, heading);
        let start =
            sec.find("```python\n").expect("section has a python block") + "```python\n".len();
        let rest = &sec[start..];
        let end = rest.find("\n```").expect("python block is closed");
        rest[..end].to_string()
    }

    /// Absolute path to the in-repo `csilgen_transport` library so the hermetic tests
    /// can import it without installing anything.
    fn transport_lib_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/python")
    }

    fn have_python3() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn genquickstart_has_all_three_sections_by_default() {
        let readme = transports_readme(&transports_spec(), "python-client");
        for heading in [
            "## CSIL-RPC (HTTP)",
            "## CSIL-Events (TLS)",
            "## CSIL-Datagrams (UDP)",
        ] {
            assert!(
                readme.contains(heading),
                "default genquickstart must contain {heading}:\n{readme}"
            );
        }
        // Install line pulls in the transport library alongside the package.
        assert!(readme.contains("pip install echo-sdk csilgen-transport"));
    }

    #[test]
    fn genquickstart_transports_subset_emits_only_listed_sections() {
        let mut cfg = create_test_config(false);
        cfg.target = "python-client".to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options.insert(
            "genquickstart_transports".to_string(),
            serde_json::json!(["rpc"]),
        );
        let files = generate_python_code_from_serialized(&transports_spec(), &cfg).unwrap();
        let readme = &files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .unwrap()
            .content;
        assert!(readme.contains("## CSIL-RPC (HTTP)"));
        assert!(
            !readme.contains("## CSIL-Events (TLS)"),
            "events suppressed"
        );
        assert!(
            !readme.contains("## CSIL-Datagrams (UDP)"),
            "datagrams suppressed"
        );
    }

    #[test]
    fn genquickstart_transports_unknown_or_empty_falls_back_to_all() {
        for opt in [serde_json::json!([]), serde_json::json!(["bogus"])] {
            let mut cfg = create_test_config(false);
            cfg.target = "python-client".to_string();
            cfg.options
                .insert("emit_packages".to_string(), serde_json::json!(["python"]));
            cfg.options
                .insert("genquickstart_transports".to_string(), opt.clone());
            let files = generate_python_code_from_serialized(&transports_spec(), &cfg).unwrap();
            let readme = &files
                .iter()
                .find(|f| f.path == "genquickstart.md")
                .unwrap()
                .content;
            assert!(
                readme.contains("## CSIL-RPC (HTTP)")
                    && readme.contains("## CSIL-Events (TLS)")
                    && readme.contains("## CSIL-Datagrams (UDP)"),
                "{opt} must fall back to all three sections"
            );
        }
    }

    /// Each section names its library imports and the lib-owned seam (not a hand-rolled
    /// envelope), plus the generated surface it drives.
    #[test]
    fn each_section_names_its_library_imports_and_seam() {
        let readme = transports_readme(&transports_spec(), "python-client");
        let rpc = section(&readme, "## CSIL-RPC (HTTP)");
        let events = section(&readme, "## CSIL-Events (TLS)");
        let datagrams = section(&readme, "## CSIL-Datagrams (UDP)");

        // RPC: the library envelope types + the canonical HTTP mount, no hand-rolled map.
        assert!(rpc.contains("from csilgen_transport.rpc import RpcRequest, RpcResponse"));
        assert!(rpc.contains("/csil/v1/rpc"));
        assert!(rpc.contains("RpcRequest(service, op, payload=req).encode()"));
        assert!(rpc.contains("RpcResponse.decode(raw).into_transport_error()"));
        assert!(rpc.contains("EchoClient(HttpRpcCarrier(\"http://localhost:5080\"))"));
        assert!(rpc.contains("client.ping(Ping(msg=\"example\"))"));

        // Events: the lib's handshake/framing/heartbeat surface + the generated router.
        assert!(events.contains("from csilgen_transport.carrier import StreamCarrier"));
        assert!(events.contains("Event, Hello, HelloAck, Heartbeat, Profile, control"));
        assert!(events.contains("$hello") || events.contains("Hello("));
        assert!(events.contains("route_echo_channel, encode_echo_pulse, EchoHandlers"));
        assert!(events.contains("encode_echo_pulse(codec, Pong(msg=\"example\"))"));
        assert!(events.contains("route_echo_channel(handlers, codec, ev.event, ev.payload, {})"));
        assert!(events.contains("control.PING_NAME"));

        // Datagrams: the lib's Datagram + UDP carrier seam, and the no-sync-response warning.
        assert!(datagrams.contains("from csilgen_transport.datagrams import Datagram"));
        assert!(datagrams.contains("from csilgen_transport.carrier import UdpDatagramCarrier"));
        assert!(datagrams.contains("Datagram(OP_ORD, 0, req.to_cbor()).encode()"));
        assert!(datagrams.contains("Pong.from_cbor(dg.payload)"));
        assert!(datagrams.contains("NO synchronous response"));
    }

    /// A spec with only a `->` op keeps the Events handshake + heartbeat but replaces the
    /// dispatch wiring with a note (no generated channel router import).
    #[test]
    fn events_section_without_channel_ops_emits_a_note() {
        let readme = transports_readme(&pingpong_spec(), "python-client");
        let events = section(&readme, "## CSIL-Events (TLS)");
        assert!(events.contains("Hello("));
        assert!(
            events.contains("no <->/<- operations"),
            "must note the absence of channel ops:\n{events}"
        );
        assert!(
            !events.contains("route_ping_channel"),
            "no channel router import when there are no channel ops"
        );
    }

    /// CSIL-RPC: run the emitted HTTP-carrier example under python3 with
    /// `urllib.request.urlopen` stubbed by an in-process CSIL-RPC echo built on the
    /// library's `RpcRequest`/`RpcResponse`. A green run proves the carrier builds the
    /// envelope via the lib, drives `urlopen`, and the typed client decodes the reply
    /// round-trip. Hermetic (no sockets). Skips when python3 is unavailable.
    #[test]
    fn genquickstart_rpc_section_round_trips_through_python() {
        if !have_python3() {
            eprintln!("skipping: no python3 on PATH");
            return;
        }
        let mut cfg = create_test_config(false);
        cfg.target = "python-client".to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options
            .insert("package_name".to_string(), serde_json::json!("echo-sdk"));
        let files = generate_python_code_from_serialized(&transports_spec(), &cfg).unwrap();
        let readme = files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .unwrap()
            .content
            .clone();
        let block = section_block(&readme, "## CSIL-RPC (HTTP)");

        let dir = std::env::temp_dir().join(format!("csilgen-py-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in &files {
            let dest = dir.join(&f.path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, &f.content).unwrap();
        }
        std::fs::write(dir.join("quickstart.py"), &block).unwrap();
        std::fs::write(dir.join("driver.py"), RPC_ROUND_TRIP_DRIVER_PYTHON).unwrap();

        let run = std::process::Command::new("python3")
            .arg("driver.py")
            .current_dir(&dir)
            .env(
                "PYTHONPATH",
                format!("{}:{}", dir.display(), transport_lib_path().display()),
            )
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "python RPC round-trip failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const RPC_ROUND_TRIP_DRIVER_PYTHON: &str = r#"from csilgen_transport.rpc import RpcRequest, RpcResponse

from echo_sdk import Ping, EchoClient

import quickstart  # defines the README's HttpRpcCarrier


class _FakeResp:
    def __init__(self, data):
        self._data = data

    def read(self):
        return self._data

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def _echo_urlopen(req, timeout=None):
    # Hermetic, socket-free stand-in for the server: decode the CSIL-RPC request via the
    # library and echo its payload back as a status-0 `Pong` reply (Ping/Pong share shape).
    rpc_req = RpcRequest.decode(req.data)
    assert rpc_req.service == "echo", rpc_req
    assert rpc_req.op == "Ping", rpc_req
    return _FakeResp(RpcResponse.ok("Pong", rpc_req.payload).encode())


import urllib.request

urllib.request.urlopen = _echo_urlopen

client = EchoClient(quickstart.HttpRpcCarrier("http://unused.invalid"))
resp = client.ping(Ping(msg="hello"))
assert resp.msg == "hello", resp
print("ok")
"#;

    /// CSIL-Datagrams: run the emitted UDP example under python3 with the real carrier
    /// swapped for the library's in-process `LoopbackDatagramCarrier`, seeded with one
    /// response datagram. Proves the example `Datagram`-encodes the request via the
    /// generated codec, sends it, and decodes an inbound response datagram back into the
    /// typed response. Hermetic (no sockets). Skips when python3 is unavailable.
    #[test]
    fn genquickstart_datagrams_section_round_trips_through_python() {
        if !have_python3() {
            eprintln!("skipping: no python3 on PATH");
            return;
        }
        let mut cfg = create_test_config(false);
        cfg.target = "python-client".to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options
            .insert("package_name".to_string(), serde_json::json!("echo-sdk"));
        let files = generate_python_code_from_serialized(&transports_spec(), &cfg).unwrap();
        let readme = files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .unwrap()
            .content
            .clone();
        // Swap the real UDP carrier for the seeded loopback (sockets are killed in the
        // sandbox; the lib loopback exercises the same send/recv codec path in-process).
        let block = section_block(&readme, "## CSIL-Datagrams (UDP)").replace(
            "carrier = open_udp_carrier(\"localhost\", 9000)",
            "carrier = _seed_loopback()",
        );
        let driver = format!("{DATAGRAM_LOOPBACK_PREAMBLE_PYTHON}\n{block}");

        let dir = std::env::temp_dir().join(format!("csilgen-py-dgram-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in &files {
            let dest = dir.join(&f.path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, &f.content).unwrap();
        }
        std::fs::write(dir.join("driver.py"), driver).unwrap();

        let run = std::process::Command::new("python3")
            .arg("driver.py")
            .current_dir(&dir)
            .env(
                "PYTHONPATH",
                format!("{}:{}", dir.display(), transport_lib_path().display()),
            )
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "python datagrams round-trip failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(
            String::from_utf8_lossy(&run.stdout).contains("late response"),
            "datagram recv path did not decode the seeded response: {}",
            String::from_utf8_lossy(&run.stdout)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Seeds a library `LoopbackDatagramCarrier` with one response datagram so the
    /// datagrams example (carrier line swapped) sends to and receives from it in-process.
    /// `_seed_loopback` is called at runtime, after the example's own module-level imports
    /// have bound `Datagram`, `Pong`, and `OP_ORD`.
    const DATAGRAM_LOOPBACK_PREAMBLE_PYTHON: &str = r#"from csilgen_transport.carrier import LoopbackDatagramCarrier


def _seed_loopback():
    lb = LoopbackDatagramCarrier()
    lb.push_inbound(Datagram(OP_ORD, 0, Pong(msg="example").to_cbor()).encode())
    return lb
"#;

    /// CSIL-Events: the full TLS session is an interactive, socket-driven loop, so it is
    /// verified compile-only (import-resolves) against the generated server package + the
    /// library — proving the handshake, heartbeat, Codec, and `route_echo_channel`
    /// dispatch wiring all bind. The `if __name__` guard keeps the session from running on
    /// import. The RPC + datagrams examples above are additionally *run*. Skips when
    /// python3 is unavailable.
    #[test]
    fn genquickstart_events_section_imports_and_binds() {
        if !have_python3() {
            eprintln!("skipping: no python3 on PATH");
            return;
        }
        // The channel router/handler/encoder live on the server surface, so stage that
        // package for the Events compile-check.
        let mut cfg = create_test_config(false);
        cfg.target = "python-server".to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options
            .insert("package_name".to_string(), serde_json::json!("echo-sdk"));
        let files = generate_python_code_from_serialized(&transports_spec(), &cfg).unwrap();
        let readme = files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .unwrap()
            .content
            .clone();
        let block = section_block(&readme, "## CSIL-Events (TLS)");

        let dir = std::env::temp_dir().join(format!("csilgen-py-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in &files {
            let dest = dir.join(&f.path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, &f.content).unwrap();
        }
        std::fs::write(dir.join("events_session.py"), &block).unwrap();
        // Importing the module resolves every symbol (lib + generated router/encoder) and
        // executes the class/function definitions without opening a socket.
        std::fs::write(
            dir.join("check.py"),
            "import events_session\nprint(\"ok\")\n",
        )
        .unwrap();

        let run = std::process::Command::new("python3")
            .arg("check.py")
            .current_dir(&dir)
            .env(
                "PYTHONPATH",
                format!("{}:{}", dir.display(), transport_lib_path().display()),
            )
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "events example failed to import/bind:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The definitive self-containment check: generate the SINGLE package a user publishes
    /// (the default `python` target), then drive ALL THREE genquickstart sections against
    /// that one staged package + the transport lib. The RPC client (RPC section), the
    /// generated channel router (Events section), and the per-type codec (every section)
    /// must all resolve from the one package — proving package mode emits every surface the
    /// quickstart references. RPC + Datagrams are run end-to-end (hermetic, socket-free);
    /// Events is import-resolved (its session is an interactive socket loop). Skips when
    /// python3 is unavailable.
    #[test]
    fn genquickstart_all_sections_resolve_against_single_package() {
        if !have_python3() {
            eprintln!("skipping: no python3 on PATH");
            return;
        }
        // The default `python` target is what a user publishes; package mode must make it
        // carry both the client (RPC/Datagrams) and the router (Events) surfaces.
        let mut cfg = create_test_config(false);
        cfg.target = "python".to_string();
        cfg.options
            .insert("emit_packages".to_string(), serde_json::json!(["python"]));
        cfg.options
            .insert("package_name".to_string(), serde_json::json!("echo-sdk"));
        let files = generate_python_code_from_serialized(&transports_spec(), &cfg).unwrap();

        // The single package must carry BOTH surfaces: the client module (RPC/Datagrams) and
        // the services module (the channel router the Events section dispatches into).
        let staged: std::collections::HashSet<&str> =
            files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            staged.contains("echo_sdk/client.py"),
            "single package must ship the typed client: {staged:?}"
        );
        assert!(
            staged.contains("echo_sdk/services.py"),
            "single package must ship the channel router: {staged:?}"
        );

        let readme = files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .unwrap()
            .content
            .clone();
        let rpc_block = section_block(&readme, "## CSIL-RPC (HTTP)");
        let events_block = section_block(&readme, "## CSIL-Events (TLS)");
        let dgram_block = section_block(&readme, "## CSIL-Datagrams (UDP)").replace(
            "carrier = open_udp_carrier(\"localhost\", 9000)",
            "carrier = _seed_loopback()",
        );

        let dir = std::env::temp_dir().join(format!("csilgen-py-single-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in &files {
            let dest = dir.join(&f.path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, &f.content).unwrap();
        }
        // RPC: the README block (defines HttpRpcCarrier) + the hermetic driver that stubs
        // urlopen and drives the typed client.
        std::fs::write(dir.join("quickstart.py"), &rpc_block).unwrap();
        std::fs::write(dir.join("rpc_driver.py"), RPC_ROUND_TRIP_DRIVER_PYTHON).unwrap();
        // Datagrams: the README block (carrier line swapped) + the loopback preamble.
        std::fs::write(
            dir.join("dgram_driver.py"),
            format!("{DATAGRAM_LOOPBACK_PREAMBLE_PYTHON}\n{dgram_block}"),
        )
        .unwrap();
        // Events: import-resolve the session module (the router/handler/codec all bind).
        std::fs::write(dir.join("events_session.py"), &events_block).unwrap();
        std::fs::write(
            dir.join("events_check.py"),
            "import events_session\nprint(\"ok\")\n",
        )
        .unwrap();

        let pythonpath = format!("{}:{}", dir.display(), transport_lib_path().display());
        let run = |script: &str| -> std::process::Output {
            std::process::Command::new("python3")
                .arg(script)
                .current_dir(&dir)
                .env("PYTHONPATH", &pythonpath)
                .output()
                .unwrap()
        };

        let rpc = run("rpc_driver.py");
        assert!(
            rpc.status.success() && String::from_utf8_lossy(&rpc.stdout).trim() == "ok",
            "RPC section did not resolve against the single package:\n{}{}",
            String::from_utf8_lossy(&rpc.stdout),
            String::from_utf8_lossy(&rpc.stderr)
        );

        let dgram = run("dgram_driver.py");
        assert!(
            dgram.status.success()
                && String::from_utf8_lossy(&dgram.stdout).contains("late response"),
            "Datagrams section did not resolve against the single package:\n{}{}",
            String::from_utf8_lossy(&dgram.stdout),
            String::from_utf8_lossy(&dgram.stderr)
        );

        let events = run("events_check.py");
        assert!(
            events.status.success() && String::from_utf8_lossy(&events.stdout).trim() == "ok",
            "Events router section did not resolve against the single package:\n{}{}",
            String::from_utf8_lossy(&events.stdout),
            String::from_utf8_lossy(&events.stderr)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
