//! Breaking change detection for CSIL specifications

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::for_kv_map)]
#![allow(clippy::ptr_arg)]

use crate::ast::{
    CsilSpec, FieldMetadata, FieldVisibility, GroupEntry, GroupExpression, GroupKey, LiteralValue,
    Occurrence, Rule, RuleType, ServiceDefinition, ServiceOperation, TypeExpression,
    ValidationConstraint,
};
use anyhow::Result;
use std::collections::HashMap;

/// Represents different types of breaking changes that can occur between CSIL versions
#[derive(Debug, Clone, PartialEq)]
pub enum BreakingChange {
    /// A rule was removed
    RuleRemoved { name: String },
    /// A rule type was changed
    RuleTypeChanged {
        name: String,
        old_type: String,
        new_type: String,
    },
    /// A required field was removed from a group
    RequiredFieldRemoved {
        rule_name: String,
        field_name: String,
    },
    /// A field type was changed incompatibly
    FieldTypeChanged {
        rule_name: String,
        field_name: String,
        old_type: String,
        new_type: String,
    },
    /// An optional field was made required
    FieldMadeRequired {
        rule_name: String,
        field_name: String,
    },
    /// Service operation was removed
    ServiceOperationRemoved {
        service_name: String,
        operation_name: String,
    },
    /// Service operation signature changed
    ServiceOperationSignatureChanged {
        service_name: String,
        operation_name: String,
        change_description: String,
    },
    /// Field visibility changed in a breaking way
    FieldVisibilityChanged {
        rule_name: String,
        field_name: String,
        old_visibility: String,
        new_visibility: String,
    },
    /// Field dependency changed
    FieldDependencyChanged {
        rule_name: String,
        field_name: String,
        change_description: String,
    },
    /// Field constraint changed in breaking way
    FieldConstraintChanged {
        rule_name: String,
        field_name: String,
        change_description: String,
    },
}

/// Result of breaking change analysis
#[derive(Debug)]
pub struct BreakingChangeReport {
    pub breaking_changes: Vec<BreakingChange>,
    pub non_breaking_changes: Vec<String>,
    pub has_breaking_changes: bool,
}

/// Check for breaking changes between two CSIL specifications
pub fn detect_breaking_changes(current: &CsilSpec, new: &CsilSpec) -> Result<BreakingChangeReport> {
    let mut breaking_changes = Vec::new();
    let mut non_breaking_changes = Vec::new();

    // Build name-indexed maps for efficient lookup
    let current_rules: HashMap<_, _> = current.rules.iter().map(|r| (&r.name, r)).collect();
    let new_rules: HashMap<_, _> = new.rules.iter().map(|r| (&r.name, r)).collect();

    // Check for removed rules
    for name in current_rules.keys() {
        if !new_rules.contains_key(name) {
            breaking_changes.push(BreakingChange::RuleRemoved {
                name: name.to_string(),
            });
        }
    }

    // Check for rule changes
    for (name, current_rule) in &current_rules {
        if let Some(new_rule) = new_rules.get(name) {
            compare_rules(
                &mut breaking_changes,
                &mut non_breaking_changes,
                current_rule,
                new_rule,
            );
        }
    }

    // Check for new rules (non-breaking)
    for name in new_rules.keys() {
        if !current_rules.contains_key(name) {
            non_breaking_changes.push(format!("Added new rule: {name}"));
        }
    }

    Ok(BreakingChangeReport {
        has_breaking_changes: !breaking_changes.is_empty(),
        breaking_changes,
        non_breaking_changes,
    })
}

/// Compare two rules for breaking changes
fn compare_rules(
    breaking_changes: &mut Vec<BreakingChange>,
    non_breaking_changes: &mut Vec<String>,
    current: &Rule,
    new: &Rule,
) {
    // Check for rule type changes
    match (&current.rule_type, &new.rule_type) {
        (RuleType::TypeDef(old_type), RuleType::TypeDef(new_type)) => {
            compare_type_expressions(
                breaking_changes,
                non_breaking_changes,
                &current.name,
                old_type,
                new_type,
            );
        }
        (RuleType::GroupDef(old_group), RuleType::GroupDef(new_group)) => {
            compare_group_expressions(
                breaking_changes,
                non_breaking_changes,
                &current.name,
                old_group,
                new_group,
            );
        }
        (RuleType::ServiceDef(old_service), RuleType::ServiceDef(new_service)) => {
            compare_service_definitions(
                breaking_changes,
                non_breaking_changes,
                &current.name,
                old_service,
                new_service,
            );
        }
        (RuleType::TypeChoice(old_choices), RuleType::TypeChoice(new_choices)) => {
            compare_type_choices(
                breaking_changes,
                non_breaking_changes,
                &current.name,
                old_choices,
                new_choices,
            );
        }
        (RuleType::GroupChoice(old_choices), RuleType::GroupChoice(new_choices)) => {
            compare_group_choices(
                breaking_changes,
                non_breaking_changes,
                &current.name,
                old_choices,
                new_choices,
            );
        }
        // Different rule types are breaking
        _ => {
            if std::mem::discriminant(&current.rule_type) != std::mem::discriminant(&new.rule_type)
            {
                breaking_changes.push(BreakingChange::RuleTypeChanged {
                    name: current.name.clone(),
                    old_type: rule_type_name(&current.rule_type),
                    new_type: rule_type_name(&new.rule_type),
                });
            }
        }
    }
}

/// Compare two type expressions for breaking changes
fn compare_type_expressions(
    breaking_changes: &mut Vec<BreakingChange>,
    _non_breaking_changes: &mut Vec<String>,
    rule_name: &str,
    old_type: &TypeExpression,
    new_type: &TypeExpression,
) {
    if !are_type_expressions_compatible(old_type, new_type) {
        breaking_changes.push(BreakingChange::FieldTypeChanged {
            rule_name: rule_name.to_string(),
            field_name: "root".to_string(),
            old_type: format!("{old_type:?}"),
            new_type: format!("{new_type:?}"),
        });
    }
}

/// Compare two group expressions for breaking changes
fn compare_group_expressions(
    breaking_changes: &mut Vec<BreakingChange>,
    non_breaking_changes: &mut Vec<String>,
    rule_name: &str,
    old_group: &GroupExpression,
    new_group: &GroupExpression,
) {
    // Build field maps for comparison
    let old_fields = build_field_map(&old_group.entries);
    let new_fields = build_field_map(&new_group.entries);

    // Check for removed required fields
    for (field_name, old_entry) in &old_fields {
        match new_fields.get(field_name) {
            None => {
                // Field was removed - check if it was required
                if is_field_required(&old_entry.occurrence) {
                    breaking_changes.push(BreakingChange::RequiredFieldRemoved {
                        rule_name: rule_name.to_string(),
                        field_name: field_name.clone(),
                    });
                } else {
                    non_breaking_changes.push(format!(
                        "Removed optional field '{}' from rule '{}'",
                        field_name, rule_name
                    ));
                }
            }
            Some(new_entry) => {
                // Field exists in both - check for changes
                compare_field_entries(
                    breaking_changes,
                    non_breaking_changes,
                    rule_name,
                    field_name,
                    old_entry,
                    new_entry,
                );
            }
        }
    }

    // Check for new fields (generally non-breaking if optional)
    for (field_name, new_entry) in &new_fields {
        if !old_fields.contains_key(field_name) {
            if is_field_required(&new_entry.occurrence) {
                breaking_changes.push(BreakingChange::FieldMadeRequired {
                    rule_name: rule_name.to_string(),
                    field_name: field_name.clone(),
                });
            } else {
                non_breaking_changes.push(format!(
                    "Added new optional field '{}' to rule '{}'",
                    field_name, rule_name
                ));
            }
        }
    }
}

/// Compare two service definitions for breaking changes
fn compare_service_definitions(
    breaking_changes: &mut Vec<BreakingChange>,
    non_breaking_changes: &mut Vec<String>,
    service_name: &str,
    old_service: &ServiceDefinition,
    new_service: &ServiceDefinition,
) {
    // Build operation maps
    let old_ops: HashMap<_, _> = old_service
        .operations
        .iter()
        .map(|op| (&op.name, op))
        .collect();
    let new_ops: HashMap<_, _> = new_service
        .operations
        .iter()
        .map(|op| (&op.name, op))
        .collect();

    // Check for removed operations
    for (op_name, _) in &old_ops {
        if !new_ops.contains_key(op_name) {
            breaking_changes.push(BreakingChange::ServiceOperationRemoved {
                service_name: service_name.to_string(),
                operation_name: op_name.to_string(),
            });
        }
    }

    // Check for operation signature changes
    for (op_name, old_op) in &old_ops {
        if let Some(new_op) = new_ops.get(op_name) {
            compare_service_operations(
                breaking_changes,
                non_breaking_changes,
                service_name,
                old_op,
                new_op,
            );
        }
    }

    // Check for new operations (non-breaking)
    for (op_name, _) in &new_ops {
        if !old_ops.contains_key(op_name) {
            non_breaking_changes.push(format!(
                "Added new operation '{}' to service '{}'",
                op_name, service_name
            ));
        }
    }
}

/// Compare two service operations for breaking changes
fn compare_service_operations(
    breaking_changes: &mut Vec<BreakingChange>,
    _non_breaking_changes: &mut Vec<String>,
    service_name: &str,
    old_op: &ServiceOperation,
    new_op: &ServiceOperation,
) {
    // Check input type compatibility
    if !are_type_expressions_compatible(&old_op.input_type, &new_op.input_type) {
        breaking_changes.push(BreakingChange::ServiceOperationSignatureChanged {
            service_name: service_name.to_string(),
            operation_name: old_op.name.clone(),
            change_description: format!(
                "Input type changed from {:?} to {:?}",
                old_op.input_type, new_op.input_type
            ),
        });
    }

    // Check output type compatibility
    if !are_type_expressions_compatible(&old_op.output_type, &new_op.output_type) {
        breaking_changes.push(BreakingChange::ServiceOperationSignatureChanged {
            service_name: service_name.to_string(),
            operation_name: old_op.name.clone(),
            change_description: format!(
                "Output type changed from {:?} to {:?}",
                old_op.output_type, new_op.output_type
            ),
        });
    }

    // Check direction compatibility
    if old_op.direction != new_op.direction {
        breaking_changes.push(BreakingChange::ServiceOperationSignatureChanged {
            service_name: service_name.to_string(),
            operation_name: old_op.name.clone(),
            change_description: format!(
                "Direction changed from {:?} to {:?}",
                old_op.direction, new_op.direction
            ),
        });
    }
}

/// Compare two field entries for breaking changes
fn compare_field_entries(
    breaking_changes: &mut Vec<BreakingChange>,
    non_breaking_changes: &mut Vec<String>,
    rule_name: &str,
    field_name: &str,
    old_entry: &GroupEntry,
    new_entry: &GroupEntry,
) {
    // Check type compatibility
    if !are_type_expressions_compatible(&old_entry.value_type, &new_entry.value_type) {
        breaking_changes.push(BreakingChange::FieldTypeChanged {
            rule_name: rule_name.to_string(),
            field_name: field_name.to_string(),
            old_type: format!("{:?}", old_entry.value_type),
            new_type: format!("{:?}", new_entry.value_type),
        });
    }

    // Check occurrence changes
    let old_required = is_field_required(&old_entry.occurrence);
    let new_required = is_field_required(&new_entry.occurrence);

    if !old_required && new_required {
        breaking_changes.push(BreakingChange::FieldMadeRequired {
            rule_name: rule_name.to_string(),
            field_name: field_name.to_string(),
        });
    } else if old_required && !new_required {
        non_breaking_changes.push(format!(
            "Field '{}' in rule '{}' changed from required to optional",
            field_name, rule_name
        ));
    }

    // Check metadata changes
    compare_field_metadata(
        breaking_changes,
        non_breaking_changes,
        rule_name,
        field_name,
        &old_entry.metadata,
        &new_entry.metadata,
    );
}

/// Compare field metadata for breaking changes
fn compare_field_metadata(
    breaking_changes: &mut Vec<BreakingChange>,
    non_breaking_changes: &mut Vec<String>,
    rule_name: &str,
    field_name: &str,
    old_metadata: &[FieldMetadata],
    new_metadata: &[FieldMetadata],
) {
    let old_visibility = extract_visibility(old_metadata);
    let new_visibility = extract_visibility(new_metadata);

    // Check visibility changes
    if old_visibility != new_visibility {
        // Some visibility changes are breaking
        let is_breaking = matches!(
            (&old_visibility, &new_visibility),
            (
                Some(FieldVisibility::Bidirectional),
                Some(FieldVisibility::SendOnly)
            ) | (
                Some(FieldVisibility::Bidirectional),
                Some(FieldVisibility::ReceiveOnly)
            ) | (
                Some(FieldVisibility::SendOnly),
                Some(FieldVisibility::ReceiveOnly)
            ) | (
                Some(FieldVisibility::ReceiveOnly),
                Some(FieldVisibility::SendOnly)
            )
        );

        if is_breaking {
            breaking_changes.push(BreakingChange::FieldVisibilityChanged {
                rule_name: rule_name.to_string(),
                field_name: field_name.to_string(),
                old_visibility: format!("{:?}", old_visibility),
                new_visibility: format!("{:?}", new_visibility),
            });
        } else {
            non_breaking_changes.push(format!(
                "Field '{}' in rule '{}' visibility changed from {:?} to {:?}",
                field_name, rule_name, old_visibility, new_visibility
            ));
        }
    }

    // Check dependency changes
    let old_deps = extract_dependencies(old_metadata);
    let new_deps = extract_dependencies(new_metadata);
    if old_deps != new_deps {
        breaking_changes.push(BreakingChange::FieldDependencyChanged {
            rule_name: rule_name.to_string(),
            field_name: field_name.to_string(),
            change_description: format!(
                "Dependencies changed from {:?} to {:?}",
                old_deps, new_deps
            ),
        });
    }

    // Check constraint changes
    let old_constraints = extract_constraints(old_metadata);
    let new_constraints = extract_constraints(new_metadata);
    if are_constraints_more_restrictive(&old_constraints, &new_constraints) {
        breaking_changes.push(BreakingChange::FieldConstraintChanged {
            rule_name: rule_name.to_string(),
            field_name: field_name.to_string(),
            change_description: format!(
                "Constraints became more restrictive: {:?} to {:?}",
                old_constraints, new_constraints
            ),
        });
    }
}

/// Helper functions for the breaking change analysis
///
/// Get a human-readable name for a rule type
fn rule_type_name(rule_type: &RuleType) -> String {
    match rule_type {
        RuleType::TypeDef(_) => "TypeDef".to_string(),
        RuleType::GroupDef(_) => "GroupDef".to_string(),
        RuleType::TypeChoice(_) => "TypeChoice".to_string(),
        RuleType::GroupChoice(_) => "GroupChoice".to_string(),
        RuleType::ServiceDef(_) => "ServiceDef".to_string(),
    }
}

/// Compare type choices for breaking changes
fn compare_type_choices(
    breaking_changes: &mut Vec<BreakingChange>,
    non_breaking_changes: &mut Vec<String>,
    rule_name: &str,
    old_choices: &[TypeExpression],
    new_choices: &[TypeExpression],
) {
    // Check if any old choices were removed (breaking)
    for old_choice in old_choices {
        if !new_choices
            .iter()
            .any(|new_choice| are_type_expressions_compatible(old_choice, new_choice))
        {
            breaking_changes.push(BreakingChange::FieldTypeChanged {
                rule_name: rule_name.to_string(),
                field_name: "choice".to_string(),
                old_type: format!("{:?}", old_choice),
                new_type: "removed".to_string(),
            });
        }
    }

    // Check for new choices (non-breaking)
    for new_choice in new_choices {
        if !old_choices
            .iter()
            .any(|old_choice| are_type_expressions_compatible(old_choice, new_choice))
        {
            non_breaking_changes.push(format!(
                "Added new choice {:?} to rule '{}'",
                new_choice, rule_name
            ));
        }
    }
}

/// Compare group choices for breaking changes
fn compare_group_choices(
    breaking_changes: &mut Vec<BreakingChange>,
    _non_breaking_changes: &mut Vec<String>,
    rule_name: &str,
    old_choices: &[GroupExpression],
    new_choices: &[GroupExpression],
) {
    // Similar logic to type choices but for group expressions
    if old_choices.len() != new_choices.len() {
        breaking_changes.push(BreakingChange::RuleTypeChanged {
            name: rule_name.to_string(),
            old_type: format!("GroupChoice with {} options", old_choices.len()),
            new_type: format!("GroupChoice with {} options", new_choices.len()),
        });
    }
}

/// Build a map of field names to entries from a group
fn build_field_map(entries: &[GroupEntry]) -> HashMap<String, &GroupEntry> {
    let mut map = HashMap::new();
    for entry in entries {
        if let Some(key) = &entry.key {
            let field_name = match key {
                GroupKey::Bare(name) => name.clone(),
                GroupKey::Type(type_expr) => format!("{:?}", type_expr),
                GroupKey::Literal(literal) => format!("{:?}", literal),
            };
            map.insert(field_name, entry);
        }
    }
    map
}

/// Check if a field is required based on its occurrence
fn is_field_required(occurrence: &Option<Occurrence>) -> bool {
    match occurrence {
        None => true, // Default is required
        Some(Occurrence::Optional) => false,
        Some(Occurrence::ZeroOrMore) => false,
        Some(Occurrence::OneOrMore) => true,
        Some(Occurrence::Exact(n)) => *n > 0,
        Some(Occurrence::Range { min, max: _ }) => min.unwrap_or(1) > 0,
    }
}

/// Check if two type expressions are compatible (new can substitute for old)
fn are_type_expressions_compatible(old: &TypeExpression, new: &TypeExpression) -> bool {
    match (old, new) {
        // Same builtin types are compatible
        (TypeExpression::Builtin(old_name), TypeExpression::Builtin(new_name)) => {
            old_name == new_name
        }

        // Same references are compatible
        (TypeExpression::Reference(old_ref), TypeExpression::Reference(new_ref)) => {
            old_ref == new_ref
        }

        // Arrays are compatible if element types are compatible
        (
            TypeExpression::Array {
                element_type: old_elem,
                occurrence: old_occ,
            },
            TypeExpression::Array {
                element_type: new_elem,
                occurrence: new_occ,
            },
        ) => {
            are_type_expressions_compatible(old_elem, new_elem)
                && are_occurrences_compatible(old_occ, new_occ)
        }

        // Maps are compatible if both key and value types are compatible
        (
            TypeExpression::Map {
                key: old_key,
                value: old_value,
                occurrence: old_occ,
            },
            TypeExpression::Map {
                key: new_key,
                value: new_value,
                occurrence: new_occ,
            },
        ) => {
            are_type_expressions_compatible(old_key, new_key)
                && are_type_expressions_compatible(old_value, new_value)
                && are_occurrences_compatible(old_occ, new_occ)
        }

        // Choices are compatible if all old choices have compatible new choices
        (TypeExpression::Choice(old_choices), TypeExpression::Choice(new_choices)) => {
            old_choices.iter().all(|old_choice| {
                new_choices
                    .iter()
                    .any(|new_choice| are_type_expressions_compatible(old_choice, new_choice))
            })
        }

        // Literals must be exactly equal
        (TypeExpression::Literal(old_lit), TypeExpression::Literal(new_lit)) => old_lit == new_lit,

        // Different expression types are generally incompatible
        _ => false,
    }
}

/// Check if occurrences are compatible (new allows at least what old allowed)
fn are_occurrences_compatible(old: &Option<Occurrence>, new: &Option<Occurrence>) -> bool {
    match (old, new) {
        (None, None) => true,                                 // Both required
        (Some(Occurrence::Optional), _) => true,              // Optional can become anything
        (None, Some(Occurrence::Optional)) => false,          // Required can't become optional
        (Some(old_occ), Some(new_occ)) => old_occ == new_occ, // Must be same
        _ => false,
    }
}

/// Extract visibility from field metadata
fn extract_visibility(metadata: &[FieldMetadata]) -> Option<FieldVisibility> {
    for meta in metadata {
        if let FieldMetadata::Visibility(visibility) = meta {
            return Some(visibility.clone());
        }
    }
    None
}

/// Extract dependencies from field metadata
fn extract_dependencies(metadata: &[FieldMetadata]) -> Vec<String> {
    let mut deps = Vec::new();
    for meta in metadata {
        if let FieldMetadata::DependsOn { field, value } = meta {
            let dep_str = if let Some(val) = value {
                format!("{}={:?}", field, val)
            } else {
                field.clone()
            };
            deps.push(dep_str);
        }
    }
    deps
}

/// Extract constraints from field metadata
fn extract_constraints(metadata: &[FieldMetadata]) -> Vec<ValidationConstraint> {
    let mut constraints = Vec::new();
    for meta in metadata {
        if let FieldMetadata::Constraint(constraint) = meta {
            constraints.push(constraint.clone());
        }
    }
    constraints
}

/// Check if new constraints are more restrictive than old constraints
fn are_constraints_more_restrictive(
    old: &[ValidationConstraint],
    new: &[ValidationConstraint],
) -> bool {
    // Check if any new constraints are more restrictive
    for new_constraint in new {
        let is_more_restrictive = match new_constraint {
            ValidationConstraint::MinLength(new_min) => {
                // More restrictive if new minimum is higher
                old.iter().any(|old_constraint| {
                    if let ValidationConstraint::MinLength(old_min) = old_constraint {
                        new_min > old_min
                    } else {
                        false
                    }
                })
            }
            ValidationConstraint::MaxLength(new_max) => {
                // More restrictive if new maximum is lower
                old.iter().any(|old_constraint| {
                    if let ValidationConstraint::MaxLength(old_max) = old_constraint {
                        new_max < old_max
                    } else {
                        false
                    }
                })
            }
            ValidationConstraint::MinItems(new_min) => old.iter().any(|old_constraint| {
                if let ValidationConstraint::MinItems(old_min) = old_constraint {
                    new_min > old_min
                } else {
                    false
                }
            }),
            ValidationConstraint::MaxItems(new_max) => old.iter().any(|old_constraint| {
                if let ValidationConstraint::MaxItems(old_max) = old_constraint {
                    new_max < old_max
                } else {
                    false
                }
            }),
            ValidationConstraint::MinValue(new_min) => {
                // More restrictive if new minimum value is higher
                old.iter().any(|old_constraint| {
                    if let ValidationConstraint::MinValue(old_min) = old_constraint {
                        match (old_min, new_min) {
                            (LiteralValue::Integer(old), LiteralValue::Integer(new)) => new > old,
                            (LiteralValue::Float(old), LiteralValue::Float(new)) => new > old,
                            _ => false,
                        }
                    } else {
                        false
                    }
                })
            }
            ValidationConstraint::MaxValue(new_max) => {
                // More restrictive if new maximum value is lower
                old.iter().any(|old_constraint| {
                    if let ValidationConstraint::MaxValue(old_max) = old_constraint {
                        match (old_max, new_max) {
                            (LiteralValue::Integer(old), LiteralValue::Integer(new)) => new < old,
                            (LiteralValue::Float(old), LiteralValue::Float(new)) => new < old,
                            _ => false,
                        }
                    } else {
                        false
                    }
                })
            }
            ValidationConstraint::Custom { name: _, value: _ } => {
                // Custom constraints are considered breaking if they're new
                !old.iter().any(|old_constraint| {
                    matches!(old_constraint, ValidationConstraint::Custom { .. })
                        && old_constraint == new_constraint
                })
            }
        };

        if is_more_restrictive {
            return true;
        }
    }

    false
}

/// Check for breaking changes between two CSIL files
pub fn detect_breaking_changes_from_files<P: AsRef<std::path::Path>>(
    current_path: P,
    new_path: P,
) -> Result<BreakingChangeReport> {
    let current_spec = crate::parse_csil_file(current_path)?;
    let new_spec = crate::parse_csil_file(new_path)?;
    detect_breaking_changes(&current_spec, &new_spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        CsilSpec, FieldMetadata, FieldVisibility, GroupEntry, GroupExpression, GroupKey,
        LiteralValue, Occurrence, Rule, RuleType, ServiceDefinition, ServiceDirection,
        ServiceOperation, TypeExpression, ValidationConstraint,
    };

    fn create_test_spec(rules: Vec<Rule>) -> CsilSpec {
        CsilSpec {
            imports: Vec::new(),
            options: None,
            rules,
        }
    }

    fn create_test_rule(name: &str, rule_type: RuleType) -> Rule {
        Rule {
            name: name.to_string(),
            rule_type,
            position: crate::lexer::Position::new(1, 1, 0),
        }
    }

    fn create_test_group_entry(
        key: Option<GroupKey>,
        value_type: TypeExpression,
        occurrence: Option<Occurrence>,
        metadata: Vec<FieldMetadata>,
    ) -> GroupEntry {
        GroupEntry {
            key,
            value_type,
            occurrence,
            metadata,
        }
    }

    fn create_test_service_operation(
        name: &str,
        input_type: TypeExpression,
        output_type: TypeExpression,
    ) -> ServiceOperation {
        ServiceOperation {
            name: name.to_string(),
            input_type,
            output_type,
            direction: ServiceDirection::Unidirectional,
            position: crate::lexer::Position::new(1, 1, 0),
        }
    }

    #[test]
    fn test_no_breaking_changes_identical_specs() {
        let spec1 = create_test_spec(vec![create_test_rule(
            "rule1",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);
        let spec2 = spec1.clone();

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(!result.has_breaking_changes);
        assert!(result.breaking_changes.is_empty());
    }

    #[test]
    fn test_breaking_change_rule_removed() {
        let spec1 = create_test_spec(vec![
            create_test_rule(
                "rule1",
                RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
            ),
            create_test_rule(
                "rule2",
                RuleType::TypeDef(TypeExpression::Builtin("text".to_string())),
            ),
        ]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "rule1",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);
        assert_eq!(result.breaking_changes.len(), 1);
        match &result.breaking_changes[0] {
            BreakingChange::RuleRemoved { name } => assert_eq!(name, "rule2"),
            _ => panic!("Expected RuleRemoved breaking change"),
        }
    }

    #[test]
    fn test_non_breaking_change_rule_added() {
        let spec1 = create_test_spec(vec![create_test_rule(
            "rule1",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);
        let spec2 = create_test_spec(vec![
            create_test_rule(
                "rule1",
                RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
            ),
            create_test_rule(
                "rule2",
                RuleType::TypeDef(TypeExpression::Builtin("text".to_string())),
            ),
        ]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(!result.has_breaking_changes);
        assert_eq!(result.non_breaking_changes.len(), 1);
        assert!(result.non_breaking_changes[0].contains("rule2"));
    }

    #[test]
    fn test_breaking_change_service_operation_removed() {
        let old_service = ServiceDefinition {
            operations: vec![
                create_test_service_operation(
                    "get_user",
                    TypeExpression::Builtin("int".to_string()),
                    TypeExpression::Builtin("text".to_string()),
                ),
                create_test_service_operation(
                    "delete_user",
                    TypeExpression::Builtin("int".to_string()),
                    TypeExpression::Builtin("bool".to_string()),
                ),
            ],
        };

        let new_service = ServiceDefinition {
            operations: vec![create_test_service_operation(
                "get_user",
                TypeExpression::Builtin("int".to_string()),
                TypeExpression::Builtin("text".to_string()),
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "UserService",
            RuleType::ServiceDef(old_service),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "UserService",
            RuleType::ServiceDef(new_service),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);
        assert_eq!(result.breaking_changes.len(), 1);
        match &result.breaking_changes[0] {
            BreakingChange::ServiceOperationRemoved {
                service_name,
                operation_name,
            } => {
                assert_eq!(service_name, "UserService");
                assert_eq!(operation_name, "delete_user");
            }
            _ => panic!(
                "Expected ServiceOperationRemoved breaking change, got {:?}",
                result.breaking_changes[0]
            ),
        }
    }

    #[test]
    fn test_breaking_change_field_removed() {
        let old_group = GroupExpression {
            entries: vec![
                create_test_group_entry(
                    Some(GroupKey::Bare("required_field".to_string())),
                    TypeExpression::Builtin("int".to_string()),
                    None, // Required by default
                    vec![],
                ),
                create_test_group_entry(
                    Some(GroupKey::Bare("optional_field".to_string())),
                    TypeExpression::Builtin("text".to_string()),
                    Some(Occurrence::Optional),
                    vec![],
                ),
            ],
        };

        let new_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("optional_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                Some(Occurrence::Optional),
                vec![],
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(old_group),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(new_group),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);

        // Should have one breaking change for required field removal
        let required_field_removed = result.breaking_changes.iter().any(|change| {
            matches!(change, BreakingChange::RequiredFieldRemoved { rule_name, field_name }
                if rule_name == "TestGroup" && field_name == "required_field")
        });
        assert!(
            required_field_removed,
            "Expected required field removal to be breaking"
        );
    }

    #[test]
    fn test_breaking_change_field_visibility_changed() {
        let old_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("test_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                None,
                vec![FieldMetadata::Visibility(FieldVisibility::Bidirectional)],
            )],
        };

        let new_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("test_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                None,
                vec![FieldMetadata::Visibility(FieldVisibility::SendOnly)],
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(old_group),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(new_group),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);

        let visibility_changed = result.breaking_changes.iter().any(|change| {
            matches!(change, BreakingChange::FieldVisibilityChanged {
                rule_name, field_name, old_visibility: _, new_visibility: _
            } if rule_name == "TestGroup" && field_name == "test_field")
        });
        assert!(
            visibility_changed,
            "Expected field visibility change to be breaking"
        );
    }

    #[test]
    fn test_breaking_change_constraint_more_restrictive() {
        let old_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("text_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                None,
                vec![FieldMetadata::Constraint(ValidationConstraint::MaxLength(
                    100,
                ))],
            )],
        };

        let new_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("text_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                None,
                vec![FieldMetadata::Constraint(ValidationConstraint::MaxLength(
                    50,
                ))],
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(old_group),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(new_group),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);

        let constraint_changed = result.breaking_changes.iter().any(|change| {
            matches!(change, BreakingChange::FieldConstraintChanged {
                rule_name, field_name, change_description: _
            } if rule_name == "TestGroup" && field_name == "text_field")
        });
        assert!(
            constraint_changed,
            "Expected more restrictive constraint to be breaking"
        );
    }

    #[test]
    fn test_non_breaking_change_field_made_optional() {
        let old_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("test_field".to_string())),
                TypeExpression::Builtin("int".to_string()),
                None, // Required by default
                vec![],
            )],
        };

        let new_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("test_field".to_string())),
                TypeExpression::Builtin("int".to_string()),
                Some(Occurrence::Optional),
                vec![],
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(old_group),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(new_group),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(!result.has_breaking_changes);
        assert!(!result.non_breaking_changes.is_empty());
    }

    #[test]
    fn test_empty_specs() {
        let spec1 = create_test_spec(vec![]);
        let spec2 = create_test_spec(vec![]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(!result.has_breaking_changes);
        assert!(result.breaking_changes.is_empty());
        assert!(result.non_breaking_changes.is_empty());
    }

    #[test]
    fn test_service_operation_signature_changed() {
        let old_service = ServiceDefinition {
            operations: vec![create_test_service_operation(
                "get_user",
                TypeExpression::Builtin("int".to_string()),
                TypeExpression::Builtin("text".to_string()),
            )],
        };

        let new_service = ServiceDefinition {
            operations: vec![create_test_service_operation(
                "get_user",
                TypeExpression::Builtin("text".to_string()), // Changed input type
                TypeExpression::Builtin("text".to_string()),
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "UserService",
            RuleType::ServiceDef(old_service),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "UserService",
            RuleType::ServiceDef(new_service),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);

        let signature_changed = result.breaking_changes.iter().any(|change| {
            matches!(change, BreakingChange::ServiceOperationSignatureChanged {
                service_name, operation_name, change_description: _
            } if service_name == "UserService" && operation_name == "get_user")
        });
        assert!(
            signature_changed,
            "Expected service operation signature change to be breaking"
        );
    }

    #[test]
    fn test_field_dependency_changed() {
        let old_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("dependent_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                None,
                vec![FieldMetadata::DependsOn {
                    field: "other_field".to_string(),
                    value: Some(LiteralValue::Bool(true)),
                }],
            )],
        };

        let new_group = GroupExpression {
            entries: vec![create_test_group_entry(
                Some(GroupKey::Bare("dependent_field".to_string())),
                TypeExpression::Builtin("text".to_string()),
                None,
                vec![FieldMetadata::DependsOn {
                    field: "other_field".to_string(),
                    value: Some(LiteralValue::Bool(false)), // Changed dependency condition
                }],
            )],
        };

        let spec1 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(old_group),
        )]);
        let spec2 = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(new_group),
        )]);

        let result = detect_breaking_changes(&spec1, &spec2).unwrap();
        assert!(result.has_breaking_changes);

        let dependency_changed = result.breaking_changes.iter().any(|change| {
            matches!(change, BreakingChange::FieldDependencyChanged {
                rule_name, field_name, change_description: _
            } if rule_name == "TestGroup" && field_name == "dependent_field")
        });
        assert!(
            dependency_changed,
            "Expected field dependency change to be breaking"
        );
    }
}
