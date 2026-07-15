//! Shared classification of a CSIL choice (`A / B / ...`) into the two wire shapes
//! every per-language codec must agree on.
//!
//! # THE contract (normative)
//!
//! A choice classifies by counting its arms' literal-ness, seeing through a
//! trailing control-operator wrapper on any one arm (`choice_arm_literal`):
//!
//! - **Every arm is a literal, of ANY kind (or mix of kinds)** — `"a" / "b"`,
//!   `1 / 2`, and `"a" / 1` are ALL an `Enum`. The wire form is the bare literal
//!   value itself (no discriminator), and decode is a membership check against
//!   the declared vocabulary — never a type-uniformity check. A generator that
//!   requires every arm to share one JS/OCaml/etc. runtime type before it will
//!   treat a choice as an enum is not following this contract (this was a real
//!   bug in both the TypeScript and OCaml generators before this module: TS's
//!   decode picked a single scalar reader by uniform kind and broke on a
//!   non-conforming member; OCaml required a uniform text-only or int-only
//!   vocabulary and misclassified a mixed one as a `Union`).
//! - **At least one arm is NOT a literal** — a `Union`, a tagged sum
//!   `[declaration_index, value]` on the wire, decode dispatches by index.
//!
//! One parser fact that trips up a naive implementation: a bare `null` arm in a
//! choice parses to `CsilTypeExpression::Builtin("null")`, never
//! `CsilTypeExpression::Literal(CsilLiteralValue::Null)` — `choice_arm_literal`
//! therefore returns `None` for it, same as any other builtin/reference/group
//! arm. So `"a" / 1 / null` is a `Union` (the `null` arm is the non-literal one),
//! matching what the Python/TypeScript/Go/PHP generators already agree on for
//! that shape. A `CsilLiteralValue::Null` literal arm can still occur (e.g. a
//! `.default null`-constrained arm), and it is treated as any other literal kind.

use crate::{CsilLiteralValue, CsilTypeExpression};

/// The literal a choice arm carries, if any, stripping a trailing control-operator
/// wrapper first. CSIL's grammar attaches a control operator to the immediately
/// preceding primary type, so the last arm of `text / "a" / "b" .default "b"`
/// parses as `Constrained { base_type: Literal("b"), .. }`, not a bare `Literal`
/// — the `.default` binds to that one arm, not to the choice as a whole. Every
/// choice-arm literal check must see through that wrapper or a `.default`-suffixed
/// literal arm falls out of the enum/union shape it belongs to.
pub fn choice_arm_literal(ty: &CsilTypeExpression) -> Option<&CsilLiteralValue> {
    match ty {
        CsilTypeExpression::Literal(lit) => Some(lit),
        CsilTypeExpression::Constrained { base_type, .. } => choice_arm_literal(base_type),
        _ => None,
    }
}

/// One arm of a classified choice: a literal value (validated by equality/
/// membership on decode) or a general, structurally-typed arm.
#[derive(Debug, Clone, Copy)]
pub enum ChoiceArmKind<'a> {
    Literal(&'a CsilLiteralValue),
    General(&'a CsilTypeExpression),
}

/// The two shapes a CSIL choice classifies into. See the module docs for the
/// normative contract. `Union`'s arms are in declaration order (`arms[i]` is
/// wire index `i`); `Enum`'s literals are likewise in declaration order.
#[derive(Debug, Clone)]
pub enum ChoiceClass<'a> {
    /// Every arm is a literal (any kind, possibly mixed) — a bare-literal wire
    /// enum. Decode must validate membership in this list, not merely a runtime
    /// type match, so an out-of-vocabulary value of the right JS/OCaml/etc. type
    /// (e.g. `"c"` when only `"a"`/`"b"` are declared) is still rejected.
    Enum(Vec<&'a CsilLiteralValue>),
    /// At least one arm is not a literal — a tagged-sum union,
    /// `[declaration_index, value]` on the wire.
    Union(Vec<ChoiceArmKind<'a>>),
}

/// Classify `choices` per the contract documented at module level. An empty
/// choice list classifies as an empty `Union` (there is no literal vocabulary to
/// be an `Enum` of); CSIL's grammar never actually produces one, but a
/// generator building a `CsilTypeExpression::Choice` synthetically (a hoist
/// pass, a test fixture) should not have to special-case it.
pub fn classify_choice(choices: &[CsilTypeExpression]) -> ChoiceClass<'_> {
    if !choices.is_empty() {
        let literals: Option<Vec<&CsilLiteralValue>> =
            choices.iter().map(choice_arm_literal).collect();
        if let Some(literals) = literals {
            return ChoiceClass::Enum(literals);
        }
    }
    ChoiceClass::Union(
        choices
            .iter()
            .map(|c| match choice_arm_literal(c) {
                Some(lit) => ChoiceArmKind::Literal(lit),
                None => ChoiceArmKind::General(c),
            })
            .collect(),
    )
}

/// Whether `choices` classifies as an `Enum` (every arm a literal, of any kind).
/// A thin convenience over `classify_choice` for a call site that only needs the
/// boolean, not the literals themselves.
pub fn all_literal(choices: &[CsilTypeExpression]) -> bool {
    matches!(classify_choice(choices), ChoiceClass::Enum(_))
}

/// Declaration indices of a `Union`'s arms, reordered so every literal arm comes
/// before any general arm (stable within each group — relative order among
/// literals, and among general arms, is unchanged).
///
/// This matters only to a generator whose union ENCODE dispatch is a sequential
/// runtime-type predicate chain (TypeScript's `emit_union_codec` is the
/// reference case): a general arm's predicate (`typeof v === "string"`) matches
/// every value of that base type, including ones a literal arm of the same
/// union declares more specifically (`text / "pending"`). Checking literal arms
/// first gives them precedence while leaving the WIRE index (declaration order,
/// `arms[i]` is index `i`) unchanged — only the order the encoder's `if` chain
/// tests them in changes. A generator that dispatches by constructor pattern
/// match instead (OCaml, Rust) has no such shadowing and has no use for this.
pub fn union_encode_order(arms: &[ChoiceArmKind]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..arms.len()).collect();
    order.sort_by_key(|&i| !matches!(arms[i], ChoiceArmKind::Literal(_)));
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> CsilTypeExpression {
        CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
    }

    fn int(n: i64) -> CsilTypeExpression {
        CsilTypeExpression::Literal(CsilLiteralValue::Integer(n))
    }

    #[test]
    fn uniform_text_choice_is_enum() {
        let choices = vec![text("a"), text("b")];
        assert!(matches!(classify_choice(&choices), ChoiceClass::Enum(_)));
        assert!(all_literal(&choices));
    }

    #[test]
    fn uniform_int_choice_is_enum() {
        let choices = vec![int(1), int(2)];
        assert!(matches!(classify_choice(&choices), ChoiceClass::Enum(_)));
    }

    /// The mixed-kind regression this module exists to fix: `"a" / 1` is ALL
    /// literal (a text literal and an integer literal), so it must classify as
    /// an `Enum` exactly like a uniform-kind literal choice, not a `Union`.
    #[test]
    fn mixed_kind_literal_choice_is_enum_not_union() {
        let choices = vec![text("a"), int(1)];
        match classify_choice(&choices) {
            ChoiceClass::Enum(literals) => {
                assert_eq!(literals.len(), 2);
                assert_eq!(literals[0], &CsilLiteralValue::Text("a".to_string()));
                assert_eq!(literals[1], &CsilLiteralValue::Integer(1));
            }
            ChoiceClass::Union(_) => panic!("\"a\" / 1 must classify as Enum, not Union"),
        }
    }

    /// A bare `null` arm parses to `Builtin("null")`, never `Literal(Null)` (see
    /// module docs) — `choice_arm_literal` must treat it as a non-literal arm, so
    /// a choice containing one is a `Union` even when every OTHER arm is a
    /// literal, matching the Python/TypeScript/Go/PHP generators.
    #[test]
    fn bare_null_arm_is_builtin_not_literal_so_choice_is_union() {
        let choices = vec![
            text("a"),
            int(1),
            CsilTypeExpression::Builtin("null".to_string()),
        ];
        match classify_choice(&choices) {
            ChoiceClass::Union(arms) => {
                assert_eq!(arms.len(), 3);
                assert!(matches!(arms[0], ChoiceArmKind::Literal(_)));
                assert!(matches!(arms[1], ChoiceArmKind::Literal(_)));
                assert!(matches!(arms[2], ChoiceArmKind::General(_)));
            }
            ChoiceClass::Enum(_) => panic!("a bare null arm is Builtin, not Literal(Null)"),
        }
        assert!(!all_literal(&choices));
    }

    #[test]
    fn a_reference_arm_makes_it_a_union() {
        let choices = vec![
            text("a"),
            CsilTypeExpression::Reference("Widget".to_string()),
        ];
        assert!(matches!(classify_choice(&choices), ChoiceClass::Union(_)));
    }

    #[test]
    fn constrained_literal_arm_still_counts_as_literal() {
        // `text / "a" / "b" .default "b"` — the last arm is `Constrained { base_type:
        // Literal("b"), .. }`, still a literal arm once unwrapped.
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(text("b")),
            constraints: vec![],
        };
        let choices = vec![text("a"), constrained];
        assert!(all_literal(&choices));
    }

    #[test]
    fn union_encode_order_puts_literals_first_stably() {
        let general_widget = CsilTypeExpression::Reference("Widget".to_string());
        let general_text = CsilTypeExpression::Builtin("text".to_string());
        let a = text("a");
        let b = text("b");
        let choices = vec![
            general_widget.clone(),
            a.clone(),
            general_text.clone(),
            b.clone(),
        ];
        let ChoiceClass::Union(arms) = classify_choice(&choices) else {
            panic!("expected Union");
        };
        let order = union_encode_order(&arms);
        // Literal arms (original indices 1, 3) come first, in their original
        // relative order; general arms (0, 2) follow, likewise stable.
        assert_eq!(order, vec![1, 3, 0, 2]);
    }

    #[test]
    fn empty_choice_is_union_not_enum() {
        let choices: Vec<CsilTypeExpression> = vec![];
        match classify_choice(&choices) {
            ChoiceClass::Union(arms) => assert!(arms.is_empty()),
            ChoiceClass::Enum(_) => panic!("an empty choice has no literal vocabulary"),
        }
        assert!(!all_literal(&choices));
    }
}
