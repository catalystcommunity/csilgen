//! Renames rules, and every reference to them, across a spec.
//!
//! CDDL allows rule names such as `open-group` that are not identifiers in most
//! target languages. A generator that needs other names renames the rules up
//! front, so the rest of the generator works on legal names only. The caller
//! records each declared name in `CsilSpecSerialized::declared_names`, because
//! an RPC reply variant puts the declared name on the wire, and that name must
//! not change.

use crate::{
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilRule, CsilRuleType, CsilSpecSerialized,
    CsilTypeExpression, GeneratorWarning, SourceLocation, WarningLevel,
};
use std::collections::{HashMap, HashSet};

/// The new name for each non-service rule whose name `legal` refuses. `fix`
/// gives the first candidate; a `_` suffix is added until the name is free, so
/// `open-group` never takes the name of a rule `open_group` that already exists.
/// Service names are left alone: the RPC wire carries them as written.
pub fn rule_renames(
    spec: &CsilSpecSerialized,
    legal: impl Fn(&str) -> bool,
    fix: impl Fn(&str) -> String,
) -> HashMap<String, String> {
    let mut taken: HashSet<String> = spec.rules.iter().map(|rule| rule.name.clone()).collect();
    let mut renames = HashMap::new();
    for rule in &spec.rules {
        if matches!(rule.rule_type, CsilRuleType::ServiceDef(_)) || legal(&rule.name) {
            continue;
        }
        let mut candidate = fix(&rule.name);
        while taken.contains(&candidate) {
            candidate.push('_');
        }
        taken.insert(candidate.clone());
        renames.insert(rule.name.clone(), candidate);
    }
    renames
}

/// Return `spec` with each rule in `renames` renamed, and with every reference,
/// socket and plug that names it changed to match.
pub fn rename_rules(
    spec: &CsilSpecSerialized,
    renames: &HashMap<String, String>,
) -> CsilSpecSerialized {
    if renames.is_empty() {
        return spec.clone();
    }
    let renamer = Renamer { renames };
    let mut declared_names = spec.declared_names.clone();
    for (from, to) in renames {
        let declared = declared_names.remove(from).unwrap_or_else(|| from.clone());
        declared_names.insert(to.clone(), declared);
    }
    CsilSpecSerialized {
        rules: spec.rules.iter().map(|rule| renamer.rule(rule)).collect(),
        source_content: spec.source_content.clone(),
        service_count: spec.service_count,
        fields_with_metadata_count: spec.fields_with_metadata_count,
        declared_names,
    }
}

/// Return `spec` with each non-service rule that `reserved` refuses renamed to a
/// free name with a `Type` suffix (`Transport` becomes `TransportType`). A
/// generator passes the names it declares itself, and the language names its
/// output refers to, so a schema type cannot hide or redeclare them. The spec
/// keeps each declared name for the wire.
pub fn rename_reserved(
    spec: &CsilSpecSerialized,
    reserved: impl Fn(&str) -> bool,
) -> CsilSpecSerialized {
    let mut taken: HashSet<String> = spec.rules.iter().map(|rule| rule.name.clone()).collect();
    let mut renames = HashMap::new();
    for rule in &spec.rules {
        if matches!(rule.rule_type, CsilRuleType::ServiceDef(_)) || !reserved(&rule.name) {
            continue;
        }
        let mut candidate = format!("{}Type", rule.name);
        while taken.contains(&candidate) || reserved(&candidate) {
            candidate.push_str("Type");
        }
        taken.insert(candidate.clone());
        renames.insert(rule.name.clone(), candidate);
    }
    rename_rules(spec, &renames)
}

/// Return `spec` with every non-service rule whose generated identifier equals the
/// identifier of an earlier rule renamed to a free `<Name>Type`. `ident` is the
/// generator's own rule-name-to-identifier function, so `Value` and `value`, or
/// `user_id` and `UserId`, collide only where the target language makes them the
/// same name. The first rule in the spec keeps its name, so adding a rule never
/// renames an existing one. Service identifiers count as taken but services are
/// never renamed, because the RPC wire carries them as written.
pub fn rename_ident_collisions(
    spec: &CsilSpecSerialized,
    ident: impl Fn(&str) -> String,
) -> CsilSpecSerialized {
    let mut names: HashSet<String> = spec.rules.iter().map(|rule| rule.name.clone()).collect();
    let mut idents: HashSet<String> = spec
        .rules
        .iter()
        .filter(|rule| matches!(rule.rule_type, CsilRuleType::ServiceDef(_)))
        .map(|rule| ident(&rule.name))
        .collect();
    let mut renames = HashMap::new();
    for rule in &spec.rules {
        if matches!(rule.rule_type, CsilRuleType::ServiceDef(_)) {
            continue;
        }
        if idents.insert(ident(&rule.name)) {
            continue;
        }
        let mut candidate = format!("{}Type", rule.name);
        while names.contains(&candidate) || idents.contains(&ident(&candidate)) {
            candidate.push_str("Type");
        }
        names.insert(candidate.clone());
        idents.insert(ident(&candidate));
        renames.insert(rule.name.clone(), candidate);
    }
    rename_rules(spec, &renames)
}

/// One warning for each rule a generator renamed, so the user learns that the
/// generated type has another name than the CSIL rule. The wire is unchanged.
pub fn rename_warnings(spec: &CsilSpecSerialized, target: &str) -> Vec<GeneratorWarning> {
    spec.declared_names
        .iter()
        .map(|(new, declared)| {
            let location = spec
                .rules
                .iter()
                .find(|rule| &rule.name == new)
                .map(|rule| SourceLocation {
                    line: rule.position.line,
                    column: rule.position.column,
                    offset: rule.position.offset,
                    context: Some(declared.clone()),
                });
            GeneratorWarning {
                level: WarningLevel::Warning,
                message: format!(
                    "rule `{declared}` is generated as `{new}` for {target}, because `{declared}` \
                     clashes with a name the generated code uses, with another rule, or is not a \
                     legal identifier there; the wire still uses `{declared}`"
                ),
                location,
                suggestion: Some(format!(
                    "rename `{declared}` in the CSIL source to choose the generated name yourself"
                )),
            }
        })
        .collect()
}

struct Renamer<'a> {
    renames: &'a HashMap<String, String>,
}

impl Renamer<'_> {
    fn name(&self, name: &str) -> String {
        self.renames
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    fn rule(&self, rule: &CsilRule) -> CsilRule {
        let rule_type = match &rule.rule_type {
            CsilRuleType::TypeDef(ty) => CsilRuleType::TypeDef(self.ty(ty)),
            CsilRuleType::GroupDef(group) => CsilRuleType::GroupDef(self.group(group)),
            CsilRuleType::TypeChoice(arms) => {
                CsilRuleType::TypeChoice(arms.iter().map(|arm| self.ty(arm)).collect())
            }
            CsilRuleType::GroupChoice(groups) => {
                CsilRuleType::GroupChoice(groups.iter().map(|group| self.group(group)).collect())
            }
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
            name: self.name(&rule.name),
            rule_type,
            ..rule.clone()
        }
    }

    fn group(&self, group: &CsilGroupExpression) -> CsilGroupExpression {
        CsilGroupExpression {
            entries: group
                .entries
                .iter()
                .map(|entry| self.entry(entry))
                .collect(),
        }
    }

    fn entry(&self, entry: &CsilGroupEntry) -> CsilGroupEntry {
        let key = match &entry.key {
            Some(CsilGroupKey::Type(ty)) => Some(CsilGroupKey::Type(self.ty(ty))),
            other => other.clone(),
        };
        CsilGroupEntry {
            key,
            value_type: self.ty(&entry.value_type),
            ..entry.clone()
        }
    }

    fn ty(&self, ty: &CsilTypeExpression) -> CsilTypeExpression {
        match ty {
            CsilTypeExpression::Reference(name) => CsilTypeExpression::Reference(self.name(name)),
            CsilTypeExpression::Socket(name) => CsilTypeExpression::Socket(self.name(name)),
            CsilTypeExpression::Plug(name) => CsilTypeExpression::Plug(self.name(name)),
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
            CsilTypeExpression::Group(group) => CsilTypeExpression::Group(self.group(group)),
            CsilTypeExpression::Tuple(group) => CsilTypeExpression::Tuple(self.group(group)),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CsilPosition;

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

    fn legal(name: &str) -> bool {
        !name.contains('-')
    }

    fn fix(name: &str) -> String {
        name.replace('-', "_")
    }

    #[test]
    fn renames_the_rule_and_each_reference_without_taking_an_existing_name() {
        let spec = CsilSpecSerialized {
            rules: vec![
                rule(
                    "open-type",
                    CsilRuleType::TypeDef(CsilTypeExpression::Builtin("text".into())),
                ),
                rule(
                    "open_type",
                    CsilRuleType::TypeDef(CsilTypeExpression::Builtin("int".into())),
                ),
                rule(
                    "Use",
                    CsilRuleType::TypeDef(CsilTypeExpression::Array {
                        element_type: Box::new(CsilTypeExpression::Reference("open-type".into())),
                        occurrence: None,
                    }),
                ),
            ],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        };
        let renames = rule_renames(&spec, legal, fix);
        assert_eq!(
            renames.get("open-type").map(String::as_str),
            Some("open_type_")
        );
        let out = rename_rules(&spec, &renames);
        assert_eq!(out.rules[0].name, "open_type_");
        assert_eq!(out.rules[1].name, "open_type");
        let CsilRuleType::TypeDef(CsilTypeExpression::Array { element_type, .. }) =
            &out.rules[2].rule_type
        else {
            panic!("array rule");
        };
        assert_eq!(
            **element_type,
            CsilTypeExpression::Reference("open_type_".into())
        );
    }

    #[test]
    fn reserved_rename_keeps_the_declared_name_and_avoids_taken_names() {
        let text = || CsilTypeExpression::Builtin("text".into());
        let spec = CsilSpecSerialized {
            rules: vec![
                rule("Transport", CsilRuleType::TypeDef(text())),
                rule("TransportType", CsilRuleType::TypeDef(text())),
                rule(
                    "Use",
                    CsilRuleType::TypeDef(CsilTypeExpression::Reference("Transport".into())),
                ),
            ],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        };
        let out = rename_reserved(&spec, |name| name == "Transport");
        assert_eq!(out.rules[0].name, "TransportTypeType");
        assert_eq!(out.rules[1].name, "TransportType");
        assert_eq!(out.declared_name("TransportTypeType"), "Transport");
        assert_eq!(out.declared_name("TransportType"), "TransportType");
        let CsilRuleType::TypeDef(CsilTypeExpression::Reference(target)) = &out.rules[2].rule_type
        else {
            panic!("reference rule");
        };
        assert_eq!(target, "TransportTypeType");
        // A second rename keeps the name from the CSIL source.
        let again = rename_rules(
            &out,
            &HashMap::from([("TransportTypeType".to_string(), "T2".to_string())]),
        );
        assert_eq!(again.declared_name("T2"), "Transport");
    }

    #[test]
    fn ident_collision_renames_the_later_rule_and_warns() {
        let text = || CsilTypeExpression::Builtin("text".into());
        let spec = CsilSpecSerialized {
            rules: vec![
                rule("Value", CsilRuleType::TypeDef(text())),
                rule("value", CsilRuleType::TypeDef(text())),
                rule(
                    "Use",
                    CsilRuleType::TypeDef(CsilTypeExpression::Reference("value".into())),
                ),
            ],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        };
        let pascal = |name: &str| {
            let mut chars = name.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        };
        let out = rename_ident_collisions(&spec, pascal);
        assert_eq!(out.rules[0].name, "Value");
        assert_eq!(out.rules[1].name, "valueType");
        assert_eq!(out.declared_name("valueType"), "value");
        let CsilRuleType::TypeDef(CsilTypeExpression::Reference(target)) = &out.rules[2].rule_type
        else {
            panic!("reference rule");
        };
        assert_eq!(target, "valueType");
        // An identity identifier function keeps both names.
        assert!(
            rename_ident_collisions(&spec, str::to_string)
                .declared_names
                .is_empty()
        );

        let warnings = rename_warnings(&out, "go");
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0]
                .message
                .contains("`value` is generated as `valueType` for go")
        );
        assert_eq!(warnings[0].location.as_ref().map(|l| l.line), Some(1));
    }
}
