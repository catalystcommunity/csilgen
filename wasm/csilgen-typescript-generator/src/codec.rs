//! Emits `codec.gen.ts`: a self-contained canonical-CBOR codec over a dynamic
//! value tree plus per-record (de)serializers.
//!
//! TypeScript has no reflection-driven, tag-aware CBOR ecosystem that pins the
//! cross-language wire contract (canonical map key order, `bytes` as a CBOR byte
//! string, tag-0 timestamps, tag-4 decimals), so — like the C/Zig/OCaml/Dart/
//! Swift/Go/Python generators — this generator emits the payload codec itself.
//! Each record gets a deep `to<T>CborValue`/`from<T>CborValue` pair plus the
//! byte-level `to<T>Cbor`/`from<T>Cbor` the typed client calls.

use crate::common::{self, DecimalMapping};
use csilgen_common::{
    CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType,
    CsilSpecSerialized, CsilTypeExpression, WasmGeneratorInput,
};
use std::collections::{BTreeSet, HashMap, HashSet};

const DEFAULT_TYPES_MODULE: &str = "./types.gen";

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
fn aliases(spec: &CsilSpecSerialized) -> HashMap<String, &CsilTypeExpression> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            // A `Name = { ... }` group is a record (its own codec path), and a
            // `Name = A / B` choice has no single underlying value to recurse into;
            // neither is a transparent alias the value handlers resolve through.
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_))
            | CsilRuleType::TypeDef(CsilTypeExpression::Choice(_)) => None,
            CsilRuleType::TypeDef(t) => Some((common::to_pascal(&r.name), t)),
            _ => None,
        })
        .collect()
}

struct Ctx<'a> {
    records: &'a HashSet<String>,
    aliases: &'a HashMap<String, &'a CsilTypeExpression>,
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
            None => !ctx.records.contains(&common::to_pascal(name)),
        },
        CsilTypeExpression::Array { element_type, .. } => enc_is_identity(element_type, ctx),
        _ => false,
    }
}

/// A TypeScript expression building the CBOR value tree node for `expr` (a typed
/// value).
fn ts_enc_value(ty: &CsilTypeExpression, expr: &str, ctx: &Ctx) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "timestamp" => format!("{{ tag: 0, value: csilTsToText({expr}) }}"),
            "decimal" => match ctx.mapping {
                DecimalMapping::Csil => format!("{{ tag: 4, value: {expr}.toTag4() }}"),
                DecimalMapping::Library => format!("{{ tag: 4, value: csilDecToTag4({expr}) }}"),
            },
            "null" | "nil" => "null".to_string(),
            _ => expr.to_string(),
        },
        CsilTypeExpression::Reference(name) if ctx.records.contains(&common::to_pascal(name)) => {
            format!("to{}CborValue({expr})", common::to_pascal(name))
        }
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            Some(underlying) => ts_enc_value(underlying, expr, ctx),
            None => expr.to_string(),
        },
        CsilTypeExpression::Array { element_type, .. } => {
            if enc_is_identity(element_type, ctx) {
                expr.to_string()
            } else {
                let inner = ts_enc_value(element_type, "csilE", ctx);
                format!("{expr}.map((csilE): CborValue => {inner})")
            }
        }
        CsilTypeExpression::Map { value, .. } => {
            // The in-memory type is `Record<string, V>`; the wire form is a CBOR
            // map, so the object's entries are rebuilt as a `Map` (insertion order).
            let inner = ts_enc_value(value, "csilV", ctx);
            format!(
                "new Map<CborValue, CborValue>(Object.entries({expr}).map(([csilK, csilV]): [CborValue, CborValue] => [csilK, {inner}]))"
            )
        }
        CsilTypeExpression::Choice(_) => expr.to_string(),
        _ => expr.to_string(),
    }
}

/// A TypeScript expression reconstructing the typed value from `expr` (a
/// `CborValue` node). The inverse of `ts_enc_value`; scalars are not the identity
/// because a decoded node is a `CborValue` union that must be narrowed.
fn ts_dec_value(ty: &CsilTypeExpression, expr: &str, ctx: &Ctx) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "nint" | "float" | "float16" | "float32" | "float64" | "double" => {
                format!("asNumber({expr})")
            }
            "text" | "tstr" => format!("asString({expr})"),
            "bytes" | "bstr" => format!("asBytes({expr})"),
            "bool" => format!("asBool({expr})"),
            "timestamp" => format!("asTimestamp({expr})"),
            "decimal" => match ctx.mapping {
                DecimalMapping::Csil => format!("CsilDecimal.fromTag4(asDecimalPayload({expr}))"),
                DecimalMapping::Library => format!("csilDecFromTag4(asDecimalPayload({expr}))"),
            },
            "any" => expr.to_string(),
            "null" | "nil" => "null".to_string(),
            _ => format!("asString({expr})"),
        },
        CsilTypeExpression::Reference(name) if ctx.records.contains(&common::to_pascal(name)) => {
            format!("from{}CborValue({expr})", common::to_pascal(name))
        }
        CsilTypeExpression::Reference(name) => match ctx.aliases.get(&common::to_pascal(name)) {
            Some(underlying) => ts_dec_value(underlying, expr, ctx),
            None => format!(
                "({expr} as unknown as {})",
                common::ts_type(ty, ctx.mapping)
            ),
        },
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = ts_dec_value(element_type, "csilE", ctx);
            format!("asArray({expr}).map((csilE) => {inner})")
        }
        CsilTypeExpression::Map { value, .. } => {
            let inner = ts_dec_value(value, "csilV", ctx);
            format!(
                "Object.fromEntries(Array.from(asMap({expr}), ([csilK, csilV]): [string, {}] => [asString(csilK), {inner}]))",
                common::ts_type(value, ctx.mapping)
            )
        }
        CsilTypeExpression::Choice(_) => format!("asString({expr})"),
        _ => format!(
            "({expr} as unknown as {})",
            common::ts_type(ty, ctx.mapping)
        ),
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

    out.push_str(&format!(
        "export function to{type_name}CborValue(v: {type_name}): CborValue {{\n"
    ));
    out.push_str("  const csilMap = new Map<CborValue, CborValue>();\n");
    let mut encode_fields: Vec<&CodecField> = fields.iter().collect();
    encode_fields.sort_by(|a, b| a.key_bytes.cmp(&b.key_bytes));
    for field in &encode_fields {
        let wire = ts_string_literal(&field.wire);
        let access = format!("v.{}", field.member);
        let enc = ts_enc_value(field.value_type, &access, ctx);
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
                let dec = ts_dec_value(field.value_type, "csilV", ctx);
                out.push_str(&format!(
                    "    {}: ((csilV: CborValue | undefined) => csilV === undefined ? undefined : {dec})(mapGet(value, {wire})),\n",
                    field.member
                ));
            } else {
                let dec =
                    ts_dec_value(field.value_type, &format!("requireKey(value, {wire})"), ctx);
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
    let aliases = aliases(spec);
    let ctx = Ctx {
        records: &records,
        aliases: &aliases,
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
    // `CsilDecimal` is a value (its `toTag4`/`fromTag4` run at runtime), so it is a
    // value import, not a type-only one.
    imports.remove("CsilDecimal");

    let module = string_option(input, "codec_types_module", DEFAULT_TYPES_MODULE);
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

/// A double-quoted TypeScript string literal with the few characters that could
/// break a wire key escaped (field names are identifiers, but be defensive).
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
