//! Helper libraries for common CSIL generator tasks
//!
//! This module provides utilities that make it easier to implement CSIL generators:
//! - Type mapping from CDDL to target languages
//! - Service iteration and processing
//! - Field metadata extraction and processing
//! - Code generation utilities
//! - Warning and error generation

use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRule, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    GeneratorWarning, SourceLocation, WarningLevel,
};
use std::collections::HashMap;

/// Helper for mapping CDDL types to target language types
#[derive(Debug, Clone)]
pub struct TypeMapper {
    /// Mapping from CDDL builtin types to target language types
    builtin_mappings: HashMap<String, String>,
    /// Custom type mappings
    custom_mappings: HashMap<String, String>,
}

impl TypeMapper {
    /// Create a new type mapper with default mappings for common languages
    pub fn new() -> Self {
        Self {
            builtin_mappings: HashMap::new(),
            custom_mappings: HashMap::new(),
        }
    }

    /// Create a type mapper with predefined mappings for Rust
    pub fn rust() -> Self {
        let mut mapper = Self::new();
        mapper.add_builtin("int", "i64");
        mapper.add_builtin("uint", "u64");
        mapper.add_builtin("nint", "i64");
        mapper.add_builtin("float", "f64");
        mapper.add_builtin("float16", "f32");
        mapper.add_builtin("float32", "f32");
        mapper.add_builtin("float64", "f64");
        mapper.add_builtin("text", "String");
        mapper.add_builtin("tstr", "String");
        mapper.add_builtin("bytes", "Vec<u8>");
        mapper.add_builtin("bstr", "Vec<u8>");
        mapper.add_builtin("bool", "bool");
        mapper.add_builtin("nil", "()");
        mapper.add_builtin("null", "()");
        mapper.add_builtin("undefined", "()");
        mapper.add_builtin("any", "serde_json::Value");
        mapper
    }

    /// Create a type mapper with predefined mappings for TypeScript
    pub fn typescript() -> Self {
        let mut mapper = Self::new();
        mapper.add_builtin("int", "number");
        mapper.add_builtin("uint", "number");
        mapper.add_builtin("nint", "number");
        mapper.add_builtin("float", "number");
        mapper.add_builtin("float16", "number");
        mapper.add_builtin("float32", "number");
        mapper.add_builtin("float64", "number");
        mapper.add_builtin("text", "string");
        mapper.add_builtin("tstr", "string");
        mapper.add_builtin("bytes", "Uint8Array");
        mapper.add_builtin("bstr", "Uint8Array");
        mapper.add_builtin("bool", "boolean");
        mapper.add_builtin("nil", "null");
        mapper.add_builtin("null", "null");
        mapper.add_builtin("undefined", "undefined");
        mapper.add_builtin("any", "any");
        mapper
    }

    /// Create a type mapper with predefined mappings for Python
    pub fn python() -> Self {
        let mut mapper = Self::new();
        mapper.add_builtin("int", "int");
        mapper.add_builtin("uint", "int");
        mapper.add_builtin("nint", "int");
        mapper.add_builtin("float", "float");
        mapper.add_builtin("float16", "float");
        mapper.add_builtin("float32", "float");
        mapper.add_builtin("float64", "float");
        mapper.add_builtin("text", "str");
        mapper.add_builtin("tstr", "str");
        mapper.add_builtin("bytes", "bytes");
        mapper.add_builtin("bstr", "bytes");
        mapper.add_builtin("bool", "bool");
        mapper.add_builtin("nil", "None");
        mapper.add_builtin("null", "None");
        mapper.add_builtin("undefined", "None");
        mapper.add_builtin("any", "Any");
        mapper
    }

    /// Add a mapping for a builtin CDDL type
    pub fn add_builtin(&mut self, cddl_type: &str, target_type: &str) {
        self.builtin_mappings
            .insert(cddl_type.to_string(), target_type.to_string());
    }

    /// Add a mapping for a custom type
    pub fn add_custom(&mut self, cddl_type: &str, target_type: &str) {
        self.custom_mappings
            .insert(cddl_type.to_string(), target_type.to_string());
    }

    /// Map a CDDL type expression to target language type
    pub fn map_type(&self, expr: &CsilTypeExpression) -> String {
        match expr {
            CsilTypeExpression::Builtin(name) => {
                self.builtin_mappings.get(name).unwrap_or(name).clone()
            }
            CsilTypeExpression::Reference(name) => {
                self.custom_mappings.get(name).unwrap_or(name).clone()
            }
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let element = self.map_type(element_type);
                self.wrap_with_occurrence(&format!("Array<{}>", element), occurrence)
            }
            CsilTypeExpression::Map {
                key,
                value,
                occurrence,
            } => {
                let key_type = self.map_type(key);
                let value_type = self.map_type(value);
                let base_type = format!("Map<{}, {}>", key_type, value_type);
                self.wrap_with_occurrence(&base_type, occurrence)
            }
            CsilTypeExpression::Group(_group) => {
                "InlineGroup".to_string() // Placeholder for inline groups
            }
            CsilTypeExpression::Choice(choices) => {
                format!("Union<{}>", choices.len()) // Placeholder for choices
            }
            CsilTypeExpression::Range { start, end, .. } => match (start, end) {
                (Some(s), Some(e)) => format!("Range<{}..{}>", s, e),
                (Some(s), None) => format!("Range<{}..*>", s),
                (None, Some(e)) => format!("Range<*..{}>", e),
                (None, None) => "Range<*>".to_string(),
            },
            CsilTypeExpression::Literal(literal) => {
                format!("Literal<{:?}>", literal)
            }
            CsilTypeExpression::Socket(name) => {
                format!("Socket<{}>", name)
            }
            CsilTypeExpression::Plug(name) => {
                format!("Plug<{}>", name)
            }
        }
    }

    /// Wrap a type with occurrence information
    fn wrap_with_occurrence(&self, base_type: &str, occurrence: &Option<CsilOccurrence>) -> String {
        match occurrence {
            Some(CsilOccurrence::Optional) => format!("Optional<{}>", base_type),
            Some(CsilOccurrence::ZeroOrMore) => base_type.to_string(), // Arrays handle this
            Some(CsilOccurrence::OneOrMore) => format!("NonEmpty<{}>", base_type),
            Some(CsilOccurrence::Exact(n)) => format!("Exactly<{}, {}>", base_type, n),
            Some(CsilOccurrence::Range { min, max }) => {
                format!(
                    "Range<{}, {}..{}>",
                    base_type,
                    min.map_or("0".to_string(), |m| m.to_string()),
                    max.map_or("*".to_string(), |m| m.to_string())
                )
            }
            None => base_type.to_string(),
        }
    }
}

/// Helper for iterating through services in a CSIL specification
#[derive(Debug)]
pub struct ServiceIterator<'a> {
    spec: &'a CsilSpecSerialized,
    current_index: usize,
}

impl<'a> ServiceIterator<'a> {
    /// Create a new service iterator
    pub fn new(spec: &'a CsilSpecSerialized) -> Self {
        Self {
            spec,
            current_index: 0,
        }
    }

    /// Get all services in the specification
    pub fn collect_services(&self) -> Vec<(&'a CsilRule, &'a CsilServiceDefinition)> {
        self.spec
            .rules
            .iter()
            .filter_map(|rule| {
                if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                    Some((rule, service))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get all service operations across all services
    pub fn collect_all_operations(&self) -> Vec<ServiceOperationInfo<'a>> {
        let mut operations = Vec::new();

        for (rule, service) in self.collect_services() {
            for operation in &service.operations {
                operations.push(ServiceOperationInfo {
                    service_name: &rule.name,
                    service_rule: rule,
                    operation,
                });
            }
        }

        operations
    }

    /// Get operations by direction
    pub fn operations_by_direction(
        &self,
        direction: CsilServiceDirection,
    ) -> Vec<ServiceOperationInfo<'a>> {
        self.collect_all_operations()
            .into_iter()
            .filter(|op| op.operation.direction == direction)
            .collect()
    }
}

/// Information about a service operation with context
#[derive(Debug)]
pub struct ServiceOperationInfo<'a> {
    pub service_name: &'a str,
    pub service_rule: &'a CsilRule,
    pub operation: &'a CsilServiceOperation,
}

/// Helper for processing field metadata
#[derive(Debug, Default)]
pub struct FieldMetadataProcessor;

impl FieldMetadataProcessor {
    /// Extract visibility information from field metadata
    pub fn get_visibility(&self, metadata: &[CsilFieldMetadata]) -> CsilFieldVisibility {
        for meta in metadata {
            if let CsilFieldMetadata::Visibility(vis) = meta {
                return vis.clone();
            }
        }
        // Default visibility
        CsilFieldVisibility::Bidirectional
    }

    /// Extract validation constraints from field metadata
    pub fn get_constraints<'a>(
        &self,
        metadata: &'a [CsilFieldMetadata],
    ) -> Vec<&'a CsilValidationConstraint> {
        metadata
            .iter()
            .filter_map(|meta| {
                if let CsilFieldMetadata::Constraint(constraint) = meta {
                    Some(constraint)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Extract description from field metadata
    pub fn get_description<'a>(&self, metadata: &'a [CsilFieldMetadata]) -> Option<&'a str> {
        metadata.iter().find_map(|meta| {
            if let CsilFieldMetadata::Description(desc) = meta {
                Some(desc.as_str())
            } else {
                None
            }
        })
    }

    /// Extract dependencies from field metadata
    pub fn get_dependencies<'a>(
        &self,
        metadata: &'a [CsilFieldMetadata],
    ) -> Vec<(&'a str, Option<&'a CsilLiteralValue>)> {
        metadata
            .iter()
            .filter_map(|meta| {
                if let CsilFieldMetadata::DependsOn { field, value } = meta {
                    Some((field.as_str(), value.as_ref()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Extract custom metadata for a specific generator/language
    pub fn get_custom_metadata<'a>(
        &self,
        metadata: &'a [CsilFieldMetadata],
        target: &str,
    ) -> Vec<&'a CsilFieldMetadata> {
        metadata
            .iter()
            .filter(|meta| {
                if let CsilFieldMetadata::Custom { name, .. } = meta {
                    name == target
                } else {
                    false
                }
            })
            .collect()
    }

    /// Check if field should be included in requests (outgoing messages)
    pub fn include_in_requests(&self, metadata: &[CsilFieldMetadata]) -> bool {
        let visibility = self.get_visibility(metadata);
        matches!(
            visibility,
            CsilFieldVisibility::SendOnly | CsilFieldVisibility::Bidirectional
        )
    }

    /// Check if field should be included in responses (incoming messages)
    pub fn include_in_responses(&self, metadata: &[CsilFieldMetadata]) -> bool {
        let visibility = self.get_visibility(metadata);
        matches!(
            visibility,
            CsilFieldVisibility::ReceiveOnly | CsilFieldVisibility::Bidirectional
        )
    }
}

/// Helper for generating common code patterns
#[derive(Debug)]
pub struct CodeGenerationHelper {
    /// Indentation string (spaces or tabs)
    pub indent: String,
    /// Line ending style
    pub line_ending: String,
}

impl Default for CodeGenerationHelper {
    fn default() -> Self {
        Self {
            indent: "  ".to_string(), // 2 spaces
            line_ending: "\n".to_string(),
        }
    }
}

impl CodeGenerationHelper {
    /// Create a new code generation helper
    pub fn new(indent_size: usize, use_tabs: bool) -> Self {
        let indent = if use_tabs {
            "\t".to_string()
        } else {
            " ".repeat(indent_size)
        };

        Self {
            indent,
            line_ending: "\n".to_string(),
        }
    }

    /// Generate indented code
    pub fn indent_lines(&self, code: &str, level: usize) -> String {
        let indent_str = self.indent.repeat(level);
        code.lines()
            .map(|line| {
                if line.trim().is_empty() {
                    String::new()
                } else {
                    format!("{}{}", indent_str, line)
                }
            })
            .collect::<Vec<_>>()
            .join(&self.line_ending)
    }

    /// Generate a code block with proper indentation
    pub fn code_block<F>(&self, header: &str, content_fn: F, indent_level: usize) -> String
    where
        F: FnOnce() -> String,
    {
        let mut result = String::new();

        // Add header
        result.push_str(&self.indent.repeat(indent_level));
        result.push_str(header);
        result.push_str(&self.line_ending);

        // Add content with increased indentation
        let content = content_fn();
        if !content.trim().is_empty() {
            result.push_str(&self.indent_lines(&content, indent_level + 1));
            result.push_str(&self.line_ending);
        }

        result
    }

    /// Generate field name from CSIL group key
    pub fn field_name_from_key(&self, key: &CsilGroupKey) -> String {
        match key {
            CsilGroupKey::Bare(name) => name.clone(),
            CsilGroupKey::Type(_) => "typed_key".to_string(),
            CsilGroupKey::Literal(literal) => match literal {
                CsilLiteralValue::Text(s) => s.clone(),
                CsilLiteralValue::Integer(i) => format!("field_{}", i),
                _ => "literal_key".to_string(),
            },
        }
    }

    /// Convert field name to target language conventions
    pub fn to_camel_case(&self, snake_case: &str) -> String {
        let mut result = String::new();
        let mut capitalize_next = false;

        for ch in snake_case.chars() {
            if ch == '_' || ch == '-' {
                capitalize_next = true;
            } else if capitalize_next {
                result.push(ch.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(ch);
            }
        }

        result
    }

    /// Convert field name to snake_case
    pub fn to_snake_case(&self, camel_case: &str) -> String {
        let mut result = String::new();

        for (i, ch) in camel_case.chars().enumerate() {
            if ch.is_ascii_uppercase() && i > 0 {
                result.push('_');
            }
            result.push(ch.to_ascii_lowercase());
        }

        result
    }

    /// Convert field name to PascalCase
    pub fn to_pascal_case(&self, input: &str) -> String {
        let camel = self.to_camel_case(input);
        if let Some(first_char) = camel.chars().next() {
            format!("{}{}", first_char.to_ascii_uppercase(), &camel[1..])
        } else {
            camel
        }
    }

    /// Generate a comment block
    pub fn comment_block(
        &self,
        lines: &[&str],
        indent_level: usize,
        comment_prefix: &str,
    ) -> String {
        let indent_str = self.indent.repeat(indent_level);
        lines
            .iter()
            .map(|line| {
                if line.trim().is_empty() {
                    format!("{}{}", indent_str, comment_prefix)
                } else {
                    format!("{}{} {}", indent_str, comment_prefix, line)
                }
            })
            .collect::<Vec<_>>()
            .join(&self.line_ending)
    }
}

/// Helper for generating warnings and validation messages
#[derive(Debug)]
pub struct WarningGenerator;

impl WarningGenerator {
    /// Check for fields that might need visibility annotations
    pub fn check_visibility_annotations(&self, spec: &CsilSpecSerialized) -> Vec<GeneratorWarning> {
        let mut warnings = Vec::new();

        for rule in &spec.rules {
            if let CsilRuleType::GroupDef(group) = &rule.rule_type {
                for entry in &group.entries {
                    if let Some(key) = &entry.key {
                        let field_name = match key {
                            CsilGroupKey::Bare(name) => name.as_str(),
                            _ => continue,
                        };

                        // Check if field looks sensitive but has no visibility annotation
                        if self.is_potentially_sensitive_field(field_name) {
                            let has_visibility = entry
                                .metadata
                                .iter()
                                .any(|m| matches!(m, CsilFieldMetadata::Visibility(_)));

                            if !has_visibility {
                                warnings.push(GeneratorWarning {
                                    level: WarningLevel::Warning,
                                    message: format!("Potentially sensitive field '{}' has no visibility annotation", field_name),
                                    location: Some(SourceLocation {
                                        line: rule.position.line,
                                        column: rule.position.column,
                                        offset: rule.position.offset,
                                        context: Some(format!("{}.{}", rule.name, field_name)),
                                    }),
                                    suggestion: Some("Consider adding @send-only, @receive-only, or @bidirectional annotation".to_string()),
                                });
                            }
                        }
                    }
                }
            }
        }

        warnings
    }

    /// Check for missing service documentation
    pub fn check_service_documentation(&self, spec: &CsilSpecSerialized) -> Vec<GeneratorWarning> {
        let mut warnings = Vec::new();

        for rule in &spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                for operation in &service.operations {
                    // Check if operation might benefit from documentation
                    if operation.name.len() > 15 || operation.name.contains('_') {
                        warnings.push(GeneratorWarning {
                            level: WarningLevel::Info,
                            message: format!(
                                "Operation '{}' might benefit from documentation",
                                operation.name
                            ),
                            location: Some(SourceLocation {
                                line: operation.position.line,
                                column: operation.position.column,
                                offset: operation.position.offset,
                                context: Some(format!("{}.{}", rule.name, operation.name)),
                            }),
                            suggestion: Some(
                                "Consider adding @description annotation to the operation"
                                    .to_string(),
                            ),
                        });
                    }
                }
            }
        }

        warnings
    }

    /// Check for inconsistent naming conventions
    pub fn check_naming_conventions(&self, spec: &CsilSpecSerialized) -> Vec<GeneratorWarning> {
        let mut warnings = Vec::new();
        let mut naming_styles = HashMap::new();

        // Analyze naming patterns
        for rule in &spec.rules {
            let style = self.detect_naming_style(&rule.name);
            *naming_styles.entry(style).or_insert(0) += 1;
        }

        // If there are multiple styles, warn about inconsistency
        if naming_styles.len() > 1 {
            let most_common_style = naming_styles
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(style, _)| *style)
                .unwrap_or(NamingStyle::Unknown);

            for rule in &spec.rules {
                let rule_style = self.detect_naming_style(&rule.name);
                if rule_style != most_common_style && most_common_style != NamingStyle::Unknown {
                    warnings.push(GeneratorWarning {
                        level: WarningLevel::Info,
                        message: format!(
                            "Rule '{}' uses {:?} naming but most rules use {:?}",
                            rule.name, rule_style, most_common_style
                        ),
                        location: Some(SourceLocation {
                            line: rule.position.line,
                            column: rule.position.column,
                            offset: rule.position.offset,
                            context: Some(rule.name.clone()),
                        }),
                        suggestion: Some(format!(
                            "Consider using {:?} naming for consistency",
                            most_common_style
                        )),
                    });
                }
            }
        }

        warnings
    }

    /// Check if a field name suggests it might contain sensitive data
    fn is_potentially_sensitive_field(&self, field_name: &str) -> bool {
        let sensitive_patterns = [
            "password",
            "secret",
            "token",
            "key",
            "auth",
            "credential",
            "private",
            "confidential",
            "sensitive",
            "ssn",
            "social",
            "api_key",
            "access_token",
            "refresh_token",
            "session",
        ];

        let lower_name = field_name.to_lowercase();
        sensitive_patterns
            .iter()
            .any(|pattern| lower_name.contains(pattern))
    }

    /// Detect the naming style of an identifier
    fn detect_naming_style(&self, name: &str) -> NamingStyle {
        if name.chars().any(|c| c.is_ascii_uppercase()) {
            if name
                .chars()
                .nth(0)
                .map_or(false, |c| c.is_ascii_uppercase())
            {
                NamingStyle::PascalCase
            } else {
                NamingStyle::CamelCase
            }
        } else if name.contains('_') {
            NamingStyle::SnakeCase
        } else if name.contains('-') {
            NamingStyle::KebabCase
        } else {
            NamingStyle::Unknown
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NamingStyle {
    CamelCase,
    PascalCase,
    SnakeCase,
    KebabCase,
    Unknown,
}

/// Utility function to get field information with metadata
pub fn extract_field_info(entry: &CsilGroupEntry) -> FieldInfo {
    let processor = FieldMetadataProcessor;

    FieldInfo {
        name: match &entry.key {
            Some(key) => {
                let helper = CodeGenerationHelper::default();
                helper.field_name_from_key(key)
            }
            None => "anonymous".to_string(),
        },
        type_expr: entry.value_type.clone(),
        occurrence: entry.occurrence.clone(),
        visibility: processor.get_visibility(&entry.metadata),
        description: processor
            .get_description(&entry.metadata)
            .map(|s| s.to_string()),
        constraints: processor
            .get_constraints(&entry.metadata)
            .into_iter()
            .cloned()
            .collect(),
        dependencies: processor
            .get_dependencies(&entry.metadata)
            .into_iter()
            .map(|(field, value)| (field.to_string(), value.cloned()))
            .collect(),
        include_in_requests: processor.include_in_requests(&entry.metadata),
        include_in_responses: processor.include_in_responses(&entry.metadata),
    }
}

/// Processed field information for easier code generation
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub type_expr: CsilTypeExpression,
    pub occurrence: Option<CsilOccurrence>,
    pub visibility: CsilFieldVisibility,
    pub description: Option<String>,
    pub constraints: Vec<CsilValidationConstraint>,
    pub dependencies: Vec<(String, Option<CsilLiteralValue>)>,
    pub include_in_requests: bool,
    pub include_in_responses: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::*;

    #[test]
    fn test_type_mapper_rust() {
        let mapper = TypeMapper::rust();
        assert_eq!(
            mapper.map_type(&CsilTypeExpression::Builtin("text".to_string())),
            "String"
        );
        assert_eq!(
            mapper.map_type(&CsilTypeExpression::Builtin("int".to_string())),
            "i64"
        );
        assert_eq!(
            mapper.map_type(&CsilTypeExpression::Reference("User".to_string())),
            "User"
        );
    }

    #[test]
    fn test_type_mapper_array() {
        let mapper = TypeMapper::rust();
        let array_type = CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
            occurrence: Some(CsilOccurrence::Optional),
        };
        let result = mapper.map_type(&array_type);
        assert!(result.contains("String"));
        assert!(result.contains("Optional"));
    }

    #[test]
    fn test_service_iterator() {
        let spec = create_test_spec_with_service();
        let iterator = ServiceIterator::new(&spec);

        let services = iterator.collect_services();
        assert_eq!(services.len(), 1);

        let operations = iterator.collect_all_operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operation.name, "test_operation");
    }

    #[test]
    fn test_field_metadata_processor() {
        let processor = FieldMetadataProcessor;
        let metadata = vec![
            CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly),
            CsilFieldMetadata::Description("Test field".to_string()),
        ];

        assert_eq!(
            processor.get_visibility(&metadata),
            CsilFieldVisibility::SendOnly
        );
        assert_eq!(processor.get_description(&metadata), Some("Test field"));
        assert!(processor.include_in_requests(&metadata));
        assert!(!processor.include_in_responses(&metadata));
    }

    #[test]
    fn test_code_generation_helper() {
        let helper = CodeGenerationHelper::new(4, false);

        assert_eq!(helper.to_camel_case("user_name"), "userName");
        assert_eq!(helper.to_snake_case("userName"), "user_name");
        assert_eq!(helper.to_pascal_case("user_name"), "UserName");

        let indented = helper.indent_lines("line1\nline2", 1);
        assert!(indented.starts_with("    ")); // 4 spaces
    }

    #[test]
    fn test_warning_generator() {
        let generator = WarningGenerator;
        let spec = create_test_spec_with_sensitive_field();

        let warnings = generator.check_visibility_annotations(&spec);
        assert!(!warnings.is_empty());
        assert!(warnings[0].message.contains("password"));
    }

    fn create_test_spec_with_service() -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "TestService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "test_operation".to_string(),
                        input_type: CsilTypeExpression::Builtin("text".to_string()),
                        output_type: CsilTypeExpression::Builtin("text".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 1,
                            column: 1,
                            offset: 0,
                        },
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    fn create_test_spec_with_sensitive_field() -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("password".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![], // No visibility annotation
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        }
    }
}
