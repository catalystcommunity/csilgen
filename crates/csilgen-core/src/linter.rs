//! CSIL linting functionality for style and best practice enforcement

use crate::ast::{
    CsilSpec, FieldMetadata, GroupEntry, GroupExpression, Rule, RuleType, TypeExpression,
    ValidationConstraint,
};
use anyhow::Result;
use std::path::Path;

/// Different severity levels for lint issues
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
}

/// A linting issue found in CSIL code
#[derive(Debug, Clone)]
pub struct LintIssue {
    pub severity: LintSeverity,
    pub rule_name: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub auto_fixable: bool,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub fix: Option<AutoFix>,
}

/// Automatic fix suggestion
#[derive(Debug, Clone)]
pub struct AutoFix {
    pub description: String,
    pub old_text: String,
    pub new_text: String,
}

pub const TEXT_ENUM_LINT_OPTION: &str = "lint_text_enums";

/// Configuration for CSIL linting
#[derive(Debug, Clone)]
pub struct LintConfig {
    /// Whether to check type, field, and operation naming conventions
    pub enforce_snake_case: bool,
    /// Optional severity for enums that transmit text labels.
    pub text_enum_severity: Option<LintSeverity>,
    /// Whether to require documentation comments
    pub require_docs: bool,
    /// Whether to warn on `;;;` documentation comments (a CSIL extension beyond CDDL)
    pub warn_doc_comments: bool,
    /// Whether to warn on degenerate constraints (e.g. `min-items(0)`/`min-length(0)`,
    /// which are always satisfied)
    pub warn_degenerate_constraints: bool,
    /// Maximum rule name length
    pub max_rule_name_length: usize,
    /// Whether to disallow unused rules
    pub disallow_unused_rules: bool,
    /// Whether to require consistent metadata usage patterns
    pub enforce_consistent_metadata: bool,
    /// Whether to check for incomplete service definitions
    pub check_incomplete_services: bool,
    /// Whether to suggest improvements for overly complex structures
    pub check_complexity: bool,
    /// Maximum allowed nested depth in type definitions
    pub max_nesting_depth: usize,
    /// Maximum number of fields in a group
    pub max_group_fields: usize,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            enforce_snake_case: true,
            text_enum_severity: Some(LintSeverity::Warning),
            require_docs: false,
            warn_doc_comments: true,
            warn_degenerate_constraints: true,
            max_rule_name_length: 64,
            disallow_unused_rules: false,
            enforce_consistent_metadata: true,
            check_incomplete_services: true,
            check_complexity: true,
            max_nesting_depth: 5,
            max_group_fields: 20,
        }
    }
}

/// Result of linting operation
#[derive(Debug)]
pub struct LintResult {
    pub issues: Vec<LintIssue>,
    pub error_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
}

impl LintResult {
    fn new() -> Self {
        Self {
            issues: Vec::new(),
            error_count: 0,
            warning_count: 0,
            info_count: 0,
        }
    }

    fn add_simple_issue(&mut self, severity: LintSeverity, rule_name: String, message: String) {
        self.add_issue(LintIssue {
            severity,
            rule_name,
            message,
            suggestion: None,
            auto_fixable: false,
            line: None,
            column: None,
            fix: None,
        });
    }

    fn add_issue_with_suggestion(
        &mut self,
        severity: LintSeverity,
        rule_name: String,
        message: String,
        suggestion: String,
        auto_fixable: bool,
    ) {
        self.add_issue(LintIssue {
            severity,
            rule_name,
            message,
            suggestion: Some(suggestion),
            auto_fixable,
            line: None,
            column: None,
            fix: None,
        });
    }

    fn add_issue(&mut self, issue: LintIssue) {
        match issue.severity {
            LintSeverity::Error => self.error_count += 1,
            LintSeverity::Warning => self.warning_count += 1,
            LintSeverity::Info => self.info_count += 1,
        }
        self.issues.push(issue);
    }

    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }
}

/// Resolve each policy against the options of the file that owns the rule.
struct FileLintConfig<'a> {
    base: &'a LintConfig,
    rule_names: Option<LintSeverity>,
    field_names: Option<LintSeverity>,
    operation_names: Option<LintSeverity>,
    text_enums: Option<LintSeverity>,
    unused_rules: Option<LintSeverity>,
    doc_comments: Option<LintSeverity>,
    rule_length: Option<LintSeverity>,
    zero_minimum: Option<LintSeverity>,
    empty_service: Option<LintSeverity>,
    nesting_depth: Option<LintSeverity>,
}

impl<'a> FileLintConfig<'a> {
    fn new(spec: &CsilSpec, base: &'a LintConfig, result: &mut LintResult) -> Self {
        let mut policy = |key: &str, default| {
            let mut entries = spec
                .options
                .iter()
                .flat_map(|o| &o.entries)
                .filter(|e| e.key == key);
            let Some(entry) = entries.next() else {
                return default;
            };
            let message = if entries.next().is_some() {
                format!("Set {key} only once per file.")
            } else {
                if let crate::ast::LiteralValue::Text(value) = &entry.value {
                    match value.as_str() {
                        "off" => return None,
                        "warn" => return Some(LintSeverity::Warning),
                        "error" => return Some(LintSeverity::Error),
                        _ => {}
                    }
                }
                format!(r#"Expected "off", "warn", or "error" for {key}."#)
            };
            result.add_simple_issue(LintSeverity::Error, key.into(), message);
            default
        };
        let warn = Some(LintSeverity::Warning);
        let naming = base.enforce_snake_case.then_some(LintSeverity::Warning);
        Self {
            base,
            rule_names: policy("lint_rule_names", naming),
            field_names: policy("lint_field_names", naming),
            operation_names: policy("lint_operation_names", naming),
            text_enums: policy(TEXT_ENUM_LINT_OPTION, base.text_enum_severity),
            unused_rules: policy(
                "lint_unused_rules",
                base.disallow_unused_rules.then_some(LintSeverity::Warning),
            ),
            doc_comments: policy(
                "lint_doc_comments",
                base.warn_doc_comments.then_some(LintSeverity::Warning),
            ),
            rule_length: policy("lint_rule_length", warn),
            zero_minimum: policy(
                "lint_zero_minimum",
                base.warn_degenerate_constraints
                    .then_some(LintSeverity::Warning),
            ),
            empty_service: policy(
                "lint_empty_service",
                base.check_incomplete_services
                    .then_some(LintSeverity::Warning),
            ),
            nesting_depth: policy(
                "lint_nesting_depth",
                base.check_complexity.then_some(LintSeverity::Warning),
            ),
        }
    }
}

/// Lint a CSIL specification
pub fn lint_spec(spec: &CsilSpec, config: &LintConfig) -> Result<LintResult> {
    let mut result = LintResult::new();
    let config = FileLintConfig::new(spec, config, &mut result);

    // Check each rule
    for rule in &spec.rules {
        lint_rule(rule, &config, &mut result);
    }

    // Top-level types can be public entry points. Only check local use on request.
    if let Some(severity) = config.unused_rules {
        check_unused_rules(spec, severity, &mut result);
    }

    Ok(result)
}

/// `;;;` documentation comments attach to the following definition, but they are a
/// CSIL extension with no CDDL equivalent (CDDL has only `;` comments), so flag
/// each definition that uses one.
fn lint_doc_comments(rule: &Rule, severity: LintSeverity, result: &mut LintResult) {
    if !rule.doc_comments.is_empty() {
        push_doc_comment_warning(
            severity,
            result,
            &rule.name,
            rule.position.line,
            rule.position.column,
        );
    }

    let group = match &rule.rule_type {
        RuleType::GroupDef(group) | RuleType::TypeDef(TypeExpression::Group(group)) => Some(group),
        RuleType::ServiceDef(service) => {
            for op in &service.operations {
                if !op.doc_comments.is_empty() {
                    push_doc_comment_warning(
                        severity,
                        result,
                        &op.name,
                        op.position.line,
                        op.position.column,
                    );
                }
            }
            None
        }
        _ => None,
    };

    // Fields have no position of their own, so a documented field is attributed to
    // its enclosing rule.
    if let Some(group) = group {
        for entry in &group.entries {
            if !entry.doc_comments.is_empty() {
                push_doc_comment_warning(
                    severity,
                    result,
                    &rule.name,
                    rule.position.line,
                    rule.position.column,
                );
            }
        }
    }
}

fn push_doc_comment_warning(
    severity: LintSeverity,
    result: &mut LintResult,
    name: &str,
    line: usize,
    column: usize,
) {
    result.add_issue(LintIssue {
        severity,
        rule_name: name.to_string(),
        message:
            "`;;;` documentation comments are a CSIL extension beyond CDDL; prefer `;;` comments"
                .to_string(),
        suggestion: None,
        auto_fixable: false,
        line: Some(line),
        column: Some(column),
        fix: None,
    });
}

fn lint_rule(rule: &Rule, config: &FileLintConfig, result: &mut LintResult) {
    if let Some(severity) = config.doc_comments {
        lint_doc_comments(rule, severity, result);
    }

    // Check naming convention
    if let Some(severity) = config.rule_names
        && !(rule.name.starts_with(char::is_uppercase)
            && rule.name.chars().all(char::is_alphanumeric))
    {
        result.add_issue(LintIssue {
            severity,
            rule_name: rule.name.clone(),
            message: "Rule name should use PascalCase convention".into(),
            suggestion: Some("Rename the declaration and update its consumers.".into()),
            // A file-local text replacement cannot update public names safely.
            auto_fixable: false,
            line: Some(rule.position.line),
            column: Some(rule.position.column),
            fix: None,
        });
    }

    // Check rule name length
    if let Some(severity) = config.rule_length
        && rule.name.len() > config.base.max_rule_name_length
    {
        result.add_issue(LintIssue {
            severity,
            rule_name: rule.name.clone(),
            message: format!(
                "Rule name exceeds maximum length of {} characters",
                config.base.max_rule_name_length
            ),
            suggestion: None,
            auto_fixable: false,
            line: Some(rule.position.line),
            column: Some(rule.position.column),
            fix: None,
        });
    }

    // Check rule-specific issues
    match &rule.rule_type {
        RuleType::TypeDef(type_expr) => {
            lint_type_expression_with_config(type_expr, &rule.name, result, config);
        }
        RuleType::GroupDef(group_expr) => {
            lint_group_expression_with_config(group_expr, &rule.name, result, config);
            lint_metadata_consistency(&group_expr.entries, &rule.name, config, result);
            lint_complexity(
                &TypeExpression::Group(group_expr.clone()),
                &rule.name,
                config,
                result,
                0,
            );
        }
        RuleType::TypeChoice(choices) => {
            lint_text_enum(choices, &rule.name, result, config);
            for choice in choices {
                lint_type_expression_with_config(choice, &rule.name, result, config);
            }
        }
        RuleType::GroupChoice(choices) => {
            for choice in choices {
                lint_group_expression_with_config(choice, &rule.name, result, config);
            }
        }
        RuleType::ServiceDef(service) => {
            lint_service_definition(service, &rule.name, config, result);
            for op in &service.operations {
                let owner = format!("{}.{}", rule.name, op.name);
                lint_type_expression_with_config(&op.input_type, &owner, result, config);
                lint_type_expression_with_config(&op.output_type, &owner, result, config);
            }
        }
    }
}

fn lint_text_enum(
    arms: &[TypeExpression],
    rule_name: &str,
    result: &mut LintResult,
    config: &FileLintConfig,
) {
    let Some(severity) = config.text_enums else {
        return;
    };
    fn is_text(ty: &TypeExpression) -> bool {
        match ty {
            TypeExpression::Literal(crate::ast::LiteralValue::Text(_)) => true,
            TypeExpression::Constrained { base_type, .. } => is_text(base_type),
            _ => false,
        }
    }
    if arms.len() > 1 && arms.iter().all(is_text) {
        result.add_issue(LintIssue {
            severity,
            rule_name: TEXT_ENUM_LINT_OPTION.into(),
            message: format!(
                "{rule_name} contains a text enum. CBOR transmits the full label string."
            ),
            suggestion: Some(
                "Use a named-number enum such as &(busy: 0, denied: 1) for a compact wire value."
                    .into(),
            ),
            auto_fixable: false,
            line: None,
            column: None,
            fix: None,
        });
    }
}

fn lint_type_expression_with_config(
    expr: &TypeExpression,
    rule_name: &str,
    result: &mut LintResult,
    config: &FileLintConfig,
) {
    // Check complexity
    lint_complexity(expr, rule_name, config, result, 0);

    match expr {
        TypeExpression::NamedNumberGroup(_, _)
        | TypeExpression::NamedNumbers(_)
        | TypeExpression::Builtin(_)
        | TypeExpression::Reference(_) => {
            // No specific linting for basic types
        }
        TypeExpression::Array { element_type, .. } => {
            lint_type_expression_with_config(element_type, rule_name, result, config);
        }
        TypeExpression::Map { key, value, .. } => {
            lint_type_expression_with_config(key, rule_name, result, config);
            lint_type_expression_with_config(value, rule_name, result, config);
        }
        TypeExpression::Group(group) => {
            lint_group_expression_with_config(group, rule_name, result, config);
        }
        TypeExpression::Tuple(group) => {
            for entry in &group.entries {
                lint_type_expression_with_config(&entry.value_type, rule_name, result, config);
            }
        }
        TypeExpression::Choice(choices) => {
            lint_text_enum(choices, rule_name, result, config);
            for choice in choices {
                lint_type_expression_with_config(choice, rule_name, result, config);
            }
        }
        TypeExpression::Range { .. } => {
            // Basic range validation could go here
        }
        TypeExpression::Socket(_) | TypeExpression::Plug(_) => {
            // Socket/plug validation could go here
        }
        TypeExpression::Literal(_) => {
            // Literal validation could go here
        }
        TypeExpression::Constrained {
            base_type,
            constraints: _,
        } => {
            // Lint the base type, constraints are already validated during parsing
            lint_type_expression_with_config(base_type, rule_name, result, config);
        }
    }
}

fn lint_group_expression_with_config(
    group: &GroupExpression,
    rule_name: &str,
    result: &mut LintResult,
    config: &FileLintConfig,
) {
    let mut seen_keys = Vec::new();

    for entry in &group.entries {
        // Check for duplicate keys (simplified for now)
        if let Some(key) = &entry.key {
            if seen_keys
                .iter()
                .any(|k| format!("{k:?}") == format!("{key:?}"))
            {
                result.add_simple_issue(
                    LintSeverity::Error,
                    rule_name.to_string(),
                    "Duplicate key in group".to_string(),
                );
            }
            seen_keys.push(key.clone());
        }

        lint_group_entry_with_config(entry, rule_name, result, config);
    }

    // Check for empty groups
    if group.entries.is_empty() {
        result.add_issue_with_suggestion(
            LintSeverity::Info,
            rule_name.to_string(),
            "Empty group definition".to_string(),
            "Consider adding fields or removing this group".to_string(),
            false,
        );
    }
}

fn lint_group_entry_with_config(
    entry: &GroupEntry,
    rule_name: &str,
    result: &mut LintResult,
    config: &FileLintConfig,
) {
    // Check key naming if present
    if let Some(severity) = config.field_names
        && let Some(crate::ast::GroupKey::Bare(key_name)) = &entry.key
        && !is_snake_case(key_name)
    {
        result.add_issue_with_suggestion(
            severity,
            rule_name.to_string(),
            format!("Field '{key_name}' should use snake_case convention"),
            to_snake_case(key_name),
            true,
        );
    }

    if let Some(severity) = config.zero_minimum {
        for meta in &entry.metadata {
            if let FieldMetadata::Constraint(
                ValidationConstraint::MinLength(0) | ValidationConstraint::MinItems(0),
            ) = meta
            {
                let field = match &entry.key {
                    Some(crate::ast::GroupKey::Bare(name)) => name.as_str(),
                    _ => "field",
                };
                result.add_simple_issue(
                    severity,
                    rule_name.to_string(),
                    format!(
                        "Field '{field}' has a zero minimum constraint, which is always satisfied; remove it or set a meaningful bound"
                    ),
                );
            }
        }
    }

    // Lint the value type
    lint_type_expression_with_config(&entry.value_type, rule_name, result, config);
}

fn check_unused_rules(spec: &CsilSpec, severity: LintSeverity, result: &mut LintResult) {
    let mut used_rules = std::collections::HashSet::new();

    // Find all rule references
    for rule in &spec.rules {
        find_rule_references(&rule.rule_type, &mut used_rules);
    }

    // Check for unused rules
    for rule in &spec.rules {
        if !matches!(rule.rule_type, RuleType::ServiceDef(_)) && !used_rules.contains(&rule.name) {
            result.add_issue_with_suggestion(
                severity,
                rule.name.clone(),
                "Rule has no references in this file".to_string(),
                "Check whether application code or another file uses this rule".to_string(),
                false,
            );
        }
    }
}

fn find_rule_references(rule_type: &RuleType, used_rules: &mut std::collections::HashSet<String>) {
    match rule_type {
        RuleType::TypeDef(type_expr) => {
            find_type_references(type_expr, used_rules);
        }
        RuleType::GroupDef(group_expr) => {
            find_group_references(group_expr, used_rules);
        }
        RuleType::TypeChoice(choices) => {
            for choice in choices {
                find_type_references(choice, used_rules);
            }
        }
        RuleType::GroupChoice(choices) => {
            for choice in choices {
                find_group_references(choice, used_rules);
            }
        }
        RuleType::ServiceDef(service) => {
            for operation in &service.operations {
                find_type_references(&operation.input_type, used_rules);
                find_type_references(&operation.output_type, used_rules);
            }
        }
    }
}

fn lint_service_definition(
    service: &crate::ast::ServiceDefinition,
    rule_name: &str,
    config: &FileLintConfig,
    result: &mut LintResult,
) {
    if let Some(severity) = config.empty_service
        && service.operations.is_empty()
    {
        result.add_issue_with_suggestion(
            severity,
            rule_name.to_string(),
            "Service definition is empty".to_string(),
            "Add at least one operation to this service".to_string(),
            false,
        );
    }

    for operation in &service.operations {
        if config.base.check_incomplete_services && is_vague_operation_name(&operation.name) {
            result.add_issue_with_suggestion(
                LintSeverity::Info,
                rule_name.to_string(),
                format!(
                    "Operation '{}' has a vague name that doesn't clearly describe its purpose",
                    operation.name
                ),
                "Consider using a more descriptive name like 'create-user', 'get-orders', etc."
                    .to_string(),
                false,
            );
        }

        // Check operation name conventions
        if let Some(severity) = config.operation_names
            && !is_kebab_case(&operation.name)
        {
            result.add_issue_with_suggestion(
                severity,
                rule_name.to_string(),
                format!(
                    "Operation '{}' should use kebab-case convention",
                    operation.name
                ),
                to_kebab_case(&operation.name),
                true,
            );
        }
    }
}

fn lint_metadata_consistency(
    entries: &[crate::ast::GroupEntry],
    rule_name: &str,
    config: &FileLintConfig,
    result: &mut LintResult,
) {
    if !config.base.enforce_consistent_metadata {
        return;
    }

    let mut has_visibility_metadata = false;
    let mut has_no_visibility_metadata = false;

    for entry in entries {
        let has_visibility = entry
            .metadata
            .iter()
            .any(|m| matches!(m, crate::ast::FieldMetadata::Visibility(_)));

        if has_visibility {
            has_visibility_metadata = true;
        } else {
            has_no_visibility_metadata = true;
        }
    }

    // Warn if some fields have visibility metadata but others don't
    if has_visibility_metadata && has_no_visibility_metadata {
        result.add_issue_with_suggestion(
            LintSeverity::Info,
            rule_name.to_string(),
            "Inconsistent visibility metadata usage - some fields have visibility annotations while others don't".to_string(),
            "Consider applying consistent visibility metadata to all fields or none".to_string(),
            false,
        );
    }
}

fn lint_complexity(
    expr: &TypeExpression,
    rule_name: &str,
    config: &FileLintConfig,
    result: &mut LintResult,
    depth: usize,
) {
    if !config.base.check_complexity && config.nesting_depth.is_none() {
        return;
    }

    // Check nesting depth
    if let Some(severity) = config.nesting_depth
        && depth > config.base.max_nesting_depth
    {
        // Issue will be added with the specified depth
        result.add_issue_with_suggestion(
            severity,
            rule_name.to_string(),
            format!(
                "Type definition exceeds maximum nesting depth of {}",
                config.base.max_nesting_depth
            ),
            "Consider breaking down complex nested structures into separate type definitions"
                .to_string(),
            false,
        );
        return; // Don't continue checking deeper
    }

    match expr {
        TypeExpression::Group(group) => {
            // Check group size
            if config.base.check_complexity && group.entries.len() > config.base.max_group_fields {
                result.add_issue_with_suggestion(
                    LintSeverity::Info,
                    rule_name.to_string(),
                    format!(
                        "Group has {} fields, exceeding recommended maximum of {}",
                        group.entries.len(),
                        config.base.max_group_fields
                    ),
                    "Consider breaking large groups into smaller, more focused types".to_string(),
                    false,
                );
            }

            for entry in &group.entries {
                lint_complexity(&entry.value_type, rule_name, config, result, depth + 1);
            }
        }
        TypeExpression::Array { element_type, .. } => {
            lint_complexity(element_type, rule_name, config, result, depth + 1);
        }
        TypeExpression::Map { key, value, .. } => {
            lint_complexity(key, rule_name, config, result, depth + 1);
            lint_complexity(value, rule_name, config, result, depth + 1);
        }
        TypeExpression::Choice(choices) => {
            // Check for overly complex choices
            if config.base.check_complexity && choices.len() > 10 {
                result.add_issue_with_suggestion(
                    LintSeverity::Info,
                    rule_name.to_string(),
                    format!(
                        "Choice type has {} alternatives, which may be overly complex",
                        choices.len()
                    ),
                    "Consider using an enum or splitting into multiple types".to_string(),
                    false,
                );
            }
            for choice in choices {
                lint_complexity(choice, rule_name, config, result, depth + 1);
            }
        }
        _ => {} // Other types don't increase complexity significantly
    }
}

fn is_vague_operation_name(name: &str) -> bool {
    let vague_names = [
        "do",
        "action",
        "execute",
        "run",
        "process",
        "handle",
        "manage",
        "operation",
        "task",
        "job",
        "work",
        "thing",
        "stuff",
        "data",
    ];
    vague_names.contains(&name.to_lowercase().as_str())
}

fn is_kebab_case(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && !s.is_empty()
}

fn to_kebab_case(s: &str) -> String {
    let mut result = String::new();
    let mut prev_char_was_upper = false;
    let mut last_was_dash = false;

    for (i, c) in s.chars().enumerate() {
        if c == '_' {
            if !last_was_dash {
                result.push('-');
            }
            last_was_dash = true;
            prev_char_was_upper = false;
        } else if c.is_uppercase() {
            if i > 0 && !prev_char_was_upper && !last_was_dash {
                result.push('-');
            }
            result.push(c.to_lowercase().next().unwrap());
            prev_char_was_upper = true;
            last_was_dash = false;
        } else {
            result.push(c);
            prev_char_was_upper = false;
            last_was_dash = false;
        }
    }

    result
}

fn find_group_references(
    group: &GroupExpression,
    used_rules: &mut std::collections::HashSet<String>,
) {
    for entry in &group.entries {
        if let Some(crate::ast::GroupKey::Type(key)) = &entry.key {
            find_type_references(key, used_rules);
        }
        find_type_references(&entry.value_type, used_rules);
    }
}

fn find_type_references(expr: &TypeExpression, used_rules: &mut std::collections::HashSet<String>) {
    match expr {
        TypeExpression::NamedNumberGroup(name, _)
        | TypeExpression::Socket(name)
        | TypeExpression::Plug(name)
        | TypeExpression::Reference(name) => {
            used_rules.insert(name.clone());
        }
        TypeExpression::NamedNumbers(_) => {}
        TypeExpression::Array { element_type, .. } => {
            find_type_references(element_type, used_rules);
        }
        TypeExpression::Map { key, value, .. } => {
            find_type_references(key, used_rules);
            find_type_references(value, used_rules);
        }
        TypeExpression::Group(group) | TypeExpression::Tuple(group) => {
            find_group_references(group, used_rules);
        }
        TypeExpression::Builtin(_) => {
            // Built-in types don't reference other rules
        }
        TypeExpression::Choice(choices) => {
            for choice in choices {
                find_type_references(choice, used_rules);
            }
        }
        TypeExpression::Range { .. } => {
            // Ranges don't reference other rules
        }
        TypeExpression::Literal(_) => {
            // Literals don't reference other rules
        }
        TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            for constraint in constraints {
                if let crate::ast::ControlOperator::And(ty)
                | crate::ast::ControlOperator::Within(ty) = constraint
                {
                    find_type_references(ty, used_rules);
                }
            }
            find_type_references(base_type, used_rules);
        }
    }
}

fn is_snake_case(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_lowercase() || c.is_ascii_digit() || c == '_')
        && !s.starts_with('_')
        && !s.ends_with('_')
        && !s.contains("__")
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    let mut prev_char_was_upper = false;

    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !prev_char_was_upper {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
            prev_char_was_upper = true;
        } else {
            result.push(c);
            prev_char_was_upper = false;
        }
    }

    result
}

/// Lint a CSIL file
pub fn lint_file<P: AsRef<Path>>(file_path: P, config: &LintConfig) -> Result<LintResult> {
    let content = std::fs::read_to_string(&file_path)?;
    let spec = crate::parse_csil(&content)?;
    lint_spec(&spec, config)
}

/// Lint all CSIL files in a directory
pub fn lint_directory<P: AsRef<Path>>(
    dir_path: P,
    config: &LintConfig,
    fix: bool,
) -> Result<Vec<(String, LintResult)>> {
    let mut results = Vec::new();

    for entry in std::fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("csil") {
            let result = lint_file(&path, config)?;

            // Apply automatic fixes if requested
            if fix {
                apply_fixes_to_file(&path, &result)?;
            }

            results.push((path.display().to_string(), result));
        }
    }

    Ok(results)
}

/// Apply automatic fixes to a file
fn apply_fixes_to_file<P: AsRef<Path>>(file_path: P, lint_result: &LintResult) -> Result<()> {
    let mut content = std::fs::read_to_string(&file_path)?;
    let mut fixes_applied = 0;

    // Sort fixes by position (line, column) in reverse order so we can apply them
    // from end to beginning without affecting positions of earlier fixes
    let mut fixable_issues: Vec<_> = lint_result
        .issues
        .iter()
        .filter(|issue| issue.auto_fixable && issue.fix.is_some())
        .collect();

    fixable_issues.sort_by_key(|issue| (issue.line.unwrap_or(0), issue.column.unwrap_or(0)));
    fixable_issues.reverse();

    for issue in fixable_issues {
        if let Some(fix) = &issue.fix {
            // Simple text replacement - in a real implementation, this would need
            // more sophisticated handling to ensure we're replacing the right text
            if content.contains(&fix.old_text) {
                content = content.replace(&fix.old_text, &fix.new_text);
                fixes_applied += 1;
            }
        }
    }

    if fixes_applied > 0 {
        std::fs::write(&file_path, content)?;
        println!(
            "Applied {} automatic fix(es) to {}",
            fixes_applied,
            file_path.as_ref().display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{CsilSpec, GroupEntry, GroupExpression, Rule, RuleType, TypeExpression};

    fn create_test_spec(rules: Vec<Rule>) -> CsilSpec {
        CsilSpec {
            imports: Vec::new(),
            options: None,
            rules,
        }
    }

    #[test]
    fn text_enum_policy_uses_each_files_options() {
        let schema = r#"numbers = &(busy: 7, denied: 42)
            quoted = "busy" / "denied"
            record = { items: [* "yes" / "no"], code: numbers }"#;
        let config = LintConfig::default();
        for (header, expected) in [
            (
                r#"options { lint_text_enums: "error" }"#,
                Some(LintSeverity::Error),
            ),
            (r#"options { lint_text_enums: "off" }"#, None),
            ("", Some(LintSeverity::Warning)),
            (r#"options { package: "api" }"#, Some(LintSeverity::Warning)),
            (
                r#"options { lint_text_enums: "warn" }"#,
                Some(LintSeverity::Warning),
            ),
        ] {
            let spec = crate::parse_csil(&format!("{header}\n{schema}")).unwrap();
            let result = lint_spec(&spec, &config).unwrap();
            let issues: Vec<_> = result
                .issues
                .iter()
                .filter(|i| i.rule_name == TEXT_ENUM_LINT_OPTION)
                .collect();
            assert_eq!(issues.len(), if expected.is_some() { 2 } else { 0 });
            assert!(
                issues
                    .iter()
                    .all(|i| Some(&i.severity) == expected.as_ref() && !i.auto_fixable)
            );
            assert_eq!(result.has_errors(), expected == Some(LintSeverity::Error));
        }
    }

    #[test]
    fn invalid_text_enum_policy_is_a_lint_error() {
        for settings in [
            r#"lint_text_enums: "warning""#,
            "lint_text_enums: false",
            r#"lint_text_enums: "off", lint_text_enums: "warn""#,
        ] {
            let spec = crate::parse_csil(&format!("options {{ {settings} }}\ncode = int")).unwrap();
            let result = lint_spec(&spec, &LintConfig::default()).unwrap();
            assert_eq!(result.error_count, 1);
            assert_eq!(result.issues[0].rule_name, TEXT_ENUM_LINT_OPTION);
        }
    }

    fn create_test_rule(name: &str, rule_type: RuleType) -> Rule {
        Rule {
            name: name.to_string(),
            rule_type,
            position: crate::lexer::Position::new(1, 1, 0),
            doc_comments: Vec::new(),
        }
    }

    // Helper to create simple lint issues for tests
    #[allow(dead_code)]
    fn create_simple_issue(severity: LintSeverity, rule_name: &str, message: &str) -> LintIssue {
        LintIssue {
            severity,
            rule_name: rule_name.to_string(),
            message: message.to_string(),
            suggestion: None,
            auto_fixable: false,
            line: None,
            column: None,
            fix: None,
        }
    }

    #[test]
    fn test_lint_empty_spec() {
        let spec = create_test_spec(vec![]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        assert_eq!(result.issues.len(), 0);
        assert_eq!(result.error_count, 0);
        assert_eq!(result.warning_count, 0);
        assert!(!result.has_errors());
    }

    #[test]
    fn test_rule_name_convention() {
        for (name, warnings) in [
            ("ApiErrorCode", 0),
            ("UserID", 0),
            ("bad_name", 1),
            ("camelCase", 1),
        ] {
            let spec = create_test_spec(vec![create_test_rule(
                name,
                RuleType::TypeDef(TypeExpression::Builtin("int".into())),
            )]);
            let result = lint_spec(&spec, &LintConfig::default()).unwrap();
            assert_eq!(result.warning_count, warnings, "{name}");
        }
    }

    #[test]
    fn test_lint_rule_name_too_long() {
        let long_name = "A".repeat(100);
        let spec = create_test_spec(vec![create_test_rule(
            &long_name,
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        assert_eq!(result.warning_count, 1);
        assert!(result.issues[0].message.contains("maximum length"));
        assert!(!result.issues[0].auto_fixable);
    }

    #[test]
    fn test_lint_duplicate_keys_in_group() {
        let spec = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("name".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("name".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                        doc_comments: Vec::new(),
                    },
                ],
            }),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        assert_eq!(result.error_count, 1);
        assert!(result.issues[0].message.contains("Duplicate key"));
        assert!(!result.issues[0].auto_fixable);
        assert!(result.has_errors());
    }

    #[test]
    fn test_lint_empty_group() {
        let spec = create_test_spec(vec![create_test_rule(
            "EmptyGroup",
            RuleType::GroupDef(GroupExpression { entries: vec![] }),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        assert_eq!(result.info_count, 1);
        assert!(result.issues[0].message.contains("Empty group"));
    }

    #[test]
    fn test_lint_field_name_snake_case() {
        let spec = create_test_spec(vec![create_test_rule(
            "TestGroup",
            RuleType::GroupDef(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(crate::ast::GroupKey::Bare("CamelCaseField".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: Vec::new(),
                    doc_comments: Vec::new(),
                }],
            }),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        assert_eq!(result.warning_count, 1);
        assert!(result.issues[0].message.contains("should use snake_case"));
        assert_eq!(
            result.issues[0].suggestion,
            Some("camel_case_field".to_string())
        );
    }

    #[test]
    fn test_unused_rule_detection() {
        let schema = "Req = { id: int }
Resp = { req: Req }
Root = text
service Api { hello: Req -> Resp }";
        let spec = crate::parse_csil(schema).unwrap();
        let result = lint_spec(&spec, &LintConfig::default()).unwrap();
        assert_eq!(result.warning_count, 0);
        let config = LintConfig {
            disallow_unused_rules: true,
            ..Default::default()
        };
        let result = lint_spec(&spec, &config).unwrap();
        assert_eq!(result.warning_count, 1);
        assert_eq!(result.issues[0].rule_name, "Root");
    }

    #[test]
    fn test_snake_case_helper_functions() {
        assert!(is_snake_case("valid_name"));
        assert!(is_snake_case("snake_case_name"));
        assert!(is_snake_case("test123"));
        assert!(!is_snake_case("CamelCase"));
        assert!(!is_snake_case("_leading_underscore"));
        assert!(!is_snake_case("trailing_underscore_"));
        assert!(!is_snake_case("double__underscore"));

        assert_eq!(to_snake_case("CamelCase"), "camel_case");
        assert_eq!(to_snake_case("XMLHttpRequest"), "xmlhttp_request");
        assert_eq!(to_snake_case("snake_case"), "snake_case");
        assert_eq!(to_snake_case("UPPER"), "upper");
    }

    #[test]
    fn test_warn_on_degenerate_min_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "user",
            RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(crate::ast::GroupKey::Bare("name".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![FieldMetadata::Constraint(ValidationConstraint::MinLength(
                        0,
                    ))],
                    doc_comments: Vec::new(),
                }],
            })),
        )]);

        let warned = lint_spec(&spec, &LintConfig::default()).unwrap();
        assert!(
            warned
                .issues
                .iter()
                .any(|i| i.message.contains("zero minimum constraint")),
            "expected a degenerate-constraint warning"
        );

        let config = LintConfig {
            warn_degenerate_constraints: false,
            ..Default::default()
        };
        let silent = lint_spec(&spec, &config).unwrap();
        assert!(
            !silent
                .issues
                .iter()
                .any(|i| i.message.contains("zero minimum constraint"))
        );
    }

    #[test]
    fn test_warn_on_doc_comments() {
        let mut rule = create_test_rule(
            "Documented",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        );
        rule.doc_comments = vec!["A documented rule.".to_string()];
        let spec = create_test_spec(vec![rule]);

        // Default config warns; disabling the flag silences it.
        let warned = lint_spec(&spec, &LintConfig::default()).unwrap();
        assert!(
            warned
                .issues
                .iter()
                .any(|i| i.message.contains("`;;;` documentation comments")),
            "expected a doc-comment warning"
        );

        let config = LintConfig {
            warn_doc_comments: false,
            ..Default::default()
        };
        let silent = lint_spec(&spec, &config).unwrap();
        assert!(
            !silent
                .issues
                .iter()
                .any(|i| i.message.contains("`;;;` documentation comments"))
        );
    }

    #[test]
    fn test_lint_config_customization() {
        let spec = create_test_spec(vec![create_test_rule(
            "bad_name",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);

        // Test with snake_case enforcement disabled
        let config = LintConfig {
            enforce_snake_case: false,
            ..Default::default()
        };
        let result = lint_spec(&spec, &config).unwrap();
        assert_eq!(result.warning_count, 0);

        // Test with unused rules disabled
        let spec2 = create_test_spec(vec![create_test_rule(
            "Unused",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);
        let config2 = LintConfig {
            disallow_unused_rules: false,
            ..Default::default()
        };
        let result2 = lint_spec(&spec2, &config2).unwrap();
        assert_eq!(result2.issues.len(), 0);
    }

    #[test]
    fn test_service_definition_linting() {
        // Test empty service
        let empty_service = create_test_rule(
            "EmptyService",
            RuleType::ServiceDef(crate::ast::ServiceDefinition {
                operations: vec![],
                metadata: Vec::new(),
            }),
        );
        let spec = create_test_spec(vec![empty_service]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        assert_eq!(result.warning_count, 1);
        assert!(
            result.issues[0]
                .message
                .contains("Service definition is empty")
        );
    }

    #[test]
    fn test_metadata_consistency_linting() {
        use crate::ast::{FieldMetadata, FieldVisibility, GroupEntry, GroupExpression, GroupKey};

        let spec = create_test_spec(vec![create_test_rule(
            "inconsistent_metadata",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare("field1".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![FieldMetadata::Visibility(FieldVisibility::SendOnly)],
                        doc_comments: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("field2".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![], // No metadata - inconsistent,
                        doc_comments: Vec::new(),
                    },
                ],
            }),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        let metadata_issues: Vec<_> = result
            .issues
            .iter()
            .filter(|issue| issue.message.contains("Inconsistent visibility metadata"))
            .collect();
        assert_eq!(metadata_issues.len(), 1);
    }

    #[test]
    fn test_complexity_linting() {
        // Test deeply nested structure - create a structure with max_nesting_depth + 2 levels
        // This should exceed the default max_nesting_depth of 5
        let deeply_nested = TypeExpression::Group(GroupExpression {
            entries: vec![GroupEntry {
                key: Some(crate::ast::GroupKey::Bare("level1".to_string())),
                value_type: TypeExpression::Group(GroupExpression {
                    entries: vec![GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("level2".to_string())),
                        value_type: TypeExpression::Group(GroupExpression {
                            entries: vec![GroupEntry {
                                key: Some(crate::ast::GroupKey::Bare("level3".to_string())),
                                value_type: TypeExpression::Group(GroupExpression {
                                    entries: vec![GroupEntry {
                                        key: Some(crate::ast::GroupKey::Bare("level4".to_string())),
                                        value_type: TypeExpression::Group(GroupExpression {
                                            entries: vec![GroupEntry {
                                                key: Some(crate::ast::GroupKey::Bare("level5".to_string())),
                                                value_type: TypeExpression::Group(GroupExpression {
                                                    entries: vec![GroupEntry {
                                                        key: Some(crate::ast::GroupKey::Bare("level6".to_string())),
                                                        value_type: TypeExpression::Group(GroupExpression {
                                                            entries: vec![GroupEntry {
                                                                key: Some(crate::ast::GroupKey::Bare("level7".to_string())),
                                                                value_type: TypeExpression::Builtin("text".to_string()),
                                                                occurrence: None,
                                                                metadata: vec![],
                                                                doc_comments: Vec::new(),
                                                            }],
                                                        }),
                                                        occurrence: None,
                                                        metadata: vec![],
                                                        doc_comments: Vec::new(),
                                                    }],
                                                }),
                                                occurrence: None,
                                                metadata: vec![],
                                                doc_comments: Vec::new(),
                                            }],
                                        }),
                                        occurrence: None,
                                        metadata: vec![],
                                        doc_comments: Vec::new(),
                                    }],
                                }),
                                occurrence: None,
                                metadata: vec![],
                                doc_comments: Vec::new(),
                            }],
                        }),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                occurrence: None,
                metadata: vec![],
                doc_comments: Vec::new(),
            }],
        });

        let spec = create_test_spec(vec![create_test_rule(
            "complex_nested",
            RuleType::TypeDef(deeply_nested),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        let complexity_issues: Vec<_> = result
            .issues
            .iter()
            .filter(|issue| issue.message.contains("nesting depth"))
            .collect();
        assert!(!complexity_issues.is_empty());
    }

    #[test]
    fn test_large_group_linting() {
        // Create a group with many fields
        let mut entries = Vec::new();
        for i in 0..25 {
            // Exceeds default max_group_fields of 20
            entries.push(GroupEntry {
                key: Some(crate::ast::GroupKey::Bare(format!("field_{i}"))),
                value_type: TypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![],
                doc_comments: Vec::new(),
            });
        }

        let spec = create_test_spec(vec![create_test_rule(
            "large_group",
            RuleType::GroupDef(GroupExpression { entries }),
        )]);
        let config = LintConfig::default();
        let result = lint_spec(&spec, &config).unwrap();

        let size_issues: Vec<_> = result
            .issues
            .iter()
            .filter(|issue| issue.message.contains("exceeding recommended maximum"))
            .collect();
        assert_eq!(size_issues.len(), 1);
    }

    #[test]
    fn test_kebab_case_helpers() {
        assert!(is_kebab_case("create-user"));
        assert!(is_kebab_case("get-user-profile"));
        assert!(!is_kebab_case("createUser"));
        assert!(!is_kebab_case("create_user"));
        assert!(!is_kebab_case("-leading-dash"));
        assert!(!is_kebab_case("trailing-dash-"));
        assert!(!is_kebab_case("double--dash"));
        assert!(is_kebab_case("hello"));
        assert!(!is_kebab_case(""));

        assert_eq!(to_kebab_case("createUser"), "create-user");
        assert_eq!(to_kebab_case("XMLHttpRequest"), "xmlhttp-request");
        assert_eq!(to_kebab_case("create_user"), "create-user");
        assert_eq!(to_kebab_case("UPPER_CASE"), "upper-case");
    }

    #[test]
    fn test_vague_operation_names() {
        assert!(is_vague_operation_name("do"));
        assert!(is_vague_operation_name("process"));
        assert!(is_vague_operation_name("handle"));
        assert!(!is_vague_operation_name("create-user"));
        assert!(!is_vague_operation_name("get-orders"));
        assert!(!is_vague_operation_name("delete-item"));
    }
}
