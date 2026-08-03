# CSIL CBOR wire contract

CSIL is the **CBOR** Service Interface Language. When two independently-generated
parties (a Go server and a Rust/Python/TypeScript client, say) exchange a message,
they must agree byte-for-byte on how that message is keyed and encoded. This
document states that contract normatively so every generator can target it.

This document applies to **codec-emitting** generators — those that produce
runtime CBOR encode/decode code, not every generator. See
[`generator-plugin-contract.md`](generator-plugin-contract.md) for the plugin
interface every generator implements, and its "Conformance tiers" section for how
the two relate.

## Map keys are the CSIL field names, verbatim

A group/record encodes as a CBOR map whose keys are the **CSIL field names as
written in the `.csil` source** — snake_case, unchanged. Generators map those
field names to each language's local naming convention for the in-memory type
(Go `PascalCase`, TypeScript `camelCase`, Rust/Python snake_case), but the **wire
key is always the CSIL name**.

Given:

```
Task = { uuid: text, current_state: text, payload: bytes }
```

every party encodes the map keys `uuid`, `current_state`, `payload` — never
`Uuid`, `currentState`, or any case-folded variant.

### Per-language notes

- **Rust** (serde): the struct field name *is* the CSIL field name (snake_case),
  so serde's default keying already matches. No attribute needed.
- **Go** (`fxamacker/cbor` or compatible): Go exported fields are `PascalCase`, and
  the codec keys by the Go field name unless told otherwise. The generator emits a
  `cbor:"<csil_field_name>"` struct tag on every field so the wire key matches.
  These tags are on by default; `use_cbor_tags: false` disables them.
- **Python**: dataclass field names are the CSIL field names; encode/decode keyed
  by those names.
- **TypeScript**: the type uses `camelCase`, but the transport encodes/decodes by
  the CSIL field name (the longhouse CBOR transport keys this way).
- **All 14 targets** now emit a generated per-type codec rather than relying on
  runtime reflection or derive/tag libraries — one uniform architecture. Each
  generator emits a self-contained CBOR codec (`codec.gen.h`, `codec.gen.zig`,
  `codec.ml`, `csil_cbor.gen.dart` + per-record `toCbor`, `Codec.swift`,
  `codec.gen.go`, `codec.gen.rs`, `codec.gen.ts`, `codec.py`, `codec.rb`,
  `codec.gen.ex`, `CsilCbor.java`, `Codec.gen.cs`, `Codec.kt`) that keys the map by
  the CSIL field name verbatim and orders a record's keys canonically at generation
  time so the bytes match — the one place a csilgen generator emits payload-wire code
  rather than shapes only. The typed client calls these codecs and hands the carrier
  only bytes; the carrier never reflects, never sees an application type. (Swift in
  particular uses a generated codec rather than `Codable`: `Codable` cannot emit CBOR
  tags and would serialize `bytes`/`[UInt8]` as an array, not a byte string.)

## Scalar encodings

| CSIL          | CBOR                                   |
| ------------- | -------------------------------------- |
| `text`/`tstr` | text string (major type 3)             |
| `bytes`/`bstr`| **byte string (major type 2)**         |
| `int`         | signed/unsigned integer                |
| `uint`        | unsigned integer                       |
| `bool`        | simple value true/false                |
| `float*`      | float                                  |
| `timestamp`   | **tag 0** + RFC3339 text string, UTC   |
| `decimal`     | **tag 4** decimal fraction `[exp, mant]`|

`bytes` MUST encode as a CBOR byte string (major type 2), not an array of
integers. Rust achieves this with `#[serde(with = "serde_bytes")]` (emitted
automatically); Go's `[]byte` does this under `fxamacker/cbor` automatically;
Python `bytes` and TypeScript `Uint8Array` map to the same.

## Tagged core types: `timestamp` and `decimal`

Two CSIL core types carry CBOR semantic tags and so cannot be expressed as a
plain CDDL primitive. Both have a single normative wire form; generators differ
only in the in-memory type they map to.

### `timestamp` — CBOR tag 0, always UTC

A `timestamp` encodes as **tag 0** (standard date/time string, RFC 3339) wrapping
a text string. The instant is **always serialized in UTC** with a `Z` offset
(e.g. `2024-01-02T03:04:05Z`); a generator MUST normalize any local time to UTC
before encoding. Sub-second precision is preserved when present. Decoders SHOULD
also accept tag 1 (epoch) on input but MUST emit tag 0.

In-memory mappings (each is a UTC-typed instant where the language allows):

| Target     | Type                                |
| ---------- | ----------------------------------- |
| Rust       | `chrono::DateTime<chrono::Utc>`     |
| Go         | `time.Time` (kept in UTC)           |
| TypeScript | `Date` (UTC-based)                  |
| Python     | `datetime.datetime` (tz-aware, UTC) |
| JSON/OpenAPI schema | `{"type":"string","format":"date-time"}` |

### `decimal` — CBOR tag 4, exact

A `decimal` is an **exact** base-10 value (never a float). It encodes as **tag 4**
(decimal fraction): a two-element array `[exponent, mantissa]` of integers, value
= mantissa × 10^exponent. This is lossless and language-independent.

By default a generator emits a small self-contained **`CsilDecimal`** helper type
(only when the spec actually uses `decimal`) that holds the exact value and is
trivially convertible to/from the language's popular decimal library. Python is
the exception: it always uses the stdlib `decimal.Decimal` and emits no helper.

The `decimal_mapping` file option selects the in-memory type:

```
options {
  decimal_mapping: "library"   ; default is "csil"
}
```

| Target     | `decimal_mapping: "csil"` (default) | `decimal_mapping: "library"`        |
| ---------- | ----------------------------------- | ----------------------------------- |
| Rust       | generated `CsilDecimal`             | `rust_decimal::Decimal`             |
| Go         | generated `CsilDecimal`             | `github.com/shopspring/decimal.Decimal` |
| TypeScript | generated `CsilDecimal`             | `Decimal` (`decimal.js`)            |
| Python     | `decimal.Decimal` (always)          | `decimal.Decimal` (always)          |
| JSON/OpenAPI schema | `{"type":"string","format":"decimal"}` (exact text) | same |

Unknown `decimal_mapping` values are a hard generation error (same validate-early
idiom as `ts_bidirectional_transport`). The wire form (tag 4) is identical for
both mappings — the option only changes the generated in-memory type.

## Optional fields

An optional field (`? name: T`) is **absent** from the map when unset, rather than
present-with-null. On decode, a missing optional field deserializes to the
language's empty/none value. (Rust additionally requires `#[serde(default)]` on
optional `bytes` fields because the custom `serde_bytes` codec otherwise turns a
missing field into a hard error; the generator emits this automatically.)

**Absent is not the same as present-and-empty.** A field whose value is an empty
byte string, empty text, empty array, or empty map is *present*: encode MUST emit
its key with the empty value (a `bytes` field becomes a zero-length CBOR byte
string, `0x40`), and decode MUST NOT normalize it back to absent. Presence is
decided by whether the value is set, never by whether it is empty — so a generator
that emits a truthiness test (`if (field)`, `if len(field) > 0`) instead of a
presence test (`if field is not None`, `if v.payload != nil`) is wrong. This
distinction is load-bearing: a caller uses it to mean "leave the stored value
alone" (absent) versus "replace the stored value with nothing" (present-empty),
and the three states — absent, present-empty, present-non-empty — must survive an
encode/decode round trip in every generated language.

## Choices: enum vs. union on the wire

A CSIL choice (`A / B / ...`, whether declared inline or via `Name = A / B` /
`Name /= A / B`) classifies into exactly one of two wire shapes, decided once by
a single shared rule (`crates/csilgen-common/src/choice.rs`) that every
generator's codec defers to rather than re-deriving:

- **Every arm is a literal, of any kind or mix of kinds** (`"a" / "b"`, `1 / 2`,
  and `"a" / 1` are all this case) — an **enum**. The wire form is the **bare
  literal value itself**, with no discriminator. Decode is a **membership
  check** against the full declared vocabulary, never merely a runtime-type
  match — an out-of-vocabulary value of the right type (e.g. `"c"` when only
  `"a"`/`"b"` are declared) is a decode error, not a silent accept.
- **At least one arm is not a literal** — a **union**. The wire form is a
  tagged sum, a two-element array `[variant_index, value]`, where
  `variant_index` is the arm's **0-based declaration order** in the choice. A
  bare `null` arm (`text / null`) counts as non-literal here — CSIL's grammar
  parses a bare `null` arm to the `null` builtin type, never a null literal, so
  a choice containing one is always a union even when every other arm is a
  literal.

For an encoder choosing among a union's arms (only relevant when more than one
arm could structurally match a given value):

- A **literal arm wins over a general arm of the same runtime type** by value
  equality — e.g. in `text / "pending" / "confirmed"`, the string `"pending"`
  encodes as the `"pending"` literal arm's index, not the general `text` arm's.
- Among several **general arms that share one runtime dispatch type**, the
  **first declared** wins.

Some dynamic languages cannot distinguish all general arms. For example,
TypeScript uses one runtime number type for `int`, `uint`, and `float`. The first
declared arm wins for these values. For record arms, the TypeScript encoder checks
the required properties of each record. This check distinguishes records that
have different required properties. It cannot distinguish records that accept the
same object shape. The first declared arm wins when two record shapes match. Use
an explicit discriminant field when each arm must have a unique runtime identity.

Decode always dispatches by `variant_index`; a literal arm's payload is then
validated for equality against that literal (a `[1, "confirmed"]` for an arm
declared as the literal `"pending"` at index 1 is a decode error, not a coerced
value).

This is normative for all 14 code-generating targets — a generator that groups
arms by runtime type before treating a choice as an enum, or that requires
literal arms be uniform in kind, does not follow this contract (a real
regression this shared module was written to fix; see the module's rustdoc for
the TypeScript/OCaml history).

## Inline composite hoisting (generated-surface naming)

A group or choice written **inline** — directly in a field, array element, map
key/value, or tuple slot, at any nesting depth — has no named CSIL rule behind
it. Every generator hoists such inline composites to a synthesized named rule
before generating (`crates/csilgen-common/src/hoist.rs`), so the same
named-rule codec machinery (per-record map codecs, per-union tagged-sum codecs,
reference dispatch) reaches every position uniformly, with no bespoke
per-position codec path and no generator re-implementing this pass itself.

Synthesized names follow one fixed scheme, not configurable per generator:

- A group/choice **field** on a record: `<Owner>_<field>`.
- An **array element**: `<Owner>_item` (suffix `_item`).
- A **map key** / **map value**: `<Owner>_key` / `<Owner>_value`.
- A **tuple slot**: `<Owner>_<index>` (0-based).
- Applied **recursively** — a nested inline composite inside an already-hoisted
  one is hoisted against the synthesized owner's name in turn.

Name collisions are resolved case-insensitively (`UserData`, `User_data`, and
`user-data` all reserve the same canonical key), so a synthesized name never
silently shadows an existing rule or another synthesized one regardless of
which casing convention a downstream generator applies to it. A control
operator on the original inline position (e.g. `.default`) is preserved at the
use site, wrapping the new reference to the synthesized rule — it is never
dropped.

An all-literal inline choice is generally **not** hoisted (most generators
render it as a bare enum directly in the field position, since the codec
already emits the correct bare-literal wire value there); OCaml is the one
exception, hoisting even an all-literal inline choice, because OCaml has no
anonymous sum-type field syntax and a variant type must be named regardless.
Either way the **wire bytes are unaffected** — hoisting only changes what name
appears in the generated in-memory API, never the CBOR on the wire.

## Decoder strictness

Every generated decoder validates the declared shape on the way in, not merely
"parse whatever bytes are provided into the expected in-memory type and hope
they line up." A decode call returns an error (never a garbage or default
value) when the wire data disagrees with the declared type in any of these
ways:

- **Wrong CBOR major type** for the field/type (e.g. a text string where the
  declaration is `bytes`, or a map where an array is expected).
- **Wrong or missing semantic tag** (`timestamp` not tag 0, `decimal` not tag 4).
- **Enum value not in the declared literal vocabulary** (see "Choices" above).
- **Union `variant_index` out of range, or a literal arm's payload not equal to
  the literal it is declared to be.**
- **Group/record missing a required (non-`?`) field**, or the wrong CBOR shape
  for the map/array.

This holds across all 14 code-generating targets — the point of the
per-generator codec architecture (see "Map keys are the CSIL field names,
verbatim" above) is that this validation is emitted **once, in generated code,
at the boundary**, rather than relied on ambiently from a runtime
reflection/derive library that may or may not enforce it.

## RPC call naming (generated clients)

The `*-client` targets emit one method per unary operation that delegates to a
caller-supplied transport with a `(service, op)` pair. Every generator derives
that pair identically so a client in any language reaches the same endpoint,
and the pair matches the `service`/`op` fields of the CSIL-RPC v1 envelope
(`csil-rpc-transport.md` §1.1) verbatim:

- **service** — the CSIL service name exactly as written in the `.csil` source:
  `service DomainKeys` → `"DomainKeys"`, `service CorndogsService` →
  `"CorndogsService"`. No case change, no suffix stripping — a transport
  implementation must be able to place the string on the wire unmodified.
- **op** — the CSIL operation name exactly as written (kebab-case):
  `get-domain-keys` → `"get-domain-keys"`, `submit-task` → `"submit-task"`.

The same verbatim rule applies to channel operations: the event name a
generated encoder/router uses on the verbose Events wire is the CSIL operation
name as written (`on-tick` → `"on-tick"`). Compact-profile routing by
`@wire-id` ordinal is unaffected.

The transport (caller-owned) maps `(service, op)` onto the concrete wire —
e.g. the `service`/`op` fields of a `CsilRpcRequest`. Earlier revisions of this
contract lowercased the service (stripping a trailing `Service`) and
PascalCased the op; that derivation was lossy (word boundaries could not be
recovered at the transport seam) and is gone — consumers dispatch on the
verbatim names.

## Operation error channel

An operation `Op: Req -> Res / ServiceError` carries `ServiceError` as the
**error channel**, not part of the success payload. On the wire a call yields
either a `Res` map or a `ServiceError` map; generators surface the error per
language idiom (Go `error`, Rust `Result::Err`, Python `ServiceError` return /
raise, TypeScript thrown) and type the success path as `Res`.
