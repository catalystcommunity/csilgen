//! Gives a name to each operation boundary that is an inline union with no reply
//! variants.
//!
//! An RPC reply carries a `variant` only when every arm of a multi-arm output is a
//! named rule or a built-in type (see `CsilServiceOperation::rpc_reply_arms`). An
//! output such as `Ev -> {* text => int} / Err` has an inline arm, so it has no
//! variants: the payload is one union value, the same as for `Out = {* text => int} /
//! Err` with `Ev -> Out`. Generators already implement named unions in every
//! language, so this pass turns the inline union into that named rule. Without it,
//! each generator chose its own shape: Rust exposed the union, while C and Zig kept
//! only the first arm and lost the others.

use crate::{CsilRule, CsilRuleType, CsilSpecSerialized, CsilTypeExpression};
use std::collections::HashSet;

/// Return `spec` with each operation input or output that is an inline choice with an
/// arm that has no variant name (an inline record, map, array or literal) replaced by
/// a reference to a new `TypeDef` rule `<Service><Op>Input` or `<Service><Op>Output`.
/// A digit suffix keeps the name free of every existing rule name, compared without
/// case or separators. The wire is unchanged, because an inline union and a named
/// union both encode as `[variant_index, value]`. A spec without such a boundary is
/// returned unchanged.
pub fn hoist_op_union_boundaries(spec: &CsilSpecSerialized) -> CsilSpecSerialized {
    let mut used: HashSet<String> = spec.rules.iter().map(|r| canonical(&r.name)).collect();
    let mut synth = Vec::new();
    let mut rules = Vec::with_capacity(spec.rules.len());
    for rule in &spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            rules.push(rule.clone());
            continue;
        };
        let mut service = service.clone();
        for op in &mut service.operations {
            for (boundary, suffix) in [
                (&mut op.input_type, "Input"),
                (&mut op.output_type, "Output"),
            ] {
                let CsilTypeExpression::Choice(arms) = &*boundary else {
                    continue;
                };
                if arms.iter().all(|arm| arm.rpc_variant_name().is_some()) {
                    continue;
                }
                let base = format!("{}{}{suffix}", pascal(&rule.name), pascal(&op.name));
                let mut name = base.clone();
                let mut n = 2;
                while used.contains(&canonical(&name)) {
                    name = format!("{base}{n}");
                    n += 1;
                }
                used.insert(canonical(&name));
                synth.push(CsilRule {
                    name: name.clone(),
                    rule_type: CsilRuleType::TypeDef(boundary.clone()),
                    position: op.position.clone(),
                    doc_comments: Vec::new(),
                });
                *boundary = CsilTypeExpression::Reference(name);
            }
        }
        rules.push(CsilRule {
            rule_type: CsilRuleType::ServiceDef(service),
            ..rule.clone()
        });
    }
    if synth.is_empty() {
        return spec.clone();
    }
    rules.extend(synth);
    CsilSpecSerialized {
        rules,
        ..spec.clone()
    }
}

fn canonical(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn pascal(name: &str) -> String {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let first = chars.next().map(|c| c.to_ascii_uppercase());
            first.into_iter().chain(chars).collect::<String>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilPosition, CsilServiceDefinition,
        CsilServiceDirection, CsilServiceOperation,
    };

    fn pos() -> CsilPosition {
        CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    fn spec_with(rules: Vec<CsilRule>) -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules,
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        }
    }

    fn service(
        name: &str,
        op: &str,
        input: CsilTypeExpression,
        output: CsilTypeExpression,
    ) -> CsilRule {
        CsilRule {
            name: name.into(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: op.into(),
                    input_type: input,
                    output_type: output,
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        }
    }

    fn map() -> CsilTypeExpression {
        CsilTypeExpression::Group(CsilGroupExpression {
            entries: vec![CsilGroupEntry {
                key: Some(CsilGroupKey::Type(CsilTypeExpression::Builtin(
                    "text".into(),
                ))),
                value_type: CsilTypeExpression::Builtin("int".into()),
                occurrence: None,
                metadata: Vec::new(),
                doc_comments: Vec::new(),
            }],
        })
    }

    fn reference(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Reference(name.into())
    }

    #[test]
    fn mixed_output_becomes_a_named_union() {
        let spec = spec_with(vec![service(
            "event-svc",
            "read-map",
            reference("Ev"),
            CsilTypeExpression::Choice(vec![map(), reference("Err")]),
        )]);
        let out = hoist_op_union_boundaries(&spec);
        let CsilRuleType::ServiceDef(svc) = &out.rules[0].rule_type else {
            panic!("service first");
        };
        assert_eq!(
            svc.operations[0].output_type,
            reference("EventSvcReadMapOutput")
        );
        assert_eq!(out.rules[1].name, "EventSvcReadMapOutput");
        let CsilRuleType::TypeDef(ty) = &out.rules[1].rule_type else {
            panic!("synthesized rule is a TypeDef");
        };
        assert_eq!(
            ty,
            &CsilTypeExpression::Choice(vec![map(), reference("Err")])
        );
    }

    #[test]
    fn reply_variant_outputs_and_plain_boundaries_are_unchanged() {
        let spec = spec_with(vec![service(
            "Svc",
            "get",
            reference("Req"),
            CsilTypeExpression::Choice(vec![reference("Ok"), reference("Err")]),
        )]);
        let out = hoist_op_union_boundaries(&spec);
        assert_eq!(out.rules.len(), 1);
        let (CsilRuleType::ServiceDef(a), CsilRuleType::ServiceDef(b)) =
            (&out.rules[0].rule_type, &spec.rules[0].rule_type)
        else {
            panic!("service");
        };
        assert_eq!(a.operations[0].output_type, b.operations[0].output_type);
    }

    #[test]
    fn mixed_input_is_named_and_the_name_avoids_existing_rules() {
        let taken = CsilRule {
            name: "svc_get_input".into(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Builtin("int".into())),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let spec = spec_with(vec![
            taken,
            service(
                "Svc",
                "get",
                CsilTypeExpression::Choice(vec![map(), reference("Req")]),
                reference("Res"),
            ),
        ]);
        let out = hoist_op_union_boundaries(&spec);
        assert_eq!(out.rules[2].name, "SvcGetInput2");
    }
}
