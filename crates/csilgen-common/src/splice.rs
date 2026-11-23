//! Resolves CDDL sockets/plugs and splices group references into records.
//!
//! The schema diagnostic (`csilgen-schema`) is the reference reader. It reads
//! `$name` and `$$name` as the rule `name`, and it reads a keyless record entry
//! that names a group rule as that group's entries, inline. A generator that
//! instead emits a nested field called `shared_fields`, or an opaque value for a
//! socket, writes a wire the reference refuses. Every generator gets this pass
//! through the shared hoister, so all of them agree on one wire.

use crate::{
    CsilGroupEntry, CsilGroupExpression, CsilRule, CsilRuleType, CsilSpecSerialized,
    CsilTypeExpression,
};
use std::collections::HashMap;

/// A spliced group can name further groups. The bound stops a cycle
/// (`a = (b)`, `b = (a)`) from looping, the same way the diagnostic bounds it.
const MAX_SPLICE_DEPTH: usize = 64;

/// Return `spec` with sockets/plugs resolved and record group references spliced.
/// A spec that uses neither comes back unchanged.
pub fn splice_group_references(spec: &CsilSpecSerialized) -> CsilSpecSerialized {
    let groups: HashMap<&str, &CsilGroupExpression> = spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group)
            | CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                Some((rule.name.as_str(), group))
            }
            _ => None,
        })
        .collect();
    let names: Vec<&str> = spec.rules.iter().map(|rule| rule.name.as_str()).collect();
    let splicer = Splicer {
        groups: &groups,
        names: &names,
    };
    CsilSpecSerialized {
        rules: spec.rules.iter().map(|rule| splicer.rule(rule)).collect(),
        source_content: spec.source_content.clone(),
        service_count: spec.service_count,
        fields_with_metadata_count: spec.fields_with_metadata_count,
    }
}

struct Splicer<'a> {
    groups: &'a HashMap<&'a str, &'a CsilGroupExpression>,
    names: &'a [&'a str],
}

impl Splicer<'_> {
    fn rule(&self, rule: &CsilRule) -> CsilRule {
        let rule_type = match &rule.rule_type {
            CsilRuleType::TypeDef(ty) => CsilRuleType::TypeDef(self.ty(ty)),
            CsilRuleType::GroupDef(group) => CsilRuleType::GroupDef(self.record(group, 0)),
            CsilRuleType::TypeChoice(arms) => {
                CsilRuleType::TypeChoice(arms.iter().map(|arm| self.ty(arm)).collect())
            }
            CsilRuleType::GroupChoice(groups) => CsilRuleType::GroupChoice(
                groups.iter().map(|group| self.record(group, 0)).collect(),
            ),
            CsilRuleType::ServiceDef(service) => {
                let mut service = service.clone();
                for op in &mut service.operations {
                    op.input_type = self.ty(&op.input_type);
                    op.output_type = self.ty(&op.output_type);
                }
                CsilRuleType::ServiceDef(service)
            }
        };
        CsilRule {
            rule_type,
            ..rule.clone()
        }
    }

    fn ty(&self, ty: &CsilTypeExpression) -> CsilTypeExpression {
        match ty {
            CsilTypeExpression::Socket(name) | CsilTypeExpression::Plug(name) => {
                if self.names.contains(&name.as_str()) {
                    CsilTypeExpression::Reference(name.clone())
                } else {
                    // No rule fills the extension point, so any value is allowed.
                    CsilTypeExpression::Builtin("any".to_string())
                }
            }
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => CsilTypeExpression::Array {
                element_type: Box::new(self.ty(element_type)),
                occurrence: occurrence.clone(),
            },
            CsilTypeExpression::Map {
                key,
                value,
                occurrence,
            } => CsilTypeExpression::Map {
                key: Box::new(self.ty(key)),
                value: Box::new(self.ty(value)),
                occurrence: occurrence.clone(),
            },
            CsilTypeExpression::Group(group) => CsilTypeExpression::Group(self.record(group, 0)),
            // A keyless tuple element is a positional value, not a splice.
            CsilTypeExpression::Tuple(group) => CsilTypeExpression::Tuple(CsilGroupExpression {
                entries: group
                    .entries
                    .iter()
                    .map(|entry| self.entry(entry))
                    .collect(),
            }),
            CsilTypeExpression::Choice(arms) => {
                CsilTypeExpression::Choice(arms.iter().map(|arm| self.ty(arm)).collect())
            }
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => CsilTypeExpression::Constrained {
                base_type: Box::new(self.ty(base_type)),
                constraints: constraints.clone(),
            },
            other => other.clone(),
        }
    }

    fn entry(&self, entry: &CsilGroupEntry) -> CsilGroupEntry {
        CsilGroupEntry {
            value_type: self.ty(&entry.value_type),
            ..entry.clone()
        }
    }

    fn record(&self, group: &CsilGroupExpression, depth: usize) -> CsilGroupExpression {
        let mut entries = Vec::with_capacity(group.entries.len());
        for entry in &group.entries {
            match self.spliced(entry, depth) {
                Some(inner) => entries.extend(inner),
                None => entries.push(self.entry(entry)),
            }
        }
        CsilGroupExpression { entries }
    }

    /// The entries a keyless, single-occurrence group reference stands for. An
    /// optional or repeated group (`? g`, `* g`) keeps its entry: it has no
    /// field-by-field meaning a record can carry.
    fn spliced(&self, entry: &CsilGroupEntry, depth: usize) -> Option<Vec<CsilGroupEntry>> {
        if entry.key.is_some() || entry.occurrence.is_some() || depth >= MAX_SPLICE_DEPTH {
            return None;
        }
        let name = match &entry.value_type {
            CsilTypeExpression::Reference(name)
            | CsilTypeExpression::Socket(name)
            | CsilTypeExpression::Plug(name) => name,
            _ => return None,
        };
        let group = match self.groups.get(name.as_str()) {
            Some(group) => *group,
            // An unfilled group socket is an empty group.
            None if matches!(entry.value_type, CsilTypeExpression::Plug(_))
                && !self.names.contains(&name.as_str()) =>
            {
                return Some(Vec::new());
            }
            None => return None,
        };
        Some(self.record(group, depth + 1).entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CsilGroupKey, CsilPosition};

    fn field(name: &str, ty: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: None,
            metadata: Vec::new(),
            doc_comments: Vec::new(),
        }
    }

    fn bare(ty: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: None,
            ..field("", ty)
        }
    }

    fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    fn spec(rules: Vec<CsilRule>) -> CsilSpecSerialized {
        CsilSpecSerialized {
            rules,
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        }
    }

    fn group(entries: Vec<CsilGroupEntry>) -> CsilTypeExpression {
        CsilTypeExpression::Group(CsilGroupExpression { entries })
    }

    fn uint() -> CsilTypeExpression {
        CsilTypeExpression::Builtin("uint".to_string())
    }

    fn record_keys(spec: &CsilSpecSerialized, name: &str) -> Vec<String> {
        let rule = spec.rules.iter().find(|r| r.name == name).expect("rule");
        let CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) = &rule.rule_type else {
            panic!("{name} is not a record");
        };
        g.entries
            .iter()
            .map(|e| match &e.key {
                Some(CsilGroupKey::Bare(k)) => k.clone(),
                _ => "<keyless>".to_string(),
            })
            .collect()
    }

    #[test]
    fn keyless_group_reference_splices_its_entries_in_place() {
        let input = spec(vec![
            rule(
                "shared",
                CsilRuleType::TypeDef(group(vec![field("id", uint()), field("note", uint())])),
            ),
            rule(
                "Outer",
                CsilRuleType::TypeDef(group(vec![
                    field("first", uint()),
                    bare(CsilTypeExpression::Reference("shared".to_string())),
                    field("last", uint()),
                ])),
            ),
        ]);
        let out = splice_group_references(&input);
        assert_eq!(record_keys(&out, "Outer"), ["first", "id", "note", "last"]);
    }

    #[test]
    fn plug_splices_its_group_and_socket_reads_as_its_rule() {
        let input = spec(vec![
            rule(
                "open-type",
                CsilRuleType::TypeDef(CsilTypeExpression::Builtin("text".into())),
            ),
            rule(
                "open-group",
                CsilRuleType::TypeDef(group(vec![field("open_value", uint())])),
            ),
            rule(
                "PlugUse",
                CsilRuleType::TypeDef(group(vec![bare(CsilTypeExpression::Plug(
                    "open-group".to_string(),
                ))])),
            ),
            rule(
                "SocketUse",
                CsilRuleType::TypeDef(CsilTypeExpression::Socket("open-type".to_string())),
            ),
        ]);
        let out = splice_group_references(&input);
        assert_eq!(record_keys(&out, "PlugUse"), ["open_value"]);
        assert!(matches!(
            &out.rules[3].rule_type,
            CsilRuleType::TypeDef(CsilTypeExpression::Reference(n)) if n == "open-type"
        ));
    }

    #[test]
    fn unfilled_sockets_allow_anything_and_unfilled_plugs_add_nothing() {
        let input = spec(vec![rule(
            "Open",
            CsilRuleType::TypeDef(group(vec![
                field("ext", CsilTypeExpression::Socket("missing".to_string())),
                bare(CsilTypeExpression::Plug("missing-group".to_string())),
            ])),
        )]);
        let out = splice_group_references(&input);
        assert_eq!(record_keys(&out, "Open"), ["ext"]);
        let CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) = &out.rules[0].rule_type else {
            panic!("record");
        };
        assert_eq!(
            g.entries[0].value_type,
            CsilTypeExpression::Builtin("any".to_string())
        );
    }

    #[test]
    fn optional_group_and_tuple_elements_and_cycles_stay_as_written() {
        let a = rule(
            "a",
            CsilRuleType::TypeDef(group(vec![bare(CsilTypeExpression::Reference(
                "b".to_string(),
            ))])),
        );
        let b = rule(
            "b",
            CsilRuleType::TypeDef(group(vec![bare(CsilTypeExpression::Reference(
                "a".to_string(),
            ))])),
        );
        let mut optional = bare(CsilTypeExpression::Reference("a".to_string()));
        optional.occurrence = Some(crate::CsilOccurrence::Optional);
        let holder = rule("Holder", CsilRuleType::TypeDef(group(vec![optional])));
        let tuple = rule(
            "Pair",
            CsilRuleType::TypeDef(CsilTypeExpression::Tuple(CsilGroupExpression {
                entries: vec![bare(CsilTypeExpression::Reference("a".to_string()))],
            })),
        );
        let input = spec(vec![a, b, holder, tuple]);
        let out = splice_group_references(&input);
        assert_eq!(record_keys(&out, "Holder"), ["<keyless>"]);
        let (CsilRuleType::TypeDef(kept), CsilRuleType::TypeDef(written)) =
            (&out.rules[3].rule_type, &input.rules[3].rule_type)
        else {
            panic!("tuple rule");
        };
        assert_eq!(kept, written);
        // The cycle ends at the depth bound instead of looping.
        assert!(record_keys(&out, "a").len() <= 1);
    }
}
