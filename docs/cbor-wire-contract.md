# CSIL CBOR wire contract

CSIL is the **CBOR** Service Interface Language. When two independently-generated
parties (a Go server and a Rust/Python/TypeScript client, say) exchange a message,
they must agree byte-for-byte on how that message is keyed and encoded. This
document states that contract normatively so every generator can target it.

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

## RPC call naming (generated clients)

The `*-client` targets (`go-client`, `rust-client`, `python-client`,
`typescript-client`) emit one method per unary operation that delegates to a
caller-supplied transport with a `(service, method)` pair. All four generators
derive that pair identically so a client in any language reaches the same
endpoint:

- **service** — the service name with a trailing `Service` stripped, lowercased:
  `CorndogsService` → `"corndogs"`.
- **method** — the operation name PascalCased with the simple rule (capitalize
  after `_`/`-`, otherwise leave each character as written, preserving acronym
  runs): `SubmitTask` → `"SubmitTask"`, `GetTaskStateByID` → `"GetTaskStateByID"`.

The transport (caller-owned) maps `(service, method)` onto the concrete wire —
e.g. an HTTP POST whose body is the CBOR-encoded request.

## Operation error channel

An operation `Op: Req -> Res / ServiceError` carries `ServiceError` as the
**error channel**, not part of the success payload. On the wire a call yields
either a `Res` map or a `ServiceError` map; generators surface the error per
language idiom (Go `error`, Rust `Result::Err`, Python `ServiceError` return /
raise, TypeScript thrown) and type the success path as `Res`.
