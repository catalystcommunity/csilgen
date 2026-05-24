//! Emits `types.gen.ts`: interfaces, type aliases, unions, and the shared
//! `ServiceError` shape when the spec declares services.

use crate::common;
use csilgen_common::{
    CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilRuleType, CsilSpecSerialized, CsilTypeExpression, WasmGeneratorInput,
};

pub fn generate(input: &WasmGeneratorInput) -> String {
    let spec = &input.csil_spec;
    let mut out = common::header(input, "typescript-typesonly");

    // Emit the synthetic transport error unless the spec already declares one
    if common::has_services(spec) && !declares_service_error(spec) {
        out.push_str(SERVICE_ERROR);
        out.push('\n');
    }

    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                out.push_str(&interface(&rule.name, group, &rule.doc_comments));
            }
            CsilRuleType::TypeDef(type_expr) => {
                out.push_str(&type_alias(&rule.name, type_expr, &rule.doc_comments));
            }
            CsilRuleType::GroupDef(group) => {
                out.push_str(&interface(&rule.name, group, &rule.doc_comments));
            }
            CsilRuleType::TypeChoice(choices) => {
                let union = choices
                    .iter()
                    .map(common::ts_type)
                    .collect::<Vec<_>>()
                    .join(" | ");
                out.push_str(&common::jsdoc(&rule.doc_comments, &[], ""));
                out.push_str(&format!(
                    "export type {} = {union};\n",
                    common::to_pascal(&rule.name)
                ));
            }
            CsilRuleType::GroupChoice(groups) => {
                let union = groups
                    .iter()
                    .map(inline_object)
                    .collect::<Vec<_>>()
                    .join(" | ");
                out.push_str(&common::jsdoc(&rule.doc_comments, &[], ""));
                out.push_str(&format!(
                    "export type {} = {union};\n",
                    common::to_pascal(&rule.name)
                ));
            }
            // Services are emitted by the client/server targets, not here
            CsilRuleType::ServiceDef(_) => continue,
        }
        out.push('\n');
    }

    out
}

const SERVICE_ERROR: &str = "\
export interface ServiceError {
  code: number;
  message: string;
}
";

fn type_alias(name: &str, type_expr: &CsilTypeExpression, docs: &[String]) -> String {
    let mut out = common::jsdoc(docs, &[], "");
    out.push_str(&format!(
        "export type {} = {};\n",
        common::to_pascal(name),
        common::ts_type(type_expr)
    ));
    out
}

fn interface(name: &str, group: &CsilGroupExpression, docs: &[String]) -> String {
    let mut out = common::jsdoc(docs, &[], "");
    out.push_str(&format!(
        "export interface {} {{\n",
        common::to_pascal(name)
    ));
    for entry in &group.entries {
        if let Some(field) = field_line(entry) {
            out.push_str(&field);
        }
    }
    out.push_str("}\n");
    out
}

fn field_line(entry: &CsilGroupEntry) -> Option<String> {
    let field_name = match &entry.key {
        Some(CsilGroupKey::Bare(name)) => common::to_camel(name),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => common::to_camel(name),
        _ => return None,
    };

    let mut docs = entry.doc_comments.clone();
    for meta in &entry.metadata {
        if let CsilFieldMetadata::Description(desc) = meta {
            docs.push(desc.clone());
        }
    }

    let optional = if common::is_optional(&entry.occurrence) {
        "?"
    } else {
        ""
    };
    let ty = common::ts_type(&entry.value_type);

    let mut out = common::jsdoc(&docs, &[], "  ");
    out.push_str(&format!("  {field_name}{optional}: {ty};\n"));
    Some(out)
}

fn inline_object(group: &CsilGroupExpression) -> String {
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter_map(|entry| {
            let name = match &entry.key {
                Some(CsilGroupKey::Bare(n)) => common::to_camel(n),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(n))) => common::to_camel(n),
                _ => return None,
            };
            let optional = if common::is_optional(&entry.occurrence) {
                "?"
            } else {
                ""
            };
            Some(format!(
                "{name}{optional}: {}",
                common::ts_type(&entry.value_type)
            ))
        })
        .collect();
    format!("{{ {} }}", fields.join("; "))
}

fn declares_service_error(spec: &CsilSpecSerialized) -> bool {
    spec.rules
        .iter()
        .any(|r| common::to_pascal(&r.name) == common::SERVICE_ERROR)
}

/// Type names available for import from `types.gen.ts`: every declared rule
/// plus the synthetic `ServiceError` when services are present.
pub fn declared_type_names(spec: &CsilSpecSerialized) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::TypeDef(_)
            | CsilRuleType::GroupDef(_)
            | CsilRuleType::TypeChoice(_)
            | CsilRuleType::GroupChoice(_) => {
                names.insert(common::to_pascal(&rule.name));
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }
    if common::has_services(spec) {
        names.insert(common::SERVICE_ERROR.to_string());
    }
    names
}
