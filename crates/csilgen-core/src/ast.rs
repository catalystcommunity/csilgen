//! Abstract Syntax Tree definitions for CSIL

use crate::lexer::Position;
use serde::{Deserialize, Serialize};

/// Root AST node representing a complete CSIL interface definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilSpec {
    pub imports: Vec<ImportStatement>,
    pub options: Option<FileOptions>,
    pub rules: Vec<Rule>,
}

/// Import statements for including other CSIL files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImportStatement {
    /// Simple include with optional alias: `include "path/file.csil" as alias`
    Include {
        path: String,
        alias: Option<String>,
        position: Position,
    },
    /// Selective import: `from "path/file.csil" include Type1, Type2`
    SelectiveImport {
        path: String,
        items: Vec<String>,
        position: Position,
    },
}

/// File-level options block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOptions {
    pub entries: Vec<OptionEntry>,
    pub position: Position,
}

/// Individual entry in file options block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionEntry {
    pub key: String,
    pub value: LiteralValue,
    pub position: Position,
}

/// A CSIL rule definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub rule_type: RuleType,
    pub position: Position,
    /// Documentation comments (;;;) preceding this rule
    #[serde(default)]
    pub doc_comments: Vec<String>,
}

/// Types of CSIL rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleType {
    /// Type definition rule (=)
    TypeDef(TypeExpression),
    /// Group definition rule (=)
    GroupDef(GroupExpression),
    /// Type choice rule (/=). The parser produces this for a raw `/=` statement, and
    /// `merge_type_choice_extensions` folds it onto the `TypeDef` it extends — per RFC
    /// 8610 socket-extension semantics, a standalone `/=` is sugar for `=` with a
    /// choice, and a `/=` on an existing name appends arms to it. That fold only
    /// finalizes (collapsing an extension with no `=` base into its own `TypeDef`) once
    /// the whole reachable include graph is merged in, i.e. by the top-level
    /// `ImportResolver::resolve_imports` call — so a `CsilSpec` produced by bare
    /// `parse_csil`/`parse_csil_file` (as `csilgen format`/`csilgen lint` do, and as a
    /// leaf file mid-resolution does) can still carry a `TypeChoice` rule whose arms
    /// are merged but not yet homed. Every consumer that also resolves imports (the
    /// `validate`/`generate` CLI paths, and so every WASM generator) never sees this
    /// variant constructed from source.
    TypeChoice(Vec<TypeExpression>),
    /// Group choice rule (//=)
    GroupChoice(Vec<GroupExpression>),
    /// Service definition
    ServiceDef(ServiceDefinition),
}

/// Fold every `/=` (type choice) rule into the `TypeDef` it extends, per CDDL's
/// socket-extension semantics (RFC 8610 SS3.8): a rule name may be defined once with
/// `=` and extended any number of times with `/=`, in any order and across files — all
/// `/=` arms for a name become alternatives of that name's choice.
///
/// Two real `=` (or `//=`/service) definitions sharing a name are left untouched: that
/// is a genuine collision, not an extension, and `validate_unique_rule_names` reports
/// it as `DuplicateRule`.
///
/// `collapse_orphans` controls what happens to a name that has `/=` rules but no real
/// `=` base *in the rule set passed in*: with `collapse_orphans: false`, those arms are
/// merged together but the rule stays tagged `TypeChoice` (its arms may yet find a base
/// once more files are merged in); with `true`, a name that still has no base is
/// finalized as its own `TypeDef` — a standalone `Name /= a / b` then produces exactly
/// the same AST as `Name = a / b`. `true` must only run once the whole include graph
/// reachable from the file under resolution has been merged in, since a leaf file
/// resolved in isolation can't tell "no base anywhere" from "the base is in whichever
/// file includes me" (see `ImportResolver::resolve_imports` vs
/// `resolve_imports_uncollapsed`).
pub(crate) fn merge_type_choice_extensions(rules: &mut Vec<Rule>, collapse_orphans: bool) {
    use std::collections::HashMap;

    // The first non-`/=` rule for a name is the real base every `/=` arm folds onto.
    let mut base_index: HashMap<String, usize> = HashMap::new();
    for (idx, rule) in rules.iter().enumerate() {
        if !matches!(rule.rule_type, RuleType::TypeChoice(_))
            && !base_index.contains_key(&rule.name)
        {
            base_index.insert(rule.name.clone(), idx);
        }
    }

    // Group every `/=` rule's index by name, preserving first-seen order, so arms from
    // separate `/=` statements for the same name concatenate in declaration order.
    let mut extension_order: Vec<String> = Vec::new();
    let mut extensions_by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, rule) in rules.iter().enumerate() {
        if matches!(rule.rule_type, RuleType::TypeChoice(_)) {
            extensions_by_name
                .entry(rule.name.clone())
                .or_insert_with(|| {
                    extension_order.push(rule.name.clone());
                    Vec::new()
                })
                .push(idx);
        }
    }

    if extension_order.is_empty() {
        return;
    }

    let mut to_remove: Vec<usize> = Vec::new();

    for name in extension_order {
        let idxs = extensions_by_name
            .remove(&name)
            .expect("just collected above");

        let mut combined_arms: Vec<TypeExpression> = Vec::new();
        for &idx in &idxs {
            if let RuleType::TypeChoice(arms) = &rules[idx].rule_type {
                combined_arms.extend(arms.iter().cloned());
            }
        }

        if let Some(&target_idx) = base_index.get(&name) {
            // Take the base's type by value so the merge can move its arms in without a
            // second live borrow of `rules[target_idx]`.
            let existing = std::mem::replace(
                &mut rules[target_idx].rule_type,
                RuleType::TypeDef(TypeExpression::Builtin(String::new())),
            );
            let merged = match existing {
                RuleType::TypeDef(TypeExpression::Choice(mut existing_arms)) => {
                    existing_arms.extend(combined_arms);
                    TypeExpression::Choice(existing_arms)
                }
                RuleType::TypeDef(base_type) => {
                    let mut merged_arms = vec![base_type];
                    merged_arms.extend(combined_arms);
                    TypeExpression::Choice(merged_arms)
                }
                other => {
                    // Not actually a type rule (group/service): restore it untouched
                    // and leave every `/=` rule in place too, so a `/=` on a
                    // group/service name surfaces as a name collision instead of
                    // silently vanishing.
                    rules[target_idx].rule_type = other;
                    continue;
                }
            };
            rules[target_idx].rule_type = RuleType::TypeDef(merged);
            to_remove.extend(idxs);
        } else if collapse_orphans {
            // No `=` base anywhere in the fully resolved rule set: the first `/=`
            // becomes the base. A single arm collapses to a plain type instead of a
            // one-arm choice, matching what `Name = <that type>` would parse to.
            let keep_idx = idxs[0];
            let type_expr = if combined_arms.len() == 1 {
                combined_arms.into_iter().next().expect("len checked above")
            } else {
                TypeExpression::Choice(combined_arms)
            };
            rules[keep_idx].rule_type = RuleType::TypeDef(type_expr);
            to_remove.extend(idxs.into_iter().skip(1));
        } else {
            // Not the final resolution pass: keep the rule tagged `TypeChoice` (merged
            // arms, still no base) so a later pass — either this same file included
            // from elsewhere, or the top-level `resolve_imports` finalize pass — can
            // still recognize it as an extension in need of a base.
            let keep_idx = idxs[0];
            rules[keep_idx].rule_type = RuleType::TypeChoice(combined_arms);
            to_remove.extend(idxs.into_iter().skip(1));
        }
    }

    to_remove.sort_unstable();
    for idx in to_remove.into_iter().rev() {
        rules.remove(idx);
    }
}

/// CSIL type expressions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeExpression {
    /// Built-in types (int, text, bool, etc.)
    Builtin(String),
    /// User-defined type reference
    Reference(String),
    /// Array type with occurrence (a homogeneous `[* T]` / `[N T]` array)
    Array {
        element_type: Box<TypeExpression>,
        occurrence: Option<Occurrence>,
    },
    /// Fixed-shape array: a heterogeneous tuple `[a, b, c]` or a keyed array
    /// `[tag: text, value: any]`. Entries are positional; keys are names only.
    Tuple(GroupExpression),
    /// Map type
    Map {
        key: Box<TypeExpression>,
        value: Box<TypeExpression>,
        occurrence: Option<Occurrence>,
    },
    /// Group expression
    Group(GroupExpression),
    /// Choice between types (type1 / type2 / type3)
    Choice(Vec<TypeExpression>),
    /// Range expression
    Range {
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    },
    /// Socket reference
    Socket(String),
    /// Plug reference
    Plug(String),
    /// Literal values
    Literal(LiteralValue),
    /// Type with CDDL control operators (constraints)
    Constrained {
        base_type: Box<TypeExpression>,
        constraints: Vec<ControlOperator>,
    },
}

/// CDDL control operators for type constraints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlOperator {
    /// Size constraint: .size (min..max) or .size value
    Size(SizeConstraint),
    /// Regular expression constraint: .regex "pattern"  
    Regex(String),
    /// Default value constraint: .default value
    Default(LiteralValue),
    /// Greater than or equal constraint: .ge value
    GreaterEqual(LiteralValue),
    /// Less than or equal constraint: .le value
    LessEqual(LiteralValue),
    /// Greater than constraint: .gt value
    GreaterThan(LiteralValue),
    /// Less than constraint: .lt value
    LessThan(LiteralValue),
    /// Equal to constraint: .eq value
    Equal(LiteralValue),
    /// Not equal constraint: .ne value
    NotEqual(LiteralValue),
    /// Bit control constraint: .bits bits-expression
    Bits(String),
    /// Type intersection constraint: .and type-expression
    And(Box<TypeExpression>),
    /// Subset constraint: .within type-expression
    Within(Box<TypeExpression>),
    /// JSON encoding constraint: .json
    Json,
    /// CBOR encoding constraint: .cbor
    Cbor,
    /// CBOR sequence constraint: .cborseq
    Cborseq,
}

/// Size constraint specifications
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SizeConstraint {
    /// Exact size: .size 5
    Exact(u64),
    /// Range size: .size (1..10)
    Range { min: u64, max: u64 },
    /// Minimum size: .size (5..)
    Min(u64),
    /// Maximum size: .size (..10)
    Max(u64),
}

/// Literal values in CDDL
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LiteralValue {
    Integer(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Null,
    Array(Vec<LiteralValue>),
}

/// CDDL occurrence indicators
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Occurrence {
    /// Optional (?)
    Optional,
    /// Zero or more (*)
    ZeroOrMore,
    /// One or more (+)
    OneOrMore,
    /// Exact count (5)
    Exact(u64),
    /// Range (1*5, *10, 1*)
    Range { min: Option<u64>, max: Option<u64> },
}

/// CSIL group expressions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExpression {
    pub entries: Vec<GroupEntry>,
}

/// Individual entries in a group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupEntry {
    pub key: Option<GroupKey>,
    pub value_type: TypeExpression,
    pub occurrence: Option<Occurrence>,
    pub metadata: Vec<FieldMetadata>,
    /// Documentation comments (;;;) preceding this field
    #[serde(default)]
    pub doc_comments: Vec<String>,
}

/// Group key types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GroupKey {
    /// Bare key (identifier)
    Bare(String),
    /// Type key (type:)
    Type(TypeExpression),
    /// Literal key ("string": or 42:)
    Literal(LiteralValue),
}

/// CSIL service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDefinition {
    pub operations: Vec<ServiceOperation>,
    /// Annotations preceding the `service` keyword (e.g. `@wire-id`)
    #[serde(default)]
    pub metadata: Vec<FieldMetadata>,
}

impl ServiceDefinition {
    /// The `@wire-id(N)` service ordinal, if assigned.
    pub fn wire_id(&self) -> Option<u64> {
        wire_id_of(&self.metadata)
    }
}

/// A service operation definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceOperation {
    pub name: String,
    pub input_type: TypeExpression,
    pub output_type: TypeExpression,
    pub direction: ServiceDirection,
    pub position: Position,
    /// Documentation comments (;;;) preceding this operation
    #[serde(default)]
    pub doc_comments: Vec<String>,
    /// Annotations preceding this operation (e.g. `@wire-id`)
    #[serde(default)]
    pub metadata: Vec<FieldMetadata>,
}

impl ServiceOperation {
    /// The `@wire-id(N)` operation ordinal, if assigned.
    pub fn wire_id(&self) -> Option<u64> {
        wire_id_of(&self.metadata)
    }
}

/// How a `@wire-id` annotation resolved on a service or operation. This is the
/// single source of truth shared by the validator (which reports `Invalid`) and
/// by `wire_id()` (used by the WASM-boundary conversion and breaking-change
/// detection), so the two can never disagree about what a given annotation means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireIdState {
    /// No `@wire-id` annotation present.
    Absent,
    /// Present and valid.
    Valid(u64),
    /// Present but the argument was missing, negative, non-integer, or there was
    /// more than one argument.
    Invalid,
}

/// Resolve the `@wire-id` annotation (if any) from a metadata list, distinguishing
/// absent from malformed. The argument must be exactly one non-negative integer.
pub fn wire_id_state(metadata: &[FieldMetadata]) -> WireIdState {
    for m in metadata {
        if let FieldMetadata::Custom { name, parameters } = m
            && name == "wire-id"
        {
            return match parameters.first().map(|p| &p.value) {
                Some(LiteralValue::Integer(n)) if *n >= 0 && parameters.len() == 1 => {
                    WireIdState::Valid(*n as u64)
                }
                _ => WireIdState::Invalid,
            };
        }
    }
    WireIdState::Absent
}

/// The resolved ordinal, or `None` when absent **or malformed**. Callers that need
/// to distinguish the two (the validator) use [`wire_id_state`] directly.
fn wire_id_of(metadata: &[FieldMetadata]) -> Option<u64> {
    match wire_id_state(metadata) {
        WireIdState::Valid(n) => Some(n),
        WireIdState::Absent | WireIdState::Invalid => None,
    }
}

/// Direction of service operation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServiceDirection {
    /// Unidirectional operation (input -> output)
    Unidirectional,
    /// Bidirectional operation (input <-> output)
    Bidirectional,
    /// Reverse operation (input <- output, rarely used)
    Reverse,
}

/// CSIL field metadata annotations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldMetadata {
    /// Field visibility metadata
    Visibility(FieldVisibility),
    /// Field dependency metadata — the simple single-comparison form
    /// (`@depends-on(field)` / `@depends-on(field = value)`).
    DependsOn {
        field: String,
        value: Option<LiteralValue>,
    },
    /// Field dependency on a boolean condition
    /// (`@depends-on(a = "x" & b != "y")`, etc.).
    DependsOnExpr(DependsCondition),
    /// Validation constraint metadata
    Constraint(ValidationConstraint),
    /// Documentation metadata
    Description(String),
    /// Custom generator hints
    Custom {
        name: String,
        parameters: Vec<MetadataParameter>,
    },
}

/// A `@depends-on(...)` boolean condition. `&` is conjunction, `|` is
/// disjunction; a bare field tests presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DependsCondition {
    /// `field` (presence, `op`/`value` are `None`) or `field <op> value`
    Compare {
        field: String,
        op: Option<DependsCompareOp>,
        value: Option<LiteralValue>,
    },
    /// All sub-conditions must hold (`&`)
    All(Vec<DependsCondition>),
    /// Any sub-condition must hold (`|`)
    Any(Vec<DependsCondition>),
}

/// Comparison operator within a `@depends-on` condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependsCompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Field visibility annotations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldVisibility {
    /// Field is only included in outgoing requests/messages
    SendOnly,
    /// Field is only included in incoming responses/messages
    ReceiveOnly,
    /// Field is included in both directions (default behavior)
    Bidirectional,
}

/// Validation constraint metadata
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValidationConstraint {
    /// Minimum length for strings/arrays
    MinLength(u64),
    /// Maximum length for strings/arrays
    MaxLength(u64),
    /// Minimum number of items for arrays/maps
    MinItems(u64),
    /// Maximum number of items for arrays/maps
    MaxItems(u64),
    /// Minimum value for numeric types
    MinValue(LiteralValue),
    /// Maximum value for numeric types
    MaxValue(LiteralValue),
    /// Custom validation constraint
    Custom { name: String, value: LiteralValue },
}

/// Parameter for metadata annotations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataParameter {
    pub name: Option<String>,
    pub value: LiteralValue,
}
