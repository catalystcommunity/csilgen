//! Emits `codec.gen.ts`: a self-contained canonical-CBOR codec over a dynamic
//! value tree plus per-record (de)serializers.
//!
//! TypeScript has no reflection-driven, tag-aware CBOR ecosystem that pins the
//! cross-language wire contract (canonical map key order, `bytes` as a CBOR byte
//! string, tag-0 timestamps, tag-4 decimals), so — like the C/Zig/OCaml/Dart/
//! Swift/Go/Python generators — this generator emits the payload codec itself.
//! Each record gets a deep `to<T>CborValue`/`from<T>CborValue` pair plus the
//! byte-level `to<T>Cbor`/`from<T>Cbor` the typed client calls.

use crate::common::{self, DecimalMapping, ImportExtension, ts_string_literal};
use csilgen_common::{
    ChoiceClass, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType,
    CsilSpecSerialized, CsilTypeExpression, WasmGeneratorInput, classify_choice,
    union_encode_order,
};
use std::collections::{BTreeSet, HashMap, HashSet};

const TYPES_STEM: &str = "types.gen";

/// The PascalCase names of every record rule (a group, or a `Name = { ... }`
/// type alias) — the rules whose CBOR form is a map and which therefore get a
/// generated codec. Mirrors `ts_type`'s `Reference` rendering so a record
/// reference resolves to the same `to<T>Cbor`/`from<T>Cbor` everywhere.
pub fn record_names(spec: &CsilSpecSerialized) -> HashSet<String> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(common::to_pascal(&r.name)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(common::to_pascal(&r.name)),
            _ => None,
        })
        .collect()
}

/// Whether a type is a reference to a record the codec covers, so the typed
/// client can call the record's own `to<T>Cbor`/`from<T>Cbor`.
pub fn is_record_ref(ty: &CsilTypeExpression, records: &HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(&common::to_pascal(name)))
}

/// The bare PascalCase name of a record reference, for building the
/// `to<T>Cbor`/`from<T>Cbor` identifiers. Only called after `is_record_ref`.
pub fn record_ref_name(ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Reference(name) => common::to_pascal(name),
        _ => String::new(),
    }
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose
/// target is a map / array / scalar / reference / tuple (a `Name = {* text => int}`
/// map alias, `Name = [* text]` list alias, or `Name = text` scalar alias). A field
/// referencing one carries no codec of its own, so it must encode/decode as the
/// underlying type rather than fall through to the blind cast a bare non-record
/// reference would emit — the named-map-alias regression.
fn aliases(spec: &CsilSpecSerialized) -> HashMap<String, CsilTypeExpression> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            // A `Name = { ... }` group is a record (its own codec path); a mixed
            // (non-literal) choice is a union with its own tagged-sum codec
            // (`unions()`, checked before the aliases fallback) — neither is a
            // transparent alias the value handlers recurse through.
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => None,
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices))
                if matches!(classify_choice(choices), ChoiceClass::Union(_)) =>
            {
                None
            }
            // A named enum (a literal-only choice) has no codec of its own — it
            // aliases straight through to `ts_dec_value`/`ts_enc_value`'s `Choice`
            // arm, the same one an inline enum field hits, so a `Reference` to it
            // gets the SAME `asEnumMember` wire-membership validation on decode
            // as an inline enum. Without this the aliases lookup misses (a
            // literal-only choice used to be excluded here entirely) and the
            // `Reference` decode fallback below is a blind, unvalidated cast.
            // NOTE: `Name /= "a" / "b"` (a rule declared with `/=` rather than `=`)
            // parses to a *different* shape than the naive `TypeChoice([lit, lit])`
            // one might expect: `parse_type_expression` (csilgen-core's parser)
            // already consumes an entire `/`-chain into one `Choice` node before the
            // `/=` handler's own choice-accumulation loop ever runs, so the real AST
            // is `TypeChoice(vec![Choice(["a", "b"])])` — a one-element `Vec` wrapping
            // a NESTED `Choice`, not a flat literal list. That mismatch is a parser-
            // level issue (see docs/csilgen-requests), not something to special-case
            // here: a naive `TypeChoice(choices) if all_literal(choices)` arm never
            // matches the real shape (its single element is a `Choice`, not a
            // `Literal`), and a naive `!all_literal` arm matches but has no
            // corresponding codec function ever emitted (the top-level codec loop
            // below only walks `TypeDef`), producing a worse failure — a reference to
            // an undefined `to<Name>CborValue`. So `TypeChoice` intentionally falls
            // through to the untyped-cast fallback here, same as before this file's
            // enum-membership fix; a `/=`-declared type needs the parser fixed first.
            CsilRuleType::TypeDef(t) => Some((common::to_pascal(&r.name), t.clone())),
            _ => None,
        })
        .collect()
}

/// The named multi-variant unions (`Name = A / B`, not all-literal) the codec
/// covers with a tagged-sum (`[variant_index, value]`) (de)serializer. A literal-only
/// choice is an enum (bare-literal wire) and is excluded; those carry no discriminator
/// and resolve through the scalar/enum paths instead. `TypeChoice` (`/=`) rules are
/// deliberately not handled here — see the comment in `aliases()`.
fn unions(spec: &CsilSpecSerialized) -> HashMap<String, &[CsilTypeExpression]> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices))
                if matches!(classify_choice(choices), ChoiceClass::Union(_)) =>
            {
                Some((common::to_pascal(&r.name), choices.as_slice()))
            }
            _ => None,
        })
        .collect()
}

struct Ctx<'a> {
    records: &'a HashSet<String>,
    aliases: &'a HashMap<String, CsilTypeExpression>,
    unions: &'a HashMap<String, &'a [CsilTypeExpression]>,
    mapping: DecimalMapping,
}

/// The CBOR encoding of a text key (major type 3 head + bytes). Comparing these
/// byte vectors lexicographically is RFC 8949 §4.2.1 canonical key ordering,
/// computed once at generation time so the emitted map is canonical without a
/// runtime sort.
fn cbor_text_key_bytes(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let n = bytes.len() as u64;
    let mt = 3u8 << 5;
    let mut head = Vec::new();
    if n < 24 {
        head.push(mt | n as u8);
    } else if n < 0x100 {
        head.push(mt | 24);
        head.push(n as u8);
    } else {
        head.push(mt | 25);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    }
    head.extend_from_slice(bytes);
    head
}

fn unwrap_constrained(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => base_type,
        other => other,
    }
}

/// One codec field: its camelCase member name, the verbatim wire key, the
/// canonical-order sort key, its value type, and whether it is optional.
struct CodecField<'a> {
    member: String,
    wire: String,
    key_bytes: Vec<u8>,
    value_type: &'a CsilTypeExpression,
    optional: bool,
}

fn codec_fields(group: &CsilGroupExpression) -> Vec<CodecField<'_>> {
    group
        .entries
        .iter()
        .filter_map(|entry| {
            let (member, wire) = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => (common::to_camel(name), name.clone()),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    (common::to_camel(name), name.clone())
                }
                _ => return None,
            };
            Some(CodecField {
                key_bytes: cbor_text_key_bytes(&wire),
                member,
                wire,
                value_type: &entry.value_type,
                optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
            })
        })
        .collect()
}

/// A scalar whose in-memory TypeScript value is already a valid `CborValue` node,
/// so encoding is the identity. `timestamp`/`decimal` (tagged), maps (object →
/// `Map`), and record references all need a transform.
fn enc_is_identity(ty: &CsilTypeExpression, ctx: &Ctx) -> bool {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => matches!(
            name.as_str(),
            "int"
                | "uint"
                | "nint"
                | "float"
                | "float16"
                | "float32"
                | "float64"
                | "double"
                | "text"
                | "tstr"
                | "bytes"
                | "bstr"
                | "bool"
                | "any"
        ),
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            // A transparent alias is identity only when its underlying type is: a map
            // alias (object -> `Map`) still needs the transform even though it is a
            // non-record reference, so resolve through it rather than assume identity.
            Some(underlying) => enc_is_identity(underlying, ctx),
            // A union (tagged sum) and a record both need their codec transform.
            None => {
                !ctx.records.contains(&common::to_pascal(name))
                    && !ctx.unions.contains_key(&common::to_pascal(name))
            }
        },
        CsilTypeExpression::Array { element_type, .. } => enc_is_identity(element_type, ctx),
        _ => false,
    }
}

/// A TypeScript expression building the CBOR value tree node for `expr` (a typed
/// value). Records every codec-module export the expression names into `imports`, so
/// a caller emitting the expression outside `codec.gen.ts` (the client, for a
/// non-record op boundary) imports exactly the helpers it references.
fn ts_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    ctx: &Ctx,
    imports: &mut BTreeSet<String>,
) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "timestamp" => {
                imports.insert("csilTsToText".to_string());
                format!("{{ tag: 0, value: csilTsToText({expr}) }}")
            }
            "decimal" => match ctx.mapping {
                DecimalMapping::Csil => format!("{{ tag: 4, value: {expr}.toTag4() }}"),
                DecimalMapping::Library => {
                    imports.insert("csilDecToTag4".to_string());
                    format!("{{ tag: 4, value: csilDecToTag4({expr}) }}")
                }
            },
            "null" | "nil" => "null".to_string(),
            _ => expr.to_string(),
        },
        CsilTypeExpression::Reference(name) if ctx.records.contains(&common::to_pascal(name)) => {
            imports.insert(format!("to{}CborValue", common::to_pascal(name)));
            format!("to{}CborValue({expr})", common::to_pascal(name))
        }
        CsilTypeExpression::Reference(name)
            if ctx.unions.contains_key(&common::to_pascal(name)) =>
        {
            imports.insert(format!("to{}CborValue", common::to_pascal(name)));
            format!("to{}CborValue({expr})", common::to_pascal(name))
        }
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            Some(underlying) => ts_enc_value(underlying, expr, ctx, imports),
            None => expr.to_string(),
        },
        CsilTypeExpression::Array { element_type, .. } => {
            if enc_is_identity(element_type, ctx) {
                expr.to_string()
            } else {
                imports.insert("CborValue".to_string());
                let inner = ts_enc_value(element_type, "csilE", ctx, imports);
                format!("{expr}.map((csilE): CborValue => {inner})")
            }
        }
        CsilTypeExpression::Map { value, .. } => {
            // The in-memory type is `Record<string, V>`; the wire form is a CBOR
            // map, so the object's entries are rebuilt as a `Map` (insertion order).
            imports.insert("CborValue".to_string());
            let inner = ts_enc_value(value, "csilV", ctx, imports);
            format!(
                "new Map<CborValue, CborValue>(Object.entries({expr}).map(([csilK, csilV]): [CborValue, CborValue] => [csilK, {inner}]))"
            )
        }
        CsilTypeExpression::Tuple(group) => {
            // A tuple is a fixed-length positional CBOR array; an absent optional
            // element is held in place as `null` so the array length is stable, matching
            // the Rust/Go/Python reference.
            imports.insert("CborValue".to_string());
            let elems: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let elem = format!("{expr}[{i}]");
                    let inner = ts_enc_value(&entry.value_type, &elem, ctx, imports);
                    if common::is_optional(&entry.occurrence) {
                        format!("({elem} === undefined ? null : {inner})")
                    } else {
                        inner
                    }
                })
                .collect();
            format!("([{}] as CborValue[])", elems.join(", "))
        }
        CsilTypeExpression::Choice(_) => expr.to_string(),
        _ => expr.to_string(),
    }
}

/// A TypeScript expression reconstructing the typed value from `expr` (a
/// `CborValue` node). The inverse of `ts_enc_value`; scalars are not the identity
/// because a decoded node is a `CborValue` union that must be narrowed. Records every
/// codec-module export the expression names into `imports` (see `ts_enc_value`).
fn ts_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    ctx: &Ctx,
    imports: &mut BTreeSet<String>,
) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "nint" | "float" | "float16" | "float32" | "float64" | "double" => {
                imports.insert("asNumber".to_string());
                format!("asNumber({expr})")
            }
            "text" | "tstr" => {
                imports.insert("asString".to_string());
                format!("asString({expr})")
            }
            "bytes" | "bstr" => {
                imports.insert("asBytes".to_string());
                format!("asBytes({expr})")
            }
            "bool" => {
                imports.insert("asBool".to_string());
                format!("asBool({expr})")
            }
            "timestamp" => {
                imports.insert("asTimestamp".to_string());
                format!("asTimestamp({expr})")
            }
            "decimal" => match ctx.mapping {
                DecimalMapping::Csil => {
                    imports.insert("CsilDecimal".to_string());
                    imports.insert("asDecimalPayload".to_string());
                    format!("CsilDecimal.fromTag4(asDecimalPayload({expr}))")
                }
                DecimalMapping::Library => {
                    imports.insert("csilDecFromTag4".to_string());
                    imports.insert("asDecimalPayload".to_string());
                    format!("csilDecFromTag4(asDecimalPayload({expr}))")
                }
            },
            "any" => expr.to_string(),
            "null" | "nil" => "null".to_string(),
            _ => {
                imports.insert("asString".to_string());
                format!("asString({expr})")
            }
        },
        CsilTypeExpression::Reference(name) if ctx.records.contains(&common::to_pascal(name)) => {
            imports.insert(format!("from{}CborValue", common::to_pascal(name)));
            format!("from{}CborValue({expr})", common::to_pascal(name))
        }
        CsilTypeExpression::Reference(name)
            if ctx.unions.contains_key(&common::to_pascal(name)) =>
        {
            imports.insert(format!("from{}CborValue", common::to_pascal(name)));
            format!("from{}CborValue({expr})", common::to_pascal(name))
        }
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            Some(underlying) => ts_dec_value(underlying, expr, ctx, imports),
            None => format!(
                "({expr} as unknown as {})",
                common::ts_type(ty, ctx.mapping)
            ),
        },
        CsilTypeExpression::Array { element_type, .. } => {
            imports.insert("asArray".to_string());
            let inner = ts_dec_value(element_type, "csilE", ctx, imports);
            format!("asArray({expr}).map((csilE) => {inner})")
        }
        CsilTypeExpression::Map { value, .. } => {
            imports.insert("asMap".to_string());
            imports.insert("asString".to_string());
            let inner = ts_dec_value(value, "csilV", ctx, imports);
            format!(
                "Object.fromEntries(Array.from(asMap({expr}), ([csilK, csilV]): [string, {}] => [asString(csilK), {inner}]))",
                common::ts_type(value, ctx.mapping)
            )
        }
        CsilTypeExpression::Tuple(group) => {
            // The inverse of the tuple encoder: read a fixed-length array and rebuild the
            // tuple, mapping a `null` held-in-place back to an absent optional element.
            imports.insert("asArray".to_string());
            imports.insert("CborValue".to_string());
            let elems: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let elem = format!("csilTup[{i}]");
                    let inner = ts_dec_value(&entry.value_type, &elem, ctx, imports);
                    if common::is_optional(&entry.occurrence) {
                        format!("({elem} === null ? undefined : {inner})")
                    } else {
                        inner
                    }
                })
                .collect();
            format!(
                "((csilTup: CborValue[]) => ([{}] as {}))(asArray({expr}))",
                elems.join(", "),
                common::ts_type(ty, ctx.mapping)
            )
        }
        // An inline literal choice (`x: "a" / "b"`) narrows to a precise TS union,
        // so the decoded scalar is validated against the declared vocabulary and cast
        // to it; pick the reader from the literal kind so a numeric enum reads a number
        // rather than a string (a `string as 1 | 2` cast would not typecheck). When a
        // member cannot be rendered as a runtime value (a byte-string/array literal),
        // fall back to the permissive read + cast — there is nothing to compare against.
        // A choice of non-literals keeps the prior permissive string read.
        CsilTypeExpression::Choice(choices) if common::all_literal(choices) => {
            let members: Option<Vec<String>> = choices
                .iter()
                .map(|c| common::choice_arm_literal(c).and_then(literal_value_ts))
                .collect();
            match (literal_choice_reader(choices), members) {
                (Some(reader), Some(members)) => {
                    imports.insert(reader.to_string());
                    imports.insert("asEnumMember".to_string());
                    format!(
                        "(asEnumMember({reader}({expr}), [{}]) as {})",
                        members.join(", "),
                        common::ts_type(ty, ctx.mapping)
                    )
                }
                (Some(reader), None) => {
                    imports.insert(reader.to_string());
                    format!("({reader}({expr}) as {})", common::ts_type(ty, ctx.mapping))
                }
                // A MIXED-kind literal choice (`"a" / 1`): no single `asNumber`/
                // `asString`/`asBool` reader fits every member's JS runtime type (a
                // uniform-kind choice above always found one), so read the value
                // generically — normalizing a decoded integer `bigint` to `number`
                // the same way `asNumber` does — and let `asEnumMember`'s membership
                // check do the real narrowing. Previously this fell to the uniform-kind
                // `else` branch's `asString` reader, which threw at runtime on any
                // non-string member (e.g. decoding the `1` in `"a" / 1`).
                (None, Some(members)) => {
                    imports.insert("asEnumScalar".to_string());
                    imports.insert("asEnumMember".to_string());
                    format!(
                        "(asEnumMember(asEnumScalar({expr}), [{}]) as {})",
                        members.join(", "),
                        common::ts_type(ty, ctx.mapping)
                    )
                }
                // Mixed-kind AND at least one member has no runtime literal spelling
                // (a byte-string/array literal) — nothing to validate membership
                // against, so keep the generic scalar read without it.
                (None, None) => {
                    imports.insert("asEnumScalar".to_string());
                    format!(
                        "(asEnumScalar({expr}) as {})",
                        common::ts_type(ty, ctx.mapping)
                    )
                }
            }
        }
        CsilTypeExpression::Choice(_) => {
            imports.insert("asString".to_string());
            format!("asString({expr})")
        }
        // A mixed union's decode dispatch (see `emit_union_codec`) reaches this arm
        // once per literal index: the payload must equal the declared literal, not
        // merely share its scalar type, so a mismatched value at the right index
        // (e.g. `[1, "confirmed"]` when index 1 is declared `"pending"`) errors
        // instead of silently returning the wrong value.
        CsilTypeExpression::Literal(v) => match v {
            CsilLiteralValue::Text(_)
            | CsilLiteralValue::Integer(_)
            | CsilLiteralValue::Float(_)
            | CsilLiteralValue::Bool(_)
            | CsilLiteralValue::Null => {
                let expected = common::ts_literal_type(v);
                let ts_ty = common::ts_type(ty, ctx.mapping);
                imports.insert("asLiteral".to_string());
                format!("asLiteral<{ts_ty}>({expr}, {expected})")
            }
            // `ts_literal_type` renders a byte-string/array literal as its TYPE
            // spelling (e.g. `Uint8Array`), not a runtime value, so it can't be
            // passed to `asLiteral` as the expected value; fall back to the same
            // permissive cast the encode-side predicate already uses for these
            // kinds (see `ts_variant_predicate`).
            CsilLiteralValue::Bytes(_) | CsilLiteralValue::Array(_) => format!(
                "({expr} as unknown as {})",
                common::ts_type(ty, ctx.mapping)
            ),
        },
        _ => format!(
            "({expr} as unknown as {})",
            common::ts_type(ty, ctx.mapping)
        ),
    }
}

/// The scalar CBOR reader for a UNIFORM-kind literal enum: `asNumber` when every
/// member is numeric, `asBool` when every member is boolean, `asString` when
/// every member is text. `None` when the kinds are mixed (`"a" / 1`) — no single
/// reader's runtime type fits every member, so the caller falls back to a
/// generic scalar read (`asEnumScalar`) instead. The cast that follows a `Some`
/// reader narrows the read scalar to the precise union, so the reader must
/// produce the union's underlying runtime type.
fn literal_choice_reader(choices: &[CsilTypeExpression]) -> Option<&'static str> {
    let all = |pred: fn(&CsilLiteralValue) -> bool| {
        choices
            .iter()
            .all(|c| common::choice_arm_literal(c).is_some_and(pred))
    };
    if all(|v| matches!(v, CsilLiteralValue::Integer(_) | CsilLiteralValue::Float(_))) {
        Some("asNumber")
    } else if all(|v| matches!(v, CsilLiteralValue::Bool(_))) {
        Some("asBool")
    } else if all(|v| matches!(v, CsilLiteralValue::Text(_))) {
        Some("asString")
    } else {
        None
    }
}

/// A CSIL literal rendered as a runtime TypeScript value (for an enum membership
/// list). `None` for a byte-string/array literal, which has no scalar runtime
/// spelling to compare against — the caller then skips membership validation.
fn literal_value_ts(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(ts_string_literal(s)),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        CsilLiteralValue::Bool(b) => Some(b.to_string()),
        CsilLiteralValue::Null => Some("null".to_string()),
        CsilLiteralValue::Bytes(_) | CsilLiteralValue::Array(_) => None,
    }
}

/// A runtime boolean expression that is true when `expr` (a value of the structural
/// union) is the `ty` variant. A CSIL union has no in-memory tag, so the tagged-sum
/// encoder discriminates by the variant's runtime shape, in declaration order. The
/// final variant is reached only when no earlier predicate matched, so an unsupported
/// shape falls through to a runtime error rather than a silent miswrite.
fn ts_variant_predicate(ty: &CsilTypeExpression, expr: &str, ctx: &Ctx) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "nint" | "float" | "float16" | "float32" | "float64" | "double" => {
                format!("typeof {expr} === \"number\" || typeof {expr} === \"bigint\"")
            }
            "text" | "tstr" => format!("typeof {expr} === \"string\""),
            "bytes" | "bstr" => format!("{expr} instanceof Uint8Array"),
            "bool" => format!("typeof {expr} === \"boolean\""),
            "null" | "nil" => format!("{expr} === null"),
            _ => "true".to_string(),
        },
        CsilTypeExpression::Literal(v) => match v {
            CsilLiteralValue::Text(s) => format!("{expr} === {}", ts_string_literal(s)),
            CsilLiteralValue::Integer(i) => format!("{expr} === {i}"),
            CsilLiteralValue::Float(f) => format!("{expr} === {f}"),
            CsilLiteralValue::Bool(b) => format!("{expr} === {b}"),
            CsilLiteralValue::Null => format!("{expr} === null"),
            _ => "true".to_string(),
        },
        CsilTypeExpression::Array { .. } => format!("Array.isArray({expr})"),
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            Some(underlying) => ts_variant_predicate(underlying, expr, ctx),
            // A record reference is an object that is none of the other CborValue shapes.
            None if ctx.records.contains(&common::to_pascal(name)) => format!(
                "typeof {expr} === \"object\" && {expr} !== null && !Array.isArray({expr}) && !({expr} instanceof Uint8Array) && !({expr} instanceof Map)"
            ),
            // A nested union reaches the wire as a 2-element tagged-sum array.
            None if ctx.unions.contains_key(&common::to_pascal(name)) => {
                format!("Array.isArray({expr})")
            }
            None => "true".to_string(),
        },
        // A map field is an object in memory; an inline group is rare in a union arm.
        _ => "true".to_string(),
    }
}

/// Emit the per-union codec: a tagged-sum (`[variant_index, value]`) encoder and
/// decoder plus the byte-level `to<T>Cbor`/`from<T>Cbor`. The variant index is the
/// 0-based declaration order, matching the Rust/Go/Python reference so a union is
/// byte-identical across languages.
fn emit_union_codec(name: &str, choices: &[CsilTypeExpression], ctx: &Ctx) -> String {
    let type_name = common::to_pascal(name);
    let mut out = String::new();
    let mut ignored_imports = BTreeSet::new();

    out.push_str(&format!(
        "export function to{type_name}CborValue(v: {type_name}): CborValue {{\n"
    ));
    // A mixed union (`text / "pending" / "confirmed" / ...`) has a general arm whose
    // predicate (`typeof v === "string"`) matches every value of that base type,
    // including the ones a literal arm of the same union declares more specifically.
    // Emitting arms in declaration order would let the general arm's broader `if`
    // shadow every literal arm that follows it, making those declared indices
    // unreachable on encode. Checking every literal arm first — stable within each
    // group, so relative order among literals and among general arms is unchanged —
    // gives literal-over-general precedence: a literal keeps its own declared index,
    // and a general arm becomes the fallback for every other value of its type.
    // Mirrors the Go/Python union codecs' literal-first grouping (see
    // `csilgen_common::union_encode_order`'s docs for why only a sequential
    // predicate-chain dispatcher like this one needs this reordering at all).
    let ChoiceClass::Union(arm_kinds) = classify_choice(choices) else {
        unreachable!(
            "emit_union_codec is only called for a choice `unions()` already classified as Union"
        );
    };
    let order = union_encode_order(&arm_kinds);
    for &i in &order {
        let variant = &choices[i];
        let pred = ts_variant_predicate(variant, "v", ctx);
        let cast = common::ts_type(variant, ctx.mapping);
        let enc = ts_enc_value(variant, "csilV", ctx, &mut ignored_imports);
        out.push_str(&format!(
            "  if ({pred}) {{ const csilV = v as {cast}; return [{i}, {enc}]; }}\n"
        ));
    }
    out.push_str(&format!(
        "  throw new Error(\"unencodable {type_name} union value\");\n}}\n\n"
    ));

    out.push_str(&format!(
        "export function from{type_name}CborValue(value: CborValue): {type_name} {{\n"
    ));
    out.push_str("  const csilArr = asArray(value);\n");
    out.push_str(&format!(
        "  if (csilArr.length !== 2) throw new Error(`{type_name} union expects a 2-element array, got ${{csilArr.length}}`);\n"
    ));
    out.push_str("  const csilIdx = asNumber(csilArr[0]);\n");
    out.push_str("  switch (csilIdx) {\n");
    for (i, variant) in choices.iter().enumerate() {
        let dec = ts_dec_value(variant, "csilArr[1]", ctx, &mut ignored_imports);
        out.push_str(&format!("    case {i}: return {dec};\n"));
    }
    out.push_str(&format!(
        "    default: throw new Error(`unknown {type_name} variant ${{csilIdx}}`);\n  }}\n}}\n\n"
    ));

    out.push_str(&format!(
        "export function to{type_name}Cbor(v: {type_name}): Uint8Array {{\n  return encodeValue(to{type_name}CborValue(v));\n}}\n\n"
    ));
    out.push_str(&format!(
        "export function from{type_name}Cbor(bytes: Uint8Array): {type_name} {{\n  return from{type_name}CborValue(decode(bytes));\n}}\n\n"
    ));
    out
}

/// Byte-level encoder for an arbitrary op-boundary value: `encodeValue(<value tree>)`.
/// The record path has `to<T>Cbor`, but a scalar/array/map request has no named
/// helper, so the client builds the call inline from the codec's generic CBOR. Returns
/// the TS expression encoding `value_expr` (a value of `ty`) to `Uint8Array` plus the
/// codec exports it names, so the client imports exactly what it references.
pub fn op_encode_expr(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
    mapping: DecimalMapping,
    ty: &CsilTypeExpression,
    value_expr: &str,
) -> (String, BTreeSet<String>) {
    let aliases = aliases(spec);
    let unions = unions(spec);
    let ctx = Ctx {
        records,
        aliases: &aliases,
        unions: &unions,
        mapping,
    };
    let mut imports = BTreeSet::new();
    imports.insert("encodeValue".to_string());
    let tree = ts_enc_value(ty, value_expr, &ctx, &mut imports);
    (format!("encodeValue({tree})"), imports)
}

/// Byte-level decoder for an arbitrary op-boundary value: the inverse of
/// `op_encode_expr`. Returns the TS expression reconstructing a value of `ty` from
/// the `Uint8Array` named by `bytes_expr`, plus the codec exports it names.
pub fn op_decode_expr(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
    mapping: DecimalMapping,
    ty: &CsilTypeExpression,
    bytes_expr: &str,
) -> (String, BTreeSet<String>) {
    let aliases = aliases(spec);
    let unions = unions(spec);
    let ctx = Ctx {
        records,
        aliases: &aliases,
        unions: &unions,
        mapping,
    };
    let mut imports = BTreeSet::new();
    imports.insert("decode".to_string());
    let dec = ts_dec_value(ty, &format!("decode({bytes_expr})"), &ctx, &mut imports);
    (dec, imports)
}

/// Decode an arbitrary value from a `CborValue` node rather than raw bytes — the
/// element decoder the rpc-mode poll needs, having already split the response array
/// into nodes. Returns the expression reconstructing a value of `ty` from the node
/// named by `value_expr`, plus the codec exports it names.
pub fn op_decode_value_expr(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
    mapping: DecimalMapping,
    ty: &CsilTypeExpression,
    value_expr: &str,
) -> (String, BTreeSet<String>) {
    let aliases = aliases(spec);
    let unions = unions(spec);
    let ctx = Ctx {
        records,
        aliases: &aliases,
        unions: &unions,
        mapping,
    };
    let mut imports = BTreeSet::new();
    let dec = ts_dec_value(ty, value_expr, &ctx, &mut imports);
    (dec, imports)
}

/// Whether any record field uses `decimal`, mirroring the condition under which
/// `generate` emits the decimal import/bridge. An op boundary that references
/// `decimal` is only expressible when that support is present, since the decimal
/// helpers are not part of the always-on runtime.
fn records_use_decimal(spec: &CsilSpecSerialized) -> bool {
    spec.rules.iter().any(|rule| {
        rule_group(&rule.rule_type).is_some_and(|group| {
            group
                .entries
                .iter()
                .any(|entry| type_uses_decimal(&entry.value_type))
        })
    })
}

/// Whether the client can (de)serialize an op-boundary type with the codec's exported
/// helpers. Records, scalars, arrays, maps, transparent aliases, and literal enums all
/// resolve to always-present runtime helpers (or a record's own codec). Two shapes do
/// not, and keep the skip-with-note path the client falls back to:
///   - a `decimal` payload when no record uses `decimal`, so the codec emitted neither
///     the decimal import nor its bridge — the expression would name an undefined
///     helper;
///   - a multi-variant choice of non-literals (e.g. a `Res / DomainError` success
///     union), which carries no wire discriminator to decode against.
pub fn op_boundary_expressible(
    spec: &CsilSpecSerialized,
    records: &HashSet<String>,
    ty: &CsilTypeExpression,
) -> bool {
    let aliases = aliases(spec);
    let unions = unions(spec);
    let ctx = Ctx {
        records,
        aliases: &aliases,
        unions: &unions,
        mapping: DecimalMapping::Csil,
    };
    expressible(ty, &ctx, records_use_decimal(spec))
}

fn expressible(ty: &CsilTypeExpression, ctx: &Ctx, codec_has_decimal: bool) -> bool {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) if name == "decimal" => codec_has_decimal,
        CsilTypeExpression::Builtin(_) => true,
        CsilTypeExpression::Reference(name) if ctx.records.contains(&common::to_pascal(name)) => {
            true
        }
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            Some(underlying) => expressible(underlying, ctx, codec_has_decimal),
            // An unknown / choice-typedef reference decodes via an opaque cast, which
            // compiles; only inline shapes need the structural checks below.
            None => true,
        },
        CsilTypeExpression::Array { element_type, .. } => {
            expressible(element_type, ctx, codec_has_decimal)
        }
        CsilTypeExpression::Map { value, .. } => expressible(value, ctx, codec_has_decimal),
        CsilTypeExpression::Choice(choices) if common::all_literal(choices) => true,
        // A single-variant choice resolves to that variant; multiple non-literal
        // variants have no discriminator on the wire.
        CsilTypeExpression::Choice(choices) => {
            choices.len() <= 1
                && choices
                    .iter()
                    .all(|c| expressible(c, ctx, codec_has_decimal))
        }
        _ => true,
    }
}

/// Emit the per-record codec: deep `to<T>CborValue`/`from<T>CborValue` plus the
/// byte-level `to<T>Cbor`/`from<T>Cbor`. The encoder inserts map entries in
/// canonical key order (computed at generation time); the decoder reads each
/// field by its verbatim wire key, defaulting an absent optional to `undefined`.
fn emit_record_codec(name: &str, group: &CsilGroupExpression, ctx: &Ctx) -> String {
    let type_name = common::to_pascal(name);
    let fields = codec_fields(group);
    let mut out = String::new();
    // The record codec lives in the same module as every helper it references, so the
    // import set the value transforms collect is irrelevant here and discarded.
    let mut ignored_imports = BTreeSet::new();

    out.push_str(&format!(
        "export function to{type_name}CborValue(v: {type_name}): CborValue {{\n"
    ));
    out.push_str("  const csilMap = new Map<CborValue, CborValue>();\n");
    let mut encode_fields: Vec<&CodecField> = fields.iter().collect();
    encode_fields.sort_by(|a, b| a.key_bytes.cmp(&b.key_bytes));
    for field in &encode_fields {
        let wire = ts_string_literal(&field.wire);
        let access = format!("v.{}", field.member);
        let enc = ts_enc_value(field.value_type, &access, ctx, &mut ignored_imports);
        if field.optional {
            // The `!== undefined` guard narrows the member to its non-optional type
            // before the encoder transforms it, and omits an absent optional.
            out.push_str(&format!(
                "  if ({access} !== undefined) csilMap.set({wire}, {enc});\n"
            ));
        } else {
            out.push_str(&format!("  csilMap.set({wire}, {enc});\n"));
        }
    }
    out.push_str("  return csilMap;\n}\n\n");

    out.push_str(&format!(
        "export function from{type_name}CborValue(value: CborValue): {type_name} {{\n"
    ));
    if fields.is_empty() {
        out.push_str(&format!(
            "  void value;\n  return {{}} as {type_name};\n}}\n\n"
        ));
    } else {
        out.push_str("  return {\n");
        // Decode keeps declaration order; only the encode is canonically sorted.
        for field in &fields {
            let wire = ts_string_literal(&field.wire);
            if field.optional {
                let dec = ts_dec_value(field.value_type, "csilV", ctx, &mut ignored_imports);
                out.push_str(&format!(
                    "    {}: ((csilV: CborValue | undefined) => csilV === undefined ? undefined : {dec})(mapGet(value, {wire})),\n",
                    field.member
                ));
            } else {
                let dec = ts_dec_value(
                    field.value_type,
                    &format!("requireKey(value, {wire})"),
                    ctx,
                    &mut ignored_imports,
                );
                out.push_str(&format!("    {}: {dec},\n", field.member));
            }
        }
        out.push_str("  };\n}\n\n");
    }

    out.push_str(&format!(
        "export function to{type_name}Cbor(v: {type_name}): Uint8Array {{\n  return encodeValue(to{type_name}CborValue(v));\n}}\n\n"
    ));
    out.push_str(&format!(
        "export function from{type_name}Cbor(bytes: Uint8Array): {type_name} {{\n  return from{type_name}CborValue(decode(bytes));\n}}\n\n"
    ));
    out
}

/// Build `codec.gen.ts`: the self-contained canonical-CBOR runtime plus a codec
/// per record. `None` when the spec declares no record types.
pub fn generate(input: &WasmGeneratorInput) -> Option<String> {
    let spec = &input.csil_spec;
    let records = record_names(spec);
    if records.is_empty() {
        return None;
    }
    let mapping = common::decimal_mapping(input).unwrap_or(DecimalMapping::Csil);
    // Already validated (Err would have aborted generation before this emitter
    // ever runs) — see `generate_files`'s eager option validation in lib.rs.
    let ext = common::import_extension(input).unwrap_or(ImportExtension::Ts);
    let aliases = aliases(spec);
    let unions = unions(spec);
    let ctx = Ctx {
        records: &records,
        aliases: &aliases,
        unions: &unions,
        mapping,
    };

    let mut out = common::header(input, "typescript-codec");

    // Records reference declared types in their fields, and the codec's signatures
    // name each record; pull both from the companion types module.
    let mut imports: BTreeSet<String> = BTreeSet::new();
    let mut uses_decimal = false;
    for rule in &spec.rules {
        if let Some(group) = rule_group(&rule.rule_type) {
            imports.insert(common::to_pascal(&rule.name));
            for entry in &group.entries {
                common::collect_type_refs(&entry.value_type, &mut imports);
                if type_uses_decimal(&entry.value_type) {
                    uses_decimal = true;
                }
            }
        }
    }
    // Each union's codec signature names the union type and may reference records or
    // other declared types in its variants; pull all of them from the types module.
    for (union_name, choices) in &unions {
        imports.insert(union_name.clone());
        for variant in *choices {
            common::collect_type_refs(variant, &mut imports);
            if type_uses_decimal(variant) {
                uses_decimal = true;
            }
        }
    }
    // `CsilDecimal` is a value (its `toTag4`/`fromTag4` run at runtime), so it is a
    // value import, not a type-only one.
    imports.remove("CsilDecimal");

    let default_types_module = ext.specifier(TYPES_STEM);
    let module = string_option(input, "codec_types_module", &default_types_module);
    if uses_decimal && mapping == DecimalMapping::Library {
        out.push_str("import Decimal from \"decimal.js\";\n");
    }
    if uses_decimal && mapping == DecimalMapping::Csil {
        out.push_str(&format!("import {{ CsilDecimal }} from \"{module}\";\n"));
    }
    if !imports.is_empty() {
        out.push_str(&format!(
            "import type {{ {} }} from \"{module}\";\n",
            imports.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    out.push('\n');

    out.push_str(CODEC_RUNTIME_TS);
    out.push('\n');

    if uses_decimal && mapping == DecimalMapping::Library {
        out.push_str(DECIMAL_LIBRARY_BRIDGE_TS);
        out.push('\n');
    }

    for rule in &spec.rules {
        if let Some(group) = rule_group(&rule.rule_type) {
            out.push_str(&emit_record_codec(&rule.name, group, &ctx));
        } else if let CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) = &rule.rule_type
            && !common::all_literal(choices)
        {
            out.push_str(&emit_union_codec(&rule.name, choices, &ctx));
        }
    }

    Some(out)
}

fn rule_group(rule_type: &CsilRuleType) -> Option<&CsilGroupExpression> {
    match rule_type {
        CsilRuleType::GroupDef(g) => Some(g),
        CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
        _ => None,
    }
}

fn type_uses_decimal(ty: &CsilTypeExpression) -> bool {
    match ty {
        CsilTypeExpression::Builtin(name) => name == "decimal",
        CsilTypeExpression::Array { element_type, .. } => type_uses_decimal(element_type),
        CsilTypeExpression::Map { key, value, .. } => {
            type_uses_decimal(key) || type_uses_decimal(value)
        }
        CsilTypeExpression::Choice(choices) => choices.iter().any(type_uses_decimal),
        CsilTypeExpression::Constrained { base_type, .. } => type_uses_decimal(base_type),
        _ => false,
    }
}

fn string_option(input: &WasmGeneratorInput, key: &str, default: &str) -> String {
    input
        .config
        .options
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string())
}

/// `decimal.js` carries no direct tag-4 accessor, so library mode bridges through
/// the canonical decimal string. Emitted only when a record uses `decimal` and
/// `decimal_mapping` is `"library"`.
const DECIMAL_LIBRARY_BRIDGE_TS: &str = r#"function csilDecToTag4(d: Decimal): [number, bigint] {
  const match = /^([+-]?)(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(d.toString().trim());
  if (match === null) throw new Error(`invalid decimal: ${d.toString()}`);
  const sign = match[1] === "-" ? -1n : 1n;
  const intPart = match[2] ?? "";
  const fracPart = match[3] ?? "";
  const expPart = match[4];
  const digits = `${intPart}${fracPart}`;
  const mantissa = sign * (digits === "" ? 0n : BigInt(digits));
  const exponent = (expPart !== undefined ? parseInt(expPart, 10) : 0) - fracPart.length;
  return [exponent, mantissa];
}

function csilDecFromTag4(payload: [number | bigint, number | bigint]): Decimal {
  const exponent = Number(payload[0]);
  const mantissa = BigInt(payload[1]);
  return new Decimal(`${mantissa}e${exponent}`);
}
"#;

/// The self-contained canonical-CBOR (RFC 8949 subset) value model and codec the
/// generated record (de)serializers build on. `bytes` is a `Uint8Array`, so it
/// encodes as a CBOR byte string (major type 2) by construction.
const CODEC_RUNTIME_TS: &str = r#"/** A CBOR semantic tag wrapping an inner value (e.g. tag 0 timestamp, tag 4 decimal). */
export type CborTag = { readonly tag: number; readonly value: CborValue };

/** A minimal canonical-CBOR value tree: a closed set of node variants. */
export type CborValue =
  | number
  | bigint
  | boolean
  | null
  | string
  | Uint8Array
  | CborValue[]
  | Map<CborValue, CborValue>
  | CborTag;

const csilTextEncoder = new TextEncoder();
const csilTextDecoder = new TextDecoder();

function head(major: number, n: number | bigint, out: number[]): void {
  const mt = major << 5;
  const big = typeof n === "bigint" ? n : BigInt(n);
  if (big < 24n) {
    out.push(mt | Number(big));
  } else if (big < 0x100n) {
    out.push(mt | 24, Number(big) & 0xff);
  } else if (big < 0x10000n) {
    out.push(mt | 25, Number((big >> 8n) & 0xffn), Number(big & 0xffn));
  } else if (big < 0x100000000n) {
    out.push(mt | 26);
    for (let s = 24n; s >= 0n; s -= 8n) out.push(Number((big >> s) & 0xffn));
  } else {
    out.push(mt | 27);
    for (let s = 56n; s >= 0n; s -= 8n) out.push(Number((big >> s) & 0xffn));
  }
}

function encInt(n: bigint, out: number[]): void {
  if (n >= 0n) head(0, n, out);
  else head(1, -1n - n, out);
}

function encFloat(d: number, out: number[]): void {
  out.push(0xfb);
  const view = new DataView(new ArrayBuffer(8));
  view.setFloat64(0, d, false);
  for (let i = 0; i < 8; i++) out.push(view.getUint8(i));
}

function encInto(v: CborValue, out: number[]): void {
  if (typeof v === "number") {
    if (Number.isInteger(v)) encInt(BigInt(v), out);
    else encFloat(v, out);
  } else if (typeof v === "bigint") {
    encInt(v, out);
  } else if (typeof v === "boolean") {
    out.push(v ? 0xf5 : 0xf4);
  } else if (v === null) {
    out.push(0xf6);
  } else if (typeof v === "string") {
    const bytes = csilTextEncoder.encode(v);
    head(3, bytes.length, out);
    for (const b of bytes) out.push(b);
  } else if (v instanceof Uint8Array) {
    head(2, v.length, out);
    for (const b of v) out.push(b);
  } else if (Array.isArray(v)) {
    head(4, v.length, out);
    for (const x of v) encInto(x, out);
  } else if (v instanceof Map) {
    head(5, v.size, out);
    for (const [k, val] of v) {
      encInto(k, out);
      encInto(val, out);
    }
  } else {
    head(6, v.tag, out);
    encInto(v.value, out);
  }
}

/** Encode a CBOR value tree to canonical CSIL CBOR bytes. */
export function encodeValue(value: CborValue): Uint8Array {
  const out: number[] = [];
  encInto(value, out);
  return Uint8Array.from(out);
}

type Cursor = { b: Uint8Array; pos: number };

function readArg(st: Cursor, low: number): bigint {
  if (low < 24) {
    st.pos += 1;
    return BigInt(low);
  }
  if (low === 24) {
    const v = BigInt(st.b[st.pos + 1]);
    st.pos += 2;
    return v;
  }
  if (low === 25) {
    let v = 0n;
    for (let i = 1; i <= 2; i++) v = (v << 8n) | BigInt(st.b[st.pos + i]);
    st.pos += 3;
    return v;
  }
  if (low === 26) {
    let v = 0n;
    for (let i = 1; i <= 4; i++) v = (v << 8n) | BigInt(st.b[st.pos + i]);
    st.pos += 5;
    return v;
  }
  if (low === 27) {
    let v = 0n;
    for (let i = 1; i <= 8; i++) v = (v << 8n) | BigInt(st.b[st.pos + i]);
    st.pos += 9;
    return v;
  }
  throw new Error("malformed CBOR argument");
}

function smallInt(arg: bigint): number | bigint {
  return arg <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(arg) : arg;
}

function readFloat(st: Cursor, low: number): number {
  const bits = readArg(st, low);
  if (low === 26) {
    const view = new DataView(new ArrayBuffer(4));
    view.setUint32(0, Number(bits), false);
    return view.getFloat32(0, false);
  }
  const view = new DataView(new ArrayBuffer(8));
  view.setBigUint64(0, bits, false);
  return view.getFloat64(0, false);
}

function decInto(st: Cursor): CborValue {
  const ib = st.b[st.pos];
  const major = ib >> 5;
  const low = ib & 0x1f;
  if (major === 7) {
    if (low === 20) {
      st.pos += 1;
      return false;
    }
    if (low === 21) {
      st.pos += 1;
      return true;
    }
    if (low === 22 || low === 23) {
      st.pos += 1;
      return null;
    }
    if (low === 26 || low === 27) return readFloat(st, low);
    throw new Error("malformed CBOR simple value");
  }
  const arg = readArg(st, low);
  switch (major) {
    case 0:
      return smallInt(arg);
    case 1: {
      const n = -1n - arg;
      return n >= BigInt(Number.MIN_SAFE_INTEGER) ? Number(n) : n;
    }
    case 2: {
      const n = Number(arg);
      const slice = st.b.slice(st.pos, st.pos + n);
      st.pos += n;
      return slice;
    }
    case 3: {
      const n = Number(arg);
      const text = csilTextDecoder.decode(st.b.subarray(st.pos, st.pos + n));
      st.pos += n;
      return text;
    }
    case 4: {
      const n = Number(arg);
      const arr: CborValue[] = [];
      for (let i = 0; i < n; i++) arr.push(decInto(st));
      return arr;
    }
    case 5: {
      const n = Number(arg);
      const m = new Map<CborValue, CborValue>();
      for (let i = 0; i < n; i++) {
        const k = decInto(st);
        const val = decInto(st);
        m.set(k, val);
      }
      return m;
    }
    case 6: {
      const inner = decInto(st);
      return { tag: Number(arg), value: inner };
    }
    default:
      throw new Error("malformed CBOR major type");
  }
}

/** Decode a CSIL CBOR byte payload into a CBOR value tree. */
export function decode(bytes: Uint8Array): CborValue {
  const st: Cursor = { b: bytes, pos: 0 };
  const v = decInto(st);
  if (st.pos !== bytes.length) throw new Error("trailing bytes after CBOR value");
  return v;
}

/** The value for `key` in a CBOR map node, or `undefined` when absent. */
export function mapGet(value: CborValue, key: string): CborValue | undefined {
  return value instanceof Map ? value.get(key) : undefined;
}

/** The value for a required `key`; throws when the field is missing. */
export function requireKey(value: CborValue, key: string): CborValue {
  const v = mapGet(value, key);
  if (v === undefined) throw new Error(`missing required field: ${key}`);
  return v;
}

export function asNumber(value: CborValue): number {
  if (typeof value === "number") return value;
  if (typeof value === "bigint") return Number(value);
  throw new Error("expected a number");
}

export function asString(value: CborValue): string {
  if (typeof value === "string") return value;
  throw new Error("expected a text string");
}

export function asBytes(value: CborValue): Uint8Array {
  if (value instanceof Uint8Array) return value;
  throw new Error("expected a byte string");
}

export function asBool(value: CborValue): boolean {
  if (typeof value === "boolean") return value;
  throw new Error("expected a boolean");
}

export function asArray(value: CborValue): CborValue[] {
  if (Array.isArray(value)) return value;
  throw new Error("expected an array");
}

export function asMap(value: CborValue): Map<CborValue, CborValue> {
  if (value instanceof Map) return value;
  throw new Error("expected a map");
}

/** A decoded integer may surface as `bigint` (see `decInto`'s large-value path), so a
 * numeric literal's expected `number` is compared against the `bigint`-normalized form
 * rather than failing on a type mismatch that isn't a value mismatch. */
export function asLiteral<T extends CborValue>(value: CborValue, expected: T): T {
  const norm = typeof value === "bigint" && typeof expected === "number" ? Number(value) : value;
  if (norm !== expected) throw new Error(`literal mismatch: expected ${JSON.stringify(expected)}`);
  return expected;
}

/** Validate a decoded scalar against a literal-enum's declared vocabulary, erroring
 * on an unknown value. The caller reads through `asNumber`/`asString`/`asBool`, which
 * already normalize a decoded `bigint` to `number`, so a plain membership check suffices. */
export function asEnumMember<T extends number | string | boolean | null>(
  value: T,
  members: readonly T[],
): T {
  if (!members.includes(value)) {
    throw new Error(`unknown enum value: ${JSON.stringify(value)}`);
  }
  return value;
}

/** Read a decoded CBOR scalar without narrowing to one JS type, normalizing a
 * decoded integer `bigint` to `number` the same way `asNumber` does. Used for a
 * MIXED-kind literal enum (`"a" / 1`), where no single `asNumber`/`asString`/
 * `asBool` reader fits every member's runtime type — `asEnumMember`'s membership
 * check does the real narrowing instead. */
export function asEnumScalar(value: CborValue): string | number | boolean | null {
  if (typeof value === "bigint") return Number(value);
  if (
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean" ||
    value === null
  ) {
    return value;
  }
  throw new Error("expected an enum scalar (string, number, boolean, or null)");
}

function asTagged(value: CborValue, tag: number): CborValue {
  if (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof Uint8Array) &&
    !(value instanceof Map) &&
    value.tag === tag
  ) {
    return value.value;
  }
  throw new Error(`expected tag ${tag}`);
}

/** Decode a tag-0 (RFC 3339, UTC) timestamp into a `Date`. */
export function asTimestamp(value: CborValue): Date {
  return new Date(asString(asTagged(value, 0)));
}

/** Decode a tag-4 decimal fraction into its `[exponent, mantissa]` payload. */
export function asDecimalPayload(value: CborValue): [number | bigint, number | bigint] {
  const arr = asArray(asTagged(value, 4));
  const intOf = (x: CborValue): number | bigint => {
    if (typeof x === "number" || typeof x === "bigint") return x;
    throw new Error("expected an integer in decimal payload");
  };
  return [intOf(arr[0]), intOf(arr[1])];
}

/** A `Date` rendered as the canonical tag-0 text: RFC 3339, UTC, `Z` offset. */
export function csilTsToText(d: Date): string {
  return d.toISOString().replace(/\.000Z$/, "Z");
}
"#;
