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
use std::collections::HashSet;

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

/// Python code generator implementation
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

        let mut files = Vec::new();

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
                CsilRuleType::ServiceDef(service) => match &surface {
                    Surface::TypesOnly => {}
                    Surface::Client => {
                        if !prelude_emitted {
                            services_code.push_str(&Self::generate_client_prelude());
                            prelude_emitted = true;
                        }
                        services_code.push_str(&self.generate_client_class(&rule.name, service)?);
                    }
                    Surface::Server => {
                        if !prelude_emitted {
                            services_code
                                .push_str(&Self::generate_services_prelude(has_channel_ops));
                            prelude_emitted = true;
                        }
                        services_code
                            .push_str(&self.generate_service_artifacts(&rule.name, service)?);
                    }
                },
            }
        }

        if !types_code.is_empty() {
            let types_file = self.generate_types_file(types_code)?;
            files.push(types_file);
        }

        if !services_code.is_empty() {
            let module_file =
                self.generate_module_file(services_code, matches!(surface, Surface::Client))?;
            files.push(module_file);
        }

        if !files.is_empty() {
            let init_file = self.generate_init_file(&files)?;
            files.push(init_file);
        }

        Ok(files)
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
    fn generate_client_prelude() -> String {
        let mut out = String::new();
        out.push_str("from typing import Protocol, Any\n\n");
        out.push_str("class ServiceError(Exception):\n");
        out.push_str(
            "    \"\"\"Structured error a service returns; raised by the transport.\"\"\"\n",
        );
        out.push_str("    def __init__(self, code: int, message: str):\n");
        out.push_str("        self.code = code\n");
        out.push_str("        self.message = message\n");
        out.push_str("        super().__init__(f\"service error {code}: {message}\")\n\n");
        out.push_str("class Transport(Protocol):\n");
        out.push_str(
            "    \"\"\"Caller-supplied wire. Encodes req (CBOR over HTTP, say), performs the\n\
             \x20   call named by (service, method), and returns the decoded response, or\n\
             \x20   raises ServiceError. The generator never owns the wire.\n\
             \x20   \"\"\"\n",
        );
        out.push_str("    def call(self, service: str, method: str, req: Any) -> Any: ...\n\n");
        out
    }

    /// Emit a typed client class for one service: one method per unary operation
    /// that delegates to the `Transport`, returning the typed success response.
    fn generate_client_class(&self, name: &str, service: &CsilServiceDefinition) -> Result<String> {
        let service_class = name.to_case(Case::Pascal);
        let base = service_class
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .unwrap_or(&service_class);
        let client_class = format!("{base}Client");
        let wire_service = base.to_lowercase();

        let mut out = String::new();
        out.push_str(&format!("class {client_class}:\n"));
        out.push_str(&format!(
            "    \"\"\"Typed client for the {name} service.\"\"\"\n"
        ));
        out.push_str("    def __init__(self, transport: Transport):\n");
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
            let method_name = op.name.to_case(Case::Snake);
            // The wire method must agree byte-for-byte with the other language
            // clients, which all PascalCase the op name with the same simple
            // rule — convert_case would diverge on acronyms, so avoid it here.
            let wire_method = wire_method_name(&op.name);
            // A `null`-input op carries no request body, so the method takes no
            // `req` parameter and passes `None` as the payload to the transport.
            let has_input = !is_null_input(&op.input_type);
            let output_type = self.map_type_expression(&python_success_type(&op.output_type))?;
            out.push('\n');
            if has_input {
                let input_type = self.map_type_expression(&op.input_type)?;
                out.push_str(&format!(
                    "    def {method_name}(self, req: {input_type}) -> {output_type}:\n"
                ));
            } else {
                out.push_str(&format!("    def {method_name}(self) -> {output_type}:\n"));
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
            let payload = if has_input { "req" } else { "None" };
            out.push_str(&format!(
                "        return self._transport.call(\"{wire_service}\", \"{wire_method}\", {payload})\n"
            ));
        }
        out.push('\n');
        Ok(out)
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

    fn generate_module_file(&self, body_code: String, want_client: bool) -> Result<GeneratedFile> {
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

        content.push_str("\n\n");
        content.push_str(&body_code);

        Ok(GeneratedFile {
            path: path.to_string(),
            content,
        })
    }

    fn generate_init_file(&self, files: &[GeneratedFile]) -> Result<GeneratedFile> {
        let mut content = String::new();

        content.push_str("# Generated package init from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n\n");

        let mut exports = Vec::new();

        for file in files {
            if file.path == "types.py" {
                content.push_str("from .types import *\n");
                exports.push("types");
            } else if file.path == "services.py" {
                content.push_str("from .services import *\n");
                exports.push("services");
            } else if file.path == "client.py" {
                content.push_str("from .client import *\n");
                exports.push("client");
            }
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

        assert_eq!(result.len(), 2); // types.py and __init__.py

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
                    }],
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
                        },
                        CsilServiceOperation {
                            name: "play".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Builtin("text".to_string()),
                            direction: CsilServiceDirection::Bidirectional,
                            position: create_test_position(),
                            doc_comments: vec!["Open a play channel.".to_string()],
                        },
                    ],
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
                    }],
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

        // Should have types.py, services.py, and __init__.py
        assert_eq!(result.len(), 3);

        let init_file = result.iter().find(|f| f.path == "__init__.py").unwrap();
        assert!(init_file.content.contains("from .types import *"));
        assert!(init_file.content.contains("from .services import *"));
        assert!(
            init_file
                .content
                .contains("__all__ = [\"types\", \"services\"]")
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

    fn service_spec_with_union_op() -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "CorndogsService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "SubmitTask".to_string(),
                        input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                        output_type: CsilTypeExpression::Choice(vec![
                            CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                            CsilTypeExpression::Reference("ServiceError".to_string()),
                        ]),
                        direction: CsilServiceDirection::Unidirectional,
                        position: create_test_position(),
                        doc_comments: Vec::new(),
                    }],
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
        assert!(client.content.contains("class CorndogsClient:"));
        // Success type is stripped from the `/ ServiceError` union.
        assert!(
            client
                .content
                .contains("def submit_task(self, req: SubmitTaskRequest) -> SubmitTaskResponse:")
        );
        assert!(
            client
                .content
                .contains("return self._transport.call(\"corndogs\", \"SubmitTask\", req)")
        );
        // The server handler surface must not be emitted for the client target.
        assert!(!result.iter().any(|f| f.path == "services.py"));
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
                        }],
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

        // Client method: no `req` parameter, passes `None` payload.
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
            client_src.contains("def heartbeat(self) -> bool:"),
            "null-input client method must take no req, got:\n{client_src}"
        );
        // The transport receives `None` as the payload, not a bound `req`.
        assert!(client_src.contains("\"ping\", \"Heartbeat\", None)"));
    }
}
