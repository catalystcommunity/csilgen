//! Python code generator for csilgen

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    CsilgenError, GeneratedFile, GeneratedFiles, GeneratorConfig, Result,
};
use csilgen_core::ast::CsilSpec;
use std::collections::HashSet;

/// Generate Python dataclasses from CDDL specification (legacy interface)
pub fn generate_python_code(spec: &CsilSpec, config: &GeneratorConfig) -> Result<GeneratedFiles> {
    let serialized_spec = convert_csil_spec_to_serialized(spec);
    generate_python_code_from_serialized(&serialized_spec, config)
}

fn convert_csil_spec_to_serialized(spec: &CsilSpec) -> CsilSpecSerialized {
    use csilgen_common::*;

    let mut service_count = 0;
    let mut fields_with_metadata_count = 0;

    let rules: Vec<_> = spec
        .rules
        .iter()
        .map(|rule| {
            if matches!(rule.rule_type, csilgen_core::ast::RuleType::ServiceDef(_)) {
                service_count += 1;
            }

            if let csilgen_core::ast::RuleType::TypeDef(csilgen_core::ast::TypeExpression::Group(
                group,
            )) = &rule.rule_type
            {
                fields_with_metadata_count += group
                    .entries
                    .iter()
                    .filter(|entry| !entry.metadata.is_empty())
                    .count();
            }

            CsilRule {
                name: rule.name.clone(),
                rule_type: convert_rule_type(&rule.rule_type),
                position: convert_position(&rule.position),
            }
        })
        .collect();

    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count,
        fields_with_metadata_count,
    }
}

fn convert_rule_type(rule_type: &csilgen_core::ast::RuleType) -> CsilRuleType {
    match rule_type {
        csilgen_core::ast::RuleType::TypeDef(type_expr) => {
            CsilRuleType::TypeDef(convert_type_expression(type_expr))
        }
        csilgen_core::ast::RuleType::GroupDef(group) => {
            CsilRuleType::GroupDef(convert_group_expression(group))
        }
        csilgen_core::ast::RuleType::GroupChoice(groups) => {
            CsilRuleType::GroupChoice(groups.iter().map(convert_group_expression).collect())
        }
        csilgen_core::ast::RuleType::TypeChoice(types) => {
            CsilRuleType::TypeChoice(types.iter().map(convert_type_expression).collect())
        }
        csilgen_core::ast::RuleType::ServiceDef(service) => {
            CsilRuleType::ServiceDef(convert_service_definition(service))
        }
    }
}

fn convert_type_expression(type_expr: &csilgen_core::ast::TypeExpression) -> CsilTypeExpression {
    match type_expr {
        csilgen_core::ast::TypeExpression::Builtin(name) => {
            CsilTypeExpression::Builtin(name.clone())
        }
        csilgen_core::ast::TypeExpression::Reference(name) => {
            CsilTypeExpression::Reference(name.clone())
        }
        csilgen_core::ast::TypeExpression::Array {
            element_type,
            occurrence,
        } => CsilTypeExpression::Array {
            element_type: Box::new(convert_type_expression(element_type)),
            occurrence: occurrence.as_ref().map(convert_occurrence),
        },
        csilgen_core::ast::TypeExpression::Map {
            key,
            value,
            occurrence,
        } => CsilTypeExpression::Map {
            key: Box::new(convert_type_expression(key)),
            value: Box::new(convert_type_expression(value)),
            occurrence: occurrence.as_ref().map(convert_occurrence),
        },
        csilgen_core::ast::TypeExpression::Group(group) => {
            CsilTypeExpression::Group(convert_group_expression(group))
        }
        csilgen_core::ast::TypeExpression::Choice(choices) => {
            CsilTypeExpression::Choice(choices.iter().map(convert_type_expression).collect())
        }
        csilgen_core::ast::TypeExpression::Literal(literal) => {
            CsilTypeExpression::Literal(convert_literal_value(literal))
        }
        csilgen_core::ast::TypeExpression::Socket(name) => CsilTypeExpression::Socket(name.clone()),
        csilgen_core::ast::TypeExpression::Plug(name) => CsilTypeExpression::Plug(name.clone()),
        csilgen_core::ast::TypeExpression::Range {
            start,
            end,
            inclusive,
        } => CsilTypeExpression::Range {
            start: *start,
            end: *end,
            inclusive: *inclusive,
        },
        csilgen_core::ast::TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            // Convert base type and track if we have .json constraint
            let base_converted = convert_type_expression(base_type);
            
            // Check for constraints
            for constraint in constraints {
                match constraint {
                    csilgen_core::ast::ControlOperator::Json => {
                        // For .json constraint, the type should be a string that contains valid JSON
                        // In Python, this is still str but with validation
                        if matches!(base_converted, CsilTypeExpression::Builtin(ref s) if s == "str" || s == "bytes") {
                            // Keep the base type but note that validation would check JSON validity
                        }
                    }
                    csilgen_core::ast::ControlOperator::Cbor | csilgen_core::ast::ControlOperator::Cborseq => {
                        // For .cbor and .cborseq constraints, the type should be bytes
                        // In Python, this is bytes but with validation for CBOR format
                        if matches!(base_converted, CsilTypeExpression::Builtin(ref s) if s == "bytes") {
                            // Keep the base type but note that validation would check CBOR validity
                        }
                    }
                    _ => {}
                }
            }
            
            base_converted
        }
    }
}

fn convert_group_expression(group: &csilgen_core::ast::GroupExpression) -> CsilGroupExpression {
    CsilGroupExpression {
        entries: group.entries.iter().map(convert_group_entry).collect(),
    }
}

fn convert_group_entry(entry: &csilgen_core::ast::GroupEntry) -> CsilGroupEntry {
    CsilGroupEntry {
        key: entry.key.as_ref().map(convert_group_key),
        value_type: convert_type_expression(&entry.value_type),
        occurrence: entry.occurrence.as_ref().map(convert_occurrence),
        metadata: entry.metadata.iter().map(convert_field_metadata).collect(),
    }
}

fn convert_group_key(key: &csilgen_core::ast::GroupKey) -> CsilGroupKey {
    match key {
        csilgen_core::ast::GroupKey::Bare(name) => CsilGroupKey::Bare(name.clone()),
        csilgen_core::ast::GroupKey::Type(type_expr) => {
            CsilGroupKey::Type(convert_type_expression(type_expr))
        }
        csilgen_core::ast::GroupKey::Literal(literal) => {
            CsilGroupKey::Literal(convert_literal_value(literal))
        }
    }
}

fn convert_service_definition(
    service: &csilgen_core::ast::ServiceDefinition,
) -> CsilServiceDefinition {
    CsilServiceDefinition {
        operations: service
            .operations
            .iter()
            .map(convert_service_operation)
            .collect(),
    }
}

fn convert_service_operation(
    operation: &csilgen_core::ast::ServiceOperation,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: operation.name.clone(),
        input_type: convert_type_expression(&operation.input_type),
        output_type: convert_type_expression(&operation.output_type),
        direction: convert_service_direction(&operation.direction),
        position: convert_position(&operation.position),
    }
}

fn convert_service_direction(
    direction: &csilgen_core::ast::ServiceDirection,
) -> CsilServiceDirection {
    match direction {
        csilgen_core::ast::ServiceDirection::Unidirectional => CsilServiceDirection::Unidirectional,
        csilgen_core::ast::ServiceDirection::Bidirectional => CsilServiceDirection::Bidirectional,
        csilgen_core::ast::ServiceDirection::Reverse => CsilServiceDirection::Reverse,
    }
}

fn convert_occurrence(occurrence: &csilgen_core::ast::Occurrence) -> CsilOccurrence {
    match occurrence {
        csilgen_core::ast::Occurrence::Optional => CsilOccurrence::Optional,
        csilgen_core::ast::Occurrence::ZeroOrMore => CsilOccurrence::ZeroOrMore,
        csilgen_core::ast::Occurrence::OneOrMore => CsilOccurrence::OneOrMore,
        csilgen_core::ast::Occurrence::Exact(count) => CsilOccurrence::Exact(*count),
        csilgen_core::ast::Occurrence::Range { min, max } => CsilOccurrence::Range {
            min: *min,
            max: *max,
        },
    }
}

fn convert_field_metadata(metadata: &csilgen_core::ast::FieldMetadata) -> CsilFieldMetadata {
    match metadata {
        csilgen_core::ast::FieldMetadata::Visibility(visibility) => {
            CsilFieldMetadata::Visibility(convert_field_visibility(visibility))
        }
        csilgen_core::ast::FieldMetadata::DependsOn { field, value } => {
            CsilFieldMetadata::DependsOn {
                field: field.clone(),
                value: value.as_ref().map(convert_literal_value),
            }
        }
        csilgen_core::ast::FieldMetadata::Constraint(constraint) => {
            CsilFieldMetadata::Constraint(convert_validation_constraint(constraint))
        }
        csilgen_core::ast::FieldMetadata::Description(desc) => {
            CsilFieldMetadata::Description(desc.clone())
        }
        csilgen_core::ast::FieldMetadata::Custom { name, parameters } => {
            CsilFieldMetadata::Custom {
                name: name.clone(),
                parameters: parameters.iter().map(convert_metadata_parameter).collect(),
            }
        }
    }
}

fn convert_field_visibility(
    visibility: &csilgen_core::ast::FieldVisibility,
) -> CsilFieldVisibility {
    match visibility {
        csilgen_core::ast::FieldVisibility::SendOnly => CsilFieldVisibility::SendOnly,
        csilgen_core::ast::FieldVisibility::ReceiveOnly => CsilFieldVisibility::ReceiveOnly,
        csilgen_core::ast::FieldVisibility::Bidirectional => CsilFieldVisibility::Bidirectional,
    }
}

fn convert_validation_constraint(
    constraint: &csilgen_core::ast::ValidationConstraint,
) -> CsilValidationConstraint {
    match constraint {
        csilgen_core::ast::ValidationConstraint::MinLength(val) => {
            CsilValidationConstraint::MinLength(*val)
        }
        csilgen_core::ast::ValidationConstraint::MaxLength(val) => {
            CsilValidationConstraint::MaxLength(*val)
        }
        csilgen_core::ast::ValidationConstraint::MinItems(val) => {
            CsilValidationConstraint::MinItems(*val)
        }
        csilgen_core::ast::ValidationConstraint::MaxItems(val) => {
            CsilValidationConstraint::MaxItems(*val)
        }
        csilgen_core::ast::ValidationConstraint::MinValue(value) => {
            CsilValidationConstraint::MinValue(convert_literal_value(value))
        }
        csilgen_core::ast::ValidationConstraint::MaxValue(value) => {
            CsilValidationConstraint::MaxValue(convert_literal_value(value))
        }
        csilgen_core::ast::ValidationConstraint::Custom { name, value } => {
            CsilValidationConstraint::Custom {
                name: name.clone(),
                value: convert_literal_value(value),
            }
        }
    }
}

fn convert_metadata_parameter(
    param: &csilgen_core::ast::MetadataParameter,
) -> csilgen_common::CsilMetadataParameter {
    csilgen_common::CsilMetadataParameter {
        name: param.name.clone(),
        value: convert_literal_value(&param.value),
    }
}

fn convert_literal_value(literal: &csilgen_core::ast::LiteralValue) -> CsilLiteralValue {
    match literal {
        csilgen_core::ast::LiteralValue::Integer(val) => CsilLiteralValue::Integer(*val),
        csilgen_core::ast::LiteralValue::Float(val) => CsilLiteralValue::Float(*val),
        csilgen_core::ast::LiteralValue::Text(val) => CsilLiteralValue::Text(val.clone()),
        csilgen_core::ast::LiteralValue::Bytes(val) => CsilLiteralValue::Bytes(val.clone()),
        csilgen_core::ast::LiteralValue::Bool(val) => CsilLiteralValue::Bool(*val),
        csilgen_core::ast::LiteralValue::Null => CsilLiteralValue::Null,
        csilgen_core::ast::LiteralValue::Array(elements) => {
            CsilLiteralValue::Array(elements.iter().map(convert_literal_value).collect())
        }
    }
}

fn convert_position(position: &csilgen_core::lexer::Position) -> csilgen_common::CsilPosition {
    csilgen_common::CsilPosition {
        line: position.line,
        column: position.column,
        offset: position.offset,
    }
}

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

/// Generate Python dataclasses from serialized CDDL specification
pub fn generate_python_code_from_serialized(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<GeneratedFiles> {
    let mut generator = PythonGenerator::new(config);
    generator.generate(spec)
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
        let mut files = Vec::new();

        self.setup_imports();

        let mut types_code = String::new();
        let mut services_code = String::new();

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
                CsilRuleType::ServiceDef(service) => {
                    services_code.push_str(&self.generate_service_client(&rule.name, service)?);
                    services_code.push_str(&self.generate_service_server(&rule.name, service)?);
                }
            }
        }

        if !types_code.is_empty() {
            let types_file = self.generate_types_file(types_code)?;
            files.push(types_file);
        }

        if !services_code.is_empty() {
            let services_file = self.generate_services_file(services_code)?;
            files.push(services_file);
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

    fn generate_type_def(&mut self, name: &str, type_expr: &CsilTypeExpression) -> Result<String> {
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
            for entry in &group.entries {
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
        let field_name = match &entry.key {
            Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => name.to_case(Case::Snake),
            _ => "field".to_string(),
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
            let default_value = if is_optional { " = None" } else { "" };
            field_definition.push_str(&format!("    {field_name}: {field_type}{default_value}\n"));
        }

        Ok(field_definition)
    }

    fn generate_pydantic_field_config(&self, entry: &CsilGroupEntry) -> Result<String> {
        let mut config_parts = Vec::new();

        if let Some(description) = self.get_field_description(&entry.metadata) {
            config_parts.push(format!(
                "description=\"{}\"",
                description.replace('"', "\\\"")
            ));
        }

        for metadata in &entry.metadata {
            match metadata {
                CsilFieldMetadata::Constraint(constraint) => match constraint {
                    CsilValidationConstraint::MinLength(min) => {
                        config_parts.push(format!("min_length={min}"));
                    }
                    CsilValidationConstraint::MaxLength(max) => {
                        config_parts.push(format!("max_length={max}"));
                    }
                    CsilValidationConstraint::MinItems(min) => {
                        config_parts.push(format!("min_items={min}"));
                    }
                    CsilValidationConstraint::MaxItems(max) => {
                        config_parts.push(format!("max_items={max}"));
                    }
                    _ => {}
                },
                CsilFieldMetadata::Custom { name, parameters } if name == "pydantic" => {
                    for param in parameters {
                        if let Some(param_name) = &param.name {
                            match &param.value {
                                CsilLiteralValue::Text(value) => {
                                    config_parts.push(format!("{param_name}=\"{value}\""));
                                }
                                CsilLiteralValue::Bool(value) => {
                                    config_parts.push(format!("{param_name}={value}"));
                                }
                                CsilLiteralValue::Integer(value) => {
                                    config_parts.push(format!("{param_name}={value}"));
                                }
                                _ => {}
                            }
                        }
                    }
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
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    name.to_case(Case::Snake)
                }
                _ => continue,
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
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    name.to_case(Case::Snake)
                }
                _ => continue,
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
        let mut code = String::new();

        let dependencies: Vec<_> = entries
            .iter()
            .filter_map(|entry| {
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::DependsOn { field, value } = metadata {
                        let field_name = match &entry.key {
                            Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                                name.to_case(Case::Snake)
                            }
                            _ => continue,
                        };
                        return Some((field_name, field.clone(), value.clone()));
                    }
                }
                None
            })
            .collect();

        if !dependencies.is_empty() {
            code.push_str("    def validate(self) -> bool:\n");
            code.push_str("        \"\"\"Validate field dependencies and constraints.\"\"\"\n");

            for (field_name, depends_on_field, depends_on_value) in &dependencies {
                let dep_field_name = depends_on_field.to_case(Case::Snake);

                match depends_on_value {
                    Some(value) => {
                        let value_str = csil_literal_to_python_str(value);

                        code.push_str(&format!(
                            "        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"
                        ));
                        code.push_str(&format!(
                            "            if not (hasattr(self, '{dep_field_name}') and self.{dep_field_name} == {value_str}):\n"
                        ));
                        code.push_str(&format!(
                            "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be {value_str}\")\n"
                        ));
                    }
                    None => {
                        code.push_str(&format!(
                            "        if hasattr(self, '{field_name}') and self.{field_name} is not None:\n"
                        ));
                        code.push_str(&format!(
                            "            if not (hasattr(self, '{dep_field_name}') and self.{dep_field_name} is not None):\n"
                        ));
                        code.push_str(&format!(
                            "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be present\")\n"
                        ));
                    }
                }
            }

            code.push_str("        return True\n\n");

            code.push_str("    def __post_init__(self):\n");
            code.push_str("        \"\"\"Validate object after initialization.\"\"\"\n");
            code.push_str("        self.validate()\n\n");
        }

        Ok(code)
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
                        let field_name = match &entry.key {
                            Some(CsilGroupKey::Bare(name)) => name.to_case(Case::Snake),
                            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                                name.to_case(Case::Snake)
                            }
                            _ => continue,
                        };
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

                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!(
                        "            if '{dep_field_name}' not in values or values['{dep_field_name}'] != {value_str}:\n"
                    ));
                    code.push_str(&format!(
                        "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be {value_str}\")\n"
                    ));
                }
                None => {
                    code.push_str("        if v is not None:\n");
                    code.push_str(&format!(
                        "            if '{dep_field_name}' not in values or values['{dep_field_name}'] is None:\n"
                    ));
                    code.push_str(&format!(
                        "                raise ValueError(\"Field '{field_name}' requires '{dep_field_name}' to be present\")\n"
                    ));
                }
            }

            code.push_str("        return v\n\n");
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

    fn generate_service_client(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String> {
        let class_name = format!("{}Client", name.to_case(Case::Pascal));
        self.generated_types.insert(class_name.clone());

        let mut code = String::new();

        if self.use_pydantic {
            code.push_str(&format!("class {class_name}(BaseModel):\n"));
            code.push_str("    \"\"\"Client for {} service operations.\"\"\"\n\n");
        } else {
            code.push_str(&format!("class {class_name}:\n"));
            code.push_str(&format!(
                "    \"\"\"Client for {name} service operations.\"\"\"\n\n"
            ));
        }

        code.push_str("    def __init__(self, endpoint: str = None):\n");
        code.push_str("        self.endpoint = endpoint\n\n");

        for operation in &service.operations {
            code.push_str(&self.generate_service_method(operation)?);
        }

        code.push('\n');
        Ok(code)
    }

    fn generate_service_method(&self, operation: &CsilServiceOperation) -> Result<String> {
        let method_name = operation.name.to_case(Case::Snake);
        let input_type = self.map_type_expression(&operation.input_type)?;
        let output_type = self.map_type_expression(&operation.output_type)?;

        let mut code = String::new();

        code.push_str(&format!(
            "    def {method_name}(self, request: {input_type}) -> {output_type}:\n"
        ));
        let operation_name = &operation.name;
        code.push_str(&format!(
            "        \"\"\"Execute {operation_name} operation.\"\"\"\n"
        ));

        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                code.push_str("        # Unidirectional operation: client -> server\n");
                code.push_str("        # Override this method in your implementation\n");
                code.push_str(
                    "        raise NotImplementedError(f\"Unidirectional operation '{operation_name}' not implemented\")\n\n",
                );
            }
            CsilServiceDirection::Bidirectional => {
                code.push_str("        # Bidirectional operation: client <-> server\n");
                code.push_str("        # Override this method in your implementation\n");
                code.push_str(
                    "        # Note: This operation can maintain persistent connections\n",
                );
                code.push_str("        raise NotImplementedError(f\"Bidirectional operation '{operation_name}' not implemented\")\n\n");
            }
            CsilServiceDirection::Reverse => {
                code.push_str("        # Reverse operation: server -> client (callback/push)\n");
                code.push_str("        # Override this method in your implementation\n");
                code.push_str(
                    "        # Note: This is typically used for server-initiated notifications\n",
                );
                code.push_str("        raise NotImplementedError(f\"Reverse operation '{operation_name}' not implemented\")\n\n");
            }
        }

        Ok(code)
    }

    fn generate_service_server(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String> {
        let server_class_name = format!("{}Server", name.to_case(Case::Pascal));
        let handler_class_name = format!("{}Handler", name.to_case(Case::Pascal));
        self.generated_types.insert(server_class_name.clone());
        self.generated_types.insert(handler_class_name.clone());

        let mut code = String::new();

        // Generate abstract handler base class
        code.push_str("from abc import ABC, abstractmethod\n\n");
        code.push_str(&format!("class {handler_class_name}(ABC):\n"));
        code.push_str(&format!(
            "    \"\"\"Abstract handler for {name} service operations.\"\"\"\n\n"
        ));

        for operation in &service.operations {
            code.push_str(&self.generate_server_handler_method(operation)?);
        }

        code.push('\n');

        // Generate concrete server class with routing
        code.push_str(&format!("class {server_class_name}:\n"));
        code.push_str(&format!(
            "    \"\"\"Server implementation for {name} service.\"\"\"\n\n"
        ));

        code.push_str(&format!(
            "    def __init__(self, handler: {handler_class_name}):\n"
        ));
        code.push_str("        self.handler = handler\n\n");

        code.push_str("    def dispatch(self, operation: str, request_data: dict) -> dict:\n");
        code.push_str("        \"\"\"Dispatch operation to appropriate handler method.\"\"\"\n");

        for operation in &service.operations {
            let method_name = operation.name.to_case(Case::Snake);
            code.push_str(&format!(
                "        if operation == \"{}\":\n",
                operation.name
            ));
            code.push_str(&format!(
                "            return self._handle_{method_name}(request_data)\n"
            ));
        }

        code.push_str("        raise ValueError(f\"Unknown operation: {operation}\")\n\n");

        // Generate handler wrapper methods
        for operation in &service.operations {
            code.push_str(&self.generate_server_dispatch_method(operation)?);
        }

        code.push('\n');
        Ok(code)
    }

    fn generate_server_handler_method(&self, operation: &CsilServiceOperation) -> Result<String> {
        let method_name = operation.name.to_case(Case::Snake);
        let input_type = self.map_type_expression(&operation.input_type)?;
        let output_type = self.map_type_expression(&operation.output_type)?;

        let mut code = String::new();

        code.push_str("    @abstractmethod\n");
        code.push_str(&format!(
            "    def {method_name}(self, request: {input_type}) -> {output_type}:\n"
        ));
        code.push_str(&format!(
            "        \"\"\"Handle {} operation.\"\"\"\n",
            operation.name
        ));
        code.push_str("        pass\n\n");

        Ok(code)
    }

    fn generate_server_dispatch_method(&self, operation: &CsilServiceOperation) -> Result<String> {
        let method_name = operation.name.to_case(Case::Snake);
        let _input_type = self.map_type_expression(&operation.input_type)?;
        let _output_type = self.map_type_expression(&operation.output_type)?;

        let mut code = String::new();

        code.push_str(&format!(
            "    def _handle_{method_name}(self, request_data: dict) -> dict:\n"
        ));
        code.push_str(&format!(
            "        \"\"\"Handle {} operation with serialization.\"\"\"\n",
            operation.name
        ));

        // Handle different input types
        match &operation.input_type {
            CsilTypeExpression::Builtin(builtin) => match builtin.as_str() {
                "text" | "tstr" => {
                    code.push_str("        request = request_data.get('value', '')\n");
                }
                "int" | "uint" => {
                    code.push_str("        request = request_data.get('value', 0)\n");
                }
                _ => {
                    code.push_str("        request = request_data\n");
                }
            },
            CsilTypeExpression::Reference(type_name) => {
                let class_name = type_name.to_case(Case::Pascal);
                code.push_str(&format!(
                    "        request = {class_name}.from_dict(request_data)\n"
                ));
            }
            _ => {
                code.push_str("        request = request_data\n");
            }
        }

        code.push_str(&format!(
            "        result = self.handler.{method_name}(request)\n"
        ));

        // Handle different output types
        match &operation.output_type {
            CsilTypeExpression::Builtin(builtin) => match builtin.as_str() {
                "text" | "tstr" | "int" | "uint" | "bool" => {
                    code.push_str("        return {'value': result}\n");
                }
                _ => {
                    code.push_str("        return result if isinstance(result, dict) else {'value': result}\n");
                }
            },
            CsilTypeExpression::Reference(_) => {
                code.push_str(
                    "        return result.to_dict() if hasattr(result, 'to_dict') else result\n",
                );
            }
            _ => {
                code.push_str(
                    "        return result if isinstance(result, dict) else {'value': result}\n",
                );
            }
        }

        code.push('\n');

        Ok(code)
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
            "int" | "uint" => "int",
            "float" | "double" => "float",
            "text" | "tstr" => "str",
            "bytes" | "bstr" => "bytes",
            "bool" => "bool",
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
        content.push_str("# Do not edit this file manually\n\n");

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

    fn generate_services_file(&self, services_code: String) -> Result<GeneratedFile> {
        let mut content = String::new();

        content.push_str("# Generated service clients from CSIL specification\n");
        content.push_str("# Do not edit this file manually\n\n");

        for import in &self.imports {
            content.push_str(import);
            content.push('\n');
        }
        content.push_str("from .types import *\n");

        content.push_str("\n\n");
        content.push_str(&services_code);

        Ok(GeneratedFile {
            path: "services.py".to_string(),
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
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("email".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![],
                        },
                    ],
                }),
                position: create_test_position(),
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
                    }],
                }),
                position: create_test_position(),
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
    fn test_generate_service_client_and_server() {
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
                    }],
                }),
                position: create_test_position(),
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        assert_eq!(result.len(), 2); // services.py and __init__.py

        let services_file = result.iter().find(|f| f.path == "services.py").unwrap();

        // Test client generation
        assert!(services_file.content.contains("class UserServiceClient:"));
        assert!(
            services_file
                .content
                .contains("def create_user(self, request: str) -> str:")
        );

        // Test server generation
        assert!(
            services_file
                .content
                .contains("class UserServiceHandler(ABC):")
        );
        assert!(services_file.content.contains("class UserServiceServer:"));
        assert!(
            services_file
                .content
                .contains("def dispatch(self, operation: str, request_data: dict) -> dict:")
        );
        assert!(
            services_file
                .content
                .contains("def _handle_create_user(self, request_data: dict) -> dict:")
        );
        assert!(
            services_file
                .content
                .contains("from abc import ABC, abstractmethod")
        );
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
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("timestamp".to_string())),
                            value_type: CsilTypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                        },
                    ],
                }),
                position: create_test_position(),
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
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("extra_data".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::DependsOn {
                                field: "type".to_string(),
                                value: Some(CsilLiteralValue::Text("advanced".to_string())),
                            }],
                        },
                    ],
                }),
                position: create_test_position(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        };

        let config = create_test_config(false);
        let result = generate_python_code_from_serialized(&spec, &config).unwrap();

        let types_file = result.iter().find(|f| f.path == "types.py").unwrap();
        assert!(types_file.content.contains("def validate(self)"));
        assert!(
            types_file
                .content
                .contains("Field 'extra_data' requires 'type' to be \"advanced\"")
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
                        },
                    ],
                }),
                position: create_test_position(),
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
                    }],
                }),
                position: create_test_position(),
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
                },
                CsilRule {
                    name: "UserService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![],
                    }),
                    position: create_test_position(),
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
}
