//! TypeScript code generator for csilgen

use csilgen_common::{GeneratedFile, GeneratedFiles, GeneratorConfig, Result};
use csilgen_core::ast::*;

/// Generate TypeScript interfaces from CSIL specification
pub fn generate_typescript_code(
    spec: &CsilSpec,
    _config: &GeneratorConfig,
) -> Result<GeneratedFiles> {
    let mut files = Vec::new();
    let mut code = String::new();

    for rule in &spec.rules {
        match &rule.rule_type {
            RuleType::TypeDef(type_expr) => {
                generate_type_alias(&mut code, &rule.name, type_expr)?;
            }
            RuleType::GroupDef(group) => {
                generate_interface(&mut code, &rule.name, group)?;
            }
            RuleType::GroupChoice(groups) => {
                generate_union_from_groups(&mut code, &rule.name, groups)?;
            }
            RuleType::TypeChoice(types) => {
                generate_union_from_types(&mut code, &rule.name, types)?;
            }
            RuleType::ServiceDef(service) => {
                generate_service_interface(&mut code, &rule.name, service)?;
            }
        }
        code.push('\n');
    }

    files.push(GeneratedFile {
        path: "generated.ts".to_string(),
        content: code,
    });

    Ok(files)
}

fn generate_type_alias(code: &mut String, name: &str, type_expr: &TypeExpression) -> Result<()> {
    match type_expr {
        TypeExpression::Group(group) => {
            generate_interface(code, name, group)?;
        }
        _ => {
            let ts_type = map_type_to_typescript(type_expr)?;
            code.push_str(&format!("export type {name} = {ts_type};\n"));
        }
    }
    Ok(())
}

fn generate_interface(code: &mut String, name: &str, group: &GroupExpression) -> Result<()> {
    code.push_str(&format!("export interface {name} {{\n"));

    for entry in &group.entries {
        if let Some(GroupKey::Bare(field_name)) = &entry.key {
            generate_interface_field(
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

fn generate_interface_field(
    code: &mut String,
    field_name: &str,
    value_type: &TypeExpression,
    occurrence: &Option<Occurrence>,
    metadata: &[FieldMetadata],
) -> Result<()> {
    let ts_field_name = to_camel_case(field_name);
    let ts_type = map_type_to_typescript(value_type)?;

    let is_optional = matches!(occurrence, Some(Occurrence::Optional));

    for meta in metadata {
        if let FieldMetadata::Description(desc) = meta {
            code.push_str(&format!("  /** {desc} */\n"));
        }
    }

    let optional_marker = if is_optional { "?" } else { "" };
    code.push_str(&format!("  {ts_field_name}{optional_marker}: {ts_type};\n"));

    Ok(())
}

fn generate_union_from_groups(
    code: &mut String,
    name: &str,
    groups: &[GroupExpression],
) -> Result<()> {
    let mut union_types = Vec::new();

    for (i, group) in groups.iter().enumerate() {
        let interface_name = format!("{}_Variant{}", name, i + 1);
        generate_interface(code, &interface_name, group)?;
        union_types.push(interface_name);
    }

    let union_str = union_types.join(" | ");
    code.push_str(&format!("export type {name} = {union_str};\n"));
    Ok(())
}

fn generate_union_from_types(
    code: &mut String,
    name: &str,
    types: &[TypeExpression],
) -> Result<()> {
    let ts_types: Result<Vec<_>> = types.iter().map(map_type_to_typescript).collect();

    let union_str = ts_types?.join(" | ");
    code.push_str(&format!("export type {name} = {union_str};\n"));
    Ok(())
}

fn generate_service_interface(
    code: &mut String,
    name: &str,
    service: &ServiceDefinition,
) -> Result<()> {
    code.push_str(&format!("export interface {name} {{\n"));

    for operation in &service.operations {
        let method_name = to_camel_case(&operation.name);
        let input_type = map_type_to_typescript(&operation.input_type)?;
        let output_type = map_type_to_typescript(&operation.output_type)?;

        match operation.direction {
            ServiceDirection::Unidirectional => {
                code.push_str(&format!(
                    "  {method_name}(input: {input_type}): Promise<{output_type}>;\n"
                ));
            }
            ServiceDirection::Bidirectional => {
                code.push_str(&format!(
                    "  {method_name}(input: {input_type}): Promise<{output_type}>;\n"
                ));
                code.push_str(&format!(
                    "  {method_name}Stream(): AsyncIterable<{output_type}>;\n"
                ));
            }
            ServiceDirection::Reverse => {
                code.push_str(&format!(
                    "  {method_name}Callback(output: {output_type}): Promise<void>;\n"
                ));
            }
        }
    }

    code.push_str("}\n");
    Ok(())
}

fn map_type_to_typescript(type_expr: &TypeExpression) -> Result<String> {
    Ok(match type_expr {
        TypeExpression::Builtin(name) => map_builtin_to_typescript(name),
        TypeExpression::Reference(name) => name.clone(),
        TypeExpression::Array { element_type, .. } => {
            format!("{}[]", map_type_to_typescript(element_type)?)
        }
        TypeExpression::Map { key, value, .. } => {
            format!(
                "Record<{}, {}>",
                map_type_to_typescript(key)?,
                map_type_to_typescript(value)?
            )
        }
        TypeExpression::Group(_) => "object".to_string(),
        TypeExpression::Choice(choices) => {
            let ts_types: Result<Vec<_>> = choices.iter().map(map_type_to_typescript).collect();
            ts_types?.join(" | ")
        }
        TypeExpression::Literal(literal) => map_literal_to_typescript(literal),
        TypeExpression::Socket(name) | TypeExpression::Plug(name) => name.clone(),
        TypeExpression::Range { .. } => "number".to_string(),
        TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            // Check if we have specific constraints that affect type mapping
            let has_json_constraint = constraints.iter().any(|c| matches!(c, csilgen_core::ast::ControlOperator::Json));
            let has_cbor_constraint = constraints.iter().any(|c| matches!(c, csilgen_core::ast::ControlOperator::Cbor | csilgen_core::ast::ControlOperator::Cborseq));
            
            if has_json_constraint {
                // For .json constraint, the type is still string but should contain valid JSON
                // Could be documented with JSDoc or use a branded type
                map_type_to_typescript(base_type)?
            } else if has_cbor_constraint {
                // For .cbor/.cborseq constraint on bytes, we use Uint8Array
                // Could be documented with JSDoc to indicate CBOR encoding
                map_type_to_typescript(base_type)?
            } else {
                // For other constraints, use the base type
                map_type_to_typescript(base_type)?
            }
        }
    })
}

fn map_builtin_to_typescript(builtin: &str) -> String {
    match builtin {
        "int" | "integer" | "uint" | "float" | "number" => "number".to_string(),
        "text" | "string" => "string".to_string(),
        "bytes" => "Uint8Array".to_string(),
        "bool" | "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "any" => "any".to_string(),
        _ => "unknown".to_string(),
    }
}

fn map_literal_to_typescript(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Integer(val) => val.to_string(),
        LiteralValue::Float(val) => val.to_string(),
        LiteralValue::Text(val) => format!("\"{val}\""),
        LiteralValue::Bool(val) => val.to_string(),
        LiteralValue::Null => "null".to_string(),
        LiteralValue::Bytes(_) => "Uint8Array".to_string(),
        LiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(map_literal_to_typescript).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;

    for c in s.chars() {
        if c == '-' || c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}
