use crate::cbor::{DiagnosticValue, SpannedValue, decode};
use crate::descriptor::{
    DescriptorError, SchemaControl, SchemaDependsCompareOp, SchemaDependsCondition,
    SchemaDescriptor, SchemaFieldMetadata, SchemaGroup, SchemaGroupEntry, SchemaGroupKey,
    SchemaLiteral, SchemaOccurrence, SchemaRuleDefinition, SchemaService, SchemaServiceDirection,
    SchemaSize, SchemaType, SchemaValidationConstraint,
};
use thiserror::Error;

const MAX_GROUP_ALTERNATIVES: usize = 256;
const MAX_EXPANDED_FIELDS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDirection {
    Sent,
    Received,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadSide {
    Input,
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteContext {
    RpcRequest {
        service: String,
        operation: String,
        direction: MessageDirection,
    },
    RpcResponse {
        service: String,
        operation: String,
        variant: Option<String>,
        direction: MessageDirection,
    },
    EventVerbose {
        service: Option<String>,
        operation: String,
        payload_side: PayloadSide,
        direction: MessageDirection,
    },
    EventCompact {
        service_wire_id: u64,
        operation_wire_id: u64,
        payload_side: PayloadSide,
        direction: MessageDirection,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRoute {
    pub service: String,
    pub operation: String,
    pub service_wire_id: Option<u64>,
    pub operation_wire_id: Option<u64>,
    pub payload_side: PayloadSide,
    pub direction: MessageDirection,
    pub schema_type: SchemaType,
    pub choice_arm: Option<ChoiceArm>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceArm {
    pub index: usize,
    pub declared_arm: SchemaType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub schema_path: String,
    pub offset: Option<usize>,
    pub expected: Option<String>,
    pub observed: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticResult {
    pub raw_payload: Vec<u8>,
    pub route: Option<ResolvedRoute>,
    pub generic_value: Option<SpannedValue>,
    pub typed_value: Option<TypedValue>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Value(SpannedValue),
    Array(Vec<TypedValue>),
    Tuple(Vec<TypedValue>),
    Map(Vec<(TypedValue, TypedValue)>),
    Record {
        fields: Vec<TypedField>,
        unknown_fields: Vec<(SpannedValue, SpannedValue)>,
    },
    Choice {
        arm_index: usize,
        declared_arm: SchemaType,
        value: Box<TypedValue>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedField {
    pub name: Option<String>,
    pub key: SpannedValue,
    pub value: TypedValue,
}

#[derive(Debug, Error)]
pub enum UnmarshalError {
    #[error(transparent)]
    Descriptor(#[from] DescriptorError),
}

/// Decode and verify a descriptor, resolve a route, and inspect one payload.
pub fn unmarshal_descriptor(
    descriptor_bytes: &[u8],
    route: &RouteContext,
    payload: &[u8],
) -> Result<DiagnosticResult, UnmarshalError> {
    let descriptor = SchemaDescriptor::decode(descriptor_bytes)?;
    Ok(unmarshal(&descriptor, route, payload))
}

/// Inspect one payload. Route and payload errors are returned inside the result
/// so one malformed captured message does not stop later messages.
pub fn unmarshal(
    descriptor: &SchemaDescriptor,
    route_context: &RouteContext,
    payload: &[u8],
) -> DiagnosticResult {
    let mut result = DiagnosticResult {
        raw_payload: payload.to_vec(),
        route: None,
        generic_value: None,
        typed_value: None,
        diagnostics: Vec::new(),
    };

    let generic = match decode(payload) {
        Ok(value) => value,
        Err(error) => {
            result.diagnostics.push(Diagnostic {
                message: error.message,
                schema_path: "$".to_string(),
                offset: Some(error.offset),
                expected: Some("one complete CBOR item".to_string()),
                observed: Some("malformed CBOR".to_string()),
            });
            return result;
        }
    };
    result.generic_value = Some(generic.clone());

    let route = match resolve_route(descriptor, route_context) {
        Ok(route) => route,
        Err(message) => {
            result.diagnostics.push(Diagnostic {
                message,
                schema_path: "$route".to_string(),
                offset: None,
                expected: Some("a service operation in the descriptor".to_string()),
                observed: Some("unresolved route".to_string()),
            });
            return result;
        }
    };

    let path = format!("service.{}.{}", route.service, route.operation);
    let mut typed = inspect(
        descriptor,
        &route.schema_type,
        &generic,
        &path,
        &mut result.diagnostics,
        0,
    );
    if let Some(choice_arm) = &route.choice_arm {
        typed = TypedValue::Choice {
            arm_index: choice_arm.index,
            declared_arm: choice_arm.declared_arm.clone(),
            value: Box::new(typed),
        };
    }
    result.route = Some(route);
    result.typed_value = Some(typed);
    result
}

pub fn resolve_route(
    descriptor: &SchemaDescriptor,
    context: &RouteContext,
) -> Result<ResolvedRoute, String> {
    let (service, operation, side, direction, response_variant) = match context {
        RouteContext::RpcRequest {
            service,
            operation,
            direction,
        } => (
            find_service_name(descriptor, Some(service))?,
            operation.as_str(),
            PayloadSide::Input,
            *direction,
            None,
        ),
        RouteContext::RpcResponse {
            service,
            operation,
            variant,
            direction,
        } => (
            find_service_name(descriptor, Some(service))?,
            operation.as_str(),
            PayloadSide::Output,
            *direction,
            Some(variant.as_deref()),
        ),
        RouteContext::EventVerbose {
            service,
            operation,
            payload_side,
            direction,
        } => (
            find_service_name(descriptor, service.as_deref())?,
            operation.as_str(),
            *payload_side,
            *direction,
            None,
        ),
        RouteContext::EventCompact {
            service_wire_id,
            operation_wire_id,
            payload_side,
            direction,
        } => {
            let service = descriptor
                .body
                .services
                .iter()
                .find(|service| service.wire_id == Some(*service_wire_id))
                .ok_or_else(|| format!("service wire ID {service_wire_id} is not in the schema"))?;
            let operation = service
                .operations
                .iter()
                .find(|operation| operation.wire_id == Some(*operation_wire_id))
                .ok_or_else(|| {
                    format!(
                        "operation wire ID {operation_wire_id} is not in service '{}'",
                        service.name
                    )
                })?;
            return resolved_route(service, operation, *payload_side, *direction, None);
        }
    };

    let operation = service
        .operations
        .iter()
        .find(|candidate| candidate.name == operation)
        .ok_or_else(|| {
            format!(
                "operation '{operation}' is not in service '{}'",
                service.name
            )
        })?;
    resolved_route(service, operation, side, direction, response_variant)
}

fn find_service_name<'a>(
    descriptor: &'a SchemaDescriptor,
    name: Option<&str>,
) -> Result<&'a SchemaService, String> {
    if let Some(name) = name {
        return descriptor
            .body
            .services
            .iter()
            .find(|service| service.name == name)
            .ok_or_else(|| format!("service '{name}' is not in the schema"));
    }
    match descriptor.body.services.as_slice() {
        [service] => Ok(service),
        [] => Err("the schema has no services".to_string()),
        _ => Err("the route omitted a service, but the schema has multiple services".to_string()),
    }
}

fn resolved_route(
    service: &SchemaService,
    operation: &crate::descriptor::SchemaOperation,
    side: PayloadSide,
    direction: MessageDirection,
    response_variant: Option<Option<&str>>,
) -> Result<ResolvedRoute, String> {
    if operation.direction == SchemaServiceDirection::Reverse && side == PayloadSide::Input {
        return Err(format!(
            "reverse operation '{}.{}' has no input payload",
            service.name, operation.name
        ));
    }
    let mut schema_type = match side {
        PayloadSide::Input => operation.input.clone(),
        PayloadSide::Output => operation.output.clone(),
    };
    let mut choice_arm = None;
    if let Some(variant) = response_variant {
        let selected = select_variant(&schema_type, variant)?;
        schema_type = selected.0;
        choice_arm = selected.1;
    }
    Ok(ResolvedRoute {
        service: service.name.clone(),
        operation: operation.name.clone(),
        service_wire_id: service.wire_id,
        operation_wire_id: operation.wire_id,
        payload_side: side,
        direction,
        schema_type,
        choice_arm,
    })
}

fn select_variant(
    output: &SchemaType,
    variant: Option<&str>,
) -> Result<(SchemaType, Option<ChoiceArm>), String> {
    let SchemaType::Choice(arms) = output else {
        return Ok((output.clone(), None));
    };
    if arms.len() == 1 && variant.is_none() {
        return Ok((
            arms[0].clone(),
            Some(ChoiceArm {
                index: 0,
                declared_arm: arms[0].clone(),
            }),
        ));
    }
    let variant = variant.ok_or_else(|| {
        "the response output has multiple arms, but the route has no variant".to_string()
    })?;
    arms.iter()
        .enumerate()
        .find(|(_, arm)| type_name(arm) == Some(variant))
        .map(|(index, arm)| {
            (
                arm.clone(),
                Some(ChoiceArm {
                    index,
                    declared_arm: arm.clone(),
                }),
            )
        })
        .ok_or_else(|| format!("response variant '{variant}' is not an output arm"))
}

fn type_name(value: &SchemaType) -> Option<&str> {
    match value {
        SchemaType::Reference(name)
        | SchemaType::Builtin(name)
        | SchemaType::Socket(name)
        | SchemaType::Plug(name) => Some(name),
        _ => None,
    }
}

fn inspect(
    descriptor: &SchemaDescriptor,
    schema: &SchemaType,
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) -> TypedValue {
    if depth > 64 {
        mismatch(
            diagnostics,
            path,
            value,
            "a schema reference depth of at most 64",
            "recursive schema",
        );
        return TypedValue::Value(value.clone());
    }

    match schema {
        SchemaType::Builtin(name) => {
            if !builtin_matches(name, &value.value) {
                mismatch(diagnostics, path, value, name, value.value.shape());
            }
            TypedValue::Value(value.clone())
        }
        SchemaType::Reference(name) | SchemaType::Socket(name) | SchemaType::Plug(name) => {
            let Some(rule) = descriptor.body.rules.iter().find(|rule| rule.name == *name) else {
                mismatch(
                    diagnostics,
                    path,
                    value,
                    name,
                    "unresolved schema reference",
                );
                return TypedValue::Value(value.clone());
            };
            match &rule.definition {
                SchemaRuleDefinition::Type(target) => inspect(
                    descriptor,
                    target,
                    value,
                    &format!("{path}->{name}"),
                    diagnostics,
                    depth + 1,
                ),
                SchemaRuleDefinition::Group(group) => inspect_group(
                    descriptor,
                    group,
                    value,
                    &format!("{path}->{name}"),
                    diagnostics,
                    depth + 1,
                ),
                SchemaRuleDefinition::GroupChoice(groups) => {
                    inspect_group_choice(descriptor, groups, value, path, diagnostics, depth + 1)
                }
            }
        }
        SchemaType::Array {
            element,
            occurrence,
        } => {
            let DiagnosticValue::Array(values) = &value.value else {
                mismatch(diagnostics, path, value, "array", value.value.shape());
                return TypedValue::Value(value.clone());
            };
            check_occurrence(occurrence.as_ref(), values.len(), path, value, diagnostics);
            TypedValue::Array(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        inspect(
                            descriptor,
                            element,
                            value,
                            &format!("{path}[{index}]"),
                            diagnostics,
                            depth + 1,
                        )
                    })
                    .collect(),
            )
        }
        SchemaType::Tuple(group) => {
            inspect_tuple(descriptor, group, value, path, diagnostics, depth + 1)
        }
        SchemaType::Map {
            key,
            value: item,
            occurrence,
        } => {
            let DiagnosticValue::Map(entries) = &value.value else {
                mismatch(diagnostics, path, value, "map", value.value.shape());
                return TypedValue::Value(value.clone());
            };
            check_occurrence(occurrence.as_ref(), entries.len(), path, value, diagnostics);
            TypedValue::Map(
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, (entry_key, entry_value))| {
                        (
                            inspect(
                                descriptor,
                                key,
                                entry_key,
                                &format!("{path}.key[{index}]"),
                                diagnostics,
                                depth + 1,
                            ),
                            inspect(
                                descriptor,
                                item,
                                entry_value,
                                &format!("{path}.value[{index}]"),
                                diagnostics,
                                depth + 1,
                            ),
                        )
                    })
                    .collect(),
            )
        }
        SchemaType::Group(group) => {
            inspect_group(descriptor, group, value, path, diagnostics, depth + 1)
        }
        SchemaType::Choice(arms) => {
            for (arm_index, arm) in arms.iter().enumerate() {
                let mut arm_diagnostics = Vec::new();
                let typed = inspect(
                    descriptor,
                    arm,
                    value,
                    &format!("{path}.choice[{arm_index}]"),
                    &mut arm_diagnostics,
                    depth + 1,
                );
                if arm_diagnostics.is_empty() {
                    return TypedValue::Choice {
                        arm_index,
                        declared_arm: arm.clone(),
                        value: Box::new(typed),
                    };
                }
            }
            mismatch(
                diagnostics,
                path,
                value,
                "one declared choice arm",
                value.value.shape(),
            );
            TypedValue::Value(value.clone())
        }
        SchemaType::Range {
            start,
            end,
            inclusive,
        } => {
            let valid = match value.value {
                DiagnosticValue::Integer(integer) => {
                    let lower = start.is_none_or(|start| integer >= start as i128);
                    let upper = end.is_none_or(|end| {
                        if *inclusive {
                            integer <= end as i128
                        } else {
                            integer < end as i128
                        }
                    });
                    lower && upper
                }
                _ => false,
            };
            if !valid {
                mismatch(
                    diagnostics,
                    path,
                    value,
                    "integer in range",
                    value.value.shape(),
                );
            }
            TypedValue::Value(value.clone())
        }
        SchemaType::Literal(literal) => {
            if !literal_matches(literal, &value.value) {
                mismatch(
                    diagnostics,
                    path,
                    value,
                    "declared literal",
                    value.value.shape(),
                );
            }
            TypedValue::Value(value.clone())
        }
        SchemaType::Constrained { base, constraints } => {
            let typed = inspect(descriptor, base, value, path, diagnostics, depth + 1);
            inspect_constraints(descriptor, constraints, value, path, diagnostics, depth + 1);
            typed
        }
    }
}

fn inspect_group(
    descriptor: &SchemaDescriptor,
    group: &SchemaGroup,
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) -> TypedValue {
    let (alternatives, truncated) = expand_group_alternatives(descriptor, group, depth);
    if truncated {
        diagnostics.push(Diagnostic {
            message: "group expansion exceeds the diagnostic safety limit".to_string(),
            schema_path: path.to_string(),
            offset: Some(value.offset),
            expected: Some(format!(
                "at most {MAX_GROUP_ALTERNATIVES} alternatives and {MAX_EXPANDED_FIELDS} fields"
            )),
            observed: Some("larger expanded group".to_string()),
        });
    }
    if alternatives.len() == 1 {
        return inspect_flat_group(
            descriptor,
            &alternatives[0],
            value,
            path,
            diagnostics,
            depth,
        );
    }

    let mut best: Option<(TypedValue, Vec<Diagnostic>)> = None;
    for alternative in alternatives {
        let mut candidate_diagnostics = Vec::new();
        let typed = inspect_flat_group(
            descriptor,
            &alternative,
            value,
            path,
            &mut candidate_diagnostics,
            depth + 1,
        );
        if candidate_diagnostics.is_empty() {
            return typed;
        }
        if best
            .as_ref()
            .is_none_or(|(_, issues)| candidate_diagnostics.len() < issues.len())
        {
            best = Some((typed, candidate_diagnostics));
        }
    }
    let (typed, issues) = best.unwrap_or_else(|| (TypedValue::Value(value.clone()), Vec::new()));
    diagnostics.extend(issues);
    typed
}

fn inspect_flat_group(
    descriptor: &SchemaDescriptor,
    group: &SchemaGroup,
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) -> TypedValue {
    let DiagnosticValue::Map(entries) = &value.value else {
        mismatch(diagnostics, path, value, "record map", value.value.shape());
        return TypedValue::Value(value.clone());
    };
    let mut used = vec![false; entries.len()];
    let mut fields = Vec::new();

    for field in &group.entries {
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(index, (key, _))| !used[*index] && key_matches(descriptor, &field.key, key))
            .map(|(index, _)| index)
            .collect();
        if matches.is_empty() {
            if occurrence_requires_value(field.occurrence.as_ref())
                || dependency_is_active(field, entries)
            {
                diagnostics.push(Diagnostic {
                    message: "required record field is missing".to_string(),
                    schema_path: field_path(path, field),
                    offset: Some(value.offset),
                    expected: Some("present field".to_string()),
                    observed: Some("absent field".to_string()),
                });
            }
            continue;
        }
        check_occurrence(
            field.occurrence.as_ref(),
            matches.len(),
            &field_path(path, field),
            value,
            diagnostics,
        );
        for index in matches {
            used[index] = true;
            let (key, field_value) = &entries[index];
            let field_path = field_path(path, field);
            let typed_value = inspect(
                descriptor,
                &field.value,
                field_value,
                &field_path,
                diagnostics,
                depth + 1,
            );
            inspect_field_constraints(field, field_value, &field_path, diagnostics);
            fields.push(TypedField {
                name: field_name(field),
                key: key.clone(),
                value: typed_value,
            });
        }
    }

    let unknown_fields = entries
        .iter()
        .zip(used)
        .filter(|(_, used)| !used)
        .map(|((key, value), _)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    for (key, _) in &unknown_fields {
        diagnostics.push(Diagnostic {
            message: "record contains an unknown field".to_string(),
            schema_path: path.to_string(),
            offset: Some(key.offset),
            expected: Some("a declared record key".to_string()),
            observed: Some(key.value.shape().to_string()),
        });
    }
    TypedValue::Record {
        fields,
        unknown_fields,
    }
}

fn dependency_is_active(
    field: &SchemaGroupEntry,
    entries: &[(SpannedValue, SpannedValue)],
) -> bool {
    field.metadata.iter().any(|metadata| match metadata {
        SchemaFieldMetadata::DependsOn {
            field,
            value: expected,
        } => record_value(entries, field).is_some_and(|actual| {
            expected
                .as_ref()
                .is_none_or(|expected| literal_matches(expected, &actual.value))
        }),
        SchemaFieldMetadata::DependsOnExpr(condition) => condition_matches(condition, entries),
        _ => false,
    })
}

fn condition_matches(
    condition: &SchemaDependsCondition,
    entries: &[(SpannedValue, SpannedValue)],
) -> bool {
    match condition {
        SchemaDependsCondition::Compare { field, op, value } => {
            let Some(actual) = record_value(entries, field) else {
                return false;
            };
            let Some(op) = op else {
                return true;
            };
            let Some(expected) = value else {
                return false;
            };
            compare_dependency(&actual.value, *op, expected)
        }
        SchemaDependsCondition::All(conditions) => conditions
            .iter()
            .all(|condition| condition_matches(condition, entries)),
        SchemaDependsCondition::Any(conditions) => conditions
            .iter()
            .any(|condition| condition_matches(condition, entries)),
    }
}

fn record_value<'a>(
    entries: &'a [(SpannedValue, SpannedValue)],
    field: &str,
) -> Option<&'a SpannedValue> {
    entries.iter().find_map(|(key, value)| match &key.value {
        DiagnosticValue::Text(name) if name == field => Some(value),
        _ => None,
    })
}

fn compare_dependency(
    actual: &DiagnosticValue,
    op: SchemaDependsCompareOp,
    expected: &SchemaLiteral,
) -> bool {
    if matches!(op, SchemaDependsCompareOp::Eq | SchemaDependsCompareOp::Ne) {
        let equal = literal_matches(expected, actual);
        return if op == SchemaDependsCompareOp::Eq {
            equal
        } else {
            !equal
        };
    }
    let ordering = match (actual, expected) {
        (DiagnosticValue::Integer(actual), SchemaLiteral::Integer(expected)) => {
            actual.partial_cmp(&(*expected as i128))
        }
        (DiagnosticValue::Float(actual), SchemaLiteral::Float(expected)) => {
            actual.as_f64().partial_cmp(expected)
        }
        (DiagnosticValue::Integer(actual), SchemaLiteral::Float(expected)) => {
            (*actual as f64).partial_cmp(expected)
        }
        (DiagnosticValue::Float(actual), SchemaLiteral::Integer(expected)) => {
            actual.as_f64().partial_cmp(&(*expected as f64))
        }
        (DiagnosticValue::Text(actual), SchemaLiteral::Text(expected)) => {
            Some(actual.as_str().cmp(expected.as_str()))
        }
        _ => None,
    };
    ordering.is_some_and(|ordering| match op {
        SchemaDependsCompareOp::Lt => ordering.is_lt(),
        SchemaDependsCompareOp::Le => ordering.is_le(),
        SchemaDependsCompareOp::Gt => ordering.is_gt(),
        SchemaDependsCompareOp::Ge => ordering.is_ge(),
        SchemaDependsCompareOp::Eq | SchemaDependsCompareOp::Ne => unreachable!(),
    })
}

fn inspect_field_constraints(
    field: &SchemaGroupEntry,
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for metadata in &field.metadata {
        let SchemaFieldMetadata::Constraint(constraint) = metadata else {
            continue;
        };
        let valid = match constraint {
            SchemaValidationConstraint::MinLength(minimum) => {
                value_length(&value.value).is_some_and(|length| length as u64 >= *minimum)
            }
            SchemaValidationConstraint::MaxLength(maximum) => {
                value_length(&value.value).is_some_and(|length| length as u64 <= *maximum)
            }
            SchemaValidationConstraint::MinItems(minimum) => {
                value_length(&value.value).is_some_and(|length| length as u64 >= *minimum)
            }
            SchemaValidationConstraint::MaxItems(maximum) => {
                value_length(&value.value).is_some_and(|length| length as u64 <= *maximum)
            }
            SchemaValidationConstraint::MinValue(minimum) => {
                compare_numeric(value, minimum).is_some_and(std::cmp::Ordering::is_ge)
            }
            SchemaValidationConstraint::MaxValue(maximum) => {
                compare_numeric(value, maximum).is_some_and(std::cmp::Ordering::is_le)
            }
            SchemaValidationConstraint::Custom { .. } => true,
        };
        if !valid {
            mismatch(
                diagnostics,
                path,
                value,
                "value that satisfies its field constraint",
                value.value.shape(),
            );
        }
    }
}

fn expand_group_alternatives(
    descriptor: &SchemaDescriptor,
    group: &SchemaGroup,
    depth: usize,
) -> (Vec<SchemaGroup>, bool) {
    if depth > 64 {
        return (vec![group.clone()], true);
    }
    let mut alternatives: Vec<Vec<SchemaGroupEntry>> = vec![Vec::new()];
    let mut truncated = false;
    for entry in &group.entries {
        if entry.key.is_none() {
            let nested_groups = groups_for_type(descriptor, &entry.value);
            if let Some(nested_groups) = nested_groups {
                let mut nested_alternatives = Vec::new();
                for nested in nested_groups {
                    let (expanded, nested_truncated) =
                        expand_group_alternatives(descriptor, nested, depth + 1);
                    truncated |= nested_truncated;
                    nested_alternatives.extend(expanded);
                }
                let mut combined = Vec::new();
                'combine: for prefix in &alternatives {
                    for nested in &nested_alternatives {
                        let mut entries = prefix.clone();
                        let remaining = MAX_EXPANDED_FIELDS.saturating_sub(entries.len());
                        truncated |= nested.entries.len() > remaining;
                        entries.extend(nested.entries.iter().take(remaining).cloned());
                        combined.push(entries);
                        if combined.len() == MAX_GROUP_ALTERNATIVES {
                            truncated = true;
                            break 'combine;
                        }
                    }
                }
                alternatives = combined;
                continue;
            }
        }
        for alternative in &mut alternatives {
            if alternative.len() < MAX_EXPANDED_FIELDS {
                alternative.push(entry.clone());
            } else {
                truncated = true;
            }
        }
    }
    (
        alternatives
            .into_iter()
            .map(|entries| SchemaGroup { entries })
            .collect(),
        truncated,
    )
}

fn groups_for_type<'a>(
    descriptor: &'a SchemaDescriptor,
    schema_type: &'a SchemaType,
) -> Option<Vec<&'a SchemaGroup>> {
    match schema_type {
        SchemaType::Group(group) | SchemaType::Tuple(group) => Some(vec![group]),
        SchemaType::Choice(arms) => {
            let mut groups = Vec::new();
            for arm in arms {
                groups.extend(groups_for_type(descriptor, arm)?);
            }
            Some(groups)
        }
        SchemaType::Reference(name) => descriptor
            .body
            .rules
            .iter()
            .find(|rule| rule.name == *name)
            .and_then(|rule| match &rule.definition {
                SchemaRuleDefinition::Group(group) => Some(vec![group]),
                SchemaRuleDefinition::GroupChoice(groups) => Some(groups.iter().collect()),
                SchemaRuleDefinition::Type(schema_type) => groups_for_type(descriptor, schema_type),
            }),
        _ => None,
    }
}

fn inspect_group_choice(
    descriptor: &SchemaDescriptor,
    groups: &[SchemaGroup],
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) -> TypedValue {
    for (index, group) in groups.iter().enumerate() {
        let mut candidate_diagnostics = Vec::new();
        let typed = inspect_group(
            descriptor,
            group,
            value,
            &format!("{path}.group-choice[{index}]"),
            &mut candidate_diagnostics,
            depth + 1,
        );
        if candidate_diagnostics.is_empty() {
            return typed;
        }
    }
    mismatch(
        diagnostics,
        path,
        value,
        "one declared group choice",
        value.value.shape(),
    );
    TypedValue::Value(value.clone())
}

fn inspect_tuple(
    descriptor: &SchemaDescriptor,
    group: &SchemaGroup,
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) -> TypedValue {
    let DiagnosticValue::Array(values) = &value.value else {
        mismatch(diagnostics, path, value, "tuple array", value.value.shape());
        return TypedValue::Value(value.clone());
    };
    let minimum = group
        .entries
        .iter()
        .map(|entry| occurrence_bounds(entry.occurrence.as_ref()).0)
        .fold(0_usize, usize::saturating_add);
    let maximum = group.entries.iter().try_fold(0_usize, |total, entry| {
        occurrence_bounds(entry.occurrence.as_ref())
            .1
            .and_then(|maximum| total.checked_add(maximum))
    });
    if values.len() < minimum || maximum.is_some_and(|maximum| values.len() > maximum) {
        let maximum = maximum.map_or_else(|| "unbounded".to_string(), |value| value.to_string());
        mismatch(
            diagnostics,
            path,
            value,
            &format!("tuple with {minimum}..{maximum} items"),
            &format!("tuple with {} items", values.len()),
        );
    }
    let mut typed = Vec::new();
    let mut cursor = 0_usize;
    for (entry_index, entry) in group.entries.iter().enumerate() {
        let (entry_minimum, entry_maximum) = occurrence_bounds(entry.occurrence.as_ref());
        let remaining_minimum = group.entries[entry_index + 1..]
            .iter()
            .map(|entry| occurrence_bounds(entry.occurrence.as_ref()).0)
            .fold(0_usize, usize::saturating_add);
        let available = values
            .len()
            .saturating_sub(cursor)
            .saturating_sub(remaining_minimum);
        let count = entry_maximum.map_or(available, |maximum| available.min(maximum));
        let count = count.max(entry_minimum).min(values.len() - cursor);
        for _ in 0..count {
            typed.push(inspect(
                descriptor,
                &entry.value,
                &values[cursor],
                &format!("{path}[{cursor}]"),
                diagnostics,
                depth + 1,
            ));
            cursor += 1;
        }
    }
    typed.extend(values[cursor..].iter().cloned().map(TypedValue::Value));
    TypedValue::Tuple(typed)
}

fn occurrence_bounds(occurrence: Option<&SchemaOccurrence>) -> (usize, Option<usize>) {
    match occurrence {
        None => (1, Some(1)),
        Some(SchemaOccurrence::Optional) => (0, Some(1)),
        Some(SchemaOccurrence::ZeroOrMore) => (0, None),
        Some(SchemaOccurrence::OneOrMore) => (1, None),
        Some(SchemaOccurrence::Exact(value)) => {
            let value = usize::try_from(*value).unwrap_or(usize::MAX);
            (value, Some(value))
        }
        Some(SchemaOccurrence::Range { min, max }) => (
            min.map_or(0, |value| usize::try_from(value).unwrap_or(usize::MAX)),
            max.map(|value| usize::try_from(value).unwrap_or(usize::MAX)),
        ),
    }
}

fn key_matches(
    descriptor: &SchemaDescriptor,
    key: &Option<SchemaGroupKey>,
    value: &SpannedValue,
) -> bool {
    match key {
        Some(SchemaGroupKey::Bare(expected)) => {
            matches!(&value.value, DiagnosticValue::Text(actual) if actual == expected)
        }
        Some(SchemaGroupKey::Literal(expected)) => literal_matches(expected, &value.value),
        Some(SchemaGroupKey::Type(expected)) => {
            let mut diagnostics = Vec::new();
            inspect(descriptor, expected, value, "$key", &mut diagnostics, 0);
            diagnostics.is_empty()
        }
        None => false,
    }
}

fn builtin_matches(name: &str, value: &DiagnosticValue) -> bool {
    match name {
        "int" => matches!(value, DiagnosticValue::Integer(_)),
        "uint" => matches!(value, DiagnosticValue::Integer(value) if *value >= 0),
        "nint" => matches!(value, DiagnosticValue::Integer(value) if *value < 0),
        "text" | "tstr" => matches!(value, DiagnosticValue::Text(_)),
        "bytes" | "bstr" => matches!(value, DiagnosticValue::Bytes(_)),
        "bool" => matches!(value, DiagnosticValue::Bool(_)),
        "true" => matches!(value, DiagnosticValue::Bool(true)),
        "false" => matches!(value, DiagnosticValue::Bool(false)),
        "null" => matches!(value, DiagnosticValue::Null),
        "undefined" => matches!(value, DiagnosticValue::Undefined),
        "float" => matches!(value, DiagnosticValue::Float(_)),
        "float16" => matches!(
            value,
            DiagnosticValue::Float(crate::FloatValue {
                width: crate::FloatWidth::Sixteen,
                ..
            })
        ),
        "float32" => matches!(
            value,
            DiagnosticValue::Float(crate::FloatValue {
                width: crate::FloatWidth::ThirtyTwo,
                ..
            })
        ),
        "float64" => matches!(
            value,
            DiagnosticValue::Float(crate::FloatValue {
                width: crate::FloatWidth::SixtyFour,
                ..
            })
        ),
        "decimal" => matches!(value, DiagnosticValue::Decimal { .. }),
        "timestamp" => matches!(value, DiagnosticValue::Timestamp { .. }),
        "any" => true,
        _ => false,
    }
}

fn literal_matches(literal: &SchemaLiteral, value: &DiagnosticValue) -> bool {
    match (literal, value) {
        (SchemaLiteral::Integer(expected), DiagnosticValue::Integer(actual)) => {
            *expected as i128 == *actual
        }
        (SchemaLiteral::Float(expected), DiagnosticValue::Float(actual)) => {
            expected.to_bits() == actual.as_f64().to_bits()
        }
        (SchemaLiteral::Text(expected), DiagnosticValue::Text(actual)) => expected == actual,
        (SchemaLiteral::Bytes(expected), DiagnosticValue::Bytes(actual)) => &expected.0 == actual,
        (SchemaLiteral::Bool(expected), DiagnosticValue::Bool(actual)) => expected == actual,
        (SchemaLiteral::Null, DiagnosticValue::Null) => true,
        (SchemaLiteral::Array(expected), DiagnosticValue::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| literal_matches(expected, &actual.value))
        }
        _ => false,
    }
}

fn inspect_constraints(
    descriptor: &SchemaDescriptor,
    constraints: &[SchemaControl],
    value: &SpannedValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) {
    for constraint in constraints {
        let valid = match constraint {
            SchemaControl::Size(size) => {
                value_length(&value.value).is_some_and(|length| size_matches(size, length as u64))
            }
            SchemaControl::GreaterEqual(bound) => {
                compare_numeric(value, bound).is_some_and(std::cmp::Ordering::is_ge)
            }
            SchemaControl::LessEqual(bound) => {
                compare_numeric(value, bound).is_some_and(std::cmp::Ordering::is_le)
            }
            SchemaControl::GreaterThan(bound) => {
                compare_numeric(value, bound).is_some_and(std::cmp::Ordering::is_gt)
            }
            SchemaControl::LessThan(bound) => {
                compare_numeric(value, bound).is_some_and(std::cmp::Ordering::is_lt)
            }
            SchemaControl::Equal(expected) => literal_matches(expected, &value.value),
            SchemaControl::NotEqual(expected) => !literal_matches(expected, &value.value),
            SchemaControl::Regex(pattern) => match &value.value {
                DiagnosticValue::Text(text) => {
                    regex::Regex::new(pattern).is_ok_and(|pattern| pattern.is_match(text))
                }
                _ => false,
            },
            SchemaControl::And(schema) | SchemaControl::Within(schema) => {
                let before = diagnostics.len();
                inspect(descriptor, schema, value, path, diagnostics, depth + 1);
                diagnostics.len() == before
            }
            SchemaControl::Cbor => match &value.value {
                DiagnosticValue::Bytes(bytes) => decode(bytes).is_ok(),
                _ => false,
            },
            // Bits, JSON text, and CBOR sequences keep their exact schema operator.
            // Their application-specific content does not change the outer CBOR type.
            _ => true,
        };
        if !valid {
            mismatch(
                diagnostics,
                path,
                value,
                "value that satisfies its control operator",
                value.value.shape(),
            );
        }
    }
}

fn value_length(value: &DiagnosticValue) -> Option<usize> {
    match value {
        DiagnosticValue::Text(value) => Some(value.len()),
        DiagnosticValue::Bytes(value) => Some(value.len()),
        DiagnosticValue::Array(value) => Some(value.len()),
        DiagnosticValue::Map(value) => Some(value.len()),
        _ => None,
    }
}

fn size_matches(size: &SchemaSize, length: u64) -> bool {
    match size {
        SchemaSize::Exact(expected) => length == *expected,
        SchemaSize::Range { min, max } => (*min..=*max).contains(&length),
        SchemaSize::Min(min) => length >= *min,
        SchemaSize::Max(max) => length <= *max,
    }
}

fn compare_numeric(value: &SpannedValue, bound: &SchemaLiteral) -> Option<std::cmp::Ordering> {
    match (&value.value, bound) {
        (DiagnosticValue::Integer(value), SchemaLiteral::Integer(bound)) => {
            value.partial_cmp(&(*bound as i128))
        }
        (DiagnosticValue::Integer(value), SchemaLiteral::Float(bound)) => {
            (*value as f64).partial_cmp(bound)
        }
        (DiagnosticValue::Float(value), SchemaLiteral::Integer(bound)) => {
            value.as_f64().partial_cmp(&(*bound as f64))
        }
        (DiagnosticValue::Float(value), SchemaLiteral::Float(bound)) => {
            value.as_f64().partial_cmp(bound)
        }
        _ => None,
    }
}

fn check_occurrence(
    occurrence: Option<&SchemaOccurrence>,
    count: usize,
    path: &str,
    value: &SpannedValue,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let valid = match occurrence {
        None => true,
        Some(SchemaOccurrence::Optional) => count <= 1,
        Some(SchemaOccurrence::ZeroOrMore) => true,
        Some(SchemaOccurrence::OneOrMore) => count >= 1,
        Some(SchemaOccurrence::Exact(expected)) => count as u64 == *expected,
        Some(SchemaOccurrence::Range { min, max }) => {
            min.is_none_or(|min| count as u64 >= min) && max.is_none_or(|max| count as u64 <= max)
        }
    };
    if !valid {
        mismatch(
            diagnostics,
            path,
            value,
            "count permitted by the occurrence",
            &format!("{count} values"),
        );
    }
}

fn occurrence_requires_value(occurrence: Option<&SchemaOccurrence>) -> bool {
    match occurrence {
        None => true,
        Some(SchemaOccurrence::Optional | SchemaOccurrence::ZeroOrMore) => false,
        Some(SchemaOccurrence::OneOrMore) => true,
        Some(SchemaOccurrence::Exact(value)) => *value > 0,
        Some(SchemaOccurrence::Range { min, .. }) => min.is_some_and(|value| value > 0),
    }
}

fn field_name(field: &SchemaGroupEntry) -> Option<String> {
    match &field.key {
        Some(SchemaGroupKey::Bare(name)) => Some(name.clone()),
        Some(SchemaGroupKey::Literal(SchemaLiteral::Text(name))) => Some(name.clone()),
        _ => None,
    }
}

fn field_path(path: &str, field: &SchemaGroupEntry) -> String {
    field_name(field).map_or_else(|| format!("{path}.<key>"), |name| format!("{path}.{name}"))
}

fn mismatch(
    diagnostics: &mut Vec<Diagnostic>,
    path: &str,
    value: &SpannedValue,
    expected: &str,
    observed: &str,
) {
    diagnostics.push(Diagnostic {
        message: format!("expected {expected}, observed {observed}"),
        schema_path: path.to_string(),
        offset: Some(value.offset),
        expected: Some(expected.to_string()),
        observed: Some(observed.to_string()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_core::parse_csil;

    fn descriptor() -> SchemaDescriptor {
        let spec = parse_csil(
            r#"
            Request = { id: uint, ? note: text }
            Response = { ok: true }
            Problem = { code: int }
            @wire-id(3)
            service Example {
                @wire-id(9)
                call: Request -> Response / Problem
            }
            "#,
        )
        .unwrap();
        SchemaDescriptor::from_spec("example", &spec).unwrap()
    }

    #[test]
    fn resolves_verbose_and_compact_routes() {
        let descriptor = descriptor();
        let verbose = resolve_route(
            &descriptor,
            &RouteContext::RpcRequest {
                service: "Example".to_string(),
                operation: "call".to_string(),
                direction: MessageDirection::Sent,
            },
        )
        .unwrap();
        let compact = resolve_route(
            &descriptor,
            &RouteContext::EventCompact {
                service_wire_id: 3,
                operation_wire_id: 9,
                payload_side: PayloadSide::Input,
                direction: MessageDirection::Received,
            },
        )
        .unwrap();
        assert_eq!(verbose.schema_type, compact.schema_type);
    }

    #[test]
    fn returns_partial_record_and_diagnostics() {
        let result = unmarshal(
            &descriptor(),
            &RouteContext::RpcRequest {
                service: "Example".to_string(),
                operation: "call".to_string(),
                direction: MessageDirection::Received,
            },
            &[
                0xa2, 0x62, b'i', b'd', 0x61, b'x', 0x65, b'e', b'x', b't', b'r', b'a', 0x01,
            ],
        );
        assert_eq!(result.raw_payload.len(), 13);
        assert!(result.generic_value.is_some());
        assert!(matches!(
            result.typed_value,
            Some(TypedValue::Record { .. })
        ));
        assert!(result.diagnostics.iter().any(|issue| {
            issue.expected.as_deref() == Some("uint")
                && issue.observed.as_deref() == Some("text string")
        }));
        assert!(
            result
                .diagnostics
                .iter()
                .any(|issue| issue.message.contains("unknown field"))
        );
    }

    #[test]
    fn reports_choice_arm_index() {
        let spec = parse_csil("service Example { call: null -> text / bytes / uint }").unwrap();
        let descriptor = SchemaDescriptor::from_spec("choice", &spec).unwrap();
        let result = unmarshal(
            &descriptor,
            &RouteContext::EventVerbose {
                service: Some("Example".to_string()),
                operation: "call".to_string(),
                payload_side: PayloadSide::Output,
                direction: MessageDirection::Received,
            },
            &[0x43, 1, 2, 3],
        );
        assert!(matches!(
            result.typed_value,
            Some(TypedValue::Choice { arm_index: 1, .. })
        ));
    }

    #[test]
    fn expands_named_groups_in_records() {
        let spec = parse_csil(
            r#"
            common = (id: uint, name: text)
            Request = { common, ? note: text }
            service Example { call: Request -> null }
            "#,
        )
        .unwrap();
        let descriptor = SchemaDescriptor::from_spec("groups", &spec).unwrap();
        let result = unmarshal(
            &descriptor,
            &RouteContext::RpcRequest {
                service: "Example".to_string(),
                operation: "call".to_string(),
                direction: MessageDirection::Received,
            },
            &[
                0xa2, 0x62, b'i', b'd', 0x01, 0x64, b'n', b'a', b'm', b'e', 0x63, b'A', b'd', b'a',
            ],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[test]
    fn rpc_response_variant_keeps_declared_arm_index() {
        let result = unmarshal(
            &descriptor(),
            &RouteContext::RpcResponse {
                service: "Example".to_string(),
                operation: "call".to_string(),
                variant: Some("Problem".to_string()),
                direction: MessageDirection::Received,
            },
            &[0xa1, 0x64, b'c', b'o', b'd', b'e', 0x21],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(matches!(
            result.typed_value,
            Some(TypedValue::Choice { arm_index: 1, .. })
        ));
    }

    #[test]
    fn expands_group_choices_in_records() {
        let spec = parse_csil(
            r#"
            a_fields = (kind: "a", a: uint)
            b_fields = (kind: "b", b: text)
            selected = a_fields // b_fields
            Request = { selected, tail: bool }
            service Example { call: Request -> null }
            "#,
        )
        .unwrap();
        let descriptor = SchemaDescriptor::from_spec("group-choice", &spec).unwrap();
        let result = unmarshal(
            &descriptor,
            &RouteContext::RpcRequest {
                service: "Example".to_string(),
                operation: "call".to_string(),
                direction: MessageDirection::Received,
            },
            &[
                0xa3, 0x64, b'k', b'i', b'n', b'd', 0x61, b'b', 0x61, b'b', 0x61, b'x', 0x64, b't',
                b'a', b'i', b'l', 0xf5,
            ],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[test]
    fn reports_active_dependencies_and_field_constraints() {
        let spec = parse_csil(
            r#"
            Request = {
              trigger: text,
              @depends-on(trigger = "yes")
              value?: text,
              @min-length(3)
              code: text,
            }
            service Example { call: Request -> null }
            "#,
        )
        .unwrap();
        let descriptor = SchemaDescriptor::from_spec("metadata", &spec).unwrap();
        let result = unmarshal(
            &descriptor,
            &RouteContext::RpcRequest {
                service: "Example".to_string(),
                operation: "call".to_string(),
                direction: MessageDirection::Received,
            },
            &[
                0xa2, 0x67, b't', b'r', b'i', b'g', b'g', b'e', b'r', 0x63, b'y', b'e', b's', 0x64,
                b'c', b'o', b'd', b'e', 0x61, b'x',
            ],
        );
        assert!(result.diagnostics.iter().any(
            |issue| issue.schema_path.ends_with(".value") && issue.message.contains("missing")
        ));
        assert!(
            result
                .diagnostics
                .iter()
                .any(|issue| issue.schema_path.ends_with(".code")
                    && issue.message.contains("constraint"))
        );
    }

    #[test]
    fn applies_occurrences_inside_a_tuple() {
        let spec = parse_csil("Tuple = [2*3 uint, text]\nservice Example { call: Tuple -> null }")
            .unwrap();
        let descriptor = SchemaDescriptor::from_spec("tuple", &spec).unwrap();
        let result = unmarshal(
            &descriptor,
            &RouteContext::RpcRequest {
                service: "Example".to_string(),
                operation: "call".to_string(),
                direction: MessageDirection::Received,
            },
            &[0x83, 0x01, 0x02, 0x61, b'x'],
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[test]
    fn distinguishes_optional_values_and_inspects_nested_any_values() {
        let spec = parse_csil(
            r#"
            Request = {
              state: "ready" / "done",
              nested: { values: [* any] },
              ? note: text,
            }
            service Example { call: Request -> null }
            "#,
        )
        .unwrap();
        let descriptor = SchemaDescriptor::from_spec("nested", &spec).unwrap();
        let route = RouteContext::RpcRequest {
            service: "Example".to_string(),
            operation: "call".to_string(),
            direction: MessageDirection::Received,
        };

        // The optional field is absent. The empty array is present, and the
        // literal choice selects its second declared arm.
        let absent = unmarshal(
            &descriptor,
            &route,
            &[
                0xa2, 0x65, b's', b't', b'a', b't', b'e', 0x64, b'd', b'o', b'n', b'e', 0x66, b'n',
                b'e', b's', b't', b'e', b'd', 0xa1, 0x66, b'v', b'a', b'l', b'u', b'e', b's', 0x80,
            ],
        );
        assert!(absent.diagnostics.is_empty(), "{:?}", absent.diagnostics);
        let Some(TypedValue::Record { fields, .. }) = absent.typed_value else {
            panic!("expected record")
        };
        assert!(fields.iter().any(|field| {
            field.name.as_deref() == Some("state")
                && matches!(field.value, TypedValue::Choice { arm_index: 1, .. })
        }));

        // An empty text value is distinct from null and is valid for the field.
        let empty = unmarshal(
            &descriptor,
            &route,
            &[
                0xa3, 0x65, b's', b't', b'a', b't', b'e', 0x65, b'r', b'e', b'a', b'd', b'y', 0x66,
                b'n', b'e', b's', b't', b'e', b'd', 0xa1, 0x66, b'v', b'a', b'l', b'u', b'e', b's',
                0x81, 0xd8, 0x64, 0xf6, 0x64, b'n', b'o', b't', b'e', 0x60,
            ],
        );
        assert!(empty.diagnostics.is_empty(), "{:?}", empty.diagnostics);
        let generic = empty.generic_value.as_ref().unwrap();
        let DiagnosticValue::Map(root_entries) = &generic.value else {
            panic!("expected map")
        };
        let nested = &root_entries[1].1;
        let DiagnosticValue::Map(nested_entries) = &nested.value else {
            panic!("expected nested map")
        };
        let DiagnosticValue::Array(values) = &nested_entries[0].1.value else {
            panic!("expected nested array")
        };
        assert!(matches!(
            values[0].value,
            DiagnosticValue::Tag { tag: 100, .. }
        ));

        let null = unmarshal(
            &descriptor,
            &route,
            &[
                0xa3, 0x65, b's', b't', b'a', b't', b'e', 0x65, b'r', b'e', b'a', b'd', b'y', 0x66,
                b'n', b'e', b's', b't', b'e', b'd', 0xa1, 0x66, b'v', b'a', b'l', b'u', b'e', b's',
                0x80, 0x64, b'n', b'o', b't', b'e', 0xf6,
            ],
        );
        assert!(null.diagnostics.iter().any(|issue| {
            issue.schema_path.ends_with(".note") && issue.observed.as_deref() == Some("null")
        }));
    }
}
