//! Hoists inline (anonymous) composite field types to synthesized named rules.
//!
//! A mixed choice or an inline group written directly in a field — or in an array
//! element, map value/key, or tuple element — has no named rule behind it, so a
//! codec's named-rule machinery (per-record map codecs, per-union tagged-sum
//! codecs, and the reference dispatch that routes through them) cannot reach it:
//! such a position previously fell through to a blind passthrough (or, in a
//! generator with no anonymous-sum-type field syntax at all, a `failwith`/opaque
//! stub) that wrote the wrong wire shape.
//!
//! Rewriting each inline composite in place to a `Reference(<synth name>)` and
//! appending a synthesized rule for it makes every position — regardless of
//! whether it is a direct field or nested inside a container — resolve through
//! exactly the same functions a named rule uses, with no bespoke per-position
//! codec path.
//!
//! This one pass replaces what were ~9 near-identical per-generator copies (see
//! git history of `wasm/csilgen-typescript-generator/src/hoist.rs` and
//! `wasm/csilgen-ocaml-generator/src/hoist.rs` before their migration to this
//! module). The one dimension those copies genuinely differed on —
//! [`HoistOptions::hoist_all_literal_choices`] — is now a parameter rather than a
//! fork.

use crate::choice::all_literal;
use crate::{
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilPosition, CsilRule,
    CsilRuleType, CsilSpecSerialized, CsilTypeExpression,
};
use std::collections::HashSet;

/// Knobs a generator sets to match its own field-type surface. The synthesized
/// naming (`<Owner>_<field>`, `_item`, `_key`/`_value`, `_<index>`, applied
/// recursively) and the collision-avoidance/constraint-preservation behavior are
/// NOT configurable — they are the fixed, correct behavior every generator gets.
#[derive(Debug, Clone, Copy, Default)]
pub struct HoistOptions {
    /// Hoist an ALL-literal choice too, not just a mixed one.
    ///
    /// Most generators (TypeScript, Java, C#, Kotlin, ...) render an inline
    /// all-literal choice as a bare enum type in the field position itself — the
    /// codec already emits a correct bare-literal wire value for it, so there is
    /// nothing to hoist. Leave this `false` for those.
    ///
    /// OCaml has no anonymous sum-type field syntax at all: an OCaml variant
    /// (`Pending | Shipped | Delivered`) must be a *named* `type`, so even a
    /// closed literal-only choice needs a synthesized name to have anywhere to
    /// live. Set this `true` for a generator in that position.
    pub hoist_all_literal_choices: bool,
}

/// Return a copy of `spec` in which every inline mixed-choice / inline-group
/// field position (subject to `options`) is replaced by a reference to a
/// synthesized rule appended to the spec. A spec with no inline composites is
/// reproduced structurally unchanged (no synth rules), so downstream output is
/// byte-identical to the un-hoisted run.
pub fn hoist_inline_composites(
    spec: &CsilSpecSerialized,
    options: HoistOptions,
) -> CsilSpecSerialized {
    let mut hoister = Hoister {
        synth: Vec::new(),
        // Reserve a case-insensitive, separator-stripped canonical key for every
        // existing rule name up front, so a synthesized name collides with — and
        // gets disambiguated against — an existing rule regardless of casing
        // convention (`UserData` reserves the same key as a later-synthesized
        // `User_data`; see `canonical_key` and the `case_insensitive_collision`
        // regression test).
        used: spec.rules.iter().map(|r| canonical_key(&r.name)).collect(),
        options,
    };
    let mut rules: Vec<CsilRule> = spec.rules.iter().map(|r| hoister.rewrite_rule(r)).collect();
    rules.extend(hoister.synth);
    CsilSpecSerialized {
        rules,
        source_content: spec.source_content.clone(),
        service_count: spec.service_count,
        fields_with_metadata_count: spec.fields_with_metadata_count,
    }
}

/// A case-insensitive, separator-stripped canonical key for a rule name:
/// lowercase, alphanumeric characters only. `User_data`, `UserData`, and
/// `user-data` all canonicalize to `"userdata"`, so a name reservation keyed on
/// this catches a collision regardless of which casing convention (PascalCase,
/// camelCase, snake_case, kebab-case) a downstream generator applies to the raw
/// rule name.
fn canonical_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The inline composite kind a position carries, if it is one the codec cannot
/// reach without a name: an inline group, or a choice `options` marks hoistable.
enum HoistKind<'a> {
    Group(&'a CsilGroupExpression),
    Choice(&'a [CsilTypeExpression]),
}

/// Whether `ty` is an inline composite needing a synthesized name, seeing through
/// a trailing control-operator wrapper (`Constrained` is peeled here only to
/// LOOK PAST it when deciding hoistability — `rewrite_type` re-wraps the
/// synthesized `Reference` in the exact same wrapper via `wrap_constrained`, so
/// the constraint is never dropped).
fn hoist_kind(ty: &CsilTypeExpression, options: HoistOptions) -> Option<HoistKind<'_>> {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => hoist_kind(base_type, options),
        CsilTypeExpression::Group(g) => Some(HoistKind::Group(g)),
        CsilTypeExpression::Choice(arms)
            if options.hoist_all_literal_choices || !all_literal(arms) =>
        {
            Some(HoistKind::Choice(arms))
        }
        _ => None,
    }
}

/// Reapply the `Constrained` wrapper(s) `ty` had around a hoistable composite
/// (if any) around `replacement`, in the same nesting order. `({...}) .default
/// X` (or any other control operator) binds to the FIELD position, not to the
/// composite's shape — the composite is what gets hoisted to a name, so the
/// constraint must stay at the use site, wrapping the new `Reference`, or it is
/// silently discarded (a confirmed review finding this function exists to fix).
fn wrap_constrained(
    ty: &CsilTypeExpression,
    replacement: CsilTypeExpression,
) -> CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained {
            base_type,
            constraints,
        } => CsilTypeExpression::Constrained {
            base_type: Box::new(wrap_constrained(base_type, replacement)),
            constraints: constraints.clone(),
        },
        _ => replacement,
    }
}

/// The name fragment identifying one group/tuple entry for a synthesized rule name:
/// the field name when the entry is keyed, else its positional index (a tuple slot
/// carries no name of its own).
fn entry_suffix(entry: &CsilGroupEntry, index: usize) -> String {
    match &entry.key {
        Some(CsilGroupKey::Bare(n)) => n.clone(),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(n))) => n.clone(),
        _ => index.to_string(),
    }
}

struct Hoister {
    synth: Vec<CsilRule>,
    used: HashSet<String>,
    options: HoistOptions,
}

impl Hoister {
    fn rewrite_rule(&mut self, rule: &CsilRule) -> CsilRule {
        let rule_type = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => CsilRuleType::GroupDef(self.rewrite_group(g, &rule.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => {
                CsilRuleType::TypeDef(CsilTypeExpression::Group(self.rewrite_group(g, &rule.name)))
            }
            CsilRuleType::TypeDef(t) => CsilRuleType::TypeDef(self.rewrite_children(t, &rule.name)),
            CsilRuleType::TypeChoice(arms) => CsilRuleType::TypeChoice(
                arms.iter()
                    .enumerate()
                    .map(|(i, a)| self.rewrite_type(a, &format!("{}_{i}", rule.name)))
                    .collect(),
            ),
            CsilRuleType::GroupChoice(groups) => CsilRuleType::GroupChoice(
                groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| self.rewrite_group(g, &format!("{}_{i}", rule.name)))
                    .collect(),
            ),
            // Service op signatures are left untouched: an operation's `Res / Error`
            // output is a choice that the client/server emitters strip specially
            // (dropping `/ ServiceError`), so hoisting it here would corrupt that path.
            CsilRuleType::ServiceDef(def) => CsilRuleType::ServiceDef(def.clone()),
        };
        CsilRule {
            name: rule.name.clone(),
            rule_type,
            position: rule.position.clone(),
            doc_comments: rule.doc_comments.clone(),
        }
    }

    fn rewrite_group(&mut self, group: &CsilGroupExpression, owner: &str) -> CsilGroupExpression {
        let entries = group
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let name = format!("{owner}_{}", entry_suffix(entry, i));
                CsilGroupEntry {
                    value_type: self.rewrite_type(&entry.value_type, &name),
                    ..entry.clone()
                }
            })
            .collect();
        CsilGroupExpression { entries }
    }

    /// Rewrite a position that may itself be an inline composite (a field value,
    /// array element, map value/key, tuple element, or choice arm). A composite is
    /// hoisted to a synthesized rule and replaced by a reference to it (re-wrapped
    /// in any `Constrained` the position carried — see `wrap_constrained`);
    /// anything else keeps its shape while its own children are rewritten.
    fn rewrite_type(&mut self, ty: &CsilTypeExpression, name: &str) -> CsilTypeExpression {
        match hoist_kind(ty, self.options) {
            Some(HoistKind::Group(g)) => {
                let synth = self.reserve(name);
                let rewritten = self.rewrite_group(g, &synth);
                self.push_rule(&synth, CsilRuleType::GroupDef(rewritten));
                wrap_constrained(ty, CsilTypeExpression::Reference(synth))
            }
            Some(HoistKind::Choice(arms)) => {
                let synth = self.reserve(name);
                let rewritten = arms
                    .iter()
                    .enumerate()
                    .map(|(i, a)| self.rewrite_type(a, &format!("{synth}_{i}")))
                    .collect();
                self.push_rule(
                    &synth,
                    CsilRuleType::TypeDef(CsilTypeExpression::Choice(rewritten)),
                );
                wrap_constrained(ty, CsilTypeExpression::Reference(synth))
            }
            None => self.rewrite_children(ty, name),
        }
    }

    /// Rewrite inline composites nested inside `ty` while preserving `ty`'s own shape
    /// (`ty` is not itself a hoistable composite). The container suffixes carry no
    /// field name — an array element becomes `<name>_item`, a map value `<name>_value`
    /// (its key `<name>_key`) — so a synthesized name reads back to its origin.
    fn rewrite_children(&mut self, ty: &CsilTypeExpression, name: &str) -> CsilTypeExpression {
        match ty {
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => CsilTypeExpression::Array {
                element_type: Box::new(self.rewrite_type(element_type, &format!("{name}_item"))),
                occurrence: occurrence.clone(),
            },
            CsilTypeExpression::Map {
                key,
                value,
                occurrence,
            } => CsilTypeExpression::Map {
                key: Box::new(self.rewrite_type(key, &format!("{name}_key"))),
                value: Box::new(self.rewrite_type(value, &format!("{name}_value"))),
                occurrence: occurrence.clone(),
            },
            CsilTypeExpression::Tuple(g) => CsilTypeExpression::Tuple(self.rewrite_group(g, name)),
            CsilTypeExpression::Group(g) => CsilTypeExpression::Group(self.rewrite_group(g, name)),
            // An all-literal choice reaches here only when `options` does not mark it
            // hoistable (its arms are literals, so recursion changes nothing, but keep
            // it general in case an arm ever nests a composite via a control operator).
            CsilTypeExpression::Choice(arms) => CsilTypeExpression::Choice(
                arms.iter()
                    .enumerate()
                    .map(|(i, a)| self.rewrite_type(a, &format!("{name}_{i}")))
                    .collect(),
            ),
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => CsilTypeExpression::Constrained {
                base_type: Box::new(self.rewrite_children(base_type, name)),
                constraints: constraints.clone(),
            },
            other => other.clone(),
        }
    }

    /// Claim a unique raw rule name, disambiguating with a numeric suffix against
    /// both the original rules and previously synthesized ones. Collision is
    /// decided on the CASE-INSENSITIVE, separator-stripped canonical key (see
    /// `canonical_key`), not the raw name, so `User_data` and an existing
    /// `UserData` rule are recognized as the same name under every casing
    /// convention a generator might apply and one of them gets disambiguated —
    /// the raw candidate returned is still the original, readable spelling.
    fn reserve(&mut self, base: &str) -> String {
        let mut candidate = base.to_string();
        let mut n = 2;
        while !self.used.insert(canonical_key(&candidate)) {
            candidate = format!("{base}_{n}");
            n += 1;
        }
        candidate
    }

    fn push_rule(&mut self, name: &str, rule_type: CsilRuleType) {
        self.synth.push(CsilRule {
            name: name.to_string(),
            rule_type,
            position: CsilPosition {
                line: 0,
                column: 0,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CsilControlOperator;

    fn builtin(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Builtin(name.to_string())
    }

    fn text_literal(s: &str) -> CsilTypeExpression {
        CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
    }

    fn bare_entry(key: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(key.to_string())),
            value_type,
            occurrence: None,
            metadata: vec![],
            doc_comments: vec![],
        }
    }

    fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: CsilPosition {
                line: 0,
                column: 0,
                offset: 0,
            },
            doc_comments: vec![],
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

    fn rule_names(spec: &CsilSpecSerialized) -> Vec<&str> {
        spec.rules.iter().map(|r| r.name.as_str()).collect()
    }

    /// Finding (a): an existing rule named `UserData` and a synthesized name
    /// `User_data` (from owner `User`, field `data`) pascal-collide — both
    /// canonicalize to `"userdata"`. Reservation must catch this across the
    /// casing convention divide and deterministically suffix the later one, or a
    /// generator emits two declarations for the same case-normalized identifier
    /// (a duplicate, non-compiling TS/Java/etc. declaration).
    #[test]
    fn case_insensitive_collision_between_existing_and_synthesized_rule_is_disambiguated() {
        let user_data = group_rule("UserData", vec![bare_entry("value", builtin("text"))]);
        // `User.data` is an inline mixed choice, so it hoists to a synthesized rule
        // named `User_data` (owner `User`, field `data`) — which pascal-collides
        // with the existing `UserData` rule above.
        let user = group_rule(
            "User",
            vec![bare_entry(
                "data",
                CsilTypeExpression::Choice(vec![
                    text_literal("x"),
                    CsilTypeExpression::Reference("UserData".to_string()),
                ]),
            )],
        );
        let input = spec(vec![user_data, user]);
        let hoisted = hoist_inline_composites(&input, HoistOptions::default());

        let names = rule_names(&hoisted);
        // The original `UserData` rule survives unchanged.
        assert!(names.contains(&"UserData"));
        // The synthesized rule must NOT be the raw "User_data" (that collides);
        // it must be a distinct, disambiguated name.
        assert!(
            !names.contains(&"User_data"),
            "synthesized name collided with UserData but was not disambiguated: {names:?}"
        );
        // Every rule name is unique even under the case-insensitive canonical key.
        let mut canonical: Vec<String> = names.iter().map(|n| canonical_key(n)).collect();
        let before = canonical.len();
        canonical.sort();
        canonical.dedup();
        assert_eq!(
            canonical.len(),
            before,
            "two rules share a case-insensitive canonical name: {names:?}"
        );
    }

    /// Finding (b): `({...}) .default X`-shaped field — an inline composite
    /// wrapped in a `Constrained`. Hoisting must move the INNER composite to a
    /// synthesized rule while keeping the `Constrained` wrapper (and its
    /// `.default` constraint) at the use site, around the new `Reference` — not
    /// discard it.
    #[test]
    fn constrained_wrapper_around_hoistable_composite_is_preserved_at_use_site() {
        let mixed_choice = CsilTypeExpression::Choice(vec![
            text_literal("a"),
            CsilTypeExpression::Reference("Widget".to_string()),
        ]);
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(mixed_choice.clone()),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "a".to_string(),
            ))],
        };
        let widget = group_rule("Widget", vec![bare_entry("name", builtin("text"))]);
        let owner = group_rule("Owner", vec![bare_entry("field", constrained)]);
        let input = spec(vec![widget, owner]);
        let hoisted = hoist_inline_composites(&input, HoistOptions::default());

        let owner_rule = hoisted
            .rules
            .iter()
            .find(|r| r.name == "Owner")
            .expect("Owner rule survives");
        let CsilRuleType::GroupDef(group) = &owner_rule.rule_type else {
            panic!("Owner is a GroupDef");
        };
        let field = &group.entries[0].value_type;
        match field {
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => {
                assert_eq!(
                    constraints,
                    &vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                        "a".to_string()
                    ))],
                    "the .default constraint must survive hoisting at the use site"
                );
                assert!(
                    matches!(base_type.as_ref(), CsilTypeExpression::Reference(_)),
                    "the composite itself must be hoisted to a Reference: {base_type:?}"
                );
            }
            other => panic!("expected the Constrained wrapper to survive hoisting, got {other:?}"),
        }

        // The hoisted composite is reachable as its own named rule with the
        // original (unconstrained) choice shape.
        let CsilTypeExpression::Constrained { base_type, .. } = field else {
            unreachable!()
        };
        let CsilTypeExpression::Reference(synth_name) = base_type.as_ref() else {
            unreachable!()
        };
        let synth_rule = hoisted
            .rules
            .iter()
            .find(|r| &r.name == synth_name)
            .expect("synthesized rule exists");
        assert!(matches!(
            &synth_rule.rule_type,
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(_))
        ));
    }

    #[test]
    fn no_inline_composites_is_structurally_unchanged() {
        let plain = group_rule("Plain", vec![bare_entry("name", builtin("text"))]);
        let input = spec(vec![plain]);
        let hoisted = hoist_inline_composites(&input, HoistOptions::default());
        assert_eq!(hoisted.rules.len(), 1);
        assert_eq!(hoisted.rules[0].name, "Plain");
    }

    #[test]
    fn all_literal_choice_stays_inline_by_default() {
        let rule = group_rule(
            "Order",
            vec![bare_entry(
                "status",
                CsilTypeExpression::Choice(vec![text_literal("pending"), text_literal("shipped")]),
            )],
        );
        let input = spec(vec![rule]);
        let hoisted = hoist_inline_composites(&input, HoistOptions::default());
        assert_eq!(
            hoisted.rules.len(),
            1,
            "an all-literal choice is not hoisted by default"
        );
    }

    /// `hoist_all_literal_choices: true` (OCaml's setting) hoists even a closed
    /// all-literal choice, since OCaml has no anonymous-sum-type field syntax to
    /// fall back to.
    #[test]
    fn all_literal_choice_is_hoisted_when_option_is_set() {
        let rule = group_rule(
            "Order",
            vec![bare_entry(
                "status",
                CsilTypeExpression::Choice(vec![text_literal("pending"), text_literal("shipped")]),
            )],
        );
        let input = spec(vec![rule]);
        let hoisted = hoist_inline_composites(
            &input,
            HoistOptions {
                hoist_all_literal_choices: true,
            },
        );
        assert_eq!(
            hoisted.rules.len(),
            2,
            "the all-literal choice must be hoisted"
        );
        let CsilRuleType::GroupDef(group) = &hoisted.rules[0].rule_type else {
            panic!("Order is a GroupDef");
        };
        assert!(matches!(
            group.entries[0].value_type,
            CsilTypeExpression::Reference(_)
        ));
    }
}
