//! Dart code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target dart` from `csilgen_dart_generator.wasm`.
//! Emits idiomatic Dart 3 source: `final class` records with const constructors
//! and `required` named params, `sealed class` hierarchies for output choices
//! (exhaustive `switch`), transport-agnostic clients, server handler interfaces,
//! and verbose/compact routers. The generator emits *shapes and routing only*,
//! never wire bytes — the `csilgen_transport` package owns the CBOR wire.

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilControlOperator, CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry,
    CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType,
    CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint, CsilSpecSerialized,
    CsilTypeExpression, CsilValidationConstraint, CsilgenError, GeneratedFile, GeneratedFiles,
    GenerationStats, GeneratorCapability, GeneratorConfig, GeneratorMetadata, GeneratorWarning,
    Result, WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "dart-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Dart code generator (Dart 3, pure-Dart, Flutter-compatible)".to_string(),
        target: "dart".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
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

    let files = generate_dart_code(&input.csil_spec, &input.config)
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
// Identifier mapping
// ---------------------------------------------------------------------------

// Dart reserved words / built-in identifiers that cannot be a plain member or
// type name. A CSIL name colliding with one is suffixed with `_` (a valid and
// idiomatic Dart escape) so the generated source still compiles. The wire key is
// never touched by this — only the Dart identifier is.
const DART_RESERVED: &[&str] = &[
    "abstract",
    "as",
    "assert",
    "async",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "covariant",
    "default",
    "deferred",
    "do",
    "dynamic",
    "else",
    "enum",
    "export",
    "extends",
    "extension",
    "external",
    "factory",
    "false",
    "final",
    "finally",
    "for",
    "Function",
    "get",
    "hide",
    "if",
    "implements",
    "import",
    "in",
    "interface",
    "is",
    "late",
    "library",
    "mixin",
    "new",
    "null",
    "on",
    "operator",
    "part",
    "required",
    "rethrow",
    "return",
    "sealed",
    "set",
    "show",
    "static",
    "super",
    "switch",
    "sync",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "var",
    "void",
    "when",
    "while",
    "with",
    "yield",
];

fn escape_reserved(name: &str) -> String {
    if DART_RESERVED.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// A Dart type name: `UpperCamelCase`. CSIL types are already PascalCase, so this
/// is effectively identity, but normalizing guards against snake/kebab spellings.
fn dart_type_name(name: &str) -> String {
    escape_reserved(&name.to_case(Case::Pascal))
}

/// A Dart member name: `lowerCamelCase`, with a reserved-word guard.
fn dart_member_name(name: &str) -> String {
    escape_reserved(&name.to_case(Case::Camel))
}

/// The verbatim wire key for a group entry's key — never case-transformed, so a
/// Dart client and a Go/Python/TS peer agree on the CBOR map keys.
fn wire_key(key: &CsilGroupKey) -> Option<String> {
    match key {
        CsilGroupKey::Bare(name) => Some(name.clone()),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => Some(name.clone()),
        _ => None,
    }
}

/// The Dart member name plus verbatim wire key for an entry, or `None` for a
/// keyless/typed-key entry that has no stable name (skipped consistently across
/// every emitter so a field is never half-declared).
fn entry_names(entry: &CsilGroupEntry) -> Option<(String, String)> {
    let wire = match &entry.key {
        Some(key) => wire_key(key)?,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => name.clone(),
            _ => return None,
        },
    };
    Some((dart_member_name(&wire), wire))
}

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// In-memory Dart type chosen for the CSIL `decimal` core type. The wire form is
/// CBOR tag 4 either way; this only changes the generated field's static type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper (no third-party dependency).
    Csil,
    /// Use `package:decimal`'s `Decimal`.
    Library,
}

/// Which client surface(s) to emit. Only the transport seam (the byte carrier that
/// performs the round-trip) and the per-method return types turn async; the codec
/// stays synchronous because it does no I/O. `Both` is the default so every consumer
/// keeps the blocking client they had plus an async twin, and can opt down to one
/// shape explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStyle {
    /// Blocking-only client at `services.gen.dart` (today's output, unchanged).
    Sync,
    /// `Future`-returning client, a drop-in replacement at `services.gen.dart` with
    /// the same symbol names. For hosts whose carrier is async.
    Async,
    /// Emit both — the sync client at `services.gen.dart` plus an async twin at
    /// `services.async.gen.dart` whose symbols carry an `Async` marker so the two
    /// coexist in one package (and one barrel) without name collisions. Default.
    Both,
}

/// The shape of one emitted client file: whether its methods are async and the
/// symbol marker that keeps an async twin distinct from the sync client when both
/// are emitted into the same package. `marker` is empty for a stand-alone client
/// (sync, or async-as-drop-in) and `"Async"` for the twin in `Both` mode.
#[derive(Debug, Clone, Copy)]
struct ClientShape {
    is_async: bool,
    marker: &'static str,
}

impl ClientShape {
    /// `async ` keyword (trailing space) for the method body opener, else empty.
    fn async_kw(&self) -> &'static str {
        if self.is_async { "async " } else { "" }
    }

    /// `await ` keyword (trailing space) before the transport call, else empty.
    fn await_kw(&self) -> &'static str {
        if self.is_async { "await " } else { "" }
    }

    /// Wrap a method's return type in `Future<...>` when async.
    fn ret(&self, ty: &str) -> String {
        if self.is_async {
            format!("Future<{ty}>")
        } else {
            ty.to_string()
        }
    }

    /// The transport seam type name (`CsilTransport`, or `AsyncCsilTransport` for
    /// the twin).
    fn transport_name(&self) -> String {
        format!("{}CsilTransport", self.marker)
    }

    /// A per-service client class name (`FooClient`, or `FooAsyncClient` for the twin).
    fn client_name(&self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }
}

struct DartConfig {
    decimal_mapping: DecimalMapping,
    library_name: String,
    client_style: ClientStyle,
}

impl DartConfig {
    fn decimal_dart_type(&self) -> &'static str {
        match self.decimal_mapping {
            DecimalMapping::Csil => "CsilDecimal",
            DecimalMapping::Library => "Decimal",
        }
    }
}

/// Map a CSIL type expression to its idiomatic Dart type. Optional occurrence
/// becomes a nullable `T?`. A fixed-shape tuple becomes a Dart 3 record type
/// (positional, or named when its entries are keyed) — the one place Dart 3's
/// records beat the older generators' anonymous-struct fallbacks.
fn map_type(
    type_expr: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    decimal: &str,
) -> String {
    let base = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => "int".to_string(),
            "float" => "double".to_string(),
            // `tstr`/`bstr` are the CDDL spellings; map identically to text/bytes.
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "Uint8List".to_string(),
            "bool" => "bool".to_string(),
            // CBOR tag 0, RFC3339, UTC; Dart's DateTime carries it (kept UTC).
            "timestamp" => "DateTime".to_string(),
            "decimal" => decimal.to_string(),
            // `any`/`null`/`nil` carry no single static type — `Object?` is Dart's
            // top type and is already nullable, so the occurrence guard below won't
            // double the `?`.
            "any" | "nil" | "null" => "Object?".to_string(),
            other => dart_type_name(other),
        },
        CsilTypeExpression::Reference(name) => dart_type_name(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("List<{}>", map_type(element_type, &None, decimal))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            format!(
                "Map<{}, {}>",
                map_type(key, &None, decimal),
                map_type(value, &None, decimal)
            )
        }
        CsilTypeExpression::Tuple(group) => dart_record_type(group, decimal),
        CsilTypeExpression::Constrained { base_type, .. } => {
            // `.size`/`.regex`/comparison constraints are validation, not a type;
            // map the base and re-apply occurrence at the outer call.
            return map_type(base_type, occurrence, decimal);
        }
        // A choice of text plus string literals (`text / "a" / "b"`) is a closed
        // string set — its wire form is just the string, so `String` is both
        // idiomatic and what the (de)serialization cast expects.
        CsilTypeExpression::Choice(choices) if is_string_choice(choices) => "String".to_string(),
        // Other inline choices/compound forms have no single static Dart type;
        // `Object?` keeps the field usable while a top-level choice rule gets a
        // proper sealed hierarchy.
        _ => "Object?".to_string(),
    };

    // `Object?` (and any other already-nullable mapping) must not gain a second
    // `?` — `Object??` is a syntax error.
    match occurrence {
        Some(CsilOccurrence::Optional) if !base.ends_with('?') => format!("{base}?"),
        _ => base,
    }
}

/// A choice whose members are all `text`/`tstr` and string literals — a closed
/// string set. Mapped to `String` (see `map_type`) rather than a sealed class,
/// because the wire form is the literal string itself.
fn is_string_choice(choices: &[CsilTypeExpression]) -> bool {
    !choices.is_empty()
        && choices
            .iter()
            .any(|c| matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Text(_))))
        && choices.iter().all(|c| {
            matches!(c, CsilTypeExpression::Builtin(n) if n == "text" || n == "tstr")
                || matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Text(_)))
        })
}

/// A Dart 3 record type for a CSIL tuple: `(int, String)` positional, or
/// `({int tag, String value})` when every slot is keyed.
fn dart_record_type(group: &CsilGroupExpression, decimal: &str) -> String {
    let all_keyed =
        !group.entries.is_empty() && group.entries.iter().all(|e| wire_key_of_entry(e).is_some());
    if all_keyed {
        let fields: Vec<String> = group
            .entries
            .iter()
            .map(|e| {
                let name = dart_member_name(&wire_key_of_entry(e).unwrap());
                format!("{} {name}", map_type(&e.value_type, &e.occurrence, decimal))
            })
            .collect();
        format!("({{{}}})", fields.join(", "))
    } else {
        let fields: Vec<String> = group
            .entries
            .iter()
            .map(|e| map_type(&e.value_type, &e.occurrence, decimal))
            .collect();
        format!("({})", fields.join(", "))
    }
}

fn wire_key_of_entry(entry: &CsilGroupEntry) -> Option<String> {
    entry.key.as_ref().and_then(wire_key)
}

// ---------------------------------------------------------------------------
// Spec walking helpers
// ---------------------------------------------------------------------------

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

fn spec_uses_builtin(spec: &CsilSpecSerialized, builtin: &str) -> bool {
    spec.rules.iter().any(|rule| match &rule.rule_type {
        CsilRuleType::GroupDef(group) => group
            .entries
            .iter()
            .any(|e| type_uses_builtin(&e.value_type, builtin)),
        CsilRuleType::TypeDef(t) => type_uses_builtin(t, builtin),
        CsilRuleType::TypeChoice(cs) => cs.iter().any(|c| type_uses_builtin(c, builtin)),
        CsilRuleType::GroupChoice(gs) => gs.iter().any(|g| {
            g.entries
                .iter()
                .any(|e| type_uses_builtin(&e.value_type, builtin))
        }),
        CsilRuleType::ServiceDef(svc) => svc.operations.iter().any(|op| {
            type_uses_builtin(&op.input_type, builtin)
                || type_uses_builtin(&op.output_type, builtin)
        }),
    })
}

/// A push op (`-> Event`) carries a `null` input type: the unary client method
/// then takes no request parameter and the router decodes no body.
fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Reduce an output choice to its success type by dropping a top-level
/// `ServiceError` arm — the error half is surfaced by the transport, not returned.
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

fn control_operators(type_expr: &CsilTypeExpression) -> &[CsilControlOperator] {
    match type_expr {
        CsilTypeExpression::Constrained { constraints, .. } => constraints,
        _ => &[],
    }
}

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

fn field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|m| match m {
        CsilFieldMetadata::Description(d) => Some(d.as_str()),
        _ => None,
    })
}

fn field_visibility(metadata: &[CsilFieldMetadata]) -> CsilFieldVisibility {
    metadata
        .iter()
        .find_map(|m| match m {
            CsilFieldMetadata::Visibility(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or(CsilFieldVisibility::Bidirectional)
}

// ---------------------------------------------------------------------------
// Literals & strings
// ---------------------------------------------------------------------------

/// A safely-escaped single-quoted Dart string literal. Dart strings interpolate
/// `$` and `${...}`, so both the quote and the dollar must be escaped to keep an
/// arbitrary message/pattern a literal.
fn dart_string_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn literal_to_dart(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Text(t) => dart_string_lit(t),
        CsilLiteralValue::Integer(n) => n.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        CsilLiteralValue::Bytes(b) => {
            let nums: Vec<String> = b.iter().map(|x| x.to_string()).collect();
            format!("Uint8List.fromList([{}])", nums.join(", "))
        }
        CsilLiteralValue::Array(items) => {
            let elems: Vec<String> = items.iter().map(literal_to_dart).collect();
            format!("[{}]", elems.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Generation entry point
// ---------------------------------------------------------------------------

enum Surface {
    Server,
    Client,
    TypesOnly,
}

/// Generate Dart source from a serialized CSIL spec. Public so the crate's
/// integration tests can assert on the emitted Dart without going through the
/// WASM boundary.
pub fn generate_dart_code(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<GeneratedFiles> {
    let decimal_mapping = match config.options.get("decimal_mapping") {
        None => DecimalMapping::Csil,
        Some(v) => match v.as_str() {
            Some("csil") => DecimalMapping::Csil,
            Some("library") => DecimalMapping::Library,
            other => {
                return Err(CsilgenError::GenerationError(format!(
                    "Unknown decimal_mapping {other:?}. Supported: \"csil\", \"library\""
                )));
            }
        },
    };
    // Validated early (before any file is emitted) so a misconfigured value fails
    // loudly rather than silently degrading. Absent -> Both: the blocking client is
    // preserved and the async twin comes for free.
    let client_style = match config.options.get("client_style") {
        None => ClientStyle::Both,
        Some(v) => match v.as_str() {
            Some("sync") => ClientStyle::Sync,
            Some("async") => ClientStyle::Async,
            Some("both") => ClientStyle::Both,
            other => {
                return Err(CsilgenError::GenerationError(format!(
                    "Unknown client_style {other:?}. Supported: \"sync\", \"async\", \"both\""
                )));
            }
        },
    };

    let library_name = config
        .options
        .get("library_name")
        .and_then(|v| v.as_str())
        .unwrap_or("models")
        .to_string();

    // Package mode (a self-contained, publishable pub package) is opt-in per target
    // via the shared `emit_packages` array; pub scaffolding is emitted only when that
    // array names "dart". Parsed defensively so a missing/malformed option is "off".
    let emit_dart_package = config
        .options
        .get("emit_packages")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|e| e.as_str() == Some("dart")));

    let package_version = config
        .options
        .get("package_version")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("0.1.0")
        .to_string();

    // The pub package name must be a valid `lowercase_with_underscores` identifier:
    // prefer an explicit `package_name`, fall back to the library name, and normalize
    // either so an upstream PascalCase/kebab spelling still yields a publishable name.
    let package_name = sanitize_pub_name(
        config
            .options
            .get("package_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            // A path-style `package_name` is the cross-ecosystem source of truth; pub
            // wants only its tail. See `package_name_last_segment`.
            .map(csilgen_common::package_name_last_segment)
            .unwrap_or(&library_name),
    );

    // In package mode the barrel doubles as the package's public library entrypoint
    // (`package:<name>/<name>.dart`), so the library name follows the package name.
    let library_name = if emit_dart_package {
        package_name.clone()
    } else {
        library_name
    };

    let surface = match config.target.as_str() {
        "dart" | "dart-server" => Surface::Server,
        "dart-client" => Surface::Client,
        "dart-typesonly" => Surface::TypesOnly,
        other => {
            return Err(CsilgenError::GenerationError(format!(
                "Unknown dart sub-target '{other}'. Supported: dart, dart-server, dart-client, dart-typesonly"
            )));
        }
    };

    let cfg = DartConfig {
        decimal_mapping,
        library_name,
        client_style,
    };
    // Captured before `surface` is consumed: the RPC section emits a live carrier +
    // typed client only for a client surface, and the Events section emits router
    // dispatch only for a server surface (where the generated router lives).
    // In package mode the genquickstart demonstrates both the calling side (RPC +
    // Datagrams, over the client) and the handling side (Events, over the router), so a
    // package carries BOTH surfaces and the README shows both as live examples regardless
    // of which surface the requested target named. A flat build keeps the per-surface
    // gating. TypesOnly carries no services either way.
    let both_surfaces = emit_dart_package && !matches!(surface, Surface::TypesOnly);
    let client_surface = matches!(surface, Surface::Client) || both_surfaces;
    let server_surface = matches!(surface, Surface::Server) || both_surfaces;
    let wanted_transports = dart_wanted_transports(config);
    let dart = DartGenerator { cfg };
    let files = dart.generate(spec, surface, emit_dart_package)?;
    if emit_dart_package {
        let readme = dart_readme(
            spec,
            &package_name,
            client_surface,
            server_surface,
            dart.cfg.client_style,
            dart.cfg.decimal_mapping,
            wanted_transports,
        );
        let mut pkg = into_dart_package(files, &package_name, &package_version);
        // The README is opt-out: only an explicit `emit_readme: false` suppresses it.
        // Absent / non-bool / `true` all keep the prior behavior so existing consumers
        // see no change.
        let emit_readme =
            config.options.get("emit_readme").and_then(|v| v.as_bool()) != Some(false);
        if emit_readme {
            // The Quickstart carrier rides at the package root beside pubspec.yaml, not
            // under lib/. It is named `genquickstart.md` rather than `README.md` so it
            // never collides with a consumer's own hand-written `README.md`.
            pkg.push(GeneratedFile {
                path: "genquickstart.md".to_string(),
                content: readme,
            });
        }
        Ok(pkg)
    } else {
        Ok(files)
    }
}

/// Normalize an arbitrary name into a valid pub package name
/// (`lowercase_with_underscores`, leading letter). Snake-casing handles
/// PascalCase/kebab inputs; the remaining map guards against any stray character a
/// rule name might carry so the emitted `pubspec.yaml` always names a publishable
/// package.
fn sanitize_pub_name(raw: &str) -> String {
    let mut name: String = raw
        .to_case(Case::Snake)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    // A pub name must begin with a letter; fall back rather than emit an invalid one.
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        name = format!("pkg_{name}");
    }
    name
}

/// Rewrite the default flat output into a publishable pub package: the generated
/// Dart moves under `lib/` (so imports resolve as `package:<name>/...`), the barrel
/// becomes the package's `lib/<name>.dart` entrypoint, and a `pubspec.yaml` is added
/// at the package root.
fn into_dart_package(files: GeneratedFiles, package_name: &str, version: &str) -> GeneratedFiles {
    let barrel_src = format!("{package_name}.gen.dart");
    let mut out: GeneratedFiles = Vec::with_capacity(files.len() + 1);
    out.push(GeneratedFile {
        path: "pubspec.yaml".to_string(),
        content: pubspec_yaml(package_name, version),
    });
    for f in files {
        let path = if f.path == barrel_src {
            format!("lib/{package_name}.dart")
        } else {
            format!("lib/{}", f.path)
        };
        out.push(GeneratedFile {
            path,
            content: f.content,
        });
    }
    out
}

/// The package manifest. No `dependencies` block: the generated code uses only
/// `dart:` libraries (`dart:typed_data`, `dart:convert`), so the package resolves
/// against an empty dependency set and stays self-contained.
fn pubspec_yaml(name: &str, version: &str) -> String {
    format!("name: {name}\nversion: {version}\nenvironment:\n  sdk: '>=3.0.0 <4.0.0'\n")
}

// ---------------------------------------------------------------------------
// Package README with a copy-paste CSIL-RPC Quickstart
// ---------------------------------------------------------------------------

/// The service base name (trailing `Service` stripped, PascalCased) — the free-fn
/// twin of `DartGenerator::service_base`, used to name the client class the README's
/// example call constructs.
fn service_base_name(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// Which transport sections the consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or
/// empty value means "all three" so the document never renders empty.
fn dart_wanted_transports(config: &GeneratorConfig) -> (bool, bool, bool) {
    match config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
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
        _ => (true, true, true),
    }
}

/// The record (Dart class) name a type reference resolves to, or `None` when the
/// type is not a reference to a generated record — so the datagram/channel sections
/// only name `Type.toCbor`/`Type.fromCbor` for shapes that actually expose them.
fn dart_record_ref(spec: &CsilSpecSerialized, ty: &CsilTypeExpression) -> Option<String> {
    match ty {
        CsilTypeExpression::Reference(name) if dart_find_record(spec, name).is_some() => {
            Some(dart_type_name(name))
        }
        _ => None,
    }
}

/// Services sorted by name, so every "first example" helper picks deterministically.
fn dart_sorted_services(spec: &CsilSpecSerialized) -> Vec<(&str, &CsilServiceDefinition)> {
    let mut services: Vec<(&str, &CsilServiceDefinition)> = spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::ServiceDef(s) => Some((r.name.as_str(), s)),
            _ => None,
        })
        .collect();
    services.sort_by_key(|(n, _)| *n);
    services
}

/// The pieces the RPC + Datagram examples need: the client class to construct, the
/// method to call, a compiling sample request literal (empty when the op takes no
/// request), the request/response record type names (so the datagram section can name
/// `Type.toCbor`/`Type.fromCbor`), and the op's datagram ordinal.
struct DartUnaryExample {
    client_class: String,
    method: String,
    has_request: bool,
    sample: String,
    req_type: Option<String>,
    res_type: Option<String>,
    op_ord: u32,
}

/// The first service (sorted) that has a `->` operation, reduced to an example call.
/// `None` for a serviceless package. `marker` is the client-name marker (`Async` for
/// the default `both` twin, empty for the async drop-in) so the example names the
/// client that actually exists in the emitted package.
fn dart_first_unary_example(
    spec: &CsilSpecSerialized,
    marker: &str,
    decimal: DecimalMapping,
) -> Option<DartUnaryExample> {
    for (name, def) in dart_sorted_services(spec) {
        if let Some(op) = def
            .operations
            .iter()
            .find(|op| matches!(op.direction, CsilServiceDirection::Unidirectional))
        {
            let base = service_base_name(name);
            let has_request = !is_null_input(&op.input_type);
            let success = success_type(&op.output_type);
            return Some(DartUnaryExample {
                client_class: format!("{base}{marker}Client"),
                method: dart_member_name(&op.name),
                has_request,
                sample: if has_request {
                    dart_sample(spec, &op.input_type, decimal)
                } else {
                    String::new()
                },
                req_type: dart_record_ref(spec, &op.input_type),
                res_type: dart_record_ref(spec, &success),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.map(|id| id as u32).unwrap_or(1),
            });
        }
    }
    None
}

/// The in-memory Dart type name for the chosen `decimal` mapping — the free-fn twin of
/// `DartConfig::decimal_dart_type` for the README helpers, which have no `DartConfig`.
fn dart_decimal_str(decimal: DecimalMapping) -> &'static str {
    match decimal {
        DecimalMapping::Csil => "CsilDecimal",
        DecimalMapping::Library => "Decimal",
    }
}

/// The `@override` method bodies a `_Handlers` class needs to satisfy the *whole*
/// generated service handler interface (the router dispatches into one handler that
/// must implement every op): a unary op is a one-line `throw UnimplementedError()`
/// stub (so any return type type-checks), a `<->` op prints, and a `<-` op contributes
/// no inbound method.
fn dart_handler_stub_methods(def: &CsilServiceDefinition, decimal: DecimalMapping) -> String {
    let dec = dart_decimal_str(decimal);
    let mut out = String::new();
    for op in &def.operations {
        let method = dart_member_name(&op.name);
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let out_type = map_type(&success_type(&op.output_type), &None, dec);
                if is_null_input(&op.input_type) {
                    out.push_str(&format!(
                        "  @override\n  {out_type} {method}() => throw UnimplementedError();\n"
                    ));
                } else {
                    let in_type = map_type(&op.input_type, &None, dec);
                    out.push_str(&format!(
                        "  @override\n  {out_type} {method}({in_type} request) => throw UnimplementedError();\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let in_type = map_type(&op.input_type, &None, dec);
                out.push_str(&format!(
                    "  @override\n  void {method}({in_type} message) {{\n    print('event {method} $message');\n  }}\n"
                ));
            }
            CsilServiceDirection::Reverse => {}
        }
    }
    out
}

/// The pieces the Events session needs: the generated handler interface + verbose
/// router names, the channel message record type, the full handler stub bodies, a
/// sample message literal, and the wire service/op strings.
struct DartChannelExample {
    service_wire: String,
    op_wire: String,
    handler_iface: String,
    route_fn: String,
    message_type: String,
    handler_methods: String,
    sample: String,
}

/// The first service (sorted) with a `<->` op whose input and output are both records
/// (so the generated router + per-type codec helpers exist). `None` when no service
/// has a usable channel op — the Events section then shows the handshake/heartbeat
/// without dispatch wiring. The router dispatches the op's input type (matching the
/// generated `route<Service>Channel`), so that is the message record.
fn dart_first_channel_example(
    spec: &CsilSpecSerialized,
    decimal: DecimalMapping,
) -> Option<DartChannelExample> {
    for (name, def) in dart_sorted_services(spec) {
        for op in &def.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let Some(message_type) = dart_record_ref(spec, &op.input_type) else {
                continue;
            };
            if dart_record_ref(spec, &success).is_none() {
                continue;
            }
            return Some(DartChannelExample {
                service_wire: wire_service_string(name),
                op_wire: wire_op_string(&op.name),
                handler_iface: format!("{}Handler", dart_type_name(name)),
                route_fn: format!("route{}Channel", dart_type_name(name)),
                message_type,
                handler_methods: dart_handler_stub_methods(def, decimal),
                sample: dart_sample(spec, &op.input_type, decimal),
            });
        }
    }
    None
}

/// The record a type reference names, if any (a `Name = { ... }` TypeDef-group or a
/// bare GroupDef — both are records).
fn dart_find_record<'a>(
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

/// The transparent alias (`Name = <map/list/scalar/reference>`) a reference resolves
/// through — the same shape `codec_aliases` collects, so the sample recurses into the
/// underlying shape rather than stubbing a named map/list to null.
fn dart_find_alias<'a>(spec: &'a CsilSpecSerialized, name: &str) -> Option<&'a CsilTypeExpression> {
    spec.rules
        .iter()
        .filter(|r| r.name == name)
        .find_map(|r| match &r.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some(other),
            },
            _ => None,
        })
}

/// A compiling Dart literal for `ty`: real values for scalars/collections and nested
/// records (required fields only). Empty `[]`/`{}` infer their element types from the
/// constructor parameter, so a named map/list field stays valid Dart. Shapes a generic
/// sample can't fabricate (choices, tuples) fall back to `null`.
fn dart_sample(
    spec: &CsilSpecSerialized,
    ty: &CsilTypeExpression,
    decimal: DecimalMapping,
) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "'example'".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "Uint8List(0)".to_string(),
            "timestamp" => "DateTime.now()".to_string(),
            "int" | "uint" => "0".to_string(),
            "float" => "0".to_string(),
            "decimal" => match decimal {
                DecimalMapping::Csil => "CsilDecimal(0, BigInt.zero)".to_string(),
                DecimalMapping::Library => "Decimal.zero".to_string(),
            },
            _ => "null".to_string(),
        },
        CsilTypeExpression::Array { .. } => "[]".to_string(),
        CsilTypeExpression::Map { .. } => "{}".to_string(),
        CsilTypeExpression::Constrained { base_type, .. } => dart_sample(spec, base_type, decimal),
        CsilTypeExpression::Reference(name) => {
            if let Some(group) = dart_find_record(spec, name) {
                dart_record_sample(spec, name, group, decimal)
            } else if let Some(alias) = dart_find_alias(spec, name) {
                dart_sample(spec, alias, decimal)
            } else {
                "null".to_string()
            }
        }
        CsilTypeExpression::Choice(choices) if is_string_choice(choices) => "'example'".to_string(),
        _ => "null".to_string(),
    }
}

/// `Name(field: <sample>, ...)` over a record's required fields, keyed by the
/// lowerCamelCase member names the generated record constructor expects.
fn dart_record_sample(
    spec: &CsilSpecSerialized,
    name: &str,
    group: &CsilGroupExpression,
    decimal: DecimalMapping,
) -> String {
    let class_name = dart_type_name(name);
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            entry_names(e).map(|(member, _wire)| {
                format!("{member}: {}", dart_sample(spec, &e.value_type, decimal))
            })
        })
        .collect();
    format!("{class_name}({})", fields.join(", "))
}

/// The package README: a transport-by-transport Quickstart over the official
/// `csilgen_transport` package. The generated codec owns CBOR (de)serialization and
/// the library owns the envelope/framing/lifecycle; you supply only a *carrier* that
/// moves bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events over TLS,
/// CSIL-Datagrams over UDP) is a complete, copy-paste example built on the library.
/// Sections adapt to the emitted surface: RPC needs the typed client (`dart-client`),
/// Events' router needs the server surface (`dart`); Datagrams ride any surface.
fn dart_readme(
    spec: &CsilSpecSerialized,
    package_name: &str,
    client_surface: bool,
    server_surface: bool,
    client_style: ClientStyle,
    decimal: DecimalMapping,
    wanted: (bool, bool, bool),
) -> String {
    let mut out = format!(
        "# {package_name}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the `csilgen_transport` package owns the envelope, framing,\n\
         and connection lifecycle. You supply only a *carrier* that moves bytes, so the\n\
         same typed surface rides HTTP, TLS, a WebSocket, WebTransport, QUIC, or raw UDP\n\
         unchanged.\n\n\
         ## Install\n\n\
         Add this package and the transport library. `csilgen_transport` is not yet\n\
         published to pub.dev, so consume it (and this package) straight from git:\n\n\
         ```yaml\n\
         dependencies:\n  \
         {package_name}:\n    \
         git:\n      url: https://github.com/your-org/your-repo.git\n      \
         path: path/to/{package_name}\n      ref: main\n  \
         csilgen_transport:\n    \
         git:\n      url: https://github.com/catalystcommunity/csilgen.git\n      \
         path: transports/dart\n      ref: main\n\
         ```\n\n"
    );

    // Only an async client surface can host a real Dart HTTP carrier (Dart has no
    // blocking HTTP). `both` (default) exposes the marked `Async` twin; `async` makes
    // the canonical client a drop-in async one; `sync` has no async seam.
    let async_seam = match client_style {
        ClientStyle::Both => Some(("AsyncCsilTransport", "Async")),
        ClientStyle::Async => Some(("CsilTransport", "")),
        ClientStyle::Sync => None,
    };
    let marker = async_seam.map(|(_, m)| m).unwrap_or("");
    let unary = dart_first_unary_example(spec, marker, decimal);
    let channel = dart_first_channel_example(spec, decimal);

    let (rpc, events, datagrams) = wanted;
    if rpc {
        out.push_str(&dart_rpc_section(
            package_name,
            client_surface,
            async_seam.map(|(seam, _)| seam),
            unary.as_ref(),
        ));
    }
    if events {
        out.push_str(&dart_events_section(
            package_name,
            server_surface,
            channel.as_ref(),
        ));
    }
    if datagrams {
        out.push_str(&dart_datagrams_section(package_name, unary.as_ref()));
    }
    out
}

/// CSIL-RPC over HTTP: a carrier implementing the generated async byte seam that
/// builds/parses the envelope with the library's `RpcRequest`/`RpcResponse` (never
/// hand-rolled) and POSTs it to `{baseUrl}/csil/v1/rpc`. Live only for a client
/// surface with an async seam; otherwise a note.
fn dart_rpc_section(
    package_name: &str,
    client_surface: bool,
    async_seam: Option<&str>,
    ex: Option<&DartUnaryExample>,
) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`); you\n\
         bring a carrier that moves bytes. The HTTP carrier below is just one example —\n\
         swap `HttpClient` for any client (it implements the generated byte seam).\n\n",
    );
    let Some(seam) = async_seam else {
        out.push_str(
            "> Generated with `client_style: sync`. Dart has no blocking HTTP, so the\n\
             > CSIL-RPC carrier needs the async client — regenerate with `client_style: both`\n\
             > (the default) or `async`.\n\n",
        );
        return out;
    };
    if !client_surface {
        out.push_str(
            "This package was generated as a server/types-only surface, so it ships no typed\n\
             client to call. Regenerate with `--target dart-client` for the RPC carrier and\n\
             client.\n\n",
        );
        return out;
    }
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```dart\n");
    out.push_str("import 'dart:io' as io;\n");
    out.push_str("import 'dart:typed_data';\n\n");
    out.push_str("import 'package:csilgen_transport/csilgen_transport.dart';\n");
    out.push_str(&format!(
        "import 'package:{package_name}/{package_name}.dart';\n\n"
    ));
    out.push_str(&DART_RPC_CARRIER.replace("__SEAM__", seam));
    out.push('\n');
    out.push_str("Future<void> main() async {\n");
    out.push_str(&format!(
        "  // Point this at your CSIL-RPC server. The HTTP carrier is one example.\n  \
         final client = {}(HttpRpcCarrier('http://localhost:5080'));\n",
        ex.client_class
    ));
    if ex.has_request {
        out.push_str(&format!(
            "  final resp = await client.{}({});\n",
            ex.method, ex.sample
        ));
    } else {
        out.push_str(&format!("  final resp = await client.{}();\n", ex.method));
    }
    out.push_str("  print(resp);\n}\n```\n\n");
    out
}

/// CSIL-Events over TLS: a full session over the library's `FrameCarrier` (length-
/// prefix framing), the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong`
/// heartbeat, and — on a server surface with a record `<->` op — dispatch into the
/// generated `route<Service>Channel`. Without a router the handshake + heartbeat are
/// still shown, with a note where the dispatch wiring would go.
fn dart_events_section(
    package_name: &str,
    server_surface: bool,
    ch: Option<&DartChannelExample>,
) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns\n\
         the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and length-\n\
         prefix framing; the generated router dispatches typed events. The TLS carrier\n\
         below is just one example — a WebSocket/WebTransport/QUIC carrier drops in\n\
         unchanged.\n\n",
    );
    // The router is a server-surface symbol, so dispatch wiring is only shown when a
    // server package actually emits a usable channel router.
    let dispatch = if server_surface { ch } else { None };
    out.push_str("```dart\n");
    out.push_str("import 'dart:io' as io;\n");
    out.push_str("import 'dart:typed_data';\n\n");
    out.push_str("import 'package:csilgen_transport/csilgen_transport.dart';\n");
    if dispatch.is_some() {
        out.push_str(&format!(
            "import 'package:{package_name}/{package_name}.dart';\n"
        ));
    }
    out.push('\n');
    out.push_str(DART_EVENTS_CARRIER);
    out.push('\n');
    match dispatch {
        Some(ch) => out.push_str(&dart_events_session(ch)),
        None => {
            if server_surface {
                out.push_str(
                    "// This package declares no record `<->`/`<-` channel operations, so there is\n\
                     // no generated router to dispatch typed events into; the handshake + heartbeat\n\
                     // below still apply to any connection.\n",
                );
            } else {
                out.push_str(
                    "// This package ships no generated channel router (it is not a server surface).\n\
                     // Regenerate with `--target dart` for route<Service>Channel + a handler\n\
                     // interface; the handshake + heartbeat below apply to any connection.\n",
                );
            }
            out.push_str(DART_EVENTS_NO_CHANNEL_SESSION);
        }
    }
    out.push_str("```\n\n");
    out
}

/// The channel session body for an Events connection with a record `<->` op: a
/// handler + a `CsilCodec` backed by the message record's per-type helpers, the
/// handshake, one outbound event via the generated codec, and the recv loop that
/// heartbeats and dispatches into the generated router.
fn dart_events_session(ch: &DartChannelExample) -> String {
    format!(
        r#"/// A handler for the {service} channel; the generated router dispatches decoded
/// events to it. The interface bundles every service op, so the unary ops are stubbed.
class _Handlers implements {handler} {{
{handler_methods}}}

/// Back the generated router's codec seam with this package's per-type helpers.
class _Codec implements CsilCodec {{
  @override
  List<int> encode(Object? value) => (value as {message}).toCbor();
  @override
  Object? decode(List<int> data) => {message}.fromCbor(data);
}}

Future<void> session() async {{
  final carrier = await TlsFrameCarrier.connect('localhost', 7443);
  final handlers = _Handlers();
  final codec = _Codec();

  // $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the
  // wire profile for the connection's lifetime.
  carrier.sendFrame(Hello([version], ['verbose'], service: '{service}').encode());
  final ackFrame = carrier.recvFrame();
  if (ackFrame == null) throw StateError('connection closed during handshake');
  final profile = parseProfile(HelloAck.decode(ackFrame).profile) ?? Profile.verbose;

  // Send one outbound event via the generated codec, framed as a verbose Event.
  final outbound = {sample};
  carrier
      .sendFrame(Event.verbose('{service}', '{op}', outbound.toCbor()).encode(profile));

  // Recv loop: decode each frame to an Event, answer $ping with $pong, and dispatch
  // the rest to the generated router. A production host drives this drain from the
  // socket's event loop as frames arrive.
  for (var frame = carrier.recvFrame(); frame != null; frame = carrier.recvFrame()) {{
    final ev = Event.decode(frame, profile);
    if (ev.event == Control.pingName) {{
      final ping = Heartbeat.decode(ev.payload);
      carrier.sendFrame(
          Event.verbose('{service}', Control.pongName, Heartbeat(ping.nonce).encode())
              .encode(profile));
      continue;
    }}
    {route}(handlers, codec, ev.event!, ev.payload);
  }}
}}

Future<void> main() => session();
"#,
        service = ch.service_wire,
        op = ch.op_wire,
        handler = ch.handler_iface,
        handler_methods = ch.handler_methods,
        route = ch.route_fn,
        message = ch.message_type,
        sample = ch.sample,
    )
}

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in
/// the library's `Datagram`, and send it fire-and-forget; a recv path decodes an
/// inbound datagram's payload into the RESPONSE type. Live when the first `->` op has
/// record request and response types; otherwise a note.
fn dart_datagrams_section(package_name: &str, ex: Option<&DartUnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram` envelope;\n\
         you bring a datagram carrier. The UDP carrier below is one example — a WebRTC\n\
         unreliable DataChannel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req), Some(res)) = (&ex.req_type, &ex.res_type) else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```dart\n");
    out.push_str("import 'dart:io' as io;\n");
    out.push_str("import 'dart:typed_data';\n\n");
    out.push_str("import 'package:csilgen_transport/csilgen_transport.dart';\n");
    out.push_str(&format!(
        "import 'package:{package_name}/{package_name}.dart';\n\n"
    ));
    out.push_str(&format!(
        "// The operation's datagram ordinal — its @wire-id, or a channel-agreed number.\n\
         const int opOrd = {};\n\n",
        ex.op_ord
    ));
    out.push_str(&format!(
        r#"// Fire-and-forget: encode the `->` request via the generated codec, wrap it in the
// library's Datagram, and send. seq 0 marks an unsequenced datagram.
void sendRequest(DatagramCarrier carrier, {req} request) {{
  carrier.sendDatagram(Datagram(opOrd, 0, request.toCbor()).encode());
}}

// Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is
// NO synchronous response; the caller must tolerate loss and reordering and handle a
// reply whenever (if ever) it shows up.
{res}? recvResponse(DatagramCarrier carrier) {{
  final inbound = carrier.recvDatagram();
  if (inbound == null) return null;
  final dg = Datagram.decode(inbound);
  return {res}.fromCbor(dg.payload);
}}
"#
    ));
    out.push('\n');
    out.push_str(DART_DATAGRAMS_CARRIER);
    out.push('\n');
    out.push_str(&format!(
        r#"Future<void> main() async {{
  final carrier = await UdpDatagramCarrier.bind('localhost', 9000);
  sendRequest(carrier, {sample});
  // No synchronous reply: a response datagram may arrive later, or never.
  final resp = recvResponse(carrier);
  print(resp);
}}
"#,
        sample = ex.sample
    ));
    out.push_str("```\n\n");
    out
}

/// The HTTP carrier body — spec-independent (the op seam name is the only variable),
/// so a constant with a `__SEAM__` placeholder. It encodes the request with the
/// library's `RpcRequest`, POSTs it to `{baseUrl}/csil/v1/rpc`, and returns the
/// success payload bytes the typed client decodes. The default sender is dart:io's
/// `HttpClient`; an injected one lets a test echo in-process with no socket.
const DART_RPC_CARRIER: &str = r#"/// Maps `(uri, body) -> response bytes`. Defaults to dart:io's HttpClient; a test
/// injects an in-process echo so verification needs no live socket.
typedef CsilRpcSender = Future<Uint8List> Function(Uri uri, Uint8List body);

/// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
/// (RpcRequest/RpcResponse); the carrier owns only the transport. Dart has no blocking
/// HTTP, so the carrier is async and pairs with the generated async client.
class HttpRpcCarrier implements __SEAM__ {
  HttpRpcCarrier(this.baseUrl, {CsilRpcSender? sender}) : _send = sender ?? _httpSend;

  final String baseUrl;
  final CsilRpcSender _send;

  @override
  Future<Uint8List> call(String service, String op, List<int> request) async {
    final uri = Uri.parse('${baseUrl.replaceAll(RegExp(r'/+$'), '')}/csil/v1/rpc');
    // The library builds the request envelope; the carrier never hand-rolls CBOR.
    final envelope = RpcRequest(service, op, Uint8List.fromList(request)).encode();
    final bytes = await _send(uri, envelope);
    // RpcResponse.decode + intoTransportError throws a StatusException for any non-zero
    // transport status, distinct from an application error.
    final resp = RpcResponse.decode(bytes).intoTransportError();
    // A typed application error rides as a status-0 `ServiceError` variant; surface it
    // so the typed client decodes success only.
    if (resp.variant == 'ServiceError') {
      throw StateError('csil-rpc $service/$op: ServiceError');
    }
    return resp.payload;
  }

  static Future<Uint8List> _httpSend(Uri uri, Uint8List body) async {
    final http = io.HttpClient();
    try {
      final req = await http.postUrl(uri);
      req.headers.set(io.HttpHeaders.contentTypeHeader, 'application/cbor');
      req.headers.set(io.HttpHeaders.acceptHeader, 'application/cbor');
      req.add(body);
      final httpResp = await req.close();
      if (httpResp.statusCode != io.HttpStatus.ok) {
        throw StateError('csil-rpc: http ${httpResp.statusCode}');
      }
      final out = BytesBuilder();
      await for (final chunk in httpResp) {
        out.add(chunk);
      }
      return out.toBytes();
    } finally {
      http.close(force: true);
    }
  }
}
"#;

/// The TLS `FrameCarrier` adapter — spec-independent. It length-prefixes each outbound
/// frame and feeds inbound socket chunks through a `LengthPrefixedDeframer`, exposing
/// the library's `sendFrame`/`recvFrame` seam so the session logic is transport-
/// agnostic. The async socket is bridged to the synchronous seam by buffering inbound
/// frames in an inbox the recv loop drains.
const DART_EVENTS_CARRIER: &str = r#"// One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
class TlsFrameCarrier implements FrameCarrier {
  TlsFrameCarrier(this._socket) {
    _socket.listen((chunk) {
      _deframer.push(Uint8List.fromList(chunk));
      for (var f = _deframer.next(); f != null; f = _deframer.next()) {
        _inbox.add(f);
      }
    });
  }

  final io.Socket _socket;
  final LengthPrefixedDeframer _deframer = LengthPrefixedDeframer();
  final List<Uint8List> _inbox = [];

  static Future<TlsFrameCarrier> connect(String host, int port) async {
    final socket = await io.SecureSocket.connect(host, port);
    return TlsFrameCarrier(socket);
  }

  @override
  void sendFrame(Uint8List frame) => _socket.add(frameLengthPrefixed(frame));

  @override
  Uint8List? recvFrame() => _inbox.isEmpty ? null : _inbox.removeAt(0);
}
"#;

/// The Events session body when the spec declares no record channel op (or the surface
/// has no router): the handshake and heartbeat still apply, so they are shown, with a
/// note where the dispatch would go. Imports only the transport library.
const DART_EVENTS_NO_CHANNEL_SESSION: &str = r#"Future<void> session() async {
  final carrier = await TlsFrameCarrier.connect('localhost', 7443);

  // $hello / $hello-ack handshake (control plane).
  carrier.sendFrame(Hello([version], ['verbose']).encode());
  final ackFrame = carrier.recvFrame();
  if (ackFrame == null) throw StateError('connection closed during handshake');
  final profile = parseProfile(HelloAck.decode(ackFrame).profile) ?? Profile.verbose;

  // Recv loop: answer $ping with $pong. A production host drives this drain from the
  // socket's event loop as frames arrive.
  for (var frame = carrier.recvFrame(); frame != null; frame = carrier.recvFrame()) {
    final ev = Event.decode(frame, profile);
    if (ev.event == Control.pingName) {
      final ping = Heartbeat.decode(ev.payload);
      carrier.sendFrame(
          Event.verbose(null, Control.pongName, Heartbeat(ping.nonce).encode())
              .encode(profile));
    }
  }
}

Future<void> main() => session();
"#;

/// The UDP `DatagramCarrier` adapter — spec-independent. `sendDatagram` writes one UDP
/// packet; `recvDatagram` drains the next buffered inbound packet (or `null`). Inbound
/// packets are buffered in an inbox because the library's seam is synchronous.
const DART_DATAGRAMS_CARRIER: &str = r#"// One example carrier: UDP via dart:io's RawDatagramSocket. Datagrams are unreliable
// and unordered, so the carrier never waits for or correlates a reply.
class UdpDatagramCarrier implements DatagramCarrier {
  UdpDatagramCarrier(this._socket, this._host, this._port) {
    _socket.listen((event) {
      if (event == io.RawSocketEvent.read) {
        final dg = _socket.receive();
        if (dg != null) _inbox.add(dg.data);
      }
    });
  }

  final io.RawDatagramSocket _socket;
  final io.InternetAddress _host;
  final int _port;
  final List<Uint8List> _inbox = [];

  static Future<UdpDatagramCarrier> bind(String host, int port) async {
    final socket = await io.RawDatagramSocket.bind(io.InternetAddress.anyIPv4, 0);
    final addr = (await io.InternetAddress.lookup(host)).first;
    return UdpDatagramCarrier(socket, addr, port);
  }

  @override
  void sendDatagram(Uint8List datagram) => _socket.send(datagram, _host, _port);

  @override
  Uint8List? recvDatagram() => _inbox.isEmpty ? null : _inbox.removeAt(0);
}
"#;

struct DartGenerator {
    cfg: DartConfig,
}

impl DartGenerator {
    fn generate(
        &self,
        spec: &CsilSpecSerialized,
        surface: Surface,
        pkg_mode: bool,
    ) -> Result<GeneratedFiles> {
        let mut files = Vec::new();

        let records = record_names(spec);
        let aliases = codec_aliases(spec);
        let mut types_code = String::new();
        for rule in &spec.rules {
            match &rule.rule_type {
                CsilRuleType::TypeDef(type_expr) => {
                    types_code.push_str(
                        &self.generate_type_def(&rule.name, type_expr, &records, &aliases),
                    );
                }
                CsilRuleType::GroupDef(group) => {
                    types_code
                        .push_str(&self.generate_record(&rule.name, group, &records, &aliases));
                }
                CsilRuleType::TypeChoice(choices) => {
                    types_code.push_str(&self.generate_sealed_choice(&rule.name, choices));
                }
                CsilRuleType::GroupChoice(groups) => {
                    types_code.push_str(&self.generate_group_choice(&rule.name, groups));
                }
                CsilRuleType::ServiceDef(_) => {}
            }
        }

        let has_types = !types_code.is_empty();
        if has_types {
            files.push(GeneratedFile {
                path: "types.gen.dart".to_string(),
                content: self.file_header("Generated CSIL types.", &[], &types_code),
            });
        }

        // The exact-decimal helper rides alongside the types under the default
        // (csil) mapping when the spec actually uses `decimal`; the library
        // mapping pulls `Decimal` from package:decimal instead.
        if self.cfg.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(spec, "decimal") {
            files.push(GeneratedFile {
                path: "csil_decimal.gen.dart".to_string(),
                content: CSIL_DECIMAL_DART.to_string(),
            });
        }

        // The self-contained CBOR codec the records' toCbor/fromCbor build on; emitted
        // whenever there are records to (de)serialize, so the output stays standalone.
        if !records.is_empty() {
            files.push(GeneratedFile {
                path: "csil_cbor.gen.dart".to_string(),
                content: CSIL_CBOR_DART.to_string(),
            });
        }

        if spec.service_count > 0 {
            // A package's `genquickstart.md` exercises both the calling side (the RPC and
            // Datagrams sections, over the client) and the handling side (the Events section,
            // over the generated `route<Service>Channel` router), so a package must carry BOTH
            // surfaces for its own quickstart to compile — regardless of which surface the
            // requested target names. A flat (non-package) build stays byte-identical: it emits
            // only the requested surface. This mirrors the OCaml generator's
            // all-surfaces-in-package-mode rule.
            let want_client = matches!(surface, Surface::Client)
                || (pkg_mode && !matches!(surface, Surface::TypesOnly));
            let want_server = matches!(surface, Surface::Server)
                || (pkg_mode && !matches!(surface, Surface::TypesOnly));

            if want_server {
                let mut services_code = String::new();
                for rule in &spec.rules {
                    if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                        services_code.push_str(&self.generate_server(&rule.name, service));
                        services_code.push_str(&self.generate_wire_ids(&rule.name, service));
                    }
                }
                if !services_code.is_empty() {
                    let mut body = String::new();
                    body.push_str(SERVER_PRELUDE_DART);
                    body.push('\n');
                    body.push_str(&services_code);
                    // The routers/handlers name the generated request/response
                    // types, so the services file must import the types library.
                    let extra = if has_types {
                        vec!["types.gen.dart".to_string()]
                    } else {
                        Vec::new()
                    };
                    files.push(GeneratedFile {
                        path: "services.gen.dart".to_string(),
                        content: self.file_header(
                            "Generated service handlers and routers.",
                            &extra,
                            &body,
                        ),
                    });
                }
            }

            if want_client {
                // The transport seam owns the I/O round-trip, so it is the only surface that
                // turns async; `client_style` selects which shape(s) to emit. When the server
                // surface is also emitted (package mode), the client lands on its own filenames
                // so it never collides with `services.gen.dart`, and it omits the wire-id consts
                // (the server file owns the single copy, so the two coexist in one barrel
                // without const clashes). A flat client surface keeps the canonical
                // `services.gen.dart` names + the wire-ids, so its byte output is unchanged.
                let (sync_path, async_path, client_wire_ids) = if want_server {
                    ("client.gen.dart", "client.async.gen.dart", false)
                } else {
                    ("services.gen.dart", "services.async.gen.dart", true)
                };
                match self.cfg.client_style {
                    ClientStyle::Sync => {
                        let shape = ClientShape {
                            is_async: false,
                            marker: "",
                        };
                        if let Some(f) = self.build_client_file(
                            spec,
                            shape,
                            sync_path,
                            client_wire_ids,
                            &records,
                            &aliases,
                            has_types,
                        ) {
                            files.push(f);
                        }
                    }
                    ClientStyle::Async => {
                        let shape = ClientShape {
                            is_async: true,
                            marker: "",
                        };
                        if let Some(f) = self.build_client_file(
                            spec,
                            shape,
                            sync_path,
                            client_wire_ids,
                            &records,
                            &aliases,
                            has_types,
                        ) {
                            files.push(f);
                        }
                    }
                    ClientStyle::Both => {
                        let sync = ClientShape {
                            is_async: false,
                            marker: "",
                        };
                        if let Some(f) = self.build_client_file(
                            spec,
                            sync,
                            sync_path,
                            client_wire_ids,
                            &records,
                            &aliases,
                            has_types,
                        ) {
                            files.push(f);
                        }
                        let twin = ClientShape {
                            is_async: true,
                            marker: "Async",
                        };
                        if let Some(f) = self.build_client_file(
                            spec, twin, async_path, false, &records, &aliases, has_types,
                        ) {
                            files.push(f);
                        }
                    }
                }
            }
        }

        // A barrel that re-exports the generated parts, mirroring Python's
        // `__init__.py` and the Go package — one import for the consumer.
        if !files.is_empty() {
            let mut barrel = String::from("// Code generated by csilgen; DO NOT EDIT.\n\n");
            for f in &files {
                barrel.push_str(&format!("export '{}';\n", f.path));
            }
            files.push(GeneratedFile {
                path: format!("{}.gen.dart", self.cfg.library_name),
                content: barrel,
            });
        }

        Ok(files)
    }

    /// Render a generated file: the banner, the imports `body` actually needs
    /// (each emitted only when referenced, so no `unused_import` slips through),
    /// then the body. `extra` carries cross-file dependencies the caller knows
    /// about (e.g. the services file depending on the types file).
    fn file_header(&self, title: &str, extra: &[String], body: &str) -> String {
        let mut imports: Vec<String> = Vec::new();
        // Bytes come in as Uint8List; pull the import only when something needs it.
        if body.contains("Uint8List") {
            imports.push("dart:typed_data".to_string());
        }
        if let Some(dec) = self.decimal_import(body) {
            imports.push(dec.to_string());
        }
        // The records' toCbor/fromCbor reference the generated CsilCbor codec.
        if body.contains("CsilCbor.") {
            imports.push("csil_cbor.gen.dart".to_string());
        }
        imports.extend(extra.iter().cloned());

        let mut out = String::new();
        out.push_str(&format!("// {title}\n//\n"));
        out.push_str("// Code generated by csilgen; DO NOT EDIT.\n\n");
        for imp in &imports {
            out.push_str(&format!("import '{imp}';\n"));
        }
        if !imports.is_empty() {
            out.push('\n');
        }
        out.push_str(body);
        out
    }

    /// The import a body needs for the chosen `decimal` mapping, or `None` when it
    /// references no decimal value. The two mappings live in different places: the
    /// generated helper file vs. the third-party `package:decimal`.
    fn decimal_import(&self, body: &str) -> Option<&'static str> {
        if !body.contains(self.cfg.decimal_dart_type()) {
            return None;
        }
        Some(match self.cfg.decimal_mapping {
            DecimalMapping::Csil => "csil_decimal.gen.dart",
            DecimalMapping::Library => "package:decimal/decimal.dart",
        })
    }

    // -- Records ------------------------------------------------------------

    fn generate_type_def(
        &self,
        name: &str,
        type_expr: &CsilTypeExpression,
        records: &std::collections::HashSet<String>,
        aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    ) -> String {
        // `Name = { ... }` parses to a TypeDef carrying a Group: emit a real
        // record class so it keeps field-level typing, as the other generators do.
        if let CsilTypeExpression::Group(group) = type_expr {
            return self.generate_record(name, group, records, aliases);
        }
        if let CsilTypeExpression::Choice(choices) = type_expr {
            return self.generate_sealed_choice(name, choices);
        }
        let class_name = dart_type_name(name);
        let dart_type = map_type(type_expr, &None, self.cfg.decimal_dart_type());
        format!("typedef {class_name} = {dart_type};\n\n")
    }

    fn generate_record(
        &self,
        name: &str,
        group: &CsilGroupExpression,
        records: &std::collections::HashSet<String>,
        aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    ) -> String {
        let class_name = dart_type_name(name);
        let decimal = self.cfg.decimal_dart_type();
        let mut out = String::new();
        out.push_str(&format!("final class {class_name} {{\n"));

        let named: Vec<(String, String, &CsilGroupEntry)> = group
            .entries
            .iter()
            .filter_map(|e| entry_names(e).map(|(m, w)| (m, w, e)))
            .collect();

        // Fields.
        for (member, _wire, entry) in &named {
            if let Some(desc) = field_description(&entry.metadata) {
                out.push_str(&format!("  /// {desc}\n"));
            }
            let ty = map_type(&entry.value_type, &entry.occurrence, decimal);
            out.push_str(&format!("  final {ty} {member};\n"));
        }
        if named.is_empty() {
            out.push_str("  // (no named fields)\n");
        }
        out.push('\n');

        // Const constructor with required named params (optional fields are not
        // required so a caller can omit them). A fieldless record uses a plain
        // unnamed constructor — Dart rejects an empty named-parameter list
        // (`const X({});` is a syntax error), so the `({ ... })` form is only
        // emitted when there is at least one field.
        if named.is_empty() {
            out.push_str(&format!("  const {class_name}();\n\n"));
        } else {
            out.push_str(&format!("  const {class_name}({{\n"));
            for (member, _wire, entry) in &named {
                let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
                if optional {
                    out.push_str(&format!("    this.{member},\n"));
                } else {
                    out.push_str(&format!("    required this.{member},\n"));
                }
            }
            out.push_str("  });\n\n");
        }

        out.push_str(&self.generate_to_map(&named));
        out.push_str(&self.generate_from_map(&class_name, &named));
        out.push_str(&self.generate_validate(&named));
        out.push_str(&self.generate_equality(&class_name, &named));
        out.push_str(&self.generate_cbor(&class_name, &named, records, aliases));

        out.push_str("}\n\n");
        out
    }

    /// The wire codec: `toCborValue`/`fromCborValue` map a record to/from the dynamic
    /// tree the self-contained `CsilCbor` encoder/decoder consumes (deep — nested
    /// records recurse), and `toCbor`/`fromCbor` are the byte-level entry points the
    /// typed client calls. A send-only field is omitted from `toCborValue` and an
    /// absent optional is dropped, exactly as `toMap` does.
    fn generate_cbor(
        &self,
        class_name: &str,
        named: &[(String, String, &CsilGroupEntry)],
        records: &std::collections::HashSet<String>,
        aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    ) -> String {
        let decimal = self.cfg.decimal_dart_type();
        let mut out = String::new();

        out.push('\n');
        out.push_str("  /// The CBOR-encodable dynamic tree for this record (deep).\n");
        out.push_str("  Map<String, Object?> toCborValue() {\n");
        out.push_str("    final map = <String, Object?>{};\n");
        for (member, wire, entry) in named {
            if matches!(
                field_visibility(&entry.metadata),
                CsilFieldVisibility::ReceiveOnly
            ) {
                continue;
            }
            let wire_lit = dart_string_lit(wire);
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            let access = if optional {
                format!("{member}!")
            } else {
                member.clone()
            };
            let value = dart_to_cbor_value(&entry.value_type, &access, records, aliases);
            if optional {
                out.push_str(&format!(
                    "    if ({member} != null) map[{wire_lit}] = {value};\n"
                ));
            } else {
                out.push_str(&format!("    map[{wire_lit}] = {value};\n"));
            }
        }
        out.push_str("    return map;\n  }\n\n");

        out.push_str("  /// Reconstruct this record from a decoded CBOR dynamic tree.\n");
        out.push_str(&format!(
            "  factory {class_name}.fromCborValue(Object? cbor) {{\n"
        ));
        // A fieldless record reads nothing, so binding `map` would land as an
        // unused local that `dart analyze` rejects; only bind it when a field uses it.
        if !named.is_empty() {
            out.push_str("    final map = cbor as Map;\n");
        }
        out.push_str(&format!("    return {class_name}(\n"));
        for (member, wire, entry) in named {
            let wire_lit = dart_string_lit(wire);
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            let read = dart_from_cbor_value(
                &entry.value_type,
                &format!("map[{wire_lit}]"),
                records,
                decimal,
                aliases,
            );
            if optional {
                out.push_str(&format!(
                    "      {member}: map[{wire_lit}] == null ? null : {read},\n"
                ));
            } else {
                out.push_str(&format!("      {member}: {read},\n"));
            }
        }
        out.push_str("    );\n  }\n\n");

        out.push_str("  /// Encode this record to canonical CSIL CBOR bytes.\n");
        out.push_str("  Uint8List toCbor() => CsilCbor.encodeValue(toCborValue());\n\n");
        out.push_str("  /// Decode a CSIL CBOR byte payload into this record.\n");
        out.push_str(&format!(
            "  factory {class_name}.fromCbor(List<int> bytes) =>\n      {class_name}.fromCborValue(CsilCbor.decode(bytes));\n"
        ));
        out
    }

    /// `toMap` maps each Dart member onto its verbatim wire key. A send-only field
    /// is omitted (`@receive-only` excludes it from the inverse), and an absent
    /// optional is dropped so it stays off the wire rather than encoding a null.
    fn generate_to_map(&self, named: &[(String, String, &CsilGroupEntry)]) -> String {
        let mut out = String::new();
        out.push_str("  Map<String, Object?> toMap() {\n");
        out.push_str("    final map = <String, Object?>{};\n");
        for (member, wire, entry) in named {
            if matches!(
                field_visibility(&entry.metadata),
                CsilFieldVisibility::ReceiveOnly
            ) {
                continue;
            }
            let wire_lit = dart_string_lit(wire);
            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                out.push_str(&format!(
                    "    if ({member} != null) map[{wire_lit}] = {member};\n"
                ));
            } else {
                out.push_str(&format!("    map[{wire_lit}] = {member};\n"));
            }
        }
        out.push_str("    return map;\n  }\n\n");
        out
    }

    fn generate_from_map(
        &self,
        class_name: &str,
        named: &[(String, String, &CsilGroupEntry)],
    ) -> String {
        let decimal = self.cfg.decimal_dart_type();
        let mut out = String::new();
        out.push_str(&format!(
            "  factory {class_name}.fromMap(Map<String, Object?> map) {{\n"
        ));
        out.push_str(&format!("    return {class_name}(\n"));
        // Every declared field is read back: the const constructor's `required`
        // named params must all be supplied, so visibility can't drop a field here
        // the way `toMap` drops a receive-only one (a map has no such obligation).
        for (member, wire, entry) in named {
            let wire_lit = dart_string_lit(wire);
            let ty = map_type(&entry.value_type, &entry.occurrence, decimal);
            // A cast keeps the typed field honest; `as T?` tolerates an absent key.
            // A field already typed `Object?` matches the map's own value type, so
            // casting it would be a redundant (lint-flagged) no-op.
            if ty == "Object?" {
                out.push_str(&format!("      {member}: map[{wire_lit}],\n"));
            } else {
                out.push_str(&format!("      {member}: map[{wire_lit}] as {ty},\n"));
            }
        }
        out.push_str("    );\n  }\n\n");
        out
    }

    /// Emit `validate()` collecting guards from both constraint systems. Only
    /// emitted when at least one guard exists, so an unconstrained record stays bare.
    fn generate_validate(&self, named: &[(String, String, &CsilGroupEntry)]) -> String {
        let mut body = String::new();
        for (member, _wire, entry) in named {
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            for meta in &entry.metadata {
                if let CsilFieldMetadata::Constraint(c) = meta {
                    body.push_str(&self.annotation_guard(member, c, &entry.value_type, optional));
                }
            }
            for op in control_operators(&entry.value_type) {
                body.push_str(&self.control_guard(member, op, optional));
            }
        }
        if body.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str("  /// Throws [ArgumentError] when a field constraint is violated.\n");
        out.push_str("  void validate() {\n");
        out.push_str(&body);
        out.push_str("  }\n\n");
        out
    }

    fn guard(&self, condition: &str, message: &str) -> String {
        // Braces (not a bare `if (..) throw`) so the body never reads as an
        // awkwardly-wrapped line and the output honors `curly_braces_in_flow_control`.
        format!(
            "    if ({condition}) {{\n      throw ArgumentError({});\n    }}\n",
            dart_string_lit(message)
        )
    }

    /// A length/count guard that prefers `isEmpty`/`isNotEmpty` at the
    /// empty-boundary (what `dart analyze`'s `prefer_is_empty`/`prefer_is_not_empty`
    /// expect) and drops a vacuous `length < 0`. `cmp` is the *failure* comparison.
    fn len_guard(
        &self,
        member: &str,
        bang: &str,
        optional: bool,
        cmp: &str,
        n: u64,
        message: &str,
    ) -> String {
        let access = format!("{member}{bang}");
        let cond = match (cmp, n) {
            // `length < 0` can never hold — the guard would be dead code.
            ("<", 0) => return String::new(),
            ("<", 1) => format!("{access}.isEmpty"),
            (">", 0) | ("!=", 0) => format!("{access}.isNotEmpty"),
            ("==", 0) => format!("{access}.isEmpty"),
            _ => format!("{access}.length {cmp} {n}"),
        };
        let cond = if optional {
            format!("{member} != null && {cond}")
        } else {
            cond
        };
        self.guard(&cond, message)
    }

    /// Read expression for a (possibly nullable) member inside a guard. The guard
    /// itself short-circuits on null via `member != null && ...`.
    fn annotation_guard(
        &self,
        member: &str,
        constraint: &CsilValidationConstraint,
        value_type: &CsilTypeExpression,
        optional: bool,
    ) -> String {
        let g = |inner: &str| -> String {
            if optional {
                format!("{member} != null && {inner}")
            } else {
                inner.to_string()
            }
        };
        let bang = if optional { "!" } else { "" };
        match constraint {
            CsilValidationConstraint::MinLength(n) => self.len_guard(
                member,
                bang,
                optional,
                "<",
                *n,
                &format!("'{member}' must have length >= {n}"),
            ),
            CsilValidationConstraint::MaxLength(n) => self.len_guard(
                member,
                bang,
                optional,
                ">",
                *n,
                &format!("'{member}' must have length <= {n}"),
            ),
            CsilValidationConstraint::MinItems(n) => self.len_guard(
                member,
                bang,
                optional,
                "<",
                *n,
                &format!("'{member}' must have at least {n} items"),
            ),
            CsilValidationConstraint::MaxItems(n) => self.len_guard(
                member,
                bang,
                optional,
                ">",
                *n,
                &format!("'{member}' must have at most {n} items"),
            ),
            CsilValidationConstraint::MinValue(v) => {
                let bound = self.bound_expr(v, value_type);
                self.guard(
                    &g(&format!("{member}{bang} < {bound}")),
                    &format!("'{member}' must be >= {}", literal_to_dart(v)),
                )
            }
            CsilValidationConstraint::MaxValue(v) => {
                let bound = self.bound_expr(v, value_type);
                self.guard(
                    &g(&format!("{member}{bang} > {bound}")),
                    &format!("'{member}' must be <= {}", literal_to_dart(v)),
                )
            }
            // Advisory only — surfaces as a comment, never a hard check.
            CsilValidationConstraint::Custom { name, .. } => {
                format!("    // custom constraint '{name}' on '{member}' is advisory\n")
            }
        }
    }

    fn control_guard(&self, member: &str, op: &CsilControlOperator, optional: bool) -> String {
        let g = |inner: &str| -> String {
            if optional {
                format!("{member} != null && {inner}")
            } else {
                inner.to_string()
            }
        };
        let bang = if optional { "!" } else { "" };
        match op {
            CsilControlOperator::Size(size) => self.size_guard(member, size, optional),
            CsilControlOperator::Regex(pattern) => {
                let pat = dart_string_lit(pattern);
                self.guard(
                    &g(&format!("!RegExp({pat}).hasMatch({member}{bang})")),
                    &format!("'{member}' must match pattern {pattern}"),
                )
            }
            CsilControlOperator::GreaterEqual(v) => self.guard(
                &g(&format!("{member}{bang} < {}", literal_to_dart(v))),
                &format!("'{member}' must be >= {}", literal_to_dart(v)),
            ),
            CsilControlOperator::LessEqual(v) => self.guard(
                &g(&format!("{member}{bang} > {}", literal_to_dart(v))),
                &format!("'{member}' must be <= {}", literal_to_dart(v)),
            ),
            CsilControlOperator::GreaterThan(v) => self.guard(
                &g(&format!("{member}{bang} <= {}", literal_to_dart(v))),
                &format!("'{member}' must be > {}", literal_to_dart(v)),
            ),
            CsilControlOperator::LessThan(v) => self.guard(
                &g(&format!("{member}{bang} >= {}", literal_to_dart(v))),
                &format!("'{member}' must be < {}", literal_to_dart(v)),
            ),
            CsilControlOperator::Equal(v) => self.guard(
                &g(&format!("{member}{bang} != {}", literal_to_dart(v))),
                &format!("'{member}' must equal {}", literal_to_dart(v)),
            ),
            CsilControlOperator::NotEqual(v) => self.guard(
                &g(&format!("{member}{bang} == {}", literal_to_dart(v))),
                &format!("'{member}' must not equal {}", literal_to_dart(v)),
            ),
            // Default lives on the field; encoding/structural ops are wire-only.
            _ => String::new(),
        }
    }

    fn size_guard(&self, member: &str, size: &CsilSizeConstraint, optional: bool) -> String {
        let bang = if optional { "!" } else { "" };
        match size {
            CsilSizeConstraint::Exact(n) => self.len_guard(
                member,
                bang,
                optional,
                "!=",
                *n,
                &format!("'{member}' must have length {n}"),
            ),
            CsilSizeConstraint::Min(n) => self.len_guard(
                member,
                bang,
                optional,
                "<",
                *n,
                &format!("'{member}' must have length >= {n}"),
            ),
            CsilSizeConstraint::Max(n) => self.len_guard(
                member,
                bang,
                optional,
                ">",
                *n,
                &format!("'{member}' must have length <= {n}"),
            ),
            CsilSizeConstraint::Range { min, max } => {
                let mut out = self.len_guard(
                    member,
                    bang,
                    optional,
                    "<",
                    *min,
                    &format!("'{member}' must have length >= {min}"),
                );
                out.push_str(&self.len_guard(
                    member,
                    bang,
                    optional,
                    ">",
                    *max,
                    &format!("'{member}' must have length <= {max}"),
                ));
                out
            }
        }
    }

    /// A `decimal`/`timestamp` bound must be reconstructed as the matching Dart
    /// value so the comparison is type-correct; numeric/text bounds stay literal.
    fn bound_expr(&self, value: &CsilLiteralValue, value_type: &CsilTypeExpression) -> String {
        let base = match value_type {
            CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
            other => other,
        };
        if let CsilTypeExpression::Builtin(name) = base {
            match name.as_str() {
                "timestamp" => {
                    if let CsilLiteralValue::Text(t) = value {
                        return format!("DateTime.parse({})", dart_string_lit(t));
                    }
                }
                "decimal" => {
                    let s = match value {
                        CsilLiteralValue::Integer(n) => n.to_string(),
                        CsilLiteralValue::Text(t) => t.clone(),
                        _ => return literal_to_dart(value),
                    };
                    return match self.cfg.decimal_mapping {
                        DecimalMapping::Csil => {
                            format!("CsilDecimal.parse({})", dart_string_lit(&s))
                        }
                        DecimalMapping::Library => {
                            format!("Decimal.parse({})", dart_string_lit(&s))
                        }
                    };
                }
                _ => {}
            }
        }
        literal_to_dart(value)
    }

    /// Value equality over fields (Dart's default is identity). Byte fields are
    /// compared by content so a decoded record equals its source — the conformance
    /// gotcha for reference-equality languages, applied to generated records too.
    fn generate_equality(
        &self,
        class_name: &str,
        named: &[(String, String, &CsilGroupEntry)],
    ) -> String {
        let has_bytes = named.iter().any(|(_, _, e)| is_bytes_type(&e.value_type));
        let mut out = String::new();
        out.push_str("  @override\n  bool operator ==(Object other) {\n");
        out.push_str(&format!("    if (other is! {class_name}) return false;\n"));
        out.push_str("    return ");
        if named.is_empty() {
            out.push_str("true");
        } else {
            let parts: Vec<String> = named
                .iter()
                .map(|(member, _wire, entry)| {
                    if is_bytes_type(&entry.value_type) {
                        format!("_bytesEqual({member}, other.{member})")
                    } else {
                        format!("{member} == other.{member}")
                    }
                })
                .collect();
            out.push_str(&parts.join(" &&\n        "));
        }
        out.push_str(";\n  }\n\n");

        out.push_str("  @override\n  int get hashCode => Object.hashAll([");
        let hash_parts: Vec<String> = named
            .iter()
            .map(|(member, _wire, entry)| {
                // Byte content (not list identity) drives the hash so it agrees with
                // `==`. A non-optional byte field can't be null, so the null branch is
                // only emitted when the field actually is optional.
                if is_bytes_type(&entry.value_type) {
                    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                        format!("{member} == null ? null : Object.hashAll({member}!)")
                    } else {
                        format!("Object.hashAll({member})")
                    }
                } else {
                    member.clone()
                }
            })
            .collect();
        out.push_str(&hash_parts.join(", "));
        out.push_str("]);\n");

        // A small content-equality helper, emitted only for records that actually
        // carry a byte field so it never lands as an unused declaration. It stays
        // private to the record so there is no cross-file dependency.
        if has_bytes {
            out.push('\n');
            out.push_str("  static bool _bytesEqual(Uint8List? a, Uint8List? b) {\n");
            out.push_str("    if (a == null || b == null) return a == b;\n");
            out.push_str("    if (a.length != b.length) return false;\n");
            out.push_str("    for (var i = 0; i < a.length; i++) {\n");
            out.push_str("      if (a[i] != b[i]) return false;\n    }\n    return true;\n  }\n");
        }
        out
    }

    // -- Sealed choices -----------------------------------------------------

    /// A CSIL output/type choice → a `sealed class` base with one `final class`
    /// arm per reference member, so the host pattern-matches exhaustively. Inline
    /// (non-reference) arms are collapsed to a single `Other` arm carrying the raw
    /// value, since they have no nameable Dart type.
    fn generate_sealed_choice(&self, name: &str, choices: &[CsilTypeExpression]) -> String {
        let base = dart_type_name(name);
        // A closed string set is a `String` alias, not a one-arm sealed class: the
        // wire carries the bare string, so a sealed wrapper would never round-trip.
        if is_string_choice(choices) {
            return format!("typedef {base} = String;\n\n");
        }
        let mut out = String::new();
        out.push_str(&format!(
            "sealed class {base} {{\n  const {base}();\n}}\n\n"
        ));
        let mut emitted_other = false;
        for choice in choices {
            match choice {
                CsilTypeExpression::Reference(ref_name) => {
                    let arm = dart_type_name(ref_name);
                    let inner = dart_type_name(ref_name);
                    out.push_str(&format!(
                        "final class {base}{arm} extends {base} {{\n  final {inner} value;\n  const {base}{arm}(this.value);\n}}\n\n"
                    ));
                }
                _ => {
                    if !emitted_other {
                        emitted_other = true;
                        let dart_type = map_type(choice, &None, self.cfg.decimal_dart_type());
                        out.push_str(&format!(
                            "final class {base}Other extends {base} {{\n  final {dart_type} value;\n  const {base}Other(this.value);\n}}\n\n"
                        ));
                    }
                }
            }
        }
        out
    }

    fn generate_group_choice(&self, name: &str, groups: &[CsilGroupExpression]) -> String {
        let base = dart_type_name(name);
        let mut out = String::new();
        out.push_str(&format!(
            "sealed class {base} {{\n  const {base}();\n}}\n\n"
        ));
        for (i, group) in groups.iter().enumerate() {
            let arm = format!("{base}Variant{i}");
            // Each variant is its own record-shaped final class extending the base.
            out.push_str(&format!("final class {arm} extends {base} {{\n"));
            let named: Vec<(String, String, &CsilGroupEntry)> = group
                .entries
                .iter()
                .filter_map(|e| entry_names(e).map(|(m, w)| (m, w, e)))
                .collect();
            for (member, _wire, entry) in &named {
                let ty = map_type(
                    &entry.value_type,
                    &entry.occurrence,
                    self.cfg.decimal_dart_type(),
                );
                out.push_str(&format!("  final {ty} {member};\n"));
            }
            // A fieldless variant uses a plain unnamed constructor — Dart rejects
            // an empty named-parameter list (`const X({});`), so the `({ ... })`
            // form is only emitted when the variant carries at least one field.
            if named.is_empty() {
                out.push_str(&format!("  const {arm}();\n}}\n\n"));
            } else {
                out.push_str(&format!("  const {arm}({{"));
                let params: Vec<String> = named
                    .iter()
                    .map(|(member, _wire, entry)| {
                        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                            format!("this.{member}")
                        } else {
                            format!("required this.{member}")
                        }
                    })
                    .collect();
                out.push_str(&params.join(", "));
                out.push_str("});\n}\n\n");
            }
        }
        out
    }

    // -- Clients ------------------------------------------------------------

    fn service_base(&self, name: &str) -> String {
        let pascal = name.to_case(Case::Pascal);
        pascal
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(pascal)
    }

    /// Build one client file for the given `shape` at `filename`. `include_wire_ids`
    /// is true only for the canonical (sync) file so the async twin never re-declares
    /// the top-level `const` ordinals the sync file already owns. Returns `None` when
    /// the spec has no services to emit a client for.
    #[allow(clippy::too_many_arguments)]
    fn build_client_file(
        &self,
        spec: &CsilSpecSerialized,
        shape: ClientShape,
        filename: &str,
        include_wire_ids: bool,
        records: &std::collections::HashSet<String>,
        aliases: &std::collections::HashMap<String, CsilTypeExpression>,
        has_types: bool,
    ) -> Option<GeneratedFile> {
        let mut services_code = String::new();
        for rule in &spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                services_code
                    .push_str(&self.generate_client(&rule.name, service, records, aliases, shape));
                if include_wire_ids {
                    services_code.push_str(&self.generate_wire_ids(&rule.name, service));
                }
            }
        }
        if services_code.is_empty() {
            return None;
        }
        let mut body = String::new();
        body.push_str(&client_prelude(shape));
        body.push('\n');
        body.push_str(&services_code);
        // The clients name the generated request/response types, so the client file
        // must import the types library.
        let extra = if has_types {
            vec!["types.gen.dart".to_string()]
        } else {
            Vec::new()
        };
        Some(GeneratedFile {
            path: filename.to_string(),
            content: self.file_header("Generated service clients.", &extra, &body),
        })
    }

    fn generate_client(
        &self,
        name: &str,
        service: &CsilServiceDefinition,
        records: &std::collections::HashSet<String>,
        aliases: &std::collections::HashMap<String, CsilTypeExpression>,
        shape: ClientShape,
    ) -> String {
        let base = self.service_base(name);
        let client = shape.client_name(&base);
        let transport = shape.transport_name();
        let decimal = self.cfg.decimal_dart_type();
        let aw = shape.await_kw();
        let asy = shape.async_kw();
        // The wire service is lowercased and the op PascalCased (the wire contract),
        // so a Dart client reaches the same endpoint as a Go/Python/TS peer.
        let wire_service = dart_string_lit(&wire_service_string(name));
        let mut out = String::new();
        out.push_str(&format!(
            "/// A typed, transport-agnostic client for the {name} service. The client owns\n/// (de)serialization; the carrier only moves bytes.\nfinal class {client} {{\n"
        ));
        out.push_str(&format!("  final {transport} transport;\n"));
        out.push_str(&format!("  const {client}(this.transport);\n\n"));

        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                out.push_str(&format!(
                    "  // channel operation '{}' rides the router surface\n",
                    op.name
                ));
                continue;
            }
            let method = dart_member_name(&op.name);
            let wire_op = dart_string_lit(&wire_op_string(&op.name));
            let out_type = map_type(&success_type(&op.output_type), &None, decimal);
            let ret = shape.ret(&out_type);
            let out_decode = dart_from_cbor_value(
                &success_type(&op.output_type),
                "CsilCbor.decode(csilResp)",
                records,
                decimal,
                aliases,
            );
            if is_null_input(&op.input_type) {
                out.push_str(&format!("  {ret} {method}() {asy}{{\n"));
                out.push_str(&format!(
                    "    final csilResp = {aw}transport.call({wire_service}, {wire_op}, const <int>[]);\n"
                ));
                out.push_str(&format!("    return {out_decode};\n  }}\n\n"));
            } else {
                let in_type = map_type(&op.input_type, &None, decimal);
                // A record request serializes itself; a scalar/list request goes
                // through the encoder directly.
                let req_bytes = if matches!(&op.input_type, CsilTypeExpression::Reference(n) if records.contains(&dart_type_name(n)))
                {
                    "request.toCbor()".to_string()
                } else {
                    format!(
                        "CsilCbor.encodeValue({})",
                        dart_to_cbor_value(&op.input_type, "request", records, aliases)
                    )
                };
                out.push_str(&format!("  {ret} {method}({in_type} request) {asy}{{\n"));
                out.push_str(&format!(
                    "    final csilResp = {aw}transport.call({wire_service}, {wire_op}, {req_bytes});\n"
                ));
                out.push_str(&format!("    return {out_decode};\n  }}\n\n"));
            }
        }
        out.push_str("}\n\n");
        out
    }

    // -- Servers ------------------------------------------------------------

    fn generate_server(&self, name: &str, service: &CsilServiceDefinition) -> String {
        let decimal = self.cfg.decimal_dart_type();
        let handler = format!("{}Handler", dart_type_name(name));
        let mut out = String::new();

        // Handler interface — host implements one method per operation.
        out.push_str(&format!(
            "/// The {name} service contract. A host `implements` this; the router\n/// dispatches decoded requests/messages to it.\nabstract interface class {handler} {{\n"
        ));
        for op in &service.operations {
            let method = dart_member_name(&op.name);
            match op.direction {
                CsilServiceDirection::Unidirectional => {
                    let out_type = map_type(&success_type(&op.output_type), &None, decimal);
                    if is_null_input(&op.input_type) {
                        out.push_str(&format!("  {out_type} {method}();\n"));
                    } else {
                        let in_type = map_type(&op.input_type, &None, decimal);
                        out.push_str(&format!("  {out_type} {method}({in_type} request);\n"));
                    }
                }
                CsilServiceDirection::Bidirectional => {
                    let in_type = map_type(&op.input_type, &None, decimal);
                    out.push_str(&format!("  void {method}({in_type} message);\n"));
                }
                CsilServiceDirection::Reverse => {
                    // Server pushes only; no inbound handler method.
                }
            }
        }
        out.push_str("}\n\n");

        // Verbose router: dispatch on the verbatim wire op name.
        if service_has_channel_ops(service) {
            out.push_str(&self.generate_router_verbose(name, service, &handler));
            if service.wire_id.is_some() {
                out.push_str(&self.generate_router_compact(name, service, &handler));
            }
        }
        out
    }

    fn generate_router_verbose(
        &self,
        name: &str,
        service: &CsilServiceDefinition,
        handler: &str,
    ) -> String {
        let decimal = self.cfg.decimal_dart_type();
        let fn_name = format!("route{}Channel", dart_type_name(name));
        let mut out = String::new();
        out.push_str(&format!(
            "/// Decode one inbound channel frame and dispatch to the matching {name}\n/// method by its verbatim wire op name (verbose profile).\nvoid {fn_name}({handler} handler, CsilCodec codec, String op, List<int> data) {{\n  switch (op) {{\n"
        ));
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let method = dart_member_name(&op.name);
            let in_type = map_type(&op.input_type, &None, decimal);
            out.push_str(&format!(
                "    case {}:\n",
                dart_string_lit(&wire_op_string(&op.name))
            ));
            out.push_str(&format!(
                "      handler.{method}(codec.decode(data) as {in_type});\n      return;\n"
            ));
        }
        out.push_str(
            "    default:\n      throw ArgumentError('unknown channel op: $op');\n  }\n}\n\n",
        );
        out
    }

    fn generate_router_compact(
        &self,
        name: &str,
        service: &CsilServiceDefinition,
        handler: &str,
    ) -> String {
        let decimal = self.cfg.decimal_dart_type();
        let fn_name = format!("route{}ChannelCompact", dart_type_name(name));
        let mut out = String::new();
        out.push_str(&format!(
            "/// Compact-profile twin of route{0}Channel: dispatch by @wire-id ordinal.\n/// The profile is negotiated on the wire, so a host keeps both routers.\nvoid {fn_name}({handler} handler, CsilCodec codec, int op, List<int> data) {{\n  switch (op) {{\n",
            dart_type_name(name)
        ));
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let Some(op_id) = op.wire_id else { continue };
            let method = dart_member_name(&op.name);
            let in_type = map_type(&op.input_type, &None, decimal);
            out.push_str(&format!("    case {op_id}:\n"));
            out.push_str(&format!(
                "      handler.{method}(codec.decode(data) as {in_type});\n      return;\n"
            ));
        }
        out.push_str(
            "    default:\n      throw ArgumentError('unknown channel ordinal: $op');\n  }\n}\n\n",
        );
        out
    }

    /// `const` wire-id ordinals exposing `@wire-id(N)` so a host references them
    /// instead of hardcoding. Additive: nothing is emitted absent a wire-id.
    fn generate_wire_ids(&self, name: &str, service: &CsilServiceDefinition) -> String {
        let Some(service_id) = service.wire_id else {
            return String::new();
        };
        let prefix = dart_member_name(name);
        let mut out = String::new();
        out.push_str(&format!(
            "// Wire-id ordinals for the {name} service (compact profiles).\n"
        ));
        out.push_str(&format!(
            "const int {prefix}ServiceWireId = {service_id};\n"
        ));
        for op in &service.operations {
            if let Some(op_id) = op.wire_id {
                let op_name = dart_member_name(&op.name).to_case(Case::Pascal);
                out.push_str(&format!("const int {prefix}Op{op_name}WireId = {op_id};\n"));
            }
        }
        out.push('\n');
        out
    }
}

fn is_bytes_type(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "bytes" || name == "bstr")
}

/// The wire `service` string: the service base (trailing `Service` stripped),
/// **lowercased**, per `docs/cbor-wire-contract.md`. Distinct from the Dart class
/// name (`service_base`), which stays PascalCase — a case transform must never reach
/// the wire differently from the other targets.
fn wire_service_string(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    let base = pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal);
    base.to_lowercase()
}

/// The wire `op`/method string: the operation name PascalCased with the simple rule
/// (capitalize after `_`/`-`, leave the rest as written so acronym runs survive),
/// matching the other targets so a Dart client reaches the same endpoint.
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

// ---------------------------------------------------------------------------
// Codec value transforms (deep toCbor/fromCbor)
// ---------------------------------------------------------------------------

/// The record type names (Dart class names) the codec covers — those whose CBOR
/// form is a map and which therefore expose `toCborValue`/`fromCborValue`.
fn record_names(spec: &CsilSpecSerialized) -> std::collections::HashSet<String> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(dart_type_name(&r.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(dart_type_name(&r.name)),
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through, keyed by the raw rule
/// name a `Reference` carries: a `TypeDef` whose target is a map / array / scalar /
/// reference (NOT a record group or a choice, which have their own handling). A
/// field referencing one is typed as the alias (`typedef StringInt64Map = Map<...>`),
/// so its value IS the underlying shape — the codec must recurse into that shape
/// rather than pass the bare reference straight through and drop nested records.
fn codec_aliases(
    spec: &CsilSpecSerialized,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    spec.rules
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

/// Whether a type contains (anywhere) a reference to a codec'd record, so a value
/// of it must be recursively mapped rather than passed straight to the CBOR encoder.
fn contains_record(
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> bool {
    match ty {
        // A reference is either a record directly, or a transparent alias whose
        // underlying shape (e.g. `M = {* text => SomeRecord}`) may itself carry one.
        CsilTypeExpression::Reference(name) if records.contains(&dart_type_name(name)) => true,
        CsilTypeExpression::Reference(name) => aliases
            .get(name)
            .is_some_and(|underlying| contains_record(underlying, records, aliases)),
        CsilTypeExpression::Array { element_type, .. } => {
            contains_record(element_type, records, aliases)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            contains_record(key, records, aliases) || contains_record(value, records, aliases)
        }
        CsilTypeExpression::Constrained { base_type, .. } => {
            contains_record(base_type, records, aliases)
        }
        _ => false,
    }
}

/// A Dart expression turning `expr` (a typed value) into the dynamic tree the CBOR
/// encoder consumes: scalars/bytes/DateTime pass straight through (the encoder keys
/// on runtime type); a nested record becomes its `toCborValue()` map; lists and maps
/// recurse only when they carry records.
fn dart_to_cbor_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            dart_to_cbor_value(base_type, expr, records, aliases)
        }
        CsilTypeExpression::Reference(name) if records.contains(&dart_type_name(name)) => {
            format!("{expr}.toCborValue()")
        }
        // A transparent alias is typed as its underlying shape, so the value flowing
        // here IS that shape: recurse into it (a map-of-record alias then maps each
        // value through `toCborValue` instead of passing the bare map straight on,
        // which would drop the nested records).
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            dart_to_cbor_value(&aliases[name], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. }
            if contains_record(element_type, records, aliases) =>
        {
            let inner = dart_to_cbor_value(element_type, "csilE", records, aliases);
            format!("{expr}.map((csilE) => {inner}).toList()")
        }
        CsilTypeExpression::Map { key, value, .. }
            if contains_record(key, records, aliases)
                || contains_record(value, records, aliases) =>
        {
            let k = dart_to_cbor_value(key, "csilK", records, aliases);
            let v = dart_to_cbor_value(value, "csilV", records, aliases);
            format!("{expr}.map((csilK, csilV) => MapEntry({k}, {v}))")
        }
        // Scalars, bytes, DateTime, decimal, string-choices, and record-free
        // lists/maps are already a shape the encoder handles by runtime type.
        _ => expr.to_string(),
    }
}

/// A Dart expression reconstructing a typed value from `expr` (the decoded dynamic
/// tree node).
fn dart_from_cbor_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    decimal: &str,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            dart_from_cbor_value(base_type, expr, records, decimal, aliases)
        }
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => format!("{expr} as int"),
            "float" => format!("({expr} as num).toDouble()"),
            "text" | "tstr" => format!("{expr} as String"),
            "bytes" | "bstr" => format!("{expr} as Uint8List"),
            "bool" => format!("{expr} as bool"),
            "timestamp" => format!("{expr} as DateTime"),
            "decimal" => format!("{expr} as {decimal}"),
            _ => expr.to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&dart_type_name(name)) => {
            format!("{}.fromCborValue({expr})", dart_type_name(name))
        }
        // A transparent alias decodes as its underlying shape (the field is typed as
        // that shape), so a map-of-record alias reconstructs each value through
        // `fromCborValue` and casts to the named-typed map rather than yielding the
        // raw decoded dynamic map, which mistypes the entries and drops the records.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            dart_from_cbor_value(&aliases[name], expr, records, decimal, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = dart_from_cbor_value(element_type, "csilE", records, decimal, aliases);
            let et = map_type(element_type, &None, decimal);
            format!("({expr} as List).map((csilE) => {inner}).cast<{et}>().toList()")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kt = map_type(key, &None, decimal);
            let vt = map_type(value, &None, decimal);
            let k = dart_from_cbor_value(key, "csilK", records, decimal, aliases);
            let v = dart_from_cbor_value(value, "csilV", records, decimal, aliases);
            format!("({expr} as Map).map((csilK, csilV) => MapEntry({k}, {v})).cast<{kt}, {vt}>()")
        }
        CsilTypeExpression::Choice(choices) if is_string_choice(choices) => {
            format!("{expr} as String")
        }
        _ => expr.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Static preludes
// ---------------------------------------------------------------------------

/// Client scaffolding: the transport seam every generated client delegates to.
/// The generator never owns the wire — the host injects a transport that encodes the
/// request, performs the (service, op) call, and decodes the reply. An async shape
/// returns a `Future<Uint8List>` (the carrier owns the I/O round-trip); the codec the
/// client builds on stays synchronous either way. The sync text is byte-for-byte the
/// pre-async output so a `sync`/pre-existing consumer sees no change.
fn client_prelude(shape: ClientShape) -> String {
    let name = shape.transport_name();
    let ret = if shape.is_async {
        "Future<Uint8List>"
    } else {
        "List<int>"
    };
    format!(
        "\
/// The carrier seam a generated client delegates to. The generated client owns
/// (de)serialization — it encodes the typed request to CBOR and decodes the typed
/// response — so the carrier only moves bytes for the named (service, op).
abstract interface class {name} {{
  /// Perform the (service, op) call with the already-encoded request bytes and
  /// return the response bytes.
  {ret} call(String service, String op, List<int> request);
}}
"
    )
}

/// Server scaffolding: the codec seam the routers use to decode inbound frames.
const SERVER_PRELUDE_DART: &str = "\
/// The (de)serialization seam the generated routers use. The host wires this to
/// `csilgen_transport`'s CBOR codec or anything else its protocol expects; the
/// router itself is codec-agnostic.
abstract interface class CsilCodec {
  List<int> encode(Object? value);
  Object? decode(List<int> data);
}
";

/// The self-contained exact-decimal helper, emitted only when the spec uses
/// `decimal` under the default (csil) mapping. CBOR tag 4 `[exponent, mantissa]`;
/// the value is kept as exact `BigInt`s so no precision is lost. Interop with
/// `package:decimal` is via the canonical string form, so this takes no dep on it.
const CSIL_DECIMAL_DART: &str = r#"// The exact, base-10 `decimal` core type (CBOR tag 4: [exponent, mantissa]).
//
// Code generated by csilgen; DO NOT EDIT.

/// An exact base-10 decimal. The value is `mantissa * 10^exponent`, kept as
/// exact integers so no float rounding can occur. Interop with package:decimal is
/// via [toString]/[parse] (the canonical decimal-text form both agree on).
final class CsilDecimal implements Comparable<CsilDecimal> {
  final int exponent;
  final BigInt mantissa;
  const CsilDecimal(this.exponent, this.mantissa);

  /// Parse canonical decimal text (what [toString] produces) into an exact value.
  static CsilDecimal parse(String s) {
    var text = s.trim();
    var negative = false;
    if (text.startsWith('-')) {
      negative = true;
      text = text.substring(1);
    } else if (text.startsWith('+')) {
      text = text.substring(1);
    }
    var intPart = text;
    var fracPart = '';
    final dot = text.indexOf('.');
    if (dot >= 0) {
      intPart = text.substring(0, dot);
      fracPart = text.substring(dot + 1);
    }
    var digits = intPart + fracPart;
    if (digits.isEmpty) digits = '0';
    var m = BigInt.parse(digits);
    if (negative) m = -m;
    return CsilDecimal(-fracPart.length, m);
  }

  @override
  String toString() {
    if (exponent == 0) return mantissa.toString();
    if (exponent > 0) {
      final scale = BigInt.from(10).pow(exponent);
      return (mantissa * scale).toString();
    }
    final negative = mantissa.isNegative;
    final digits = mantissa.abs().toString();
    final scale = -exponent;
    final sign = negative ? '-' : '';
    if (digits.length <= scale) {
      return '${sign}0.${'0' * (scale - digits.length)}$digits';
    }
    final cut = digits.length - scale;
    return '$sign${digits.substring(0, cut)}.${digits.substring(cut)}';
  }

  @override
  int compareTo(CsilDecimal other) {
    var dm = mantissa;
    var om = other.mantissa;
    if (exponent > other.exponent) {
      dm = dm * BigInt.from(10).pow(exponent - other.exponent);
    } else if (other.exponent > exponent) {
      om = om * BigInt.from(10).pow(other.exponent - exponent);
    }
    return dm.compareTo(om);
  }

  bool operator <(CsilDecimal other) => compareTo(other) < 0;
  bool operator <=(CsilDecimal other) => compareTo(other) <= 0;
  bool operator >(CsilDecimal other) => compareTo(other) > 0;
  bool operator >=(CsilDecimal other) => compareTo(other) >= 0;

  @override
  bool operator ==(Object other) =>
      other is CsilDecimal && compareTo(other) == 0;

  @override
  int get hashCode => toString().hashCode;
}
"#;

/// The self-contained canonical CSIL CBOR codec the generated records' toCbor/
/// fromCbor build on, so the output stays standalone (no third-party CBOR dep).
const CSIL_CBOR_DART: &str = r#"// The self-contained canonical CSIL CBOR codec.
//
// Code generated by csilgen; DO NOT EDIT.

import 'dart:convert';
import 'dart:typed_data';

/// A minimal canonical-CBOR (RFC 8949 subset) encoder/decoder over the dynamic
/// value tree the generated records map to. Scalars, bytes (major type 2),
/// timestamps (DateTime -> tag 0), lists, and maps are supported; the generated
/// records call this behind their toCbor/fromCbor.
class CsilCbor {
  static Uint8List encodeValue(Object? value) {
    final b = BytesBuilder();
    _enc(b, value);
    return b.toBytes();
  }

  static void _head(BytesBuilder b, int major, int n) {
    final mt = major << 5;
    if (n < 24) {
      b.addByte(mt | n);
    } else if (n < 0x100) {
      b.addByte(mt | 24);
      b.addByte(n);
    } else if (n < 0x10000) {
      b.addByte(mt | 25);
      b.addByte((n >> 8) & 0xff);
      b.addByte(n & 0xff);
    } else if (n < 0x100000000) {
      b.addByte(mt | 26);
      for (var i = 3; i >= 0; i--) {
        b.addByte((n >> (i * 8)) & 0xff);
      }
    } else {
      b.addByte(mt | 27);
      for (var i = 7; i >= 0; i--) {
        b.addByte((n >> (i * 8)) & 0xff);
      }
    }
  }

  static void _enc(BytesBuilder b, Object? v) {
    if (v == null) {
      b.addByte(0xf6);
    } else if (v is bool) {
      b.addByte(v ? 0xf5 : 0xf4);
    } else if (v is int) {
      if (v >= 0) {
        _head(b, 0, v);
      } else {
        _head(b, 1, -1 - v);
      }
    } else if (v is double) {
      b.addByte(0xfb);
      final bd = ByteData(8)..setFloat64(0, v, Endian.big);
      b.add(bd.buffer.asUint8List());
    } else if (v is String) {
      final u = utf8.encode(v);
      _head(b, 3, u.length);
      b.add(u);
    } else if (v is Uint8List) {
      _head(b, 2, v.length);
      b.add(v);
    } else if (v is DateTime) {
      b.addByte(0xc0);
      final u = utf8.encode(v.toUtc().toIso8601String());
      _head(b, 3, u.length);
      b.add(u);
    } else if (v is List) {
      _head(b, 4, v.length);
      for (final e in v) {
        _enc(b, e);
      }
    } else if (v is Map) {
      _head(b, 5, v.length);
      v.forEach((k, val) {
        _enc(b, k);
        _enc(b, val);
      });
    } else {
      throw ArgumentError('CsilCbor: cannot encode ${v.runtimeType}');
    }
  }

  static Object? decode(List<int> bytes) {
    final b = Uint8List.fromList(bytes);
    final r = _dec(b, 0);
    if (r.next != b.length) {
      throw ArgumentError('CsilCbor: trailing bytes');
    }
    return r.value;
  }

  static _CsilRead _readArg(Uint8List b, int pos, int low) {
    if (low < 24) return _CsilRead(low, pos + 1);
    if (low == 24) return _CsilRead(b[pos + 1], pos + 2);
    if (low == 25) return _CsilRead((b[pos + 1] << 8) | b[pos + 2], pos + 3);
    if (low == 26) {
      var v = 0;
      for (var i = 1; i <= 4; i++) {
        v = (v << 8) | b[pos + i];
      }
      return _CsilRead(v, pos + 5);
    }
    if (low == 27) {
      var v = 0;
      for (var i = 1; i <= 8; i++) {
        v = (v << 8) | b[pos + i];
      }
      return _CsilRead(v, pos + 9);
    }
    throw ArgumentError('CsilCbor: bad head');
  }

  static _CsilDecoded _dec(Uint8List b, int pos) {
    final ib = b[pos];
    final major = ib >> 5;
    final low = ib & 0x1f;
    if (major == 7) {
      if (low == 20) return _CsilDecoded(false, pos + 1);
      if (low == 21) return _CsilDecoded(true, pos + 1);
      if (low == 22 || low == 23) return _CsilDecoded(null, pos + 1);
      if (low == 26) {
        final bd = ByteData.sublistView(b, pos + 1, pos + 5);
        return _CsilDecoded(bd.getFloat32(0, Endian.big), pos + 5);
      }
      if (low == 27) {
        final bd = ByteData.sublistView(b, pos + 1, pos + 9);
        return _CsilDecoded(bd.getFloat64(0, Endian.big), pos + 9);
      }
      throw ArgumentError('CsilCbor: unsupported simple value');
    }
    final a = _readArg(b, pos, low);
    final arg = a.value;
    final p = a.next;
    switch (major) {
      case 0:
        return _CsilDecoded(arg, p);
      case 1:
        return _CsilDecoded(-1 - arg, p);
      case 2:
        return _CsilDecoded(Uint8List.sublistView(b, p, p + arg), p + arg);
      case 3:
        return _CsilDecoded(utf8.decode(b.sublist(p, p + arg)), p + arg);
      case 4:
        final list = <Object?>[];
        var off4 = p;
        for (var i = 0; i < arg; i++) {
          final d = _dec(b, off4);
          list.add(d.value);
          off4 = d.next;
        }
        return _CsilDecoded(list, off4);
      case 5:
        final map = <Object?, Object?>{};
        var off5 = p;
        for (var i = 0; i < arg; i++) {
          final k = _dec(b, off5);
          final v = _dec(b, k.next);
          map[k.value] = v.value;
          off5 = v.next;
        }
        return _CsilDecoded(map, off5);
      case 6:
        final inner = _dec(b, p);
        if (arg == 0 && inner.value is String) {
          return _CsilDecoded(
              DateTime.parse(inner.value as String).toUtc(), inner.next);
        }
        return _CsilDecoded(inner.value, inner.next);
      default:
        throw ArgumentError('CsilCbor: unsupported major $major');
    }
  }
}

class _CsilRead {
  final int value;
  final int next;
  const _CsilRead(this.value, this.next);
}

class _CsilDecoded {
  final Object? value;
  final int next;
  const _CsilDecoded(this.value, this.next);
}
"#;
