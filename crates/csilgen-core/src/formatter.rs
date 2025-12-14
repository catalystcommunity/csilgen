//! CSIL code formatting functionality

use crate::ast::CsilSpec;
use anyhow::Result;
use std::path::Path;

/// Configuration options for CSIL formatting
#[derive(Debug, Clone)]
pub struct FormatConfig {
    /// Number of spaces for indentation (default: 2)
    pub indent_size: usize,
    /// Maximum line length before wrapping (default: 100)
    pub max_line_length: usize,
    /// Whether to add trailing commas in groups (default: true)
    pub trailing_commas: bool,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            indent_size: 2,
            max_line_length: 100,
            trailing_commas: true,
        }
    }
}

/// Result of formatting operation
#[derive(Debug)]
pub struct FormatResult {
    /// The formatted CSIL code
    pub formatted_content: String,
    /// Whether the content was changed from the original
    pub changed: bool,
}

/// Format a CSIL specification to canonical style
pub fn format_spec(spec: &CsilSpec, config: &FormatConfig) -> Result<String> {
    let mut output = String::new();

    for (i, rule) in spec.rules.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }

        // Format rule based on its type
        match &rule.rule_type {
            crate::ast::RuleType::TypeDef(type_expr) => {
                output.push_str(&format!("{} = ", rule.name));
                format_type_expression(type_expr, &mut output, 0, config);
            }
            crate::ast::RuleType::GroupDef(group_expr) => {
                output.push_str(&format!("{} = ", rule.name));
                format_group_expression(group_expr, &mut output, 0, config);
            }
            crate::ast::RuleType::TypeChoice(types) => {
                output.push_str(&format!("{} //= ", rule.name));
                for (i, type_expr) in types.iter().enumerate() {
                    if i > 0 {
                        output.push_str(" / ");
                    }
                    format_type_expression(type_expr, &mut output, 0, config);
                }
            }
            crate::ast::RuleType::GroupChoice(groups) => {
                output.push_str(&format!("{} //= ", rule.name));
                for (i, group_expr) in groups.iter().enumerate() {
                    if i > 0 {
                        output.push_str(" / ");
                    }
                    format_group_expression(group_expr, &mut output, 0, config);
                }
            }
            crate::ast::RuleType::ServiceDef(service) => {
                output.push_str(&format!("service {} {{", rule.name));
                if !service.operations.is_empty() {
                    output.push('\n');
                    for operation in &service.operations {
                        for _ in 0..config.indent_size {
                            output.push(' ');
                        }

                        output.push_str(&operation.name);
                        output.push_str(": ");
                        format_type_expression(
                            &operation.input_type,
                            &mut output,
                            config.indent_size,
                            config,
                        );

                        match operation.direction {
                            crate::ast::ServiceDirection::Unidirectional => output.push_str(" -> "),
                            crate::ast::ServiceDirection::Bidirectional => output.push_str(" <-> "),
                            crate::ast::ServiceDirection::Reverse => output.push_str(" <- "),
                        }

                        format_type_expression(
                            &operation.output_type,
                            &mut output,
                            config.indent_size,
                            config,
                        );
                        output.push(',');
                        output.push('\n');
                    }
                }
                output.push('}');
            }
        }

        output.push('\n');
    }

    Ok(output)
}

fn format_type_expression(
    expr: &crate::ast::TypeExpression,
    output: &mut String,
    indent: usize,
    config: &FormatConfig,
) {
    match expr {
        crate::ast::TypeExpression::Builtin(name) => {
            output.push_str(name);
        }
        crate::ast::TypeExpression::Reference(name) => {
            output.push_str(name);
        }
        crate::ast::TypeExpression::Array {
            element_type,
            occurrence: _,
        } => {
            output.push('[');
            format_type_expression(element_type, output, indent, config);
            output.push(']');
        }
        crate::ast::TypeExpression::Map {
            key,
            value,
            occurrence: _,
        } => {
            output.push('{');
            format_type_expression(key, output, indent, config);
            output.push_str(" => ");
            format_type_expression(value, output, indent, config);
            output.push('}');
        }
        crate::ast::TypeExpression::Group(group) => {
            format_group_expression(group, output, indent, config);
        }
        crate::ast::TypeExpression::Choice(types) => {
            for (i, type_expr) in types.iter().enumerate() {
                if i > 0 {
                    output.push_str(" / ");
                }
                format_type_expression(type_expr, output, indent, config);
            }
        }
        crate::ast::TypeExpression::Range {
            start,
            end,
            inclusive: _,
        } => {
            if let Some(s) = start {
                output.push_str(&s.to_string());
            }

            output.push_str("..");

            if let Some(e) = end {
                output.push_str(&e.to_string());
            }
        }
        crate::ast::TypeExpression::Socket(name) => {
            output.push_str(&format!("${name}"));
        }
        crate::ast::TypeExpression::Plug(name) => {
            output.push_str(&format!("$${name}"));
        }
        crate::ast::TypeExpression::Literal(lit) => match lit {
            crate::ast::LiteralValue::Integer(i) => output.push_str(&i.to_string()),
            crate::ast::LiteralValue::Float(f) => output.push_str(&f.to_string()),
            crate::ast::LiteralValue::Text(s) => output.push_str(&format!("\"{s}\"")),
            crate::ast::LiteralValue::Bytes(bytes) => {
                output.push_str("h'");
                for byte in bytes {
                    output.push_str(&format!("{byte:02x}"));
                }
                output.push('\'')
            }
            crate::ast::LiteralValue::Bool(b) => output.push_str(&b.to_string()),
            crate::ast::LiteralValue::Null => output.push_str("null"),
            crate::ast::LiteralValue::Array(elements) => {
                output.push('[');
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        output.push_str(", ");
                    }
                    format_literal_value(elem, output);
                }
                output.push(']');
            }
        },
        crate::ast::TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            format_type_expression(base_type, output, indent, config);
            for constraint in constraints {
                match constraint {
                    crate::ast::ControlOperator::Size(size_constraint) => {
                        output.push_str(" .size ");
                        format_size_constraint(size_constraint, output);
                    }
                    crate::ast::ControlOperator::Regex(pattern) => {
                        output.push_str(" .regex \"");
                        output.push_str(pattern);
                        output.push('"');
                    }
                    crate::ast::ControlOperator::Default(value) => {
                        output.push_str(" .default ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::GreaterEqual(value) => {
                        output.push_str(" .ge ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::LessEqual(value) => {
                        output.push_str(" .le ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::GreaterThan(value) => {
                        output.push_str(" .gt ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::LessThan(value) => {
                        output.push_str(" .lt ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::Equal(value) => {
                        output.push_str(" .eq ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::NotEqual(value) => {
                        output.push_str(" .ne ");
                        format_literal_value(value, output);
                    }
                    crate::ast::ControlOperator::Bits(bits_expr) => {
                        output.push_str(" .bits ");
                        output.push_str(bits_expr);
                    }
                    crate::ast::ControlOperator::And(type_expr) => {
                        output.push_str(" .and ");
                        format_type_expression(type_expr, output, 0, config);
                    }
                    crate::ast::ControlOperator::Within(type_expr) => {
                        output.push_str(" .within ");
                        format_type_expression(type_expr, output, 0, config);
                    }
                    crate::ast::ControlOperator::Json => {
                        output.push_str(" .json");
                    }
                    crate::ast::ControlOperator::Cbor => {
                        output.push_str(" .cbor");
                    }
                    crate::ast::ControlOperator::Cborseq => {
                        output.push_str(" .cborseq");
                    }
                }
            }
        }
    }
}

fn format_size_constraint(constraint: &crate::ast::SizeConstraint, output: &mut String) {
    match constraint {
        crate::ast::SizeConstraint::Exact(size) => output.push_str(&size.to_string()),
        crate::ast::SizeConstraint::Range { min, max } => {
            output.push('(');
            output.push_str(&min.to_string());
            output.push_str("..");
            output.push_str(&max.to_string());
            output.push(')');
        }
        crate::ast::SizeConstraint::Min(min) => {
            output.push('(');
            output.push_str(&min.to_string());
            output.push_str("..)");
        }
        crate::ast::SizeConstraint::Max(max) => {
            output.push_str("(..");
            output.push_str(&max.to_string());
            output.push(')');
        }
    }
}

fn format_group_expression(
    group: &crate::ast::GroupExpression,
    output: &mut String,
    indent: usize,
    config: &FormatConfig,
) {
    output.push('{');

    if !group.entries.is_empty() {
        output.push('\n');

        for (i, entry) in group.entries.iter().enumerate() {
            // Add indentation
            for _ in 0..(indent + config.indent_size) {
                output.push(' ');
            }

            // Format key if present
            if let Some(key) = &entry.key {
                match key {
                    crate::ast::GroupKey::Bare(name) => output.push_str(name),
                    crate::ast::GroupKey::Literal(lit) => match lit {
                        crate::ast::LiteralValue::Text(s) => output.push_str(&format!("\"{s}\"")),
                        crate::ast::LiteralValue::Integer(i) => output.push_str(&i.to_string()),
                        crate::ast::LiteralValue::Float(f) => output.push_str(&f.to_string()),
                        crate::ast::LiteralValue::Bytes(bytes) => {
                            output.push_str("h'");
                            for byte in bytes {
                                output.push_str(&format!("{byte:02x}"));
                            }
                            output.push('\'')
                        }
                        crate::ast::LiteralValue::Bool(b) => output.push_str(&b.to_string()),
                        crate::ast::LiteralValue::Null => output.push_str("null"),
                        crate::ast::LiteralValue::Array(elements) => {
                            output.push('[');
                            for (i, elem) in elements.iter().enumerate() {
                                if i > 0 {
                                    output.push_str(", ");
                                }
                                format_literal_value(elem, output);
                            }
                            output.push(']');
                        }
                    },
                    crate::ast::GroupKey::Type(type_expr) => {
                        output.push('(');
                        format_type_expression(
                            type_expr,
                            output,
                            indent + config.indent_size,
                            config,
                        );
                        output.push(')');
                    }
                }

                // Check for occurrence right after key name
                if let Some(occurrence) = &entry.occurrence {
                    match occurrence {
                        crate::ast::Occurrence::Optional => output.push('?'),
                        crate::ast::Occurrence::ZeroOrMore => output.push('*'),
                        crate::ast::Occurrence::OneOrMore => output.push('+'),
                        crate::ast::Occurrence::Exact(n) => output.push_str(&n.to_string()),
                        crate::ast::Occurrence::Range { min, max } => {
                            if let Some(min_val) = min {
                                output.push_str(&min_val.to_string());
                            }
                            output.push('*');
                            if let Some(max_val) = max {
                                output.push_str(&max_val.to_string());
                            }
                        }
                    }
                }

                output.push_str(": ");
            }

            // Format value type
            format_type_expression(
                &entry.value_type,
                output,
                indent + config.indent_size,
                config,
            );

            // Format metadata annotations
            if !entry.metadata.is_empty() {
                output.push(' ');
                for (j, metadata) in entry.metadata.iter().enumerate() {
                    if j > 0 {
                        output.push(' ');
                    }
                    format_field_metadata(metadata, output);
                }
            }

            // Add comma (and potentially trailing comma)
            if i < group.entries.len() - 1 || config.trailing_commas {
                output.push(',');
            }

            output.push('\n');
        }

        // Closing brace with proper indentation
        for _ in 0..indent {
            output.push(' ');
        }
    }

    output.push('}');
}

fn format_field_metadata(metadata: &crate::ast::FieldMetadata, output: &mut String) {
    match metadata {
        crate::ast::FieldMetadata::Visibility(visibility) => match visibility {
            crate::ast::FieldVisibility::SendOnly => output.push_str("@send-only"),
            crate::ast::FieldVisibility::ReceiveOnly => output.push_str("@receive-only"),
            crate::ast::FieldVisibility::Bidirectional => output.push_str("@bidirectional"),
        },
        crate::ast::FieldMetadata::DependsOn { field, value } => {
            output.push_str(&format!("@depends-on({field})"));
            if let Some(val) = value {
                output.push_str(" = ");
                format_literal_value(val, output);
            }
        }
        crate::ast::FieldMetadata::Constraint(constraint) => match constraint {
            crate::ast::ValidationConstraint::MinLength(n) => {
                output.push_str(&format!("@min-length({n})"));
            }
            crate::ast::ValidationConstraint::MaxLength(n) => {
                output.push_str(&format!("@max-length({n})"));
            }
            crate::ast::ValidationConstraint::MinItems(n) => {
                output.push_str(&format!("@min-items({n})"));
            }
            crate::ast::ValidationConstraint::MaxItems(n) => {
                output.push_str(&format!("@max-items({n})"));
            }
            crate::ast::ValidationConstraint::MinValue(value) => {
                output.push_str("@min-value(");
                format_literal_value(value, output);
                output.push(')');
            }
            crate::ast::ValidationConstraint::MaxValue(value) => {
                output.push_str("@max-value(");
                format_literal_value(value, output);
                output.push(')');
            }
            crate::ast::ValidationConstraint::Custom { name, value } => {
                output.push_str(&format!("@{name}("));
                format_literal_value(value, output);
                output.push(')');
            }
        },
        crate::ast::FieldMetadata::Description(desc) => {
            output.push_str(&format!("@description(\"{desc}\")"));
        }
        crate::ast::FieldMetadata::Custom { name, parameters } => {
            output.push_str(&format!("@{name}("));
            for (i, param) in parameters.iter().enumerate() {
                if i > 0 {
                    output.push_str(", ");
                }
                if let Some(param_name) = &param.name {
                    output.push_str(&format!("{param_name} = "));
                }
                format_literal_value(&param.value, output);
            }
            output.push(')');
        }
    }
}

fn format_literal_value(value: &crate::ast::LiteralValue, output: &mut String) {
    match value {
        crate::ast::LiteralValue::Integer(i) => output.push_str(&i.to_string()),
        crate::ast::LiteralValue::Float(f) => output.push_str(&f.to_string()),
        crate::ast::LiteralValue::Text(s) => output.push_str(&format!("\"{s}\"")),
        crate::ast::LiteralValue::Bytes(bytes) => {
            output.push_str("h'");
            for byte in bytes {
                output.push_str(&format!("{byte:02x}"));
            }
            output.push('\'')
        }
        crate::ast::LiteralValue::Bool(b) => output.push_str(&b.to_string()),
        crate::ast::LiteralValue::Null => output.push_str("null"),
        crate::ast::LiteralValue::Array(elements) => {
            output.push('[');
            for (i, elem) in elements.iter().enumerate() {
                if i > 0 {
                    output.push_str(", ");
                }
                format_literal_value(elem, output);
            }
            output.push(']');
        }
    }
}

/// Format a CSIL file and return the result
pub fn format_file<P: AsRef<Path>>(file_path: P, config: &FormatConfig) -> Result<FormatResult> {
    let original_content = std::fs::read_to_string(&file_path)?;
    let spec = crate::parse_csil(&original_content)?;
    let formatted_content = format_spec(&spec, config)?;

    Ok(FormatResult {
        changed: original_content != formatted_content,
        formatted_content,
    })
}

/// Format all CSIL files in a directory recursively
pub fn format_directory<P: AsRef<Path>>(
    dir_path: P,
    config: &FormatConfig,
    dry_run: bool,
) -> Result<Vec<(String, FormatResult)>> {
    let mut results = Vec::new();
    format_directory_recursive(dir_path.as_ref(), config, dry_run, &mut results)?;
    Ok(results)
}

/// Format all CSIL files in a directory with progress reporting
pub fn format_directory_with_progress<P: AsRef<Path>, F>(
    dir_path: P,
    config: &FormatConfig,
    dry_run: bool,
    progress_callback: F,
) -> Result<Vec<(String, FormatResult)>>
where
    F: Fn(&str),
{
    let mut results = Vec::new();

    // First, collect all .csil files to get a total count
    let mut all_csil_files = Vec::new();
    collect_csil_files(dir_path.as_ref(), &mut all_csil_files)?;

    let total_files = all_csil_files.len();
    progress_callback(&format!("Found {total_files} CSIL files to process"));

    for (index, path) in all_csil_files.iter().enumerate() {
        progress_callback(&format!(
            "Processing ({}/{total_files}) {}",
            index + 1,
            path.display()
        ));

        let result = format_file(path, config)?;

        if !dry_run && result.changed {
            std::fs::write(path, &result.formatted_content)?;
        }

        results.push((path.display().to_string(), result));
    }

    Ok(results)
}

fn collect_csil_files(dir_path: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("csil") {
            files.push(path);
        } else if path.is_dir() {
            collect_csil_files(&path, files)?;
        }
    }
    Ok(())
}

fn format_directory_recursive(
    dir_path: &Path,
    config: &FormatConfig,
    dry_run: bool,
    results: &mut Vec<(String, FormatResult)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("csil") {
            let result = format_file(&path, config)?;

            if !dry_run && result.changed {
                std::fs::write(&path, &result.formatted_content)?;
            }

            results.push((path.display().to_string(), result));
        } else if path.is_dir() {
            // Recursively process subdirectories
            format_directory_recursive(&path, config, dry_run, results)?;
        }
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

    fn create_test_rule(name: &str, rule_type: RuleType) -> Rule {
        Rule {
            name: name.to_string(),
            rule_type,
            position: crate::lexer::Position::new(1, 1, 0),
        }
    }

    #[test]
    fn test_format_empty_spec() {
        let spec = create_test_spec(vec![]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_simple_type_def() {
        let spec = create_test_spec(vec![create_test_rule(
            "my_int",
            RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "my_int = int\n");
    }

    #[test]
    fn test_format_array_type() {
        let spec = create_test_spec(vec![create_test_rule(
            "int_array",
            RuleType::TypeDef(TypeExpression::Array {
                element_type: Box::new(TypeExpression::Builtin("int".to_string())),
                occurrence: None,
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "int_array = [int]\n");
    }

    #[test]
    fn test_format_empty_group() {
        let spec = create_test_spec(vec![create_test_rule(
            "empty_group",
            RuleType::GroupDef(GroupExpression { entries: vec![] }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "empty_group = {}\n");
    }

    #[test]
    fn test_format_group_with_entries() {
        let spec = create_test_spec(vec![create_test_rule(
            "person",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("name".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("age".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: Some(crate::ast::Occurrence::Optional),
                        metadata: Vec::new(),
                    },
                ],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();

        let expected = "person = {\n  name: text,\n  age?: int,\n}\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_multiple_rules() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "rule1",
                RuleType::TypeDef(TypeExpression::Builtin("int".to_string())),
            ),
            create_test_rule(
                "rule2",
                RuleType::TypeDef(TypeExpression::Builtin("text".to_string())),
            ),
        ]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "rule1 = int\n\nrule2 = text\n");
    }

    #[test]
    fn test_format_config_custom_indent() {
        let spec = create_test_spec(vec![create_test_rule(
            "person",
            RuleType::GroupDef(GroupExpression {
                entries: vec![GroupEntry {
                    key: Some(crate::ast::GroupKey::Bare("name".to_string())),
                    value_type: TypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: Vec::new(),
                }],
            }),
        )]);
        let config = FormatConfig {
            indent_size: 4,
            ..Default::default()
        };
        let result = format_spec(&spec, &config).unwrap();

        let expected = "person = {\n    name: text,\n}\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_type_choice() {
        let spec = create_test_spec(vec![create_test_rule(
            "number_or_text",
            RuleType::TypeChoice(vec![
                TypeExpression::Builtin("int".to_string()),
                TypeExpression::Builtin("text".to_string()),
            ]),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "number_or_text //= int / text\n");
    }

    #[test]
    fn test_format_group_choice() {
        let spec = create_test_spec(vec![create_test_rule(
            "entity",
            RuleType::GroupChoice(vec![
                GroupExpression {
                    entries: vec![GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("name".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                    }],
                },
                GroupExpression {
                    entries: vec![GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("id".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: Vec::new(),
                    }],
                },
            ]),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "entity //= {\n  name: text,\n} / {\n  id: int,\n}\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_service_definition() {
        let spec = create_test_spec(vec![create_test_rule(
            "UserAPI",
            RuleType::ServiceDef(crate::ast::ServiceDefinition {
                operations: vec![
                    crate::ast::ServiceOperation {
                        name: "get_user".to_string(),
                        input_type: TypeExpression::Reference("UserRequest".to_string()),
                        output_type: TypeExpression::Reference("UserResponse".to_string()),
                        direction: crate::ast::ServiceDirection::Unidirectional,
                        position: crate::lexer::Position::new(1, 1, 0),
                    },
                    crate::ast::ServiceOperation {
                        name: "stream_updates".to_string(),
                        input_type: TypeExpression::Reference("StreamRequest".to_string()),
                        output_type: TypeExpression::Reference("StreamResponse".to_string()),
                        direction: crate::ast::ServiceDirection::Bidirectional,
                        position: crate::lexer::Position::new(2, 1, 0),
                    },
                ],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "service UserAPI {\n  get_user: UserRequest -> UserResponse,\n  stream_updates: StreamRequest <-> StreamResponse,\n}\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_type_choice_expression() {
        let spec = create_test_spec(vec![create_test_rule(
            "flexible",
            RuleType::TypeDef(TypeExpression::Choice(vec![
                TypeExpression::Builtin("int".to_string()),
                TypeExpression::Builtin("text".to_string()),
                TypeExpression::Builtin("bool".to_string()),
            ])),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "flexible = int / text / bool\n");
    }

    #[test]
    fn test_format_range_expression() {
        let spec = create_test_spec(vec![create_test_rule(
            "small_int",
            RuleType::TypeDef(TypeExpression::Range {
                start: Some(1),
                end: Some(100),
                inclusive: true,
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(result, "small_int = 1..100\n");
    }

    #[test]
    fn test_format_socket_plug() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "socket_example",
                RuleType::TypeDef(TypeExpression::Socket("MySocket".to_string())),
            ),
            create_test_rule(
                "plug_example",
                RuleType::TypeDef(TypeExpression::Plug("MyPlug".to_string())),
            ),
        ]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        assert_eq!(
            result,
            "socket_example = $MySocket\n\nplug_example = $$MyPlug\n"
        );
    }

    #[test]
    fn test_format_literal_values() {
        let spec = create_test_spec(vec![
            create_test_rule(
                "int_literal",
                RuleType::TypeDef(TypeExpression::Literal(crate::ast::LiteralValue::Integer(
                    42,
                ))),
            ),
            create_test_rule(
                "text_literal",
                RuleType::TypeDef(TypeExpression::Literal(crate::ast::LiteralValue::Text(
                    "hello".to_string(),
                ))),
            ),
            create_test_rule(
                "bool_literal",
                RuleType::TypeDef(TypeExpression::Literal(crate::ast::LiteralValue::Bool(
                    true,
                ))),
            ),
            create_test_rule(
                "bytes_literal",
                RuleType::TypeDef(TypeExpression::Literal(crate::ast::LiteralValue::Bytes(
                    vec![0xde, 0xad, 0xbe, 0xef],
                ))),
            ),
        ]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "int_literal = 42\n\ntext_literal = \"hello\"\n\nbool_literal = true\n\nbytes_literal = h'deadbeef'\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_field_metadata() {
        let spec = create_test_spec(vec![create_test_rule(
            "annotated_group",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("send_field".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![crate::ast::FieldMetadata::Visibility(
                            crate::ast::FieldVisibility::SendOnly,
                        )],
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("constrained_field".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            crate::ast::FieldMetadata::Constraint(
                                crate::ast::ValidationConstraint::MinLength(5),
                            ),
                            crate::ast::FieldMetadata::Constraint(
                                crate::ast::ValidationConstraint::MaxLength(100),
                            ),
                        ],
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("described_field".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![crate::ast::FieldMetadata::Description(
                            "User age".to_string(),
                        )],
                    },
                ],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "annotated_group = {\n  send_field: text @send-only,\n  constrained_field: text @min-length(5) @max-length(100),\n  described_field: int @description(\"User age\"),\n}\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_occurrence_indicators() {
        let spec = create_test_spec(vec![create_test_rule(
            "occurrences",
            RuleType::GroupDef(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("optional".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: Some(crate::ast::Occurrence::Optional),
                        metadata: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("zero_or_more".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: Some(crate::ast::Occurrence::ZeroOrMore),
                        metadata: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("one_or_more".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: Some(crate::ast::Occurrence::OneOrMore),
                        metadata: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("exact_five".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: Some(crate::ast::Occurrence::Exact(5)),
                        metadata: Vec::new(),
                    },
                    GroupEntry {
                        key: Some(crate::ast::GroupKey::Bare("range_one_to_ten".to_string())),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: Some(crate::ast::Occurrence::Range {
                            min: Some(1),
                            max: Some(10),
                        }),
                        metadata: Vec::new(),
                    },
                ],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "occurrences = {\n  optional?: text,\n  zero_or_more*: text,\n  one_or_more+: text,\n  exact_five5: text,\n  range_one_to_ten1*10: text,\n}\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_json_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "JsonData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![crate::ast::ControlOperator::Json],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "JsonData = text .json\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_json_constraint_with_size() {
        let spec = create_test_spec(vec![create_test_rule(
            "ApiResponse",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                constraints: vec![
                    crate::ast::ControlOperator::Size(crate::ast::SizeConstraint::Range { min: 10, max: 1000 }),
                    crate::ast::ControlOperator::Json,
                ],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "ApiResponse = text .size (10..1000) .json\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_cbor_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "CborData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![crate::ast::ControlOperator::Cbor],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "CborData = bytes .cbor\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_cborseq_constraint() {
        let spec = create_test_spec(vec![create_test_rule(
            "CborSequence",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![crate::ast::ControlOperator::Cborseq],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "CborSequence = bytes .cborseq\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_cbor_constraint_with_size() {
        let spec = create_test_spec(vec![create_test_rule(
            "CompactData",
            RuleType::TypeDef(TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![
                    crate::ast::ControlOperator::Size(crate::ast::SizeConstraint::Range { min: 10, max: 1000 }),
                    crate::ast::ControlOperator::Cbor,
                ],
            }),
        )]);
        let config = FormatConfig::default();
        let result = format_spec(&spec, &config).unwrap();
        let expected = "CompactData = bytes .size (10..1000) .cbor\n";
        assert_eq!(result, expected);
    }
}
