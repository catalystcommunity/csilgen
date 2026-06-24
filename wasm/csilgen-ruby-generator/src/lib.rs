//! Ruby code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target ruby` from `csilgen_ruby_generator.wasm`.
//! Emits idiomatic Ruby 3.2+ source: `Data.define` value objects, transport-agnostic
//! clients, server handler classes with verbose/compact router twins, and `validate`
//! methods. The generator emits *shapes and routing only* — never wire bytes; the host
//! supplies a duck-typed transport/codec seam.
//!
//! Sub-targets dispatch on `config.target`:
//! - `ruby` / `ruby-server` → value types + server handlers + routers + encoders
//! - `ruby-client`          → value types + transport-agnostic client classes
//! - `ruby-typesonly`       → value types alone
//!
//! The WASM-boundary exports (`get_metadata`/`allocate`/`deallocate`/`generate`) and the
//! `write_json` helper are the stable ABI; the codegen lives in
//! `generate_ruby_code_from_serialized`, which is also the entry the integration tests call.

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation,
    CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    GeneratedFile, GenerationStats, GeneratorCapability, GeneratorConfig, GeneratorMetadata,
    WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "ruby-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Ruby code generator".to_string(),
        target: "ruby".to_string(),
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

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = generate_ruby_code_from_serialized(&input.csil_spec, &input.config)?;
    let total_size: usize = files.iter().map(|f| f.content.len()).sum();
    let files_generated = files.len();

    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::new(),
        stats: GenerationStats {
            files_generated,
            total_size_bytes: total_size,
            services_count: input.csil_spec.service_count,
            fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
            generation_time_ms: 0,
            peak_memory_bytes: None,
        },
    })
}

/// Which generated surface a target requests. Mirrors Go/Python's `Surface` so the
/// three sub-targets (server, client, types-only) stay aligned across generators.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

/// Public entry the integration tests call directly (the cdylib's `generate` export
/// goes through `process_generation` instead). Returns the emitted `.rb` files, or a
/// WASM error code on an unknown sub-target so a typo fails loudly rather than
/// silently degrading to the default surface.
pub fn generate_ruby_code_from_serialized(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<Vec<GeneratedFile>, i32> {
    let surface = match config.target.as_str() {
        "ruby" | "ruby-server" => Surface::Server,
        "ruby-client" => Surface::Client,
        "ruby-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let mut files = Vec::new();

    if let Some(types) = generate_types_file(spec) {
        files.push(GeneratedFile {
            path: "types.rb".to_string(),
            content: types,
        });
    }

    if spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_client_file(spec) {
                    files.push(GeneratedFile {
                        path: "client.rb".to_string(),
                        content: client,
                    });
                }
            }
            Surface::Server => {
                if let Some(server) = generate_server_file(spec) {
                    files.push(GeneratedFile {
                        path: "server.rb".to_string(),
                        content: server,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// A CSIL type name (snake_case) → a Ruby class/constant name (PascalCase).
fn ruby_class_name(name: &str) -> String {
    name.to_case(Case::Pascal)
}

/// A CSIL operation name (kebab-case) → a Ruby method name (snake_case). A kebab is
/// illegal in a Ruby method name, so `deposit-claim` → `deposit_claim`.
fn ruby_method_name(name: &str) -> String {
    name.to_case(Case::Snake)
}

/// PascalCase a name with the same simple rule the Go/Python/TS clients use for the
/// wire. convert_case diverges on acronyms, and the wire string must agree
/// byte-for-byte across every language, so this is hand-rolled rather than
/// `to_case(Case::Pascal)` — a case transform must never leak onto the wire.
fn wire_method_name(name: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
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

/// The wire service base: strip a trailing `Service`, matching the Go/Python clients
/// so all languages address the same service string. Built from `wire_method_name`
/// (not convert_case) so the lowercased result agrees on the wire.
fn wire_service_base(name: &str) -> String {
    let pascal = wire_method_name(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

// ---------------------------------------------------------------------------
// Ruby string literals
// ---------------------------------------------------------------------------

/// A complete, always-valid Ruby double-quoted string literal for `s`, so an embedded
/// quote, backslash, or newline can never break the surrounding source. `#` is escaped
/// because Ruby performs `#{...}`/`#@`/`#$` interpolation inside double quotes.
fn ruby_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '#' => out.push_str("\\#"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

const FROZEN_HEADER: &str = "# frozen_string_literal: true\n";

/// Normalize a generated file to exactly one trailing newline. The per-type/-class
/// emitters each append a separating blank line, which would otherwise leave the file
/// ending in two newlines (a `Layout/TrailingEmptyLines` offense under standardrb).
fn finalize(mut content: String) -> String {
    while content.ends_with('\n') {
        content.pop();
    }
    content.push('\n');
    content
}

/// True when the spec uses the named builtin anywhere, so a `require` is emitted only
/// when the feature is actually present.
fn spec_uses_builtin(spec: &CsilSpecSerialized, builtin: &str) -> bool {
    spec.rules.iter().any(|rule| match &rule.rule_type {
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

fn generate_types_file(spec: &CsilSpecSerialized) -> Option<String> {
    let mut body = String::new();
    let mut has_types = false;

    for rule in &spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            has_types = true;
            body.push_str(&emit_value_type(&rule.name, &rule.doc_comments, group));
            continue;
        }
        if let CsilRuleType::TypeDef(type_expr) = &rule.rule_type {
            has_types = true;
            // Ruby is dynamically typed and has no type alias; surface the aliased
            // shape as a comment so the intent stays visible to a reader.
            for line in &rule.doc_comments {
                body.push_str(&format!("# {line}\n"));
            }
            body.push_str(&format!(
                "# {} is an alias for {}.\n\n",
                ruby_class_name(&rule.name),
                map_csil_type_to_ruby(type_expr)
            ));
        }
    }

    if !has_types {
        return None;
    }

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    // Only `require` what mapped/validated types actually use, so a spec of plain
    // scalars never pulls in bigdecimal/time.
    let mut needs_blank = false;
    if spec_uses_builtin(spec, "decimal") {
        content.push_str("require \"bigdecimal\"\n");
        needs_blank = true;
    }
    if spec_uses_builtin(spec, "timestamp") {
        content.push_str("require \"time\"\n");
        needs_blank = true;
    }
    if needs_blank {
        content.push('\n');
    }
    content.push_str(&body);
    Some(finalize(content))
}

/// Emit one `Data.define` value object. Optional fields and `.default`/`@default`
/// operators force an `initialize` override (Data has no default-arg constructor), and
/// any field carrying a runtime constraint adds a `validate` method.
fn emit_value_type(name: &str, doc_comments: &[String], group: &CsilGroupExpression) -> String {
    let class_name = ruby_class_name(name);
    let mut out = String::new();

    for line in doc_comments {
        out.push_str(&format!("# {line}\n"));
    }

    let fields: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();

    // A per-field summary comment: Ruby can't attach a doc to a `Data.define` member,
    // so the field/type lines sit above the class as a readable header.
    for entry in &fields {
        let field = field_name(entry.key.as_ref().unwrap());
        let ty = map_csil_type_to_ruby(&entry.value_type);
        if let Some(desc) = field_description(&entry.metadata) {
            out.push_str(&format!("# {field} [{ty}] {desc}\n"));
        } else {
            out.push_str(&format!("# {field} [{ty}]\n"));
        }
        if let Some(depends) = depends_comment(&entry.metadata) {
            out.push_str(&format!("#   depends-on: {depends}\n"));
        }
    }

    let members: Vec<String> = fields
        .iter()
        .map(|e| format!(":{}", field_name(e.key.as_ref().unwrap())))
        .collect();

    let needs_initialize = fields.iter().any(|e| {
        matches!(e.occurrence, Some(CsilOccurrence::Optional)) || entry_default_value(e).is_some()
    });
    let needs_validate = fields.iter().any(|e| entry_has_check(e));

    if members.is_empty() {
        out.push_str(&format!("{class_name} = Data.define\n\n"));
        return out;
    }

    if !needs_initialize && !needs_validate {
        out.push_str(&format!(
            "{class_name} = Data.define({})\n\n",
            members.join(", ")
        ));
        return out;
    }

    out.push_str(&format!(
        "{class_name} = Data.define({}) do\n",
        members.join(", ")
    ));

    if needs_initialize {
        out.push_str(&emit_initialize(&fields));
    }

    if needs_validate {
        if needs_initialize {
            out.push('\n');
        }
        out.push_str(&emit_validate(&fields));
    }

    out.push_str("end\n\n");
    out
}

/// `Data.define` generates a keyword constructor with no defaults; reopening it lets
/// optional fields default to `nil` and `.default(...)` fields to their literal. A bare
/// `super` forwards the same-named keyword args to the generated constructor.
fn emit_initialize(fields: &[&CsilGroupEntry]) -> String {
    // Ruby requires every optional keyword parameter to follow the required ones, so
    // the two are collected separately and concatenated. `super` forwards arguments by
    // name, so this reordering relative to the field/member order is purely cosmetic.
    let mut required = Vec::new();
    let mut optional = Vec::new();
    for entry in fields {
        let field = field_name(entry.key.as_ref().unwrap());
        if let Some(default) = entry_default_value(entry) {
            let value = literal_value_to_ruby_value(default, &entry.value_type);
            optional.push(format!("{field}: {value}"));
        } else if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            optional.push(format!("{field}: nil"));
        } else {
            required.push(format!("{field}:"));
        }
    }
    let mut params = required;
    params.extend(optional);
    let mut out = String::new();
    out.push_str(&format!("  def initialize({})\n", params.join(", ")));
    out.push_str("    super\n");
    out.push_str("  end\n");
    out
}

/// A field's Ruby reader name plus whether it is optional (may be `nil`). Threaded
/// through the check emitters so each guards a `nil` optional before reading it.
#[derive(Clone, Copy)]
struct FieldRef<'a> {
    name: &'a str,
    optional: bool,
}

/// Emit the `validate` method: idiomatic Ruby raises `ArgumentError` on the first
/// violation. Optional fields are guarded so a `nil` value is skipped rather than
/// raising a `NoMethodError`.
fn emit_validate(fields: &[&CsilGroupEntry]) -> String {
    let mut out = String::new();
    out.push_str("  # Raises ArgumentError on the first constraint violation.\n");
    out.push_str("  def validate\n");
    for entry in fields {
        let field = field_name(entry.key.as_ref().unwrap());
        let fref = FieldRef {
            name: &field,
            optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
        };
        for metadata in &entry.metadata {
            if let CsilFieldMetadata::Constraint(constraint) = metadata {
                emit_metadata_constraint(&mut out, fref, &entry.value_type, constraint);
            }
        }
        if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
            for op in constraints {
                emit_control_op_check(&mut out, fref, &entry.value_type, op);
            }
        }
    }
    out.push_str("    nil\n");
    out.push_str("  end\n");
    out
}

/// Emit a single guarded check: `raise ArgumentError, <msg> if <guard><condition>`. The
/// guard skips the check when an optional field is `nil`.
fn push_guarded(out: &mut String, field: FieldRef, condition: &str, message: &str) {
    let lit = ruby_string_literal(message);
    let guard = if field.optional {
        format!("!{}.nil? && ", field.name)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "    raise ArgumentError, {lit} if {guard}{condition}\n"
    ));
}

// ---------------------------------------------------------------------------
// Validation: which constraints actually yield a runtime check
// ---------------------------------------------------------------------------

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
    // `@default` is a constructor concern; every other annotation (including a `regex`
    // Custom) produces a runtime check.
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

/// Whether a field's (possibly constrained) base is an ordered core type needing a
/// typed bound rather than a bare scalar compare: `decimal` parses through
/// `BigDecimal`, `timestamp` through `Time.iso8601`.
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

/// Emit one ordered comparison honoring the field's type. `ruby_op` is the operator
/// whose truth means the constraint is violated (`.ge` is violated when the value is
/// `<` the bound). Numeric fields compare directly; `decimal`/`timestamp` parse the
/// bound into the matching Ruby value so the comparison always type-checks at runtime.
fn emit_ordered_check(
    out: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
) {
    let (ruby_op, desc) = op;
    let access = field.name;
    let name = field.name;
    match ordered_field_kind(value_type) {
        OrderedKind::Numeric => {
            let bound = literal_value_to_ruby(value);
            let condition = format!("{access} {ruby_op} {bound}");
            let message = format!("field '{name}' must be {desc} {bound}");
            push_guarded(out, field, &condition, &message);
        }
        OrderedKind::Decimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let bound = format!("BigDecimal({})", ruby_string_literal(&text));
            let condition = format!("{access} {ruby_op} {bound}");
            let message = format!("field '{name}' must be {desc} {text}");
            push_guarded(out, field, &condition, &message);
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            let bound = format!("Time.iso8601({})", ruby_string_literal(&text));
            let condition = format!("{access} {ruby_op} {bound}");
            let message = format!("field '{name}' must be {desc} {text}");
            push_guarded(out, field, &condition, &message);
        }
    }
}

/// Emit a single `@`-annotation ValidationConstraint as a Ruby check.
fn emit_metadata_constraint(
    out: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
) {
    match constraint {
        CsilValidationConstraint::MinLength(n) => {
            let unit = if *n == 1 { "character" } else { "characters" };
            emit_len_check(out, field, "<", *n, &format!("at least {n} {unit}"));
        }
        CsilValidationConstraint::MaxLength(n) => {
            let unit = if *n == 1 { "character" } else { "characters" };
            emit_len_check(out, field, ">", *n, &format!("at most {n} {unit}"));
        }
        CsilValidationConstraint::MinItems(n) => {
            let unit = if *n == 1 { "item" } else { "items" };
            emit_len_check(out, field, "<", *n, &format!("at least {n} {unit}"));
        }
        CsilValidationConstraint::MaxItems(n) => {
            let unit = if *n == 1 { "item" } else { "items" };
            emit_len_check(out, field, ">", *n, &format!("at most {n} {unit}"));
        }
        CsilValidationConstraint::MinValue(v) => {
            emit_ordered_check(out, field, value_type, ("<", "at least"), v);
        }
        CsilValidationConstraint::MaxValue(v) => {
            emit_ordered_check(out, field, value_type, (">", "at most"), v);
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                emit_regex_check(out, field, pattern);
            }
        }
    }
}

/// Emit a single `.`-control-operator. Comparisons and size/regex become runtime
/// checks; `.default` is applied by `initialize`; the encoding-only operators
/// (.bits/.and/.within/.json/.cbor/.cborseq) leave a comment so their presence is
/// visible but they never fail validation.
fn emit_control_op_check(
    out: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
) {
    let name = field.name;
    match op {
        CsilControlOperator::GreaterEqual(v) => {
            emit_ordered_check(out, field, value_type, ("<", "at least"), v)
        }
        CsilControlOperator::LessEqual(v) => {
            emit_ordered_check(out, field, value_type, (">", "at most"), v)
        }
        CsilControlOperator::GreaterThan(v) => {
            emit_ordered_check(out, field, value_type, ("<=", "greater than"), v)
        }
        CsilControlOperator::LessThan(v) => {
            emit_ordered_check(out, field, value_type, (">=", "less than"), v)
        }
        CsilControlOperator::Equal(v) => {
            emit_ordered_check(out, field, value_type, ("!=", "equal to"), v)
        }
        CsilControlOperator::NotEqual(v) => {
            emit_ordered_check(out, field, value_type, ("==", "not equal to"), v)
        }
        CsilControlOperator::Size(size) => emit_size_check(out, field, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(out, field, pattern),
        CsilControlOperator::Default(_) => {}
        CsilControlOperator::Bits(bits) => {
            out.push_str(&format!(
                "    # field '{name}' carries .bits({bits}); a bit-set encoding hint, not a runtime check\n"
            ));
        }
        CsilControlOperator::And(_) => {
            out.push_str(&format!(
                "    # field '{name}' carries .and; intersection constraint left to the consumer\n"
            ));
        }
        CsilControlOperator::Within(_) => {
            out.push_str(&format!(
                "    # field '{name}' carries .within; range membership left to the consumer\n"
            ));
        }
        CsilControlOperator::Json | CsilControlOperator::Cbor | CsilControlOperator::Cborseq => {
            out.push_str(&format!(
                "    # field '{name}' carries an embedded-encoding operator; handled at (de)serialization, not validated\n"
            ));
        }
    }
}

/// A `.length`-based check shared by `@min-length`/`.size`/etc.; Ruby strings, arrays,
/// and hashes all respond to `.length`.
fn emit_len_check(out: &mut String, field: FieldRef, op: &str, n: u64, tail: &str) {
    let access = field.name;
    let name = field.name;
    let condition = format!("{access}.length {op} {n}");
    let message = format!("field '{name}' must have {tail}");
    push_guarded(out, field, &condition, &message);
}

fn emit_size_check(out: &mut String, field: FieldRef, size: &CsilSizeConstraint) {
    let mut one = |op: &str, n: u64, word: &str| {
        emit_len_check(out, field, op, n, &format!("{word} {n} elements"));
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

/// `Regexp.new(<literal>)` is used rather than a `/.../ ` literal so an arbitrary
/// pattern string can't break out of the regex delimiter; `match?` avoids setting the
/// `$~` global. The check fails when the value does NOT match.
fn emit_regex_check(out: &mut String, field: FieldRef, pattern: &str) {
    let access = field.name;
    let name = field.name;
    let condition = format!(
        "!{access}.match?(Regexp.new({}))",
        ruby_string_literal(pattern)
    );
    let message = format!("field '{name}' must match pattern '{pattern}'");
    push_guarded(out, field, &condition, &message);
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

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

/// A literal as a Ruby value for an `initialize` default, honoring `decimal`/`timestamp`
/// fields (a bare string literal would be the wrong runtime type for those).
fn literal_value_to_ruby_value(
    value: &CsilLiteralValue,
    value_type: &CsilTypeExpression,
) -> String {
    match ordered_field_kind(value_type) {
        OrderedKind::Decimal => {
            if let Some(text) = literal_as_decimal_text(value) {
                return format!("BigDecimal({})", ruby_string_literal(&text));
            }
        }
        OrderedKind::Timestamp => {
            if let Some(text) = literal_as_timestamp_text(value) {
                return format!("Time.iso8601({})", ruby_string_literal(&text));
            }
        }
        OrderedKind::Numeric => {}
    }
    literal_value_to_ruby(value)
}

fn literal_value_to_ruby(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => ruby_string_literal(s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "\"\".b".to_string(),
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(literal_value_to_ruby).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Type mapping (doc-comment only — Ruby is dynamically typed)
// ---------------------------------------------------------------------------

fn map_csil_type_to_ruby(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => "Integer".to_string(),
            "float" => "Float".to_string(),
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "String".to_string(),
            "bool" => "Boolean".to_string(),
            "timestamp" => "Time".to_string(),
            "decimal" => "BigDecimal".to_string(),
            "nil" | "null" => "nil".to_string(),
            other => ruby_class_name(other),
        },
        CsilTypeExpression::Reference(name) => ruby_class_name(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("Array<{}>", map_csil_type_to_ruby(element_type))
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "Hash<{}, {}>",
            map_csil_type_to_ruby(key),
            map_csil_type_to_ruby(value)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => map_csil_type_to_ruby(base_type),
        CsilTypeExpression::Choice(choices) => {
            // Literal arms (a string/enum choice like `"a" / "b"`) all map to the same
            // underlying Ruby class, so collapse duplicates: `String | "a" | "b"`
            // becomes a clean `String` rather than `String | Object | Object`.
            let mut parts: Vec<String> = Vec::new();
            for choice in choices {
                let mapped = map_csil_type_to_ruby(choice);
                if !parts.contains(&mapped) {
                    parts.push(mapped);
                }
            }
            parts.join(" | ")
        }
        CsilTypeExpression::Literal(value) => literal_type_name(value).to_string(),
        CsilTypeExpression::Tuple(_) => "Array".to_string(),
        _ => "Object".to_string(),
    }
}

/// The Ruby class a literal value documents as. A literal type arm (e.g. an enum's
/// `"pending"`) is shown by its class so a choice of string literals reads as `String`.
fn literal_type_name(value: &CsilLiteralValue) -> &'static str {
    match value {
        CsilLiteralValue::Integer(_) => "Integer",
        CsilLiteralValue::Float(_) => "Float",
        CsilLiteralValue::Text(_) | CsilLiteralValue::Bytes(_) => "String",
        CsilLiteralValue::Bool(_) => "Boolean",
        CsilLiteralValue::Null => "nil",
        CsilLiteralValue::Array(_) => "Array",
    }
}

// ---------------------------------------------------------------------------
// Field metadata helpers
// ---------------------------------------------------------------------------

fn field_name(key: &CsilGroupKey) -> String {
    // CSIL fields are snake_case and double as the verbatim CBOR map key, so they are
    // kept as-is — no case transform that could leak onto the wire.
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

fn field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.as_str())
        } else {
            None
        }
    })
}

fn depends_comment(metadata: &[CsilFieldMetadata]) -> Option<String> {
    metadata
        .iter()
        .find_map(|meta| match meta {
            CsilFieldMetadata::DependsOnExpr(condition) => {
                Some(render_depends_condition(condition))
            }
            CsilFieldMetadata::DependsOn { field, value } => Some(match value {
                Some(value) => format!("{field} == {}", literal_value_to_ruby(value)),
                None => field.clone(),
            }),
            _ => None,
        })
        // The condition lands in a `#` line comment; an embedded break would push the
        // remainder onto an uncommented line, so collapse it to one line.
        .map(|rendered| rendered.replace(['\n', '\r'], " "))
}

fn render_depends_condition(condition: &CsilDependsCondition) -> String {
    match condition {
        CsilDependsCondition::Compare { field, op, value } => match (op, value) {
            (Some(op), Some(value)) => format!(
                "{field} {} {}",
                depends_compare_op_str(op),
                literal_value_to_ruby(value)
            ),
            _ => field.clone(),
        },
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

// ---------------------------------------------------------------------------
// Operation helpers
// ---------------------------------------------------------------------------

/// A push op (`<- Event`) carries a `null` input type: there is no request body to
/// send, so the client/handler method drops the request parameter.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — that error half rides the
/// transport, not the returned value.
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

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

const CLIENT_PRELUDE: &str = "\
# Each generated client delegates to a host-supplied transport seam: a duck-typed
# object responding to `call(service, op, payload)`, returning the decoded response.
# The generator never owns the wire — it emits call shapes only.
";

fn generate_client_file(spec: &CsilSpecSerialized) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_client_class(&rule.name, service));
            if let Some(wire_ids) = emit_wire_ids(&rule.name, service) {
                body.push_str(&wire_ids);
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    content.push_str(CLIENT_PRELUDE);
    content.push('\n');
    content.push_str(&body);
    Some(finalize(content))
}

fn emit_client_class(name: &str, service: &CsilServiceDefinition) -> String {
    let base = wire_service_base(name);
    let client = format!("{base}Client");
    let wire_service = base.to_lowercase();

    let mut out = String::new();
    out.push_str(&format!("# Typed client for the {name} service.\n"));
    out.push_str(&format!("class {client}\n"));
    out.push_str("  def initialize(transport)\n");
    out.push_str("    @transport = transport\n");
    out.push_str("  end\n");

    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops ride
        // the router/encoder surface emitted by the server target.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            out.push_str(&format!(
                "\n  # channel operation {} is not part of the RPC client\n",
                op.name
            ));
            continue;
        }
        let method = ruby_method_name(&op.name);
        let wire_method = wire_method_name(&op.name);
        let has_input = !op_input_is_null(&op.input_type);
        let out_ty = map_csil_type_to_ruby(&success_type(&op.output_type));

        out.push('\n');
        if op.doc_comments.is_empty() {
            out.push_str(&format!("  # {}: -> {out_ty}\n", op.name));
        } else {
            for line in &op.doc_comments {
                out.push_str(&format!("  # {line}\n"));
            }
        }
        if has_input {
            out.push_str(&format!("  def {method}(req)\n"));
            out.push_str(&format!(
                "    @transport.call(\"{wire_service}\", \"{wire_method}\", req)\n"
            ));
        } else {
            // A null-input op carries no request body; pass nil as the payload.
            out.push_str(&format!("  def {method}\n"));
            out.push_str(&format!(
                "    @transport.call(\"{wire_service}\", \"{wire_method}\", nil)\n"
            ));
        }
        out.push_str("  end\n");
    }

    out.push_str("end\n\n");
    out
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

const SERVER_CODEC_NOTE: &str = "\
# The router/encoder functions take a host-supplied codec: a duck-typed object
# responding to `encode(value) -> bytes` and `decode(bytes, type) -> value`. The
# generator is codec-agnostic; the implementer wires it to CBOR, JSON, or anything else.
";

fn generate_server_file(spec: &CsilSpecSerialized) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    let has_channel_ops = spec.rules.iter().any(
        |r| matches!(&r.rule_type, CsilRuleType::ServiceDef(def) if service_has_channel_ops(def)),
    );

    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_handlers_class(&rule.name, service));
            if service_has_channel_ops(service) {
                body.push_str(&emit_router_module(&rule.name, service));
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    if has_channel_ops {
        content.push_str(SERVER_CODEC_NOTE);
        content.push('\n');
    }
    content.push_str(&body);
    Some(finalize(content))
}

fn emit_handlers_class(name: &str, service: &CsilServiceDefinition) -> String {
    let base = wire_service_base(name);
    let handler_class = format!("{base}Handlers");
    let mut out = String::new();

    out.push_str(&format!(
        "# Server-side handlers for the {name} service. Subclass and override each\n"
    ));
    out.push_str("# operation; the unimplemented base raises NotImplementedError.\n");
    out.push_str(&format!("class {handler_class}\n"));

    // Unidirectional ops are request/response; bidirectional are fire-and-forget
    // inbound. Reverse ops are server-push only and have no inbound handler.
    let inbound: Vec<&CsilServiceOperation> = service
        .operations
        .iter()
        .filter(|op| {
            matches!(
                op.direction,
                CsilServiceDirection::Unidirectional | CsilServiceDirection::Bidirectional
            )
        })
        .collect();

    if inbound.is_empty() {
        out.push_str("  # Reverse-only service: the server only pushes, never receives.\n");
    } else {
        for (i, op) in inbound.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let method = ruby_method_name(&op.name);
            let has_input = !op_input_is_null(&op.input_type);
            let param = match (has_input, &op.direction) {
                (false, _) => String::new(),
                (true, CsilServiceDirection::Bidirectional) => "(msg)".to_string(),
                (true, _) => "(req)".to_string(),
            };
            if op.doc_comments.is_empty() {
                out.push_str(&format!("  # {}\n", op.name));
            } else {
                for line in &op.doc_comments {
                    out.push_str(&format!("  # {line}\n"));
                }
            }
            out.push_str(&format!("  def {method}{param}\n"));
            out.push_str(&format!(
                "    raise NotImplementedError, \"{handler_class}#{method}\"\n"
            ));
            out.push_str("  end\n");
        }
    }
    out.push_str("end\n\n");
    out
}

/// Emit the router module for a channel-bearing service: wire-id constants, the verbose
/// router (dispatch on the wire method string), the compact router twin (dispatch on
/// the `@wire-id` ordinal, only when wire-ids are present so wire-id-free output stays
/// byte-identical), and the per-op outbound encoders.
fn emit_router_module(name: &str, service: &CsilServiceDefinition) -> String {
    let base = wire_service_base(name);
    let router = format!("{base}Router");
    let mut out = String::new();

    out.push_str(&format!(
        "# Channel router and encoders for the {name} service.\n"
    ));
    out.push_str(&format!("module {router}\n"));
    out.push_str("  module_function\n\n");

    // Wire-id constants, additive: nothing emitted unless the service carries them.
    if let Some(consts) = emit_wire_id_consts(service) {
        out.push_str(&consts);
    }

    let bidi: Vec<&CsilServiceOperation> = service
        .operations
        .iter()
        .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
        .collect();

    // Verbose router: dispatch one inbound frame by its wire method name.
    out.push_str(&format!(
        "  # Decode one inbound channel frame for {name} and dispatch to a handler\n"
    ));
    out.push_str("  # method, keyed by the verbose wire method name.\n");
    out.push_str("  def route_channel(handlers, codec, method, data)\n");
    out.push_str("    case method\n");
    for op in &bidi {
        let wire = wire_method_name(&op.name);
        let method = ruby_method_name(&op.name);
        out.push_str(&format!("    when \"{wire}\"\n"));
        if op_input_is_null(&op.input_type) {
            out.push_str(&format!("      handlers.{method}\n"));
        } else {
            let ty = map_csil_type_to_ruby(&op.input_type);
            out.push_str(&format!("      msg = codec.decode(data, {ty})\n"));
            out.push_str(&format!("      handlers.{method}(msg)\n"));
        }
    }
    out.push_str("    else\n");
    out.push_str("      raise ArgumentError, \"unknown channel method #{method}\"\n");
    out.push_str("    end\n");
    out.push_str("  end\n\n");

    // Compact router twin: dispatch by @wire-id ordinal, only for wire-id services.
    if service.wire_id.is_some() {
        out.push_str("  # Compact transport profile: dispatch one inbound frame by its @wire-id\n");
        out.push_str("  # ordinal. The verbose twin is route_channel; the host calls whichever\n");
        out.push_str("  # matches the profile negotiated on the wire.\n");
        out.push_str("  def route_channel_compact(handlers, codec, op, data)\n");
        out.push_str("    case op\n");
        for op in &bidi {
            let Some(op_id) = op.wire_id else { continue };
            let method = ruby_method_name(&op.name);
            out.push_str(&format!("    when {op_id}\n"));
            if op_input_is_null(&op.input_type) {
                out.push_str(&format!("      handlers.{method}\n"));
            } else {
                let ty = map_csil_type_to_ruby(&op.input_type);
                out.push_str(&format!("      msg = codec.decode(data, {ty})\n"));
                out.push_str(&format!("      handlers.{method}(msg)\n"));
            }
        }
        out.push_str("    else\n");
        out.push_str("      raise ArgumentError, \"unknown channel ordinal #{op}\"\n");
        out.push_str("    end\n");
        out.push_str("  end\n\n");
    }

    // Outbound encoders for <-> and <- ops (server pushes Output to a peer).
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let wire = wire_method_name(&op.name);
        let method = ruby_method_name(&op.name);
        out.push_str(&format!(
            "  # Encode a `{wire}` message the server pushes to a peer; returns\n"
        ));
        out.push_str("  # [method, bytes] for the implementer to frame on its connection.\n");
        out.push_str(&format!("  def encode_{method}(codec, msg)\n"));
        out.push_str(&format!("    [\"{wire}\", codec.encode(msg)]\n"));
        out.push_str("  end\n\n");
    }

    // Trim the trailing blank line before the module's `end`.
    if out.ends_with("\n\n") {
        out.pop();
    }
    out.push_str("end\n\n");
    out
}

/// The wire-id constants for a router module, indented two spaces. Returns None for a
/// wire-id-free service so its output stays byte-identical.
fn emit_wire_id_consts(service: &CsilServiceDefinition) -> Option<String> {
    let service_id = service.wire_id?;
    let mut out = String::new();
    out.push_str("  # Wire-id ordinals (transport compact profiles).\n");
    out.push_str(&format!("  SERVICE_WIRE_ID = {service_id}\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            let const_name = format!("OP_{}_WIRE_ID", op.name.to_case(Case::ScreamingSnake));
            out.push_str(&format!("  {const_name} = {op_id}\n"));
        }
    }
    out.push('\n');
    Some(out)
}

/// Module-level wire-id constants emitted alongside a client class (so a host can
/// reference ordinals without a router). Additive: None when the service is wire-id-free.
fn emit_wire_ids(name: &str, service: &CsilServiceDefinition) -> Option<String> {
    let service_id = service.wire_id?;
    let base = wire_service_base(name);
    let module = format!("{base}WireIds");
    let mut out = String::new();
    out.push_str(&format!(
        "# Wire-id ordinals for the {name} service (transport compact profiles).\n"
    ));
    out.push_str(&format!("module {module}\n"));
    out.push_str(&format!("  SERVICE = {service_id}\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            let const_name = format!("OP_{}", op.name.to_case(Case::ScreamingSnake));
            out.push_str(&format!("  {const_name} = {op_id}\n"));
        }
    }
    out.push_str("end\n\n");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_and_method_naming() {
        assert_eq!(ruby_class_name("user_profile"), "UserProfile");
        assert_eq!(ruby_method_name("deposit-claim"), "deposit_claim");
        assert_eq!(wire_method_name("deposit-claim"), "DepositClaim");
        assert_eq!(wire_service_base("CorndogsService"), "Corndogs");
    }

    #[test]
    fn string_literal_escapes_interpolation() {
        assert_eq!(ruby_string_literal("a#{b}"), "\"a\\#{b}\"");
        assert_eq!(ruby_string_literal("x\"y"), "\"x\\\"y\"");
    }
}
