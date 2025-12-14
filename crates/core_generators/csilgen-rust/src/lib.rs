//! Rust code generator for csilgen

use csilgen_common::{GeneratedFile, GeneratedFiles, GeneratorConfig, Result};
use csilgen_core::ast::*;

/// Generate Rust structs and enums from CSIL specification
pub fn generate_rust_code(spec: &CsilSpec, _config: &GeneratorConfig) -> Result<GeneratedFiles> {
    let mut files = Vec::new();
    let mut code = String::new();

    code.push_str("use serde::{Deserialize, Serialize};\n");
    code.push('\n');

    for rule in &spec.rules {
        match &rule.rule_type {
            RuleType::TypeDef(type_expr) => {
                generate_type_alias(&mut code, &rule.name, type_expr)?;
            }
            RuleType::GroupDef(group) => {
                generate_struct(&mut code, &rule.name, group)?;
            }
            RuleType::GroupChoice(groups) => {
                generate_enum_from_groups(&mut code, &rule.name, groups)?;
            }
            RuleType::TypeChoice(types) => {
                generate_enum_from_types(&mut code, &rule.name, types)?;
            }
            RuleType::ServiceDef(service) => {
                generate_service_trait(&mut code, &rule.name, service)?;
            }
        }
        code.push('\n');
    }

    files.push(GeneratedFile {
        path: "generated.rs".to_string(),
        content: code,
    });

    Ok(files)
}

fn generate_type_alias(code: &mut String, name: &str, type_expr: &TypeExpression) -> Result<()> {
    match type_expr {
        TypeExpression::Group(group) => {
            generate_struct(code, name, group)?;
        }
        _ => {
            let rust_type = map_type_to_rust(type_expr)?;
            code.push_str(&format!("pub type {name} = {rust_type};\n"));
        }
    }
    Ok(())
}

fn generate_struct(code: &mut String, name: &str, group: &GroupExpression) -> Result<()> {
    code.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    code.push_str(&format!("pub struct {name} {{\n"));

    for entry in &group.entries {
        if let Some(GroupKey::Bare(field_name)) = &entry.key {
            generate_struct_field(
                code,
                field_name,
                &entry.value_type,
                &entry.occurrence,
                &entry.metadata,
            )?;
        }
    }

    code.push_str("}\n");
    Ok(())
}

fn generate_struct_field(
    code: &mut String,
    field_name: &str,
    value_type: &TypeExpression,
    occurrence: &Option<Occurrence>,
    metadata: &[FieldMetadata],
) -> Result<()> {
    let rust_field_name = to_snake_case(field_name);
    let mut rust_type = map_type_to_rust(value_type)?;

    let mut serde_attrs = Vec::new();

    for meta in metadata {
        match meta {
            FieldMetadata::Visibility(FieldVisibility::SendOnly) => {
                serde_attrs.push("skip_deserializing".to_string());
            }
            FieldMetadata::Visibility(FieldVisibility::ReceiveOnly) => {
                serde_attrs.push("skip_serializing".to_string());
            }
            FieldMetadata::Custom { name, parameters } if name == "rust" => {
                for param in parameters {
                    if let Some(param_name) = &param.name {
                        if let LiteralValue::Text(value) = &param.value {
                            serde_attrs.push(format!("{param_name} = \"{value}\""));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if matches!(occurrence, Some(Occurrence::Optional)) {
        rust_type = format!("Option<{rust_type}>");
        if !serde_attrs.contains(&"skip_serializing_if = \"Option::is_none\"".to_string()) {
            serde_attrs.push("skip_serializing_if = \"Option::is_none\"".to_string());
        }
    }

    if field_name != rust_field_name {
        serde_attrs.push(format!("rename = \"{field_name}\""));
    }

    if !serde_attrs.is_empty() {
        let serde_attr_str = serde_attrs.join(", ");
        code.push_str(&format!("    #[serde({serde_attr_str})]\n"));
    }

    code.push_str(&format!("    pub {rust_field_name}: {rust_type},\n"));
    Ok(())
}

fn generate_enum_from_groups(
    code: &mut String,
    name: &str,
    groups: &[GroupExpression],
) -> Result<()> {
    code.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    code.push_str(&format!("pub enum {name} {{\n"));

    for (i, group) in groups.iter().enumerate() {
        let variant_name = format!("Variant{}", i + 1);
        if group.entries.is_empty() {
            code.push_str(&format!("    {variant_name},\n"));
        } else {
            code.push_str(&format!("    {variant_name} {{\n"));
            for entry in &group.entries {
                if let Some(GroupKey::Bare(field_name)) = &entry.key {
                    let rust_type = map_type_to_rust(&entry.value_type)?;
                    let rust_field_name = to_snake_case(field_name);
                    code.push_str(&format!("        {rust_field_name}: {rust_type},\n"));
                }
            }
            code.push_str("    },\n");
        }
    }

    code.push_str("}\n");
    Ok(())
}

fn generate_enum_from_types(code: &mut String, name: &str, types: &[TypeExpression]) -> Result<()> {
    code.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    code.push_str("#[serde(untagged)]\n");
    code.push_str(&format!("pub enum {name} {{\n"));

    for (i, type_expr) in types.iter().enumerate() {
        let variant_name = match type_expr {
            TypeExpression::Builtin(builtin) => capitalize(builtin),
            TypeExpression::Reference(ref_name) => ref_name.clone(),
            _ => format!("Variant{}", i + 1),
        };

        let rust_type = map_type_to_rust(type_expr)?;
        code.push_str(&format!("    {variant_name}({rust_type}),\n"));
    }

    code.push_str("}\n");
    Ok(())
}

fn generate_service_trait(
    code: &mut String,
    name: &str,
    service: &ServiceDefinition,
) -> Result<()> {
    code.push_str(&format!("pub trait {name} {{\n"));

    for operation in &service.operations {
        let method_name = to_snake_case(&operation.name);
        let input_type = map_type_to_rust(&operation.input_type)?;
        let output_type = map_type_to_rust(&operation.output_type)?;

        match operation.direction {
            ServiceDirection::Unidirectional => {
                code.push_str(&format!(
                    "    fn {method_name}(&self, input: {input_type}) -> Result<{output_type}>;\n"
                ));
            }
            ServiceDirection::Bidirectional => {
                code.push_str(&format!(
                    "    fn {method_name}(&self, input: {input_type}) -> Result<{output_type}>;\n"
                ));
                code.push_str(&format!(
                    "    fn {method_name}_stream(&self) -> Result<Box<dyn Stream<Item = {output_type}>>>;\n"
                ));
            }
            ServiceDirection::Reverse => {
                code.push_str(&format!(
                    "    fn {method_name}_callback(&self, output: {output_type}) -> Result<()>;\n"
                ));
            }
        }
    }

    code.push_str("}\n");
    Ok(())
}

fn map_type_to_rust(type_expr: &TypeExpression) -> Result<String> {
    Ok(match type_expr {
        TypeExpression::Builtin(name) => map_builtin_to_rust(name),
        TypeExpression::Reference(name) => name.clone(),
        TypeExpression::Array { element_type, .. } => {
            format!("Vec<{}>", map_type_to_rust(element_type)?)
        }
        TypeExpression::Map { key, value, .. } => {
            format!(
                "std::collections::HashMap<{}, {}>",
                map_type_to_rust(key)?,
                map_type_to_rust(value)?
            )
        }
        TypeExpression::Group(_) => "serde_json::Value".to_string(),
        TypeExpression::Choice(_) => "serde_json::Value".to_string(),
        TypeExpression::Literal(literal) => map_literal_to_rust(literal),
        TypeExpression::Socket(name) | TypeExpression::Plug(name) => name.clone(),
        TypeExpression::Range { .. } => "i64".to_string(),
        TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            // Check if we have specific constraints that affect type mapping
            let has_json_constraint = constraints.iter().any(|c| matches!(c, csilgen_core::ast::ControlOperator::Json));
            let has_cbor_constraint = constraints.iter().any(|c| matches!(c, csilgen_core::ast::ControlOperator::Cbor | csilgen_core::ast::ControlOperator::Cborseq));
            
            if has_json_constraint {
                // For .json constraint on text/bytes, we could use serde_json::Value or keep as String
                // and add validation. For now, we'll keep the base type and note validation is needed
                map_type_to_rust(base_type)?
            } else if has_cbor_constraint {
                // For .cbor/.cborseq constraint on bytes, we keep as Vec<u8>
                // and add validation. Could potentially use ciborium::Value in the future
                map_type_to_rust(base_type)?
            } else {
                // For other constraints, use the base type
                map_type_to_rust(base_type)?
            }
        }
    })
}

fn map_builtin_to_rust(builtin: &str) -> String {
    match builtin {
        "int" | "integer" => "i64".to_string(),
        "uint" => "u64".to_string(),
        "float" | "number" => "f64".to_string(),
        "text" | "string" => "String".to_string(),
        "bytes" => "Vec<u8>".to_string(),
        "bool" | "boolean" => "bool".to_string(),
        "null" => "()".to_string(),
        "any" => "serde_json::Value".to_string(),
        _ => "serde_json::Value".to_string(),
    }
}

fn map_literal_to_rust(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Integer(_) => "i64".to_string(),
        LiteralValue::Float(_) => "f64".to_string(),
        LiteralValue::Text(_) => "String".to_string(),
        LiteralValue::Bool(_) => "bool".to_string(),
        LiteralValue::Null => "()".to_string(),
        LiteralValue::Bytes(_) => "Vec<u8>".to_string(),
        LiteralValue::Array(_) => "Vec<serde_json::Value>".to_string(),
    }
}

fn to_snake_case(s: &str) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| {
            if c.is_uppercase() && i > 0 {
                format!("_{}", c.to_lowercase())
            } else if c == '-' {
                "_".to_string()
            } else {
                c.to_lowercase().to_string()
            }
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}
