//! WASM entry points for the OpenAPI generator.
//!
//! The existing `generate_openapi_spec` operates on `csilgen_core::ast::CsilSpec`
//! (a structurally-richer AST). The wasm boundary delivers a
//! `csilgen_common::CsilSpecSerialized`. The two are isomorphic for the rules
//! they share, so we convert `Serialized → Core` here and call straight into
//! the existing generator. Refactoring the generator's internals to consume
//! the serialized form is deferred (see `docs/csilgen-requests/`).

use csilgen_common::{
    CsilControlOperator, CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry,
    CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilPosition, CsilRule,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation,
    CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};
use csilgen_core::ast::{
    ControlOperator, CsilSpec, FieldMetadata, FieldVisibility, GroupEntry, GroupExpression,
    GroupKey, LiteralValue, MetadataParameter, Occurrence, Rule, RuleType, ServiceDefinition,
    ServiceDirection, ServiceOperation, SizeConstraint, TypeExpression, ValidationConstraint,
};
use csilgen_core::lexer::Position;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "openapi-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "OpenAPI 3.0 specification generator".to_string(),
        target: "openapi".to_string(),
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

    let core_spec = to_core_spec(&input.csil_spec);
    let files = crate::generate_openapi_spec(&core_spec, &input.config)
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
// Serialized -> Core conversion (structurally isomorphic, mechanical)
// ---------------------------------------------------------------------------

fn to_core_spec(s: &CsilSpecSerialized) -> CsilSpec {
    CsilSpec {
        imports: Vec::new(),
        options: None,
        rules: s.rules.iter().map(to_rule).collect(),
    }
}

fn to_rule(r: &CsilRule) -> Rule {
    Rule {
        name: r.name.clone(),
        rule_type: to_rule_type(&r.rule_type),
        position: to_position(&r.position),
        doc_comments: r.doc_comments.clone(),
    }
}

fn to_rule_type(t: &CsilRuleType) -> RuleType {
    match t {
        CsilRuleType::TypeDef(e) => RuleType::TypeDef(to_type_expression(e)),
        CsilRuleType::GroupDef(g) => RuleType::GroupDef(to_group_expression(g)),
        CsilRuleType::TypeChoice(cs) => {
            RuleType::TypeChoice(cs.iter().map(to_type_expression).collect())
        }
        CsilRuleType::GroupChoice(gs) => {
            RuleType::GroupChoice(gs.iter().map(to_group_expression).collect())
        }
        CsilRuleType::ServiceDef(d) => RuleType::ServiceDef(to_service_definition(d)),
    }
}

fn to_type_expression(t: &CsilTypeExpression) -> TypeExpression {
    match t {
        CsilTypeExpression::Builtin(n) => TypeExpression::Builtin(n.clone()),
        CsilTypeExpression::Reference(n) => TypeExpression::Reference(n.clone()),
        CsilTypeExpression::Array {
            element_type,
            occurrence,
        } => TypeExpression::Array {
            element_type: Box::new(to_type_expression(element_type)),
            occurrence: occurrence.as_ref().map(to_occurrence),
        },
        CsilTypeExpression::Map {
            key,
            value,
            occurrence,
        } => TypeExpression::Map {
            key: Box::new(to_type_expression(key)),
            value: Box::new(to_type_expression(value)),
            occurrence: occurrence.as_ref().map(to_occurrence),
        },
        CsilTypeExpression::Group(g) => TypeExpression::Group(to_group_expression(g)),
        CsilTypeExpression::Choice(cs) => {
            TypeExpression::Choice(cs.iter().map(to_type_expression).collect())
        }
        CsilTypeExpression::Range {
            start,
            end,
            inclusive,
        } => TypeExpression::Range {
            start: *start,
            end: *end,
            inclusive: *inclusive,
        },
        CsilTypeExpression::Socket(n) => TypeExpression::Socket(n.clone()),
        CsilTypeExpression::Plug(n) => TypeExpression::Plug(n.clone()),
        CsilTypeExpression::Literal(l) => TypeExpression::Literal(to_literal_value(l)),
        CsilTypeExpression::Constrained {
            base_type,
            constraints,
        } => TypeExpression::Constrained {
            base_type: Box::new(to_type_expression(base_type)),
            constraints: constraints.iter().map(to_control_operator).collect(),
        },
    }
}

fn to_group_expression(g: &CsilGroupExpression) -> GroupExpression {
    GroupExpression {
        entries: g.entries.iter().map(to_group_entry).collect(),
    }
}

fn to_group_entry(e: &CsilGroupEntry) -> GroupEntry {
    GroupEntry {
        key: e.key.as_ref().map(to_group_key),
        value_type: to_type_expression(&e.value_type),
        occurrence: e.occurrence.as_ref().map(to_occurrence),
        metadata: e.metadata.iter().map(to_field_metadata).collect(),
        doc_comments: e.doc_comments.clone(),
    }
}

fn to_group_key(k: &CsilGroupKey) -> GroupKey {
    match k {
        CsilGroupKey::Bare(n) => GroupKey::Bare(n.clone()),
        CsilGroupKey::Type(t) => GroupKey::Type(to_type_expression(t)),
        CsilGroupKey::Literal(l) => GroupKey::Literal(to_literal_value(l)),
    }
}

fn to_occurrence(o: &CsilOccurrence) -> Occurrence {
    match o {
        CsilOccurrence::Optional => Occurrence::Optional,
        CsilOccurrence::ZeroOrMore => Occurrence::ZeroOrMore,
        CsilOccurrence::OneOrMore => Occurrence::OneOrMore,
        CsilOccurrence::Exact(n) => Occurrence::Exact(*n),
        CsilOccurrence::Range { min, max } => Occurrence::Range {
            min: *min,
            max: *max,
        },
    }
}

fn to_service_definition(d: &CsilServiceDefinition) -> ServiceDefinition {
    ServiceDefinition {
        operations: d.operations.iter().map(to_service_operation).collect(),
    }
}

fn to_service_operation(op: &CsilServiceOperation) -> ServiceOperation {
    ServiceOperation {
        name: op.name.clone(),
        input_type: to_type_expression(&op.input_type),
        output_type: to_type_expression(&op.output_type),
        direction: to_service_direction(&op.direction),
        position: to_position(&op.position),
        doc_comments: op.doc_comments.clone(),
    }
}

fn to_service_direction(d: &CsilServiceDirection) -> ServiceDirection {
    match d {
        CsilServiceDirection::Unidirectional => ServiceDirection::Unidirectional,
        CsilServiceDirection::Bidirectional => ServiceDirection::Bidirectional,
        CsilServiceDirection::Reverse => ServiceDirection::Reverse,
    }
}

fn to_field_metadata(m: &CsilFieldMetadata) -> FieldMetadata {
    match m {
        CsilFieldMetadata::Visibility(v) => FieldMetadata::Visibility(to_field_visibility(v)),
        CsilFieldMetadata::DependsOn { field, value } => FieldMetadata::DependsOn {
            field: field.clone(),
            value: value.as_ref().map(to_literal_value),
        },
        CsilFieldMetadata::Constraint(c) => FieldMetadata::Constraint(to_validation_constraint(c)),
        CsilFieldMetadata::Description(s) => FieldMetadata::Description(s.clone()),
        CsilFieldMetadata::Custom { name, parameters } => FieldMetadata::Custom {
            name: name.clone(),
            parameters: parameters
                .iter()
                .map(|p| MetadataParameter {
                    name: p.name.clone(),
                    value: to_literal_value(&p.value),
                })
                .collect(),
        },
    }
}

fn to_field_visibility(v: &CsilFieldVisibility) -> FieldVisibility {
    match v {
        CsilFieldVisibility::SendOnly => FieldVisibility::SendOnly,
        CsilFieldVisibility::ReceiveOnly => FieldVisibility::ReceiveOnly,
        CsilFieldVisibility::Bidirectional => FieldVisibility::Bidirectional,
    }
}

fn to_validation_constraint(c: &CsilValidationConstraint) -> ValidationConstraint {
    match c {
        CsilValidationConstraint::MinLength(n) => ValidationConstraint::MinLength(*n),
        CsilValidationConstraint::MaxLength(n) => ValidationConstraint::MaxLength(*n),
        CsilValidationConstraint::MinValue(l) => {
            ValidationConstraint::MinValue(to_literal_value(l))
        }
        CsilValidationConstraint::MaxValue(l) => {
            ValidationConstraint::MaxValue(to_literal_value(l))
        }
        CsilValidationConstraint::MinItems(n) => ValidationConstraint::MinItems(*n),
        CsilValidationConstraint::MaxItems(n) => ValidationConstraint::MaxItems(*n),
        CsilValidationConstraint::Custom { name, value } => ValidationConstraint::Custom {
            name: name.clone(),
            value: to_literal_value(value),
        },
    }
}

fn to_control_operator(o: &CsilControlOperator) -> ControlOperator {
    match o {
        CsilControlOperator::Size(s) => ControlOperator::Size(to_size_constraint(s)),
        CsilControlOperator::Regex(p) => ControlOperator::Regex(p.clone()),
        CsilControlOperator::Default(l) => ControlOperator::Default(to_literal_value(l)),
        CsilControlOperator::GreaterEqual(l) => ControlOperator::GreaterEqual(to_literal_value(l)),
        CsilControlOperator::LessEqual(l) => ControlOperator::LessEqual(to_literal_value(l)),
        CsilControlOperator::GreaterThan(l) => ControlOperator::GreaterThan(to_literal_value(l)),
        CsilControlOperator::LessThan(l) => ControlOperator::LessThan(to_literal_value(l)),
        CsilControlOperator::Equal(l) => ControlOperator::Equal(to_literal_value(l)),
        CsilControlOperator::NotEqual(l) => ControlOperator::NotEqual(to_literal_value(l)),
        CsilControlOperator::Bits(s) => ControlOperator::Bits(s.clone()),
        CsilControlOperator::And(t) => ControlOperator::And(Box::new(to_type_expression(t))),
        CsilControlOperator::Within(t) => ControlOperator::Within(Box::new(to_type_expression(t))),
        CsilControlOperator::Json => ControlOperator::Json,
        CsilControlOperator::Cbor => ControlOperator::Cbor,
        CsilControlOperator::Cborseq => ControlOperator::Cborseq,
    }
}

fn to_size_constraint(s: &CsilSizeConstraint) -> SizeConstraint {
    match s {
        CsilSizeConstraint::Exact(n) => SizeConstraint::Exact(*n),
        CsilSizeConstraint::Range { min, max } => SizeConstraint::Range {
            min: *min,
            max: *max,
        },
        CsilSizeConstraint::Min(n) => SizeConstraint::Min(*n),
        CsilSizeConstraint::Max(n) => SizeConstraint::Max(*n),
    }
}

fn to_literal_value(l: &CsilLiteralValue) -> LiteralValue {
    match l {
        CsilLiteralValue::Integer(n) => LiteralValue::Integer(*n),
        CsilLiteralValue::Float(f) => LiteralValue::Float(*f),
        CsilLiteralValue::Text(t) => LiteralValue::Text(t.clone()),
        CsilLiteralValue::Bytes(b) => LiteralValue::Bytes(b.clone()),
        CsilLiteralValue::Bool(b) => LiteralValue::Bool(*b),
        CsilLiteralValue::Null => LiteralValue::Null,
        CsilLiteralValue::Array(els) => {
            LiteralValue::Array(els.iter().map(to_literal_value).collect())
        }
    }
}

fn to_position(p: &CsilPosition) -> Position {
    Position {
        line: p.line,
        column: p.column,
        offset: p.offset,
    }
}
