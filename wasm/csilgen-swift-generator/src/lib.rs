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

fn build_files(input: &WasmGeneratorInput) -> Result<Vec<GeneratedFile>, i32> {
    let surface = match input.config.target.as_str() {
        "swift" | "swift-server" => Surface::Server,
        "swift-client" => Surface::Client,
        "swift-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let mut files = Vec::new();

    if let Some(types) = generate_types(input) {
        files.push(GeneratedFile {
            path: "Types.swift".to_string(),
            content: types,
        });
    }

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_client(input) {
                    files.push(GeneratedFile {
                        path: "Client.swift".to_string(),
                        content: client,
                    });
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

    Ok(files)
}

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

fn generate_client(input: &WasmGeneratorInput) -> Option<String> {
    let mut body = String::new();
    let mut any = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_client_struct(&rule.name, service));
            body.push_str(&emit_wire_ids(&rule.name, service));
            any = true;
        }
    }
    if !any {
        return None;
    }
    let mut content = header("Generated CSIL service clients.");
    content.push_str(CLIENT_PRELUDE_SWIFT);
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

fn emit_client_struct(name: &str, service: &CsilServiceDefinition) -> String {
    let base = service_base(name);
    let client = format!("{base}Client");
    let wire_service = name; // wire service name stays verbatim
    let mut out = String::new();
    out.push_str(&format!(
        "/// {client} is a typed client for the {name} service.\n"
    ));
    out.push_str(&format!("public struct {client} {{\n"));
    out.push_str("    public let transport: CsilTransport\n");
    out.push_str("    public init(transport: CsilTransport) {\n");
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
        let method = swift_ident(&op.name);
        let output = map_type(&success_type(&op.output_type), false);
        let wire_op = &op.name; // wire op name stays verbatim
        if is_null_input(&op.input_type) {
            out.push_str(&format!(
                "    public func {method}() throws -> {output} {{\n"
            ));
            out.push_str(&format!(
                "        try transport.call(service: {}, op: {}, request: nil as String?, responseType: {}.self)\n",
                swift_string_lit(wire_service),
                swift_string_lit(wire_op),
                output
            ));
        } else {
            let input = map_type(&op.input_type, false);
            out.push_str(&format!(
                "    public func {method}(_ request: {input}) throws -> {output} {{\n"
            ));
            out.push_str(&format!(
                "        try transport.call(service: {}, op: {}, request: request, responseType: {}.self)\n",
                swift_string_lit(wire_service),
                swift_string_lit(wire_op),
                output
            ));
        }
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

const CLIENT_PRELUDE_SWIFT: &str = "\
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

/// The caller-supplied transport: it encodes `request`, performs the call named by
/// (service, op), and decodes the response into `Resp`, or throws. Synchronous and
/// blocking — the host owns the I/O loop; the generator never owns the wire.
public protocol CsilTransport {
    func call<Req, Resp>(service: String, op: String, request: Req?, responseType: Resp.Type) throws -> Resp
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

#[cfg(test)]
mod tests;
