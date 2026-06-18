//! Emits `types.gen.ts`: interfaces, type aliases, unions, and the shared
//! `ServiceError` shape when the spec declares services.

use crate::common::{self, DecimalMapping};
use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilRuleType,
    CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    WasmGeneratorInput,
};

pub fn generate(input: &WasmGeneratorInput) -> String {
    let spec = &input.csil_spec;
    // Validated in `generate_files` before any file is emitted; a bad value
    // never reaches here, so default to `csil` rather than re-surfacing an error
    // from this infallible path.
    let mapping = common::decimal_mapping(input).unwrap_or(DecimalMapping::Csil);
    let uses_decimal = common::spec_uses_decimal(spec);
    let mut out = common::header(input, "typescript-typesonly");

    // The `decimal.js` import must lead the file; only library mode needs it and
    // only when the spec actually carries a `decimal`.
    if uses_decimal && mapping == DecimalMapping::Library {
        out.push_str(DECIMAL_JS_IMPORT);
        out.push('\n');
    }

    // Emit the synthetic transport error unless the spec already declares one
    if common::has_services(spec) && !declares_service_error(spec) {
        out.push_str(SERVICE_ERROR);
        out.push('\n');
    }

    // The self-contained helper is injected once, only when `decimal` is used and
    // the consumer opted out of `decimal.js`.
    if uses_decimal && mapping == DecimalMapping::Csil {
        out.push_str(CSIL_DECIMAL);
        out.push('\n');
    }

    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                out.push_str(&interface(&rule.name, group, &rule.doc_comments, mapping));
            }
            CsilRuleType::TypeDef(type_expr) => {
                out.push_str(&type_alias(
                    &rule.name,
                    type_expr,
                    &rule.doc_comments,
                    mapping,
                ));
            }
            CsilRuleType::GroupDef(group) => {
                out.push_str(&interface(&rule.name, group, &rule.doc_comments, mapping));
            }
            CsilRuleType::TypeChoice(choices) => {
                let union = choices
                    .iter()
                    .map(|c| common::ts_type(c, mapping))
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
                    .map(|g| inline_object(g, mapping))
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

    // Runtime validators trail the type declarations they reference.
    for rule in &spec.rules {
        if let Some(validator) = validator_for(rule, mapping) {
            out.push_str(&validator);
            out.push('\n');
        }
    }

    out
}

const SERVICE_ERROR: &str = "\
export interface ServiceError {
  code: number;
  message: string;
}
";

// Library mode pulls the decimal type from `decimal.js`. The generator emits no
// package.json, so this import is the sole place the dependency is declared:
// consumers must add `decimal.js` to their own manifest when they select
// `decimal_mapping: "library"`. Default (`csil`) mode emits no such import.
const DECIMAL_JS_IMPORT: &str = "import Decimal from \"decimal.js\";\n";

// Self-contained exact decimal. Holds the CBOR tag-4 payload verbatim
// (`exponent` + `mantissa`, value = mantissa * 10^exponent) so a round trip
// through the wire is lossless, and bridges to/from `decimal.js` purely through
// the canonical string — no `import` of that library in default mode.
const CSIL_DECIMAL: &str = r#"/**
 * An exact base-10 decimal, carried on the wire as a CBOR tag-4 decimal
 * fraction `[exponent, mantissa]` (value = `mantissa * 10 ** exponent`).
 *
 * Emitted only when the spec uses `decimal` and `decimal_mapping` is `"csil"`
 * (the default), so the generated output depends on no third-party package.
 * Bridge to/from `decimal.js` via the canonical string:
 * `new Decimal(d.toString())` / `CsilDecimal.fromString(dec.toString())`.
 */
export class CsilDecimal {
  /** CBOR semantic tag for a decimal fraction. */
  static readonly CBOR_TAG = 4;

  constructor(
    readonly exponent: number,
    readonly mantissa: bigint,
  ) {}

  /** Reconstruct from the CBOR tag-4 payload `[exponent, mantissa]`. */
  static fromTag4(payload: readonly [number | bigint, number | bigint]): CsilDecimal {
    return new CsilDecimal(Number(payload[0]), BigInt(payload[1]));
  }

  /**
   * The CBOR tag-4 payload `[exponent, mantissa]`. The transport owns CBOR
   * encoding; hand this (tagged 4) to the encoder, e.g.
   * `new Tagged(CsilDecimal.CBOR_TAG, d.toTag4())`.
   */
  toTag4(): [number, bigint] {
    return [this.exponent, this.mantissa];
  }

  /**
   * Sign of `this - other` as `-1`, `0`, or `1`. Exact: both values are rescaled
   * to a shared exponent and compared as bigints, so no float rounding occurs.
   * Drives generated validation guards (`d.compare(bound) >= 0`).
   */
  compare(other: CsilDecimal): number {
    const exponent = Math.min(this.exponent, other.exponent);
    const left = this.mantissa * 10n ** BigInt(this.exponent - exponent);
    const right = other.mantissa * 10n ** BigInt(other.exponent - exponent);
    return left < right ? -1 : left > right ? 1 : 0;
  }

  /** Parse a decimal string (`"-12.340"`, `"5e-3"`) without loss of precision. */
  static fromString(text: string): CsilDecimal {
    const match = /^([+-]?)(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(text.trim());
    if (match === null || (match[2] === "" && (match[3] ?? "") === "")) {
      throw new Error(`invalid decimal: ${text}`);
    }
    const sign = match[1] === "-" ? -1n : 1n;
    const intPart = match[2] ?? "";
    const fracPart = match[3] ?? "";
    const expPart = match[4];
    const digits = `${intPart}${fracPart}`;
    const mantissa = sign * (digits === "" ? 0n : BigInt(digits));
    const exponent = (expPart !== undefined ? parseInt(expPart, 10) : 0) - fracPart.length;
    return new CsilDecimal(exponent, mantissa);
  }

  /** Canonical decimal string; round-trips through `decimal.js` via its `toString`. */
  toString(): string {
    const negative = this.mantissa < 0n;
    const digits = (negative ? -this.mantissa : this.mantissa).toString();
    let body: string;
    if (this.exponent >= 0) {
      body = digits + "0".repeat(this.exponent);
    } else {
      const point = digits.length + this.exponent;
      if (point <= 0) {
        body = `0.${"0".repeat(-point)}${digits}`;
      } else {
        body = `${digits.slice(0, point)}.${digits.slice(point)}`;
      }
    }
    return negative ? `-${body}` : body;
  }

  /** JSON form is the exact canonical string so structured logs stay lossless. */
  toJSON(): string {
    return this.toString();
  }
}
"#;

fn type_alias(
    name: &str,
    type_expr: &CsilTypeExpression,
    docs: &[String],
    mapping: DecimalMapping,
) -> String {
    let mut out = common::jsdoc(docs, &[], "");
    out.push_str(&format!(
        "export type {} = {};\n",
        common::to_pascal(name),
        common::ts_type(type_expr, mapping)
    ));
    out
}

fn interface(
    name: &str,
    group: &CsilGroupExpression,
    docs: &[String],
    mapping: DecimalMapping,
) -> String {
    let mut out = common::jsdoc(docs, &[], "");
    out.push_str(&format!(
        "export interface {} {{\n",
        common::to_pascal(name)
    ));
    for entry in &group.entries {
        if let Some(field) = field_line(entry, mapping) {
            out.push_str(&field);
        }
    }
    out.push_str("}\n");
    out
}

fn field_line(entry: &CsilGroupEntry, mapping: DecimalMapping) -> Option<String> {
    let field_name = match &entry.key {
        Some(CsilGroupKey::Bare(name)) => common::to_camel(name),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => common::to_camel(name),
        _ => return None,
    };

    let mut docs = entry.doc_comments.clone();
    for meta in &entry.metadata {
        match meta {
            CsilFieldMetadata::Description(desc) => docs.push(desc.clone()),
            // A boolean `@depends-on` is documented as a JSDoc note: TS has no
            // way to express "this field is required only when …" structurally,
            // so the condition is surfaced for the reader/runtime to honor.
            CsilFieldMetadata::DependsOnExpr(cond) => {
                docs.push(format!("@depends-on {}", depends_expr(cond)));
            }
            // The simple single-comparison form never reaches `DependsOnExpr`, so
            // it must be surfaced here too or `@depends-on(status = "active")`
            // would emit no note while the `!=` variant does.
            CsilFieldMetadata::DependsOn { field, value } => {
                docs.push(format!("@depends-on {}", depends_simple(field, value)));
            }
            _ => {}
        }
    }

    let optional = if common::is_optional(&entry.occurrence) {
        "?"
    } else {
        ""
    };
    let ty = common::ts_type(&entry.value_type, mapping);

    let mut out = common::jsdoc(&docs, &[], "  ");
    out.push_str(&format!("  {field_name}{optional}: {ty};\n"));
    Some(out)
}

/// Render a `@depends-on` boolean condition as a readable, TS-flavored
/// expression for a JSDoc note. Field names are camelCased to match the emitted
/// interface; `All`/`Any` join their terms with `&&`/`||`, and a compound child
/// is parenthesized so the precedence a reader infers matches the source tree.
fn depends_expr(cond: &CsilDependsCondition) -> String {
    match cond {
        CsilDependsCondition::Compare { field, op, value } => {
            let name = common::to_camel(field);
            match (op, value) {
                (Some(op), Some(v)) => format!("{name} {} {}", depends_op(op), literal_ts(v)),
                // A bare `@depends-on(field)` is a presence check on that field.
                _ => name,
            }
        }
        CsilDependsCondition::All(conds) => join_depends(conds, "&&"),
        CsilDependsCondition::Any(conds) => join_depends(conds, "||"),
    }
}

/// Render the simple single-comparison `@depends-on` form as a JSDoc note. The
/// simple form carries an implicit equality (`field = value`), or is a bare
/// presence check when no value is given. Field names are camelCased to match
/// the emitted interface.
fn depends_simple(field: &str, value: &Option<CsilLiteralValue>) -> String {
    let name = common::to_camel(field);
    match value {
        Some(v) => format!("{name} === {}", literal_ts(v)),
        None => name,
    }
}

fn join_depends(conds: &[CsilDependsCondition], sep: &str) -> String {
    conds
        .iter()
        .map(|c| match c {
            // Nested boolean groups get parentheses so mixed `&&`/`||` reads
            // unambiguously in the note.
            CsilDependsCondition::All(_) | CsilDependsCondition::Any(_) => {
                format!("({})", depends_expr(c))
            }
            CsilDependsCondition::Compare { .. } => depends_expr(c),
        })
        .collect::<Vec<_>>()
        .join(&format!(" {sep} "))
}

fn depends_op(op: &CsilDependsCompareOp) -> &'static str {
    match op {
        CsilDependsCompareOp::Eq => "===",
        CsilDependsCompareOp::Ne => "!==",
        CsilDependsCompareOp::Lt => "<",
        CsilDependsCompareOp::Le => "<=",
        CsilDependsCompareOp::Gt => ">",
        CsilDependsCompareOp::Ge => ">=",
    }
}

fn inline_object(group: &CsilGroupExpression, mapping: DecimalMapping) -> String {
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
                common::ts_type(&entry.value_type, mapping)
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

// ---------------------------------------------------------------------------
// Runtime validation
//
// Two parallel constraint systems must both be honored: CDDL-style control
// operators on a `Constrained` type, and `@`-annotation `ValidationConstraint`
// metadata. Both reduce to the same set of TS guards. Encoding-only operators
// (`.json`/`.cbor`/`.cborseq`/`.bits`/`.and`/`.within`) describe wire framing,
// not value validity, so they never emit a runtime check.
// ---------------------------------------------------------------------------

/// Emit a `validate<TypeName>(value): string[]` function when the rule carries
/// at least one enforceable constraint. Returns `None` otherwise so types with
/// nothing to check stay free of empty validators.
/// Destination and naming for regexes hoisted out of validators to module
/// scope. Bundled so the guard-emitting helpers can thread it as one argument.
struct RegexHoist<'a> {
    /// camelCase type name, prefixed onto each const so names stay unique per type.
    prefix: &'a str,
    /// Accumulated `const <name> = new RegExp(...)` declarations.
    consts: &'a mut Vec<String>,
}

fn validator_for(rule: &csilgen_common::CsilRule, mapping: DecimalMapping) -> Option<String> {
    let type_name = common::to_pascal(&rule.name);
    // Prefix hoisted regex consts with the (camelCase) type name so two types
    // sharing a field name never collide at module scope.
    let prefix = common::to_camel(&rule.name);
    let mut regex_consts: Vec<String> = Vec::new();
    // Scope the mutable borrow so `regex_consts` can be read once emission ends.
    let body = {
        let mut hoist = RegexHoist {
            prefix: &prefix,
            consts: &mut regex_consts,
        };
        match &rule.rule_type {
            // A group becomes per-field checks against `value.<field>`.
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group))
            | CsilRuleType::GroupDef(group) => group_checks(group, mapping, &mut hoist),
            // A constrained alias validates the value itself. Field metadata lives
            // on group entries, not bare aliases, so only control operators apply.
            CsilRuleType::TypeDef(CsilTypeExpression::Constrained {
                base_type,
                constraints,
            }) => scoped_checks(
                "value",
                constraints,
                &[],
                false,
                compare_kind(base_type),
                mapping,
                &mut hoist,
            ),
            _ => Vec::new(),
        }
    };

    if body.is_empty() {
        return None;
    }

    // Hoist each compiled pattern to a module-level const so a validator called
    // in a loop compiles its `RegExp` once rather than on every invocation.
    let mut out = String::new();
    for decl in &regex_consts {
        out.push_str(decl);
    }
    out.push_str(&format!(
        "export function validate{type_name}(value: {type_name}): string[] {{\n"
    ));
    out.push_str("  const errors: string[] = [];\n");
    for line in body {
        out.push_str(&line);
    }
    out.push_str("  return errors;\n");
    out.push_str("}\n");
    Some(out)
}

/// Per-field checks for a group. Optional fields guard their checks behind a
/// `!== undefined` test so a missing optional never trips validation.
fn group_checks(
    group: &CsilGroupExpression,
    mapping: DecimalMapping,
    hoist: &mut RegexHoist,
) -> Vec<String> {
    let mut out = Vec::new();
    for entry in &group.entries {
        let field = match &entry.key {
            Some(CsilGroupKey::Bare(n)) => common::to_camel(n),
            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(n))) => common::to_camel(n),
            _ => continue,
        };
        let access = format!("value.{field}");
        let constraints: &[CsilControlOperator] = match &entry.value_type {
            CsilTypeExpression::Constrained { constraints, .. } => constraints,
            _ => &[],
        };
        let optional = common::is_optional(&entry.occurrence);
        let checks = scoped_checks(
            &access,
            constraints,
            &entry.metadata,
            optional,
            compare_kind(&entry.value_type),
            mapping,
            hoist,
        );
        out.extend(checks);
    }
    out
}

/// How a comparison/min-max bound must be evaluated for a field, decided by its
/// (unwrapped) base type. `decimal` and `timestamp` carry their bound as text and
/// require ordered comparison through their in-memory type rather than a raw `<`.
#[derive(Clone, Copy)]
enum CompareKind {
    /// Plain numeric field: `value <op> literal` is correct as-is.
    Numeric,
    /// `decimal`: compare via `CsilDecimal`/`Decimal` ordering against the bound.
    Decimal,
    /// `timestamp`: compare chronologically via `Date.getTime()`.
    Timestamp,
    /// Anything else (e.g. text): keep the literal-against-literal comparison.
    Plain,
}

/// Classify a field's base type for comparison emission, unwrapping any
/// `Constrained` wrapper so a `decimal .ge "0.00"` is seen as a `decimal`.
fn compare_kind(type_expr: &CsilTypeExpression) -> CompareKind {
    let base = match type_expr {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    match base {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "decimal" => CompareKind::Decimal,
            "timestamp" => CompareKind::Timestamp,
            "int" | "uint" | "integer" | "float" | "float16" | "float32" | "float64" | "number" => {
                CompareKind::Numeric
            }
            _ => CompareKind::Plain,
        },
        _ => CompareKind::Plain,
    }
}

/// Build the guard lines for one value (`access`), wrapping them in an optional
/// presence test when `optional` is set. Indentation is fixed at two spaces
/// (function body) plus two more when guarded.
fn scoped_checks(
    access: &str,
    constraints: &[CsilControlOperator],
    metadata: &[CsilFieldMetadata],
    optional: bool,
    kind: CompareKind,
    mapping: DecimalMapping,
    hoist: &mut RegexHoist,
) -> Vec<String> {
    let inner_indent = if optional { "    " } else { "  " };
    let mut checks = Vec::new();
    for op in constraints {
        if let Some(line) = control_check(access, op, inner_indent, kind, mapping, hoist) {
            checks.push(line);
        }
    }
    for meta in metadata {
        if let CsilFieldMetadata::Constraint(c) = meta
            && let Some(line) = constraint_check(access, c, inner_indent, kind, mapping)
        {
            checks.push(line);
        }
    }

    if checks.is_empty() {
        return Vec::new();
    }
    if !optional {
        return checks;
    }

    // Guard the whole block so optional absence is not an error.
    let mut wrapped = vec![format!("  if ({access} !== undefined) {{\n")];
    wrapped.extend(checks);
    wrapped.push("  }\n".to_string());
    wrapped
}

/// One CDDL control operator → one TS guard, or `None` for encoding-only
/// operators and `.default` (a value, not a constraint).
fn control_check(
    access: &str,
    op: &CsilControlOperator,
    indent: &str,
    kind: CompareKind,
    mapping: DecimalMapping,
    hoist: &mut RegexHoist,
) -> Option<String> {
    let line = match op {
        CsilControlOperator::Size(size) => match size {
            CsilSizeConstraint::Exact(n) => guard(
                indent,
                &format!("{access}.length !== {n}"),
                &format!("length must equal {n}"),
                access,
            ),
            CsilSizeConstraint::Range { min, max } => guard(
                indent,
                &format!("{access}.length < {min} || {access}.length > {max}"),
                &format!("length must be between {min} and {max}"),
                access,
            ),
            CsilSizeConstraint::Min(n) => guard(
                indent,
                &format!("{access}.length < {n}"),
                &format!("length must be >= {n}"),
                access,
            ),
            CsilSizeConstraint::Max(n) => guard(
                indent,
                &format!("{access}.length > {n}"),
                &format!("length must be <= {n}"),
                access,
            ),
        },
        CsilControlOperator::Regex(pattern) => {
            // Hoist the pattern to a module-level const, named per (type, field),
            // so the `RegExp` is compiled once instead of on each validate call.
            let field = access.strip_prefix("value.").unwrap_or("");
            let const_name = if field.is_empty() {
                format!("{}Re", hoist.prefix)
            } else {
                format!("{}{}Re", hoist.prefix, common::to_pascal(field))
            };
            hoist
                .consts
                .push(format!("const {const_name} = new RegExp({pattern:?});\n"));
            guard(
                indent,
                &format!("!{const_name}.test({access})"),
                "must match the required pattern",
                access,
            )
        }
        CsilControlOperator::GreaterEqual(v) => {
            cmp(indent, access, "<", v, "must be >=", kind, mapping)
        }
        CsilControlOperator::LessEqual(v) => {
            cmp(indent, access, ">", v, "must be <=", kind, mapping)
        }
        CsilControlOperator::GreaterThan(v) => {
            cmp(indent, access, "<=", v, "must be >", kind, mapping)
        }
        CsilControlOperator::LessThan(v) => {
            cmp(indent, access, ">=", v, "must be <", kind, mapping)
        }
        CsilControlOperator::Equal(v) => cmp(indent, access, "!==", v, "must equal", kind, mapping),
        CsilControlOperator::NotEqual(v) => {
            cmp(indent, access, "===", v, "must not equal", kind, mapping)
        }
        // Encoding/framing operators and `.default` carry no runtime check.
        CsilControlOperator::Default(_)
        | CsilControlOperator::Bits(_)
        | CsilControlOperator::And(_)
        | CsilControlOperator::Within(_)
        | CsilControlOperator::Json
        | CsilControlOperator::Cbor
        | CsilControlOperator::Cborseq => return None,
    };
    Some(line)
}

/// One `@`-annotation validation constraint → one TS guard. `Custom` cannot be
/// enforced generically, so it produces no check.
fn constraint_check(
    access: &str,
    constraint: &CsilValidationConstraint,
    indent: &str,
    kind: CompareKind,
    mapping: DecimalMapping,
) -> Option<String> {
    let line = match constraint {
        CsilValidationConstraint::MinLength(n) => guard(
            indent,
            &format!("{access}.length < {n}"),
            &format!("length must be >= {n}"),
            access,
        ),
        CsilValidationConstraint::MaxLength(n) => guard(
            indent,
            &format!("{access}.length > {n}"),
            &format!("length must be <= {n}"),
            access,
        ),
        CsilValidationConstraint::MinItems(n) => guard(
            indent,
            &format!("{access}.length < {n}"),
            &format!("must have >= {n} items"),
            access,
        ),
        CsilValidationConstraint::MaxItems(n) => guard(
            indent,
            &format!("{access}.length > {n}"),
            &format!("must have <= {n} items"),
            access,
        ),
        CsilValidationConstraint::MinValue(v) => {
            cmp(indent, access, "<", v, "must be >=", kind, mapping)
        }
        CsilValidationConstraint::MaxValue(v) => {
            cmp(indent, access, ">", v, "must be <=", kind, mapping)
        }
        CsilValidationConstraint::Custom { .. } => return None,
    };
    Some(line)
}

/// A comparison guard that trips when the value violates the bound. `op` is the
/// *failure* operator (e.g. `<` for `.ge`). For a `decimal` or `timestamp` field
/// the bound arrives as text, so a raw `value <op> "literal"` would compare the
/// wrong types; those kinds reconstruct the bound as their in-memory type and
/// compare through its ordering instead. Numeric/plain kinds keep the direct
/// comparison.
fn cmp(
    indent: &str,
    access: &str,
    op: &str,
    value: &CsilLiteralValue,
    message: &str,
    kind: CompareKind,
    mapping: DecimalMapping,
) -> String {
    let lit = literal_ts(value);
    let condition = match kind {
        // `compare`/`cmp` return the sign of the difference, so the failure
        // operator applies unchanged against `0`. The bound is passed to
        // `fromString`/`new Decimal`, both typed `(text: string)`, so it must be
        // a quoted decimal string — an Integer bound like `0` rendered bare is a
        // type error (and `fromString` would call `.trim()` on a number).
        CompareKind::Decimal => {
            let bound = decimal_literal_ts(value);
            match mapping {
                DecimalMapping::Csil => {
                    format!("{access}.compare(CsilDecimal.fromString({bound})) {op} 0")
                }
                DecimalMapping::Library => format!("{access}.cmp(new Decimal({bound})) {op} 0"),
            }
        }
        // Compare instants chronologically by epoch milliseconds.
        CompareKind::Timestamp => {
            format!("{access}.getTime() {op} new Date({lit}).getTime()")
        }
        CompareKind::Numeric | CompareKind::Plain => format!("{access} {op} {lit}"),
    };
    guard(indent, &condition, &format!("{message} {lit}"), access)
}

/// Render one `if (<condition>) errors.push(...)` guard. The message is prefixed
/// with the field accessor (minus the `value.` root) so callers see which field
/// failed. The pushed message is emitted as a properly escaped string literal so
/// an embedded quote or backslash (e.g. a `decimal` bound rendered as `"0.00"`)
/// can never break the generated source.
fn guard(indent: &str, condition: &str, message: &str, access: &str) -> String {
    let field = access.strip_prefix("value.").unwrap_or(access);
    let literal = ts_string_literal(&format!("{field}: {message}"));
    format!("{indent}if ({condition}) errors.push({literal});\n")
}

/// Render `s` as a double-quoted TypeScript string literal with every special
/// character escaped, so interpolated values can never produce invalid source.
fn ts_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a `decimal` comparison bound as a quoted TS string suitable for
/// `CsilDecimal.fromString` / `new Decimal`, both of which accept a string. The
/// core guarantees a `decimal` bound is an Integer or a well-formed decimal
/// `Text` literal; both collapse to a decimal string, quoted here so a numeric
/// bound (`0`) is never passed where `(text: string)` is required.
fn decimal_literal_ts(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => ts_string_literal(&i.to_string()),
        CsilLiteralValue::Text(s) => ts_string_literal(s),
        // The core forbids any other literal kind on a `decimal` bound; fall back
        // to the general rendering so output still parses if that ever changes.
        other => literal_ts(other),
    }
}

/// Render a CSIL literal as a TypeScript value literal for comparisons.
fn literal_ts(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => {
            // Ensure a decimal point so the value reads as a TS number literal.
            if f.fract() == 0.0 {
                format!("{f:.1}")
            } else {
                f.to_string()
            }
        }
        // Route text through the crate's own escaper: Rust's `{:?}` emits
        // `\u{NN}` brace escapes, which are not valid TypeScript string escapes.
        CsilLiteralValue::Text(s) => ts_string_literal(s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        // Byte/array literals are not meaningful comparison operands; emit a
        // harmless `null` so output still parses rather than failing generation.
        CsilLiteralValue::Bytes(_) | CsilLiteralValue::Array(_) => "null".to_string(),
    }
}
