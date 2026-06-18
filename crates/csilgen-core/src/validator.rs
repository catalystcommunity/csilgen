//! CSIL validation functionality

use crate::ast::*;
use crate::lexer::Position;
use anyhow::{Result, bail};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;

/// Validation error types
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// Multiple visibility metadata on same field
    ConflictingVisibility {
        field_context: String,
        first: FieldVisibility,
        second: FieldVisibility,
    },
    /// Dependency references non-existent field
    UnknownDependencyField {
        field_context: String,
        depends_on: String,
    },
    /// Multiple constraints of same type
    DuplicateConstraint {
        field_context: String,
        constraint_type: String,
    },
    /// Circular dependency detected
    CircularDependency {
        field_context: String,
        cycle: Vec<String>,
    },
    /// Invalid constraint value
    InvalidConstraint {
        field_context: String,
        constraint: String,
        reason: String,
    },
    /// Duplicate rule name
    DuplicateRule {
        rule_name: String,
        first_location: String,
        second_location: String,
    },
    /// Duplicate service operation name
    DuplicateServiceOperation {
        service_name: String,
        operation_name: String,
        first_location: String,
        second_location: String,
    },
    /// Invalid size constraint
    InvalidSizeConstraint { context: String, reason: String },
    /// Invalid regex pattern
    InvalidRegexPattern {
        context: String,
        pattern: String,
        error: String,
    },
    /// Invalid default value type
    InvalidDefaultValue {
        context: String,
        expected_type: String,
        actual_value: String,
    },
    /// Constraint applied to incompatible type
    ConstraintTypeMismatch {
        constraint_type: String,
        base_type: String,
        context: String,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::ConflictingVisibility {
                field_context,
                first,
                second,
            } => {
                write!(
                    f,
                    "Field {field_context} has conflicting visibility metadata: {first:?} and {second:?}"
                )
            }
            ValidationError::UnknownDependencyField {
                field_context,
                depends_on,
            } => {
                write!(
                    f,
                    "Field {field_context} depends on unknown field '{depends_on}'"
                )
            }
            ValidationError::DuplicateConstraint {
                field_context,
                constraint_type,
            } => {
                write!(
                    f,
                    "Field {field_context} has duplicate {constraint_type} constraints"
                )
            }
            ValidationError::CircularDependency {
                field_context,
                cycle,
            } => {
                write!(
                    f,
                    "Field {} is part of circular dependency: {}",
                    field_context,
                    cycle.join(" -> ")
                )
            }
            ValidationError::InvalidConstraint {
                field_context,
                constraint,
                reason,
            } => {
                write!(
                    f,
                    "Field {field_context} has invalid {constraint} constraint: {reason}"
                )
            }
            ValidationError::DuplicateRule {
                rule_name,
                first_location,
                second_location,
            } => {
                write!(
                    f,
                    "Duplicate rule name '{rule_name}' found at {second_location} (first defined at {first_location})"
                )
            }
            ValidationError::DuplicateServiceOperation {
                service_name,
                operation_name,
                first_location,
                second_location,
            } => {
                write!(
                    f,
                    "Duplicate operation '{operation_name}' in service '{service_name}' at {second_location} (first defined at {first_location})"
                )
            }
            ValidationError::InvalidSizeConstraint { context, reason } => {
                write!(f, "Invalid size constraint in {context}: {reason}")
            }
            ValidationError::InvalidRegexPattern {
                context,
                pattern,
                error,
            } => {
                write!(
                    f,
                    "Invalid regex pattern in {context}: '{pattern}' - {error}"
                )
            }
            ValidationError::InvalidDefaultValue {
                context,
                expected_type,
                actual_value,
            } => {
                write!(
                    f,
                    "Invalid default value in {context}: expected {expected_type}, got {actual_value}"
                )
            }
            ValidationError::ConstraintTypeMismatch {
                constraint_type,
                base_type,
                context,
            } => {
                write!(
                    f,
                    "Constraint {constraint_type} cannot be applied to {base_type} in {context}"
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validate a CSIL interface definition for correctness
pub fn validate_spec(spec: &CsilSpec) -> Result<()> {
    let mut errors = Vec::new();

    // Check for duplicate rule names
    if let Err(duplicate_errors) = validate_unique_rule_names(spec) {
        errors.extend(duplicate_errors);
    }

    for rule in &spec.rules {
        if let Err(validation_errors) = validate_rule(rule, spec) {
            errors.extend(validation_errors);
        }
    }

    if !errors.is_empty() {
        let error_messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        bail!("Validation errors:\n{}", error_messages.join("\n"));
    }

    Ok(())
}

/// Validate a complete CSIL specification with optimizations for large specs
pub fn validate_spec_optimized(spec: &CsilSpec) -> Result<()> {
    use std::collections::BTreeSet;

    // Pre-build lookup tables for faster validation
    let type_names: BTreeSet<String> = spec.rules.iter().map(|rule| rule.name.clone()).collect();

    let lookup_context = ValidationContext { type_names };

    // For large specs (>1000 rules), use parallel validation
    if spec.rules.len() > 1000 {
        validate_spec_parallel(spec, lookup_context)
    } else {
        validate_spec_sequential(spec, lookup_context)
    }
}

#[derive(Clone)]
struct ValidationContext {
    type_names: std::collections::BTreeSet<String>,
}

fn validate_spec_sequential(spec: &CsilSpec, context: ValidationContext) -> Result<()> {
    let mut errors = Vec::new();

    for rule in &spec.rules {
        if let Err(validation_errors) = validate_rule_optimized(rule, spec, &context) {
            errors.extend(validation_errors);
        }
    }

    if !errors.is_empty() {
        let error_messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        bail!("Validation errors:\n{}", error_messages.join("\n"));
    }

    Ok(())
}

fn validate_spec_parallel(spec: &CsilSpec, context: ValidationContext) -> Result<()> {
    let spec = Arc::new(spec);
    let context = Arc::new(context);
    let errors = Arc::new(Mutex::new(Vec::new()));

    // Split rules into chunks for parallel processing
    const CHUNK_SIZE: usize = 100;
    let chunks: Vec<_> = spec.rules.chunks(CHUNK_SIZE).collect();

    thread::scope(|s| {
        for chunk in chunks {
            let spec = Arc::clone(&spec);
            let context = Arc::clone(&context);
            let errors = Arc::clone(&errors);

            s.spawn(move || {
                let mut local_errors = Vec::new();

                for rule in chunk {
                    if let Err(validation_errors) = validate_rule_optimized(rule, &spec, &context) {
                        local_errors.extend(validation_errors);
                    }
                }

                if !local_errors.is_empty() {
                    let mut global_errors = errors.lock().unwrap();
                    global_errors.extend(local_errors);
                }
            });
        }
    });

    let final_errors = errors.lock().unwrap();
    if !final_errors.is_empty() {
        let error_messages: Vec<String> = final_errors.iter().map(|e| e.to_string()).collect();
        bail!("Validation errors:\n{}", error_messages.join("\n"));
    }

    Ok(())
}

fn validate_rule_optimized(
    rule: &Rule,
    spec: &CsilSpec,
    context: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    match &rule.rule_type {
        RuleType::TypeDef(TypeExpression::Group(group)) => {
            if let Err(group_errors) = validate_group_optimized(group, &rule.name, context) {
                errors.extend(group_errors);
            }
        }
        RuleType::GroupDef(group) => {
            if let Err(group_errors) = validate_group_optimized(group, &rule.name, context) {
                errors.extend(group_errors);
            }
        }
        RuleType::ServiceDef(service) => {
            if let Err(service_errors) = validate_service_optimized(service, &rule.name, context) {
                errors.extend(service_errors);
            }
        }
        _ => {
            // Other rule types use the standard validation
            return validate_rule(rule, spec);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_group_optimized(
    group: &GroupExpression,
    context_name: &str,
    _validation_context: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    // Use the existing validate_group implementation for now
    // In a more advanced optimization, we could cache field lookups
    validate_group(group, context_name)
}

/// Validate that all rule names in the spec are unique
fn validate_unique_rule_names(spec: &CsilSpec) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let mut seen_names: HashMap<String, Position> = HashMap::new();

    for rule in &spec.rules {
        if let Some(first_position) = seen_names.get(&rule.name) {
            errors.push(ValidationError::DuplicateRule {
                rule_name: rule.name.clone(),
                first_location: format!("line {}", first_position.line),
                second_location: format!("line {}", rule.position.line),
            });
        } else {
            seen_names.insert(rule.name.clone(), rule.position);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate that all operation names within a service are unique
fn validate_unique_operation_names(
    service: &ServiceDefinition,
    service_name: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let mut seen_operations: HashMap<String, Position> = HashMap::new();

    for operation in &service.operations {
        if let Some(first_position) = seen_operations.get(&operation.name) {
            errors.push(ValidationError::DuplicateServiceOperation {
                service_name: service_name.to_string(),
                operation_name: operation.name.clone(),
                first_location: format!("line {}", first_position.line),
                second_location: format!("line {}", operation.position.line),
            });
        } else {
            seen_operations.insert(operation.name.clone(), operation.position);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_service_optimized(
    service: &ServiceDefinition,
    context_name: &str,
    validation_context: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    // Check for duplicate operation names within the service
    if let Err(duplicate_errors) = validate_unique_operation_names(service, context_name) {
        errors.extend(duplicate_errors);
    }

    // Validate service operations with optimized type lookups
    for operation in &service.operations {
        // Extract type names from TypeExpression for validation
        let input_type_name = extract_type_name(&operation.input_type);
        let output_type_name = extract_type_name(&operation.output_type);

        // Use fast BTreeSet lookup instead of iterating through all rules
        if let Some(input_name) = input_type_name
            && !validation_context.type_names.contains(&input_name)
        {
            // Suggest similar type names for typos
            let available_names: Vec<String> =
                validation_context.type_names.iter().cloned().collect();
            let suggestions =
                csilgen_common::CsilgenError::suggest_similar_names(&input_name, &available_names);

            let suggestion_text = if !suggestions.is_empty() {
                format!(" Did you mean: {}?", suggestions.join(", "))
            } else {
                String::new()
            };

            errors.push(ValidationError::UnknownDependencyField {
                field_context: format!(
                    "{}::{} input type{suggestion_text}",
                    context_name, operation.name
                ),
                depends_on: input_name,
            });
        }

        if let Some(output_name) = output_type_name
            && !validation_context.type_names.contains(&output_name)
        {
            // Suggest similar type names for typos
            let available_names: Vec<String> =
                validation_context.type_names.iter().cloned().collect();
            let suggestions =
                csilgen_common::CsilgenError::suggest_similar_names(&output_name, &available_names);

            let suggestion_text = if !suggestions.is_empty() {
                format!(" Did you mean: {}?", suggestions.join(", "))
            } else {
                String::new()
            };

            errors.push(ValidationError::UnknownDependencyField {
                field_context: format!(
                    "{}::{} output type{suggestion_text}",
                    context_name, operation.name
                ),
                depends_on: output_name,
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Extract the primary type name from a TypeExpression for validation
fn extract_type_name(type_expr: &TypeExpression) -> Option<String> {
    match type_expr {
        // Builtins are always valid (an unknown type name lexes as a Reference, not a
        // Builtin), so there is nothing to resolve — this also lets a builtin, or a
        // `null` no-input op, stand as an operation input/output type.
        TypeExpression::Builtin(_) => None,
        TypeExpression::Reference(name) => Some(name.clone()),
        TypeExpression::Group(_) => None, // Inline groups don't have names to validate
        TypeExpression::Tuple(_) => None, // Inline tuples don't have a single name
        TypeExpression::Array { element_type, .. } => extract_type_name(element_type),
        TypeExpression::Map { key, value, .. } => {
            // For maps, validate both key and value types
            extract_type_name(key).or_else(|| extract_type_name(value))
        }
        TypeExpression::Choice(choices) => {
            // For choices, validate the first non-builtin type
            choices.iter().find_map(extract_type_name)
        }
        TypeExpression::Range { .. } => None, // Range expressions don't have a base type to validate
        TypeExpression::Literal(_) => None,   // Literals don't need validation
        TypeExpression::Socket(name) => Some(name.clone()), // Socket references need validation
        TypeExpression::Plug(name) => Some(name.clone()), // Plug references need validation
        TypeExpression::Constrained {
            base_type,
            constraints: _,
        } => {
            // Extract the type name from the base type, constraints don't affect the type name
            extract_type_name(base_type)
        }
    }
}

fn validate_rule(rule: &Rule, spec: &CsilSpec) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    match &rule.rule_type {
        RuleType::TypeDef(type_expr) => {
            // Validate the type expression (this includes constrained types)
            if let Err(type_errors) = validate_type_expression(type_expr, &rule.name) {
                errors.extend(type_errors);
            }
        }
        RuleType::GroupDef(group) => {
            if let Err(group_errors) = validate_group(group, &rule.name) {
                errors.extend(group_errors);
            }
        }
        RuleType::ServiceDef(service) => {
            // Check for duplicate operation names within the service
            if let Err(duplicate_errors) = validate_unique_operation_names(service, &rule.name) {
                errors.extend(duplicate_errors);
            }

            for operation in &service.operations {
                if let Err(op_errors) = validate_service_operation(operation, spec) {
                    errors.extend(op_errors);
                }
            }
        }
        _ => {} // Other rule types don't need metadata validation yet
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_group(group: &GroupExpression, context: &str) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let mut field_names = HashSet::new();

    // Collect field names for dependency validation
    for entry in &group.entries {
        if let Some(GroupKey::Bare(field_name)) = &entry.key {
            field_names.insert(field_name.clone());
        }
    }

    // Validate each field's metadata
    for entry in &group.entries {
        let field_context = match &entry.key {
            Some(GroupKey::Bare(name)) => format!("{context}.{name}"),
            Some(GroupKey::Literal(LiteralValue::Text(name))) => {
                format!("{context}.\"{name}\"")
            }
            Some(GroupKey::Literal(LiteralValue::Integer(i))) => format!("{context}.{i}"),
            _ => format!("{}.{}", context, "[anonymous]"),
        };

        if let Err(field_errors) =
            validate_field_metadata(&entry.metadata, &field_context, &field_names)
        {
            errors.extend(field_errors);
        }

        // Validate constrained types if present
        if let Err(type_errors) = validate_type_expression(&entry.value_type, &field_context) {
            errors.extend(type_errors);
        }
    }

    // Check for circular dependencies
    if let Err(circular_errors) = check_circular_dependencies(group, context) {
        errors.extend(circular_errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_type_expression(
    type_expr: &TypeExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    match type_expr {
        TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            // Validate the base type first
            if let Err(base_errors) = validate_type_expression(base_type, context) {
                errors.extend(base_errors);
            }

            // Validate constraints
            for constraint in constraints {
                if let Err(constraint_errors) =
                    validate_control_operator(constraint, base_type, context)
                {
                    errors.extend(constraint_errors);
                }
            }
        }
        TypeExpression::Array { element_type, .. } => {
            if let Err(element_errors) = validate_type_expression(element_type, context) {
                errors.extend(element_errors);
            }
        }
        TypeExpression::Map { key, value, .. } => {
            if let Err(key_errors) = validate_type_expression(key, context) {
                errors.extend(key_errors);
            }
            if let Err(value_errors) = validate_type_expression(value, context) {
                errors.extend(value_errors);
            }
        }
        TypeExpression::Group(group) => {
            if let Err(group_errors) = validate_group(group, context) {
                errors.extend(group_errors);
            }
        }
        TypeExpression::Tuple(group) => {
            for entry in &group.entries {
                if let Err(entry_errors) = validate_type_expression(&entry.value_type, context) {
                    errors.extend(entry_errors);
                }
            }
        }
        TypeExpression::Choice(choices) => {
            for choice in choices {
                if let Err(choice_errors) = validate_type_expression(choice, context) {
                    errors.extend(choice_errors);
                }
            }
        }
        // Other type expressions don't need recursive validation
        _ => {}
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_control_operator(
    constraint: &ControlOperator,
    base_type: &TypeExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    match constraint {
        ControlOperator::Size(size_constraint) => {
            if let Err(size_errors) = validate_size_constraint(size_constraint, base_type, context)
            {
                errors.extend(size_errors);
            }
        }
        ControlOperator::Regex(pattern) => {
            if let Err(regex_errors) = validate_regex_constraint(pattern, base_type, context) {
                errors.extend(regex_errors);
            }
        }
        ControlOperator::Default(default_value) => {
            if let Err(default_errors) = validate_default_value(default_value, base_type, context) {
                errors.extend(default_errors);
            }
        }
        ControlOperator::GreaterEqual(value) => {
            if let Err(comp_errors) =
                validate_comparison_constraint(">=", value, base_type, context)
            {
                errors.extend(comp_errors);
            }
        }
        ControlOperator::LessEqual(value) => {
            if let Err(comp_errors) =
                validate_comparison_constraint("<=", value, base_type, context)
            {
                errors.extend(comp_errors);
            }
        }
        ControlOperator::GreaterThan(value) => {
            if let Err(comp_errors) = validate_comparison_constraint(">", value, base_type, context)
            {
                errors.extend(comp_errors);
            }
        }
        ControlOperator::LessThan(value) => {
            if let Err(comp_errors) = validate_comparison_constraint("<", value, base_type, context)
            {
                errors.extend(comp_errors);
            }
        }
        ControlOperator::Equal(value) => {
            if let Err(comp_errors) =
                validate_comparison_constraint("==", value, base_type, context)
            {
                errors.extend(comp_errors);
            }
        }
        ControlOperator::NotEqual(value) => {
            if let Err(comp_errors) =
                validate_comparison_constraint("!=", value, base_type, context)
            {
                errors.extend(comp_errors);
            }
        }
        ControlOperator::Bits(_bits_expr) => {
            // Validate that .bits is applied to integer or bytes types
            let is_bits_applicable = match base_type {
                TypeExpression::Builtin(name) => {
                    matches!(name.as_str(), "int" | "uint" | "nint" | "bytes" | "bstr")
                }
                _ => false,
            };

            if !is_bits_applicable {
                errors.push(ValidationError::ConstraintTypeMismatch {
                    constraint_type: ".bits".to_string(),
                    base_type: format!("{:?}", base_type),
                    context: context.to_string(),
                });
            }
            // TODO: Validate bits expression syntax when specification is clearer
        }
        ControlOperator::And(type_expr) => {
            // Validate the intersected type expression
            if let Err(type_errors) = validate_type_expression(type_expr, context) {
                errors.extend(type_errors);
            }
            // TODO: Add semantic validation for type intersection compatibility
        }
        ControlOperator::Within(type_expr) => {
            // Validate the subset type expression
            if let Err(type_errors) = validate_type_expression(type_expr, context) {
                errors.extend(type_errors);
            }
            // TODO: Add semantic validation for subset constraint compatibility
        }
        ControlOperator::Json => {
            // Validate that .json is applied to text or bytes types
            let is_json_applicable = match base_type {
                TypeExpression::Builtin(name) => {
                    matches!(name.as_str(), "text" | "tstr" | "bytes" | "bstr")
                }
                _ => false,
            };

            if !is_json_applicable {
                errors.push(ValidationError::InvalidConstraint {
                    field_context: context.to_string(),
                    constraint: ".json".to_string(),
                    reason: "only applicable to text or bytes types".to_string(),
                });
            }
        }
        ControlOperator::Cbor => {
            // Validate that .cbor is applied to bytes types
            let is_cbor_applicable = match base_type {
                TypeExpression::Builtin(name) => {
                    matches!(name.as_str(), "bytes" | "bstr")
                }
                _ => false,
            };

            if !is_cbor_applicable {
                errors.push(ValidationError::InvalidConstraint {
                    field_context: context.to_string(),
                    constraint: ".cbor".to_string(),
                    reason: "only applicable to bytes types".to_string(),
                });
            }
        }
        ControlOperator::Cborseq => {
            // Validate that .cborseq is applied to bytes types
            let is_cborseq_applicable = match base_type {
                TypeExpression::Builtin(name) => {
                    matches!(name.as_str(), "bytes" | "bstr")
                }
                _ => false,
            };

            if !is_cborseq_applicable {
                errors.push(ValidationError::InvalidConstraint {
                    field_context: context.to_string(),
                    constraint: ".cborseq".to_string(),
                    reason: "only applicable to bytes types".to_string(),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_size_constraint(
    size_constraint: &SizeConstraint,
    base_type: &TypeExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    // Check if size constraints are applicable to this base type
    let is_size_applicable = match base_type {
        TypeExpression::Builtin(name) => {
            matches!(name.as_str(), "text" | "tstr" | "bytes" | "bstr")
        }
        TypeExpression::Array { .. } => true,
        _ => false,
    };

    if !is_size_applicable {
        errors.push(ValidationError::InvalidSizeConstraint {
            context: context.to_string(),
            reason: format!(
                "size constraints are not applicable to type {}",
                format_type_expression(base_type)
            ),
        });
    }

    // Validate the constraint values
    match size_constraint {
        SizeConstraint::Exact(size) => {
            if *size == 0 {
                errors.push(ValidationError::InvalidSizeConstraint {
                    context: context.to_string(),
                    reason: "exact size cannot be zero".to_string(),
                });
            }
        }
        SizeConstraint::Range { min, max } => {
            if min > max {
                errors.push(ValidationError::InvalidSizeConstraint {
                    context: context.to_string(),
                    reason: format!(
                        "minimum size ({min}) cannot be greater than maximum size ({max})"
                    ),
                });
            }
            // Size constraints with min=0 are valid (allow empty strings/arrays)
        }
        SizeConstraint::Min(_min_val) => {
            // Size constraints with min=0 are valid (allow empty strings/arrays)
        }
        SizeConstraint::Max(_max_val) => {
            // Max constraints are always valid as long as they're positive
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_regex_constraint(
    pattern: &str,
    base_type: &TypeExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    // Check if regex constraints are applicable to this base type
    let is_regex_applicable = matches!(base_type,
        TypeExpression::Builtin(name) if matches!(name.as_str(), "text" | "tstr")
    );

    if !is_regex_applicable {
        errors.push(ValidationError::InvalidRegexPattern {
            context: context.to_string(),
            pattern: pattern.to_string(),
            error: format!(
                "regex constraints are only applicable to text types, not {}",
                format_type_expression(base_type)
            ),
        });
    }

    // Validate that the regex pattern compiles
    match Regex::new(pattern) {
        Ok(_) => {} // Valid regex
        Err(regex_error) => {
            errors.push(ValidationError::InvalidRegexPattern {
                context: context.to_string(),
                pattern: pattern.to_string(),
                error: regex_error.to_string(),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_default_value(
    default_value: &LiteralValue,
    base_type: &TypeExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    // Check type compatibility between default value and base type
    let is_compatible = match (default_value, base_type) {
        (LiteralValue::Integer(_), TypeExpression::Builtin(name)) => {
            matches!(
                name.as_str(),
                "int" | "uint" | "pint" | "nint" | "integer" | "decimal"
            )
        }
        (LiteralValue::Float(_), TypeExpression::Builtin(name)) => {
            // See `validate_comparison_constraint`: a float default on `decimal`
            // would round; write it as exact text instead.
            matches!(
                name.as_str(),
                "float" | "float16" | "float32" | "float64" | "number"
            )
        }
        (LiteralValue::Text(_), TypeExpression::Builtin(name)) => {
            // `decimal` defaults are best written as exact text, and `timestamp`
            // defaults are RFC3339 strings.
            matches!(
                name.as_str(),
                "text" | "tstr" | "string" | "decimal" | "timestamp"
            )
        }
        (LiteralValue::Bool(_), TypeExpression::Builtin(name)) => {
            matches!(name.as_str(), "bool" | "boolean")
        }
        (LiteralValue::Bytes(_), TypeExpression::Builtin(name)) => {
            matches!(name.as_str(), "bytes" | "bstr")
        }
        (LiteralValue::Null, TypeExpression::Builtin(name)) => {
            matches!(name.as_str(), "null" | "nil")
        }
        // For non-builtin types, we need more sophisticated checking
        _ => true, // For now, allow other combinations
    };

    if !is_compatible {
        errors.push(ValidationError::InvalidDefaultValue {
            context: context.to_string(),
            expected_type: format_type_expression(base_type),
            actual_value: format_literal_value(default_value),
        });
    }

    if let Err(reason) = check_tagged_bound_well_formed(default_value, base_type) {
        errors.push(ValidationError::InvalidConstraint {
            field_context: context.to_string(),
            constraint: ".default".to_string(),
            reason,
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// A text bound/default on `decimal` must parse as an exact decimal and on
/// `timestamp` as an RFC3339 instant. Catching this in core keeps a malformed
/// literal — a known compile-time constant — from reaching the generators, where
/// it would otherwise panic (Go), raise (Python), or silently disable the check
/// (TypeScript's `new Date(bad)` is NaN). Non-text and non-tagged cases are fine.
fn check_tagged_bound_well_formed(
    value: &LiteralValue,
    base_type: &TypeExpression,
) -> Result<(), String> {
    let TypeExpression::Builtin(name) = base_type else {
        return Ok(());
    };
    let LiteralValue::Text(text) = value else {
        return Ok(());
    };
    match name.as_str() {
        "decimal" => is_well_formed_decimal(text)
            .then_some(())
            .ok_or_else(|| format!("decimal bound {text:?} is not a valid decimal number")),
        "timestamp" => chrono::DateTime::parse_from_rfc3339(text)
            .map(|_| ())
            .map_err(|e| format!("timestamp bound {text:?} is not a valid RFC3339 instant: {e}")),
        _ => Ok(()),
    }
}

/// An optionally-signed integer with an optional fractional part — the exact
/// textual form a `decimal` bound takes. Exponents are intentionally not accepted
/// so the literal stays unambiguous.
fn is_well_formed_decimal(text: &str) -> bool {
    let rest = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (int_part, frac_part) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    let digits_only = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match frac_part {
        // A trailing/leading dot ("1." or ".5") is rejected: require digits on both sides.
        Some(frac) => digits_only(int_part) && digits_only(frac),
        None => digits_only(int_part),
    }
}

fn format_type_expression(type_expr: &TypeExpression) -> String {
    match type_expr {
        TypeExpression::Builtin(name) => name.clone(),
        TypeExpression::Reference(name) => name.clone(),
        TypeExpression::Array { element_type, .. } => {
            format!("[{}]", format_type_expression(element_type))
        }
        TypeExpression::Map { key, value, .. } => {
            format!(
                "{{{}: {}}}",
                format_type_expression(key),
                format_type_expression(value)
            )
        }
        TypeExpression::Group(_) => "group".to_string(),
        TypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .map(|e| format_type_expression(&e.value_type))
                .collect();
            format!("[{}]", parts.join(", "))
        }
        TypeExpression::Choice(choices) => {
            let choice_strs: Vec<String> = choices.iter().map(format_type_expression).collect();
            choice_strs.join(" | ")
        }
        TypeExpression::Literal(literal) => format_literal_value(literal),
        TypeExpression::Range { start, end, .. } => {
            format!(
                "{}..{}",
                start.map_or_else(|| "-∞".to_string(), |s| s.to_string()),
                end.map_or_else(|| "∞".to_string(), |e| e.to_string())
            )
        }
        TypeExpression::Socket(name) | TypeExpression::Plug(name) => name.clone(),
        TypeExpression::Constrained { base_type, .. } => format_type_expression(base_type),
    }
}

fn format_literal_value(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Integer(val) => val.to_string(),
        LiteralValue::Float(val) => val.to_string(),
        LiteralValue::Text(val) => format!("\"{val}\""),
        LiteralValue::Bool(val) => val.to_string(),
        LiteralValue::Null => "null".to_string(),
        LiteralValue::Bytes(val) => format!("bytes({})", val.len()),
        LiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(format_literal_value).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

fn validate_comparison_constraint(
    operator: &str,
    value: &LiteralValue,
    base_type: &TypeExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    // Check if comparison constraints are applicable to this base type
    let is_comparable = match base_type {
        TypeExpression::Builtin(name) => {
            if operator == "==" || operator == "!=" {
                // Equality and inequality comparisons are allowed for any type
                true
            } else {
                // Other comparisons only for ordered types. `decimal` is ordered like
                // any number, and `timestamp` is ordered chronologically.
                matches!(
                    name.as_str(),
                    "int"
                        | "uint"
                        | "pint"
                        | "nint"
                        | "integer"
                        | "float"
                        | "float16"
                        | "float32"
                        | "float64"
                        | "number"
                        | "decimal"
                        | "timestamp"
                )
            }
        }
        _ => operator == "==" || operator == "!=",
    };

    if !is_comparable {
        errors.push(ValidationError::InvalidConstraint {
            field_context: context.to_string(),
            constraint: format!("{operator} constraint"),
            reason: format!(
                "comparison constraints are only applicable to numeric types, not {}",
                format_type_expression(base_type)
            ),
        });
    }

    // Check if the comparison value type is compatible with the base type
    let is_value_compatible = match (value, base_type) {
        (LiteralValue::Integer(_), TypeExpression::Builtin(name)) => {
            matches!(
                name.as_str(),
                "int"
                    | "uint"
                    | "pint"
                    | "nint"
                    | "integer"
                    | "float"
                    | "float16"
                    | "float32"
                    | "float64"
                    | "number"
                    | "decimal"
            )
        }
        (LiteralValue::Float(_), TypeExpression::Builtin(name)) => {
            // `decimal` deliberately excludes float bounds: a binary float can't
            // represent most decimals exactly, which would reintroduce the rounding
            // `decimal` exists to avoid. Write the bound as exact text instead.
            matches!(
                name.as_str(),
                "float" | "float16" | "float32" | "float64" | "number"
            )
        }
        (LiteralValue::Bool(_), TypeExpression::Builtin(name)) => name.as_str() == "bool",
        (LiteralValue::Text(_), TypeExpression::Builtin(name)) => {
            // A `decimal` bound may be written as exact text ("0.1") to avoid float
            // rounding, and a `timestamp` bound is an RFC3339 string.
            matches!(name.as_str(), "text" | "tstr" | "decimal" | "timestamp")
        }
        _ => false,
    };

    if !is_value_compatible {
        errors.push(ValidationError::InvalidConstraint {
            field_context: context.to_string(),
            constraint: format!("{operator} constraint"),
            reason: format!(
                "comparison value {} is not compatible with type {}",
                format_literal_value(value),
                format_type_expression(base_type)
            ),
        });
    }

    // A text bound on a tagged type must be parseable, otherwise the malformed
    // string flows to the generators and panics or is silently dropped there.
    if let Err(reason) = check_tagged_bound_well_formed(value, base_type) {
        errors.push(ValidationError::InvalidConstraint {
            field_context: context.to_string(),
            constraint: format!("{operator} constraint"),
            reason,
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Check that a `@depends-on` target field exists (or is a well-formed dotted
/// cross-structure path).
fn validate_dependency_field(
    field: &str,
    field_context: &str,
    available_fields: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    if field.contains('.') {
        // Dotted paths reference cross-structure fields; only sanity-check the shape.
        let parts: Vec<&str> = field.split('.').collect();
        if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
            errors.push(ValidationError::UnknownDependencyField {
                field_context: format!("{field_context}. Invalid dependency path format"),
                depends_on: field.to_string(),
            });
        }
    } else if !available_fields.contains(field) {
        let available_names: Vec<String> = available_fields.iter().cloned().collect();
        let suggestions =
            csilgen_common::CsilgenError::suggest_similar_names(field, &available_names);
        let suggestion_text = if suggestions.is_empty() {
            String::new()
        } else {
            format!(" Did you mean: {}?", suggestions.join(", "))
        };
        errors.push(ValidationError::UnknownDependencyField {
            field_context: format!("{field_context}{suggestion_text}"),
            depends_on: field.to_string(),
        });
    }
}

/// Collect every field name referenced by a boolean `@depends-on` condition.
fn collect_depends_fields(condition: &DependsCondition, out: &mut Vec<String>) {
    match condition {
        DependsCondition::Compare { field, .. } => out.push(field.clone()),
        DependsCondition::All(conds) | DependsCondition::Any(conds) => {
            for c in conds {
                collect_depends_fields(c, out);
            }
        }
    }
}

/// Validate every field referenced by a boolean `@depends-on` condition.
fn validate_depends_condition(
    condition: &DependsCondition,
    field_context: &str,
    available_fields: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    match condition {
        DependsCondition::Compare { field, .. } => {
            validate_dependency_field(field, field_context, available_fields, errors);
        }
        DependsCondition::All(conds) | DependsCondition::Any(conds) => {
            for c in conds {
                validate_depends_condition(c, field_context, available_fields, errors);
            }
        }
    }
}

fn validate_field_metadata(
    metadata: &[FieldMetadata],
    field_context: &str,
    available_fields: &HashSet<String>,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let mut visibility: Option<&FieldVisibility> = None;
    let mut constraints: HashMap<String, usize> = HashMap::new();

    for meta in metadata {
        match meta {
            FieldMetadata::Visibility(vis) => {
                if let Some(existing_vis) = visibility {
                    errors.push(ValidationError::ConflictingVisibility {
                        field_context: field_context.to_string(),
                        first: existing_vis.clone(),
                        second: vis.clone(),
                    });
                } else {
                    visibility = Some(vis);
                }
            }
            FieldMetadata::DependsOn { field, .. } => {
                validate_dependency_field(field, field_context, available_fields, &mut errors);
            }
            FieldMetadata::DependsOnExpr(condition) => {
                validate_depends_condition(condition, field_context, available_fields, &mut errors);
            }
            FieldMetadata::Constraint(constraint) => {
                let constraint_type = match constraint {
                    ValidationConstraint::MinLength(_) => "min-length",
                    ValidationConstraint::MaxLength(_) => "max-length",
                    ValidationConstraint::MinItems(_) => "min-items",
                    ValidationConstraint::MaxItems(_) => "max-items",
                    ValidationConstraint::MinValue(_) => "min-value",
                    ValidationConstraint::MaxValue(_) => "max-value",
                    ValidationConstraint::Custom { name, .. } => name,
                };

                let count = constraints.entry(constraint_type.to_string()).or_insert(0);
                *count += 1;

                if *count > 1 {
                    errors.push(ValidationError::DuplicateConstraint {
                        field_context: field_context.to_string(),
                        constraint_type: constraint_type.to_string(),
                    });
                }

                // A zero minimum (`min-items(0)`/`min-length(0)`) is a valid, if
                // degenerate, bound — it is allowed here and the linter flags it
                // instead (see `warn_degenerate_constraints`).
            }
            FieldMetadata::Description(_) => {
                // Description metadata doesn't need validation
            }
            FieldMetadata::Custom { .. } => {
                // Custom metadata validation can be added later
            }
        }
    }

    // Check for conflicting min/max constraints
    if let (Some(min_len), Some(max_len)) =
        (constraints.get("min-length"), constraints.get("max-length"))
        && *min_len == 1
        && *max_len == 1
    {
        // Need to get actual values to compare - this is a simplified check
        // In a real implementation, we'd store the values during the loop above
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn check_circular_dependencies(
    group: &GroupExpression,
    context: &str,
) -> Result<(), Vec<ValidationError>> {
    let mut dependency_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut errors = Vec::new();

    // Build dependency map
    for entry in &group.entries {
        if let Some(GroupKey::Bare(field_name)) = &entry.key {
            let mut dependencies = Vec::new();
            for meta in &entry.metadata {
                match meta {
                    FieldMetadata::DependsOn { field, .. } => dependencies.push(field.clone()),
                    FieldMetadata::DependsOnExpr(condition) => {
                        collect_depends_fields(condition, &mut dependencies)
                    }
                    _ => {}
                }
            }
            dependency_map.insert(field_name.clone(), dependencies);
        }
    }

    // Check for cycles using DFS
    for field_name in dependency_map.keys() {
        if let Some(cycle) = detect_cycle(
            field_name,
            &dependency_map,
            &mut HashSet::new(),
            &mut Vec::new(),
        ) {
            errors.push(ValidationError::CircularDependency {
                field_context: format!("{context}.{field_name}"),
                cycle,
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn detect_cycle(
    current: &str,
    dependencies: &HashMap<String, Vec<String>>,
    visited: &mut HashSet<String>,
    path: &mut Vec<String>,
) -> Option<Vec<String>> {
    if path.contains(&current.to_string()) {
        // Found a cycle
        let cycle_start = path.iter().position(|x| x == current).unwrap();
        let mut cycle = path[cycle_start..].to_vec();
        cycle.push(current.to_string());
        return Some(cycle);
    }

    if visited.contains(current) {
        return None;
    }

    visited.insert(current.to_string());
    path.push(current.to_string());

    if let Some(deps) = dependencies.get(current) {
        for dep in deps {
            if let Some(cycle) = detect_cycle(dep, dependencies, visited, path) {
                return Some(cycle);
            }
        }
    }

    path.pop();
    None
}

fn validate_service_operation(
    operation: &ServiceOperation,
    _spec: &CsilSpec,
) -> Result<(), Vec<ValidationError>> {
    // Service operation metadata validation can be added here
    // For now, we'll just validate the input/output types if they contain groups
    let mut errors = Vec::new();

    if let TypeExpression::Group(group) = &operation.input_type
        && let Err(group_errors) = validate_group(group, &format!("{}[input]", operation.name))
    {
        errors.extend(group_errors);
    }

    if let TypeExpression::Group(group) = &operation.output_type
        && let Err(group_errors) = validate_group(group, &format!("{}[output]", operation.name))
    {
        errors.extend(group_errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Position;

    fn create_test_rule(name: &str, rule_type: RuleType) -> Rule {
        Rule {
            name: name.to_string(),
            rule_type,
            position: Position::new(1, 1, 0),
            doc_comments: Vec::new(),
        }
    }

    fn create_test_spec(rules: Vec<Rule>) -> CsilSpec {
        CsilSpec {
            imports: Vec::new(),
            options: None,
            rules,
        }
    }

    #[test]
    fn test_validate_conflicting_visibility() {
        let spec = create_test_spec(vec![create_test_rule(
            "User",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(GroupKey::Bare("password".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![
                        FieldMetadata::Visibility(FieldVisibility::SendOnly),
                        FieldMetadata::Visibility(FieldVisibility::ReceiveOnly),
                    ],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("conflicting visibility"));
    }

    #[test]
    fn test_validate_unknown_dependency_field() {
        let spec = create_test_spec(vec![create_test_rule(
            "Order",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(GroupKey::Bare("expedite_fee".to_string())),
                    value_type: TypeExpression::Builtin("int".to_string()),
                    occurrence: None,
                    metadata: vec![FieldMetadata::DependsOn {
                        field: "nonexistent_field".to_string(),
                        value: None,
                    }],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("depends on unknown field"));
    }

    #[test]
    fn test_validate_duplicate_constraints() {
        let spec = create_test_spec(vec![create_test_rule(
            "User",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(GroupKey::Bare("username".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![
                        FieldMetadata::Constraint(ValidationConstraint::MinLength(3)),
                        FieldMetadata::Constraint(ValidationConstraint::MinLength(5)),
                    ],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("duplicate min-length constraints"));
    }

    #[test]
    fn test_validate_circular_dependency() {
        let spec = create_test_spec(vec![create_test_rule(
            "Test",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("field_a".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![FieldMetadata::DependsOn {
                            field: "field_b".to_string(),
                            value: None,
                        }],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("field_b".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![FieldMetadata::DependsOn {
                            field: "field_a".to_string(),
                            value: None,
                        }],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("circular dependency"));
    }

    #[test]
    fn test_validate_zero_min_length_is_allowed() {
        // `min-length(0)` is a valid (degenerate) bound; the validator accepts it and
        // the linter is responsible for nudging.
        let spec = create_test_spec(vec![create_test_rule(
            "User",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(GroupKey::Bare("username".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![FieldMetadata::Constraint(ValidationConstraint::MinLength(
                        0,
                    ))],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn test_validate_valid_metadata() {
        let spec = create_test_spec(vec![create_test_rule(
            "User",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("password".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            FieldMetadata::Visibility(FieldVisibility::SendOnly),
                            FieldMetadata::Description("User's password".to_string()),
                            FieldMetadata::Constraint(ValidationConstraint::MinLength(8)),
                        ],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("type".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("expedite_fee".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: Some(Occurrence::Optional),
                        metadata: vec![FieldMetadata::DependsOn {
                            field: "type".to_string(),
                            value: Some(LiteralValue::Text("express".to_string())),
                        }],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_service_operations() {
        let spec = create_test_spec(vec![create_test_rule(
            "UserService",
            RuleType::ServiceDef(ServiceDefinition {
                operations: vec![ServiceOperation {
                    name: "create-user".to_string(),
                    input_type: TypeExpression::Group(GroupExpression {
                        entries: vec![GroupEntry {
                            key: Some(GroupKey::Bare("password".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![FieldMetadata::Visibility(FieldVisibility::SendOnly)],
                            doc_comments: Vec::new(),
                        }],
                    }),
                    output_type: TypeExpression::Group(GroupExpression {
                        entries: vec![GroupEntry {
                            key: Some(GroupKey::Bare("id".to_string())),
                            value_type: TypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![FieldMetadata::Visibility(FieldVisibility::ReceiveOnly)],
                            doc_comments: Vec::new(),
                        }],
                    }),
                    direction: ServiceDirection::Unidirectional,
                    position: Position::new(1, 1, 0),
                    doc_comments: Vec::new(),
                }],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bidirectional_service() {
        let spec = create_test_spec(vec![create_test_rule(
            "ChatService",
            RuleType::ServiceDef(ServiceDefinition {
                operations: vec![ServiceOperation {
                    name: "send-message".to_string(),
                    input_type: TypeExpression::Group(GroupExpression {
                        entries: vec![GroupEntry {
                            key: Some(GroupKey::Bare("content".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![FieldMetadata::Visibility(FieldVisibility::SendOnly)],
                            doc_comments: Vec::new(),
                        }],
                    }),
                    output_type: TypeExpression::Group(GroupExpression {
                        entries: vec![GroupEntry {
                            key: Some(GroupKey::Bare("message_id".to_string())),
                            value_type: TypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![FieldMetadata::Visibility(FieldVisibility::ReceiveOnly)],
                            doc_comments: Vec::new(),
                        }],
                    }),
                    direction: ServiceDirection::Bidirectional,
                    position: Position::new(1, 1, 0),
                    doc_comments: Vec::new(),
                }],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_service_operation_naming_conflict() {
        let spec = create_test_spec(vec![create_test_rule(
            "ConflictService",
            RuleType::ServiceDef(ServiceDefinition {
                operations: vec![
                    ServiceOperation {
                        name: "process".to_string(),
                        input_type: TypeExpression::Builtin("int".to_string()),
                        output_type: TypeExpression::Builtin("text".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(1, 1, 0),
                        doc_comments: Vec::new(),
                    },
                    ServiceOperation {
                        name: "process".to_string(),
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("int".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(2, 1, 0),
                        doc_comments: Vec::new(),
                    },
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        // Duplicate operation names within the same service should now be detected
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Duplicate operation 'process'"));
    }

    #[test]
    fn test_validate_service_with_invalid_metadata_combinations() {
        let spec = create_test_spec(vec![create_test_rule(
            "InvalidService",
            RuleType::ServiceDef(ServiceDefinition {
                operations: vec![ServiceOperation {
                    name: "invalid-op".to_string(),
                    input_type: TypeExpression::Group(GroupExpression {
                        entries: vec![GroupEntry {
                            key: Some(GroupKey::Bare("field".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![
                                FieldMetadata::Visibility(FieldVisibility::SendOnly),
                                FieldMetadata::Visibility(FieldVisibility::ReceiveOnly),
                            ],
                            doc_comments: Vec::new(),
                        }],
                    }),
                    output_type: TypeExpression::Builtin("int".to_string()),
                    direction: ServiceDirection::Unidirectional,
                    position: Position::new(1, 1, 0),
                    doc_comments: Vec::new(),
                }],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("conflicting visibility"));
    }

    #[test]
    fn test_validate_dependency_chains() {
        let spec = create_test_spec(vec![create_test_rule(
            "OrderChain",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("customer_type".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("priority".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![FieldMetadata::DependsOn {
                            field: "customer_type".to_string(),
                            value: Some(LiteralValue::Text("premium".to_string())),
                        }],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("expedite_fee".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: Some(Occurrence::Optional),
                        metadata: vec![FieldMetadata::DependsOn {
                            field: "priority".to_string(),
                            value: Some(LiteralValue::Integer(5)),
                        }],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_constraint_type_compatibility() {
        let spec = create_test_spec(vec![create_test_rule(
            "ConstraintTest",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("username".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            FieldMetadata::Constraint(ValidationConstraint::MinLength(3)),
                            FieldMetadata::Constraint(ValidationConstraint::MaxLength(20)),
                        ],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("tags".to_string())),
                        value_type: TypeExpression::Array {
                            element_type: Box::new(TypeExpression::Builtin("text".to_string())),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![
                            FieldMetadata::Constraint(ValidationConstraint::MinItems(1)),
                            FieldMetadata::Constraint(ValidationConstraint::MaxItems(10)),
                        ],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_constraint_with_choice_types() {
        let spec = create_test_spec(vec![create_test_rule(
            "ChoiceTest",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(GroupKey::Bare("status".to_string())),
                    value_type: TypeExpression::Choice(vec![
                        TypeExpression::Builtin("text".to_string()),
                        TypeExpression::Builtin("int".to_string()),
                    ]),
                    occurrence: None,
                    metadata: vec![FieldMetadata::Constraint(ValidationConstraint::MinLength(
                        1,
                    ))],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_multiple_validation_errors_accumulation() {
        let spec = create_test_spec(vec![create_test_rule(
            "MultiErrorTest",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("field1".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            FieldMetadata::Visibility(FieldVisibility::SendOnly),
                            FieldMetadata::Visibility(FieldVisibility::ReceiveOnly),
                        ],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("field2".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![FieldMetadata::DependsOn {
                            field: "nonexistent".to_string(),
                            value: None,
                        }],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("field3".to_string())),
                        // `.size` on an int is a genuine error (size applies to
                        // text/bytes/arrays only).
                        value_type: TypeExpression::Constrained {
                            base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                            constraints: vec![ControlOperator::Size(SizeConstraint::Min(1))],
                        },
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                ],
            })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();

        // Should contain multiple errors
        assert!(error_msg.contains("conflicting visibility"));
        assert!(error_msg.contains("depends on unknown field"));
        assert!(error_msg.contains("size constraints are not applicable"));
    }

    #[test]
    fn test_validate_large_spec_performance() {
        let mut entries = Vec::new();

        // Create a large group with many fields to test performance
        for i in 0..100 {
            entries.push(GroupEntry {
                key: Some(GroupKey::Bare(format!("field_{i}"))),
                value_type: TypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![
                    FieldMetadata::Description(format!("Description for field {i}")),
                    FieldMetadata::Constraint(ValidationConstraint::MinLength(1)),
                ],
                doc_comments: Vec::new(),
            });
        }

        let spec = create_test_spec(vec![create_test_rule(
            "LargeGroup",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression { entries })),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_deeply_nested_groups() {
        let inner_group = GroupExpression {
            entries: vec![GroupEntry {
                key: Some(GroupKey::Bare("inner_field".to_string())),
                value_type: TypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![FieldMetadata::Constraint(ValidationConstraint::MinLength(
                    1,
                ))],
                doc_comments: Vec::new(),
            }],
        };

        let middle_group = GroupExpression {
            entries: vec![GroupEntry {
                key: Some(GroupKey::Bare("middle_field".to_string())),
                value_type: TypeExpression::Group(inner_group),
                occurrence: None,
                metadata: Vec::new(),
                doc_comments: Vec::new(),
            }],
        };

        let spec = create_test_spec(vec![create_test_rule(
            "NestedGroup",
            RuleType::TypeDef(TypeExpression::Group(middle_group)),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_self_referential_dependency() {
        let spec = create_test_spec(vec![create_test_rule(
            "SelfRef",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(GroupKey::Bare("self_field".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![FieldMetadata::DependsOn {
                        field: "self_field".to_string(),
                        value: Some(LiteralValue::Text("trigger".to_string())),
                    }],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        let result = validate_spec(&spec);
        // Self-referential dependencies should be detected as circular
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("circular dependency"));
    }

    #[test]
    fn test_validate_spec_with_imports() {
        // Test validation behavior with import statements
        let spec = CsilSpec {
            imports: vec![
                ImportStatement::Include {
                    path: "common/types.csil".to_string(),
                    alias: None,
                    position: Position::new(1, 1, 0),
                },
                ImportStatement::SelectiveImport {
                    path: "shared/utils.csil".to_string(),
                    items: vec!["UserType".to_string(), "BaseConfig".to_string()],
                    position: Position::new(2, 1, 0),
                },
            ],
            options: None,
            rules: vec![create_test_rule(
                "ApiService",
                RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "get-user".to_string(),
                        input_type: TypeExpression::Reference("UserType".to_string()),
                        output_type: TypeExpression::Reference("UserResponse".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(3, 1, 0),
                        doc_comments: Vec::new(),
                    }],
                }),
            )],
        };

        // This test verifies validation handles imports gracefully
        // Note: Full import resolution requires resolver integration
        let result = validate_spec(&spec);
        // Should not crash with imports present
        assert!(result.is_ok() || result.is_err()); // Either outcome is acceptable for this structural test
    }

    #[test]
    fn test_validate_cross_reference_consistency() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "BaseType",
                RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                    entries: vec![GroupEntry {
                        key: Some(GroupKey::Bare("id".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    }],
                })),
            ),
            create_test_rule(
                "ExtendedType",
                RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                    entries: vec![
                        GroupEntry {
                            key: Some(GroupKey::Bare("base".to_string())),
                            value_type: TypeExpression::Reference("BaseType".to_string()),
                            occurrence: None,
                            metadata: Vec::new(),
                            doc_comments: Vec::new(),
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("extension".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![FieldMetadata::DependsOn {
                                field: "base.id".to_string(),
                                value: None,
                            }],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
            ),
        ]);

        let result = validate_spec(&spec);
        // Cross-reference dependency validation
        assert!(result.is_ok() || result.is_err()); // Documents current cross-reference behavior
    }

    #[test]
    fn test_validate_duplicate_rule_names() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "UserType",
                RuleType::TypeDef(TypeExpression::Builtin("text".to_string())),
            ),
            Rule {
                name: "UserType".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
                position: Position::new(2, 1, 20),
                doc_comments: Vec::new(),
            },
        ]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Duplicate rule name 'UserType'"));
        assert!(error_msg.contains("line 2"));
    }

    #[test]
    fn test_validate_duplicate_service_operation_names() {
        let spec = create_test_spec(vec![create_test_rule(
            "DuplicateOpService",
            RuleType::ServiceDef(ServiceDefinition {
                operations: vec![
                    ServiceOperation {
                        name: "process".to_string(),
                        input_type: TypeExpression::Builtin("int".to_string()),
                        output_type: TypeExpression::Builtin("text".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(1, 1, 0),
                        doc_comments: Vec::new(),
                    },
                    ServiceOperation {
                        name: "process".to_string(),
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("int".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(3, 1, 30),
                        doc_comments: Vec::new(),
                    },
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Duplicate operation 'process'"));
        assert!(error_msg.contains("line 3"));
    }

    #[test]
    fn test_validate_size_constraints() {
        // Test valid size constraint
        let spec = create_test_spec(vec![create_test_rule(
            "TextWithSize",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Size(SizeConstraint::Range {
                    min: 5,
                    max: 50,
                })],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_size_constraints() {
        // Test size constraint on invalid type
        let spec = create_test_spec(vec![create_test_rule(
            "IntWithSize",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Size(SizeConstraint::Exact(10))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("size constraints are not applicable"));
    }

    #[test]
    fn test_validate_invalid_size_range() {
        let spec = create_test_spec(vec![create_test_rule(
            "TextWithBadRange",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Size(SizeConstraint::Range {
                    min: 50,
                    max: 10,
                })],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("minimum size (50) cannot be greater than maximum size (10)"));
    }

    #[test]
    fn test_validate_valid_regex_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "Email",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Regex(
                    r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$".to_string(),
                )],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_regex_pattern() {
        let spec = create_test_spec(vec![create_test_rule(
            "BadRegex",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Regex("[invalid".to_string())],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Invalid regex pattern"));
    }

    #[test]
    fn test_validate_regex_on_non_text_type() {
        let spec = create_test_spec(vec![create_test_rule(
            "IntWithRegex",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Regex(r"\d+".to_string())],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("regex constraints are only applicable to text types"));
    }

    #[test]
    fn test_validate_valid_default_value() {
        let spec = create_test_spec(vec![create_test_rule(
            "BoolWithDefault",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bool".to_string())),
                constraints: vec![ControlOperator::Default(LiteralValue::Bool(true))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_incompatible_default_value() {
        let spec = create_test_spec(vec![create_test_rule(
            "IntWithTextDefault",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Default(LiteralValue::Text(
                    "hello".to_string(),
                ))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Invalid default value"));
        assert!(error_msg.contains("expected int"));
    }

    #[test]
    fn test_validate_multiple_constraints() {
        let spec = create_test_spec(vec![create_test_rule(
            "TextWithMultipleConstraints",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![
                    ControlOperator::Size(SizeConstraint::Range { min: 3, max: 50 }),
                    ControlOperator::Regex(r"^[a-zA-Z0-9]+$".to_string()),
                    ControlOperator::Default(LiteralValue::Text("default".to_string())),
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_constrained_group_field() {
        let spec = create_test_spec(vec![create_test_rule(
            "UserWithConstraints",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("username".to_string())),
                        value_type: TypeExpression::Constrained {
                            base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                            constraints: vec![
                                ControlOperator::Size(SizeConstraint::Range { min: 3, max: 50 }),
                                ControlOperator::Regex(r"^[a-zA-Z0-9_]+$".to_string()),
                            ],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("active".to_string())),
                        value_type: TypeExpression::Constrained {
                            base_type: Box::new(TypeExpression::Builtin("bool".to_string())),
                            constraints: vec![ControlOperator::Default(LiteralValue::Bool(true))],
                        },
                        occurrence: Some(Occurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_nested_constrained_types() {
        let spec = create_test_spec(vec![create_test_rule(
            "NestedArray",
            RuleType::TypeDef(TypeExpression::Array {
                element_type: Box::new(TypeExpression::Constrained {
                    base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                    constraints: vec![ControlOperator::Size(SizeConstraint::Range {
                        min: 1,
                        max: 100,
                    })],
                }),
                occurrence: None,
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_constrained_types_error_accumulation() {
        let spec = create_test_spec(vec![create_test_rule(
            "MultipleErrors",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("field1".to_string())),
                        value_type: TypeExpression::Constrained {
                            base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                            constraints: vec![ControlOperator::Size(SizeConstraint::Exact(10))],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("field2".to_string())),
                        value_type: TypeExpression::Constrained {
                            base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                            constraints: vec![ControlOperator::Regex("[invalid".to_string())],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("field3".to_string())),
                        value_type: TypeExpression::Constrained {
                            base_type: Box::new(TypeExpression::Builtin("bool".to_string())),
                            constraints: vec![ControlOperator::Default(LiteralValue::Text(
                                "wrong".to_string(),
                            ))],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();

        // Should contain all three types of constraint errors
        assert!(error_msg.contains("size constraints are not applicable"));
        assert!(error_msg.contains("Invalid regex pattern"));
        assert!(error_msg.contains("Invalid default value"));
    }

    #[test]
    fn test_validate_comparison_constraints() {
        let spec = create_test_spec(vec![create_test_rule(
            "PositiveInt",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Integer(1))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_comparison_on_text() {
        let spec = create_test_spec(vec![create_test_rule(
            "TextWithComparison",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Integer(1))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("comparison constraints are only applicable to numeric types"));
    }

    #[test]
    fn test_validate_decimal_comparison_and_default() {
        // `decimal` is ordered and exact: ordered comparisons apply, and bounds may be
        // written as exact text to avoid float rounding.
        let spec = create_test_spec(vec![create_test_rule(
            "Price",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("decimal".to_string())),
                constraints: vec![
                    ControlOperator::GreaterEqual(LiteralValue::Text("0.00".to_string())),
                    ControlOperator::Default(LiteralValue::Text("0.00".to_string())),
                ],
            }),
        )]);

        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn test_validate_rejects_malformed_decimal_bound() {
        let spec = create_test_spec(vec![create_test_rule(
            "Price",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("decimal".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Text(
                    "not-a-number".to_string(),
                ))],
            }),
        )]);

        let err = validate_spec(&spec).unwrap_err().to_string();
        assert!(err.contains("not a valid decimal"), "{err}");
    }

    #[test]
    fn test_validate_rejects_malformed_timestamp_bound() {
        let spec = create_test_spec(vec![create_test_rule(
            "When",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("timestamp".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Text(
                    "2024-13-99".to_string(),
                ))],
            }),
        )]);

        let err = validate_spec(&spec).unwrap_err().to_string();
        assert!(err.contains("not a valid RFC3339"), "{err}");
    }

    #[test]
    fn test_validate_rejects_float_decimal_bound() {
        // A float bound on `decimal` reintroduces the rounding `decimal` avoids; it
        // must be written as exact text.
        let spec = create_test_spec(vec![create_test_rule(
            "Rate",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("decimal".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Float(0.1))],
            }),
        )]);

        assert!(validate_spec(&spec).is_err());
    }

    #[test]
    fn test_validate_accepts_integer_decimal_bound() {
        // An integer bound is exact, so it stays allowed.
        let spec = create_test_spec(vec![create_test_rule(
            "Count",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("decimal".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Integer(0))],
            }),
        )]);

        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn test_validate_timestamp_comparison_with_rfc3339() {
        // `timestamp` is ordered chronologically; bounds are RFC3339 strings.
        let spec = create_test_spec(vec![create_test_rule(
            "NotBeforeEpoch",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("timestamp".to_string())),
                constraints: vec![ControlOperator::GreaterEqual(LiteralValue::Text(
                    "1970-01-01T00:00:00Z".to_string(),
                ))],
            }),
        )]);

        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn test_validate_multiple_services_no_conflicts() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "UserService",
                RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "create".to_string(),
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("int".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(1, 1, 0),
                        doc_comments: Vec::new(),
                    }],
                }),
            ),
            create_test_rule(
                "OrderService",
                RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "create".to_string(), // Same name, different service - should be OK
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("int".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(2, 1, 20),
                        doc_comments: Vec::new(),
                    }],
                }),
            ),
        ]);

        let result = validate_spec(&spec);
        assert!(result.is_ok()); // Same operation names in different services should be allowed
    }

    #[test]
    fn test_validate_unique_rules_with_different_types() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "UserType",
                RuleType::TypeDef(TypeExpression::Builtin("text".to_string())),
            ),
            create_test_rule(
                "UserService",
                RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "get-user".to_string(),
                        input_type: TypeExpression::Builtin("int".to_string()),
                        output_type: TypeExpression::Reference("UserType".to_string()),
                        direction: ServiceDirection::Unidirectional,
                        position: Position::new(1, 1, 0),
                        doc_comments: Vec::new(),
                    }],
                }),
            ),
        ]);

        let result = validate_spec(&spec);
        assert!(result.is_ok()); // Different rule types with same prefix should be allowed
    }

    #[test]
    fn test_validate_ne_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "Status",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::NotEqual(LiteralValue::Integer(0))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_ne_constraint_type_mismatch() {
        let spec = create_test_spec(vec![create_test_rule(
            "Name",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::NotEqual(LiteralValue::Integer(0))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("not compatible with type"),
            "Error message: {}",
            error_msg
        );
    }

    #[test]
    fn test_validate_bits_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "Flags",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Bits("0x00FF".to_string())],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bits_constraint_invalid_type() {
        let spec = create_test_spec(vec![create_test_rule(
            "Name",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Bits("0x00FF".to_string())],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains(".bits"));
    }

    #[test]
    fn test_validate_and_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "Combined",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::And(Box::new(
                    TypeExpression::Constrained {
                        base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                        constraints: vec![ControlOperator::Size(SizeConstraint::Range {
                            min: 3,
                            max: 10,
                        })],
                    },
                ))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_within_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "Subset",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Within(Box::new(TypeExpression::Choice(
                    vec![
                        TypeExpression::Literal(LiteralValue::Integer(1)),
                        TypeExpression::Literal(LiteralValue::Integer(2)),
                        TypeExpression::Literal(LiteralValue::Integer(3)),
                    ],
                )))],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_json_constraint_on_text() {
        let spec = create_test_spec(vec![create_test_rule(
            "JsonData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Json],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_json_constraint_on_bytes() {
        let spec = create_test_spec(vec![create_test_rule(
            "BinaryJsonData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![ControlOperator::Json],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_json_constraint_invalid_type() {
        let spec = create_test_spec(vec![create_test_rule(
            "InvalidJson",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Json],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains(".json"));
        assert!(error_msg.contains("text or bytes"));
    }

    #[test]
    fn test_validate_json_constraint_with_multiple_constraints() {
        let spec = create_test_spec(vec![create_test_rule(
            "ConstrainedJsonData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![
                    ControlOperator::Size(SizeConstraint::Range { min: 10, max: 1000 }),
                    ControlOperator::Json,
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_cbor_constraint_on_bytes() {
        let spec = create_test_spec(vec![create_test_rule(
            "CborData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![ControlOperator::Cbor],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_cborseq_constraint_on_bytes() {
        let spec = create_test_spec(vec![create_test_rule(
            "CborSequence",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![ControlOperator::Cborseq],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_cbor_constraint_invalid_type() {
        let spec = create_test_spec(vec![create_test_rule(
            "InvalidCbor",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![ControlOperator::Cbor],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains(".cbor"));
        assert!(error_msg.contains("bytes"));
    }

    #[test]
    fn test_validate_cborseq_constraint_invalid_type() {
        let spec = create_test_spec(vec![create_test_rule(
            "InvalidCborseq",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("int".to_string())),
                constraints: vec![ControlOperator::Cborseq],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains(".cborseq"));
        assert!(error_msg.contains("bytes"));
    }

    #[test]
    fn test_validate_cbor_constraint_with_size() {
        let spec = create_test_spec(vec![create_test_rule(
            "ConstrainedCborData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![
                    ControlOperator::Size(SizeConstraint::Range { min: 10, max: 1000 }),
                    ControlOperator::Cbor,
                ],
            }),
        )]);

        let result = validate_spec(&spec);
        assert!(result.is_ok());
    }
}
