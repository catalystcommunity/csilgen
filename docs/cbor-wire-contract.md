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

## Dynamic map key order and duplicate keys

A dynamic map (`{ * K => V }`, named or inline, in any position) has one encoding
for each value:

- **Encode.** Sort the entries by the bytes of each encoded key, in bytewise
  lexicographic order (RFC 8949 §4.2.1, "Core Deterministic Encoding"). A shorter
  encoded key comes first, so text key `"b"` (`61 62`) comes before `"aa"`
  (`62 61 61`). Integer keys go `1` (`01`), `10` (`0a`), `-1` (`20`). This is not
  the string order or the number order of the language.
- **Decode.** Reject a map whose encoded keys are not in strictly ascending order
  of that kind. This also rejects a key that occurs two times. The check compares
  each key with the key before it, so it is O(n) and does not sort.
- **Shortest form.** A map key (in a dynamic map or a record) must use the
  shortest head for its length or value (RFC 8949 §4.2.1). A decoder rejects
  `18 01` for the integer 1, or `78 01 61` for the text `"a"`. Otherwise one key
  has two byte forms, and the order check could accept a map that a canonical
  encoder never writes.

A record (group) is different. The encoder writes its fields in declared order.
The decoder accepts the fields in any order, but it rejects a key that occurs two
times.

### Difference from CDDL and CBOR, and why

CDDL (RFC 8610) describes data, not encodings: a map matches its CDDL type with
its keys in any order. CBOR (RFC 8949) also treats map keys as unordered. Its
deterministic encoding is optional, and each protocol chooses. A plain CBOR
decoder accepts any key order.

CSIL chooses to require the order, and to reject input that does not obey it.
The decoder rule enforces the wire contract:

- **One byte form for each value.** The same map gives the same bytes in all 14
  languages. Hashes, signatures, caches and byte-for-byte tests then work across
  languages.
- **A lenient decoder hides a bad encoder.** If decoders accepted any order, an
  encoder that does not sort would pass every interop test, and the byte form
  would not be reliable. A decoder that rejects unsorted input finds that encoder
  at once.
- **Duplicate keys.** RFC 8949 calls a map with a duplicate key not valid, but
  decoders differ: some keep the first value, some the last, some all of them. Two
  parsers can then see different values in one message. The order check rejects
  duplicates at no extra cost.

The cost is a sort on encode: O(n log n) key comparisons, plus a buffer for each
encoded entry. A peer that is not generated by csilgen must also sort its dynamic
maps, or a CSIL decoder rejects them.

## Scalar encodings

| CSIL          | CBOR                                   |
| ------------- | -------------------------------------- |
| `text`/`tstr` | text string (major type 3)             |
| `bytes`/`bstr`| **byte string (major type 2)**         |
| `int`         | signed/unsigned integer                |
| `uint`        | unsigned integer                       |
| `bool`        | simple value true/false                |
| `float*`      | float                                  |
| `date`        | **tag 1004** + `YYYY-MM-DD` text        |
| `timestamp`   | **tag 0** + RFC3339 text string, UTC   |
| `cddl_time`   | **tag 1** + epoch seconds (int or float) |
| `decimal`     | **tag 4** decimal fraction `[exp, mant]`|

`bytes` MUST encode as a CBOR byte string (major type 2), not an array of
integers. Rust achieves this with `#[serde(with = "serde_bytes")]` (emitted
automatically); Go's `[]byte` does this under `fxamacker/cbor` automatically;
Python `bytes` and TypeScript `Uint8Array` map to the same.

A `float*` field decodes a half, single, or double precision float. Encode
writes a double precision float. A target that cannot hold NaN or an infinity
(Elixir) refuses it with a decode error.

## Tagged core types

Each tagged core type has one wire format.

### Calendar date

Use `date` for a Gregorian calendar day. A date has no time or time zone.
Encode it with CBOR tag 1004 and a text string of ten ASCII characters.
Use four digits for the year and two each for the month and day. Put a hyphen
between the parts. For example, `2000-02-29` encodes as
`d9 03 ec 6a 32 30 30 30 2d 30 32 2d 32 39`. Reject an incorrect tag, an
incorrect text format, or an impossible date. [RFC 8943](https://www.rfc-editor.org/rfc/rfc8943.html#section-1)

| Target | Calendar date type |
| --- | --- |
| Rust | `time::Date` |
| Java and Kotlin | `java.time.LocalDate` |
| C# | `System.DateOnly` |
| Python | `datetime.date` |
| Ruby and Elixir | `Date` |
| C, Dart, Go, OCaml, PHP, Swift, TypeScript, and Zig | Validated ISO date string |
| JSON Schema and OpenAPI | `{"type":"string","format":"date"}` |

A native date type can limit the year range. For exchange with Python or
C#, use years 0001 through 9999.

### `timestamp` — CBOR tag 0, always UTC

A `timestamp` is an instant. Encode it as **tag 0** around an RFC 3339 text
string. Every target uses the same strict rules, so a value that one peer
refuses is refused by all peers.

**Decode.** Accept only tag 0 around a text string with this form:

```text
YYYY-MM-DD ("T" / "t") hh:mm:ss [ "." 1*9DIGIT ] ( "Z" / "z" / ("+" / "-") hh:mm )
```

- All digits are ASCII. Each field has the exact number of digits shown.
- The month is 01–12. The day exists in that month of the Gregorian calendar.
- The hour is 00–23, the minute is 00–59, and the second is 00–59. A leap
  second (`60`) is an error.
- An offset hour is 00–23 and an offset minute is 00–59.
- The year is 0001–9999 as written, and also after conversion to UTC.
- The fraction has one to nine digits. If the target type cannot hold a
  digit that is not zero, decoding fails. A decoder never rounds or cuts a
  fraction.

Any other value is a decode error: no tag, tag 1 (epoch, use `cddl_time`), a different tag,
content that is not text, a space for `T`, no offset, or extra characters.
A decoder reports the error through the codec's normal decode error. It
never returns a sentinel value such as an Invalid Date.

**Encode.** Convert the instant to UTC and write uppercase `T` and `Z`. Write
no fraction when it is zero. Otherwise write three, six, or nine digits: the
fewest of these that hold the value exactly. Thus every target writes the
same bytes for the same instant.

| Instant | Encoded text |
| --- | --- |
| 12:00 UTC | `2000-02-29T12:00:00Z` |
| 12:00 UTC plus 0.5 s | `2000-02-29T12:00:00.500Z` |
| 12:00 UTC plus 500 µs | `2000-02-29T12:00:00.000500Z` |
| `2000-02-29T13:30:00+01:30` after decode | `2000-02-29T12:00:00Z` |

The vectors in `tests/interop/timestamp-vectors/vectors.tsv` hold these
rules. Run `tests/interop/timestamp-vectors/check.sh <target>` to check one
target.

In-memory mappings and the fraction digits each target holds:

| Target     | Type                                | Digits |
| ---------- | ----------------------------------- | ------ |
| Rust       | `time::OffsetDateTime`              | 9      |
| Go         | `time.Time` (kept in UTC)           | 9      |
| TypeScript | `Date` (UTC-based)                  | 3      |
| Python     | `datetime.datetime` (tz-aware, UTC) | 6      |
| Java and Kotlin | `java.time.Instant`            | 9      |
| C#         | `System.DateTimeOffset` (UTC ticks) | 7      |
| Swift      | Canonical UTC RFC 3339 `String` (checked and rewritten on encode and decode) | 9 |
| Dart       | `DateTime` (UTC)                    | 6 (3 on the web) |
| Elixir     | `DateTime` (`Etc/UTC`)              | 6      |
| C          | `CsilTimestamp` (canonical UTC text in `rfc3339`, plus `epoch_seconds`) | 9 |
| Zig        | `CsilTimestamp` (canonical UTC text in `rfc3339`, plus `epoch_seconds`) | 9 |
| Ruby       | `Time` (UTC on encode)              | 9      |
| OCaml      | Canonical UTC RFC 3339 `string` (checked and rewritten on encode and decode) | 9 |
| PHP        | `\DateTimeImmutable` (UTC on decode; encode also accepts `\DateTime`) | 6 |
| JSON/OpenAPI schema | `{"type":"string","format":"date-time"}` | — |

C# holds 100 ns ticks, so fraction digits 8 and 9 must be zero. It writes a
value with seven significant digits as nine digits. Dart on the web holds
milliseconds and refuses a finer fraction.

C, Zig, OCaml, and Swift keep the timestamp as text. Decode writes the canonical UTC text, so
equal instants have equal text. In C and Zig, the text is in `rfc3339`, and
decode also sets `epoch_seconds`. Encode reads only the text. It writes the canonical form of that text, and it fails
if the text does not obey the decode rules.

### `cddl_time` — CBOR tag 1, epoch seconds

A `cddl_time` is an instant, as in the CDDL prelude `time = #6.1(number)`.
Use it to exchange data with a peer that sends epoch seconds, for example a
sensor. Use `timestamp` for all other instants, because a float cannot hold
all fractions exactly. Each target uses the same in-memory type for
`cddl_time` and `timestamp`. Only the wire form is different.

**Decode.** Accept only tag 1 around an integer (major type 0 or 1) or a
float (half, single, or double precision). Any other value is a decode error:
no tag, tag 0, a different tag, or content that is not a number.

- An integer is a count of whole seconds since `1970-01-01T00:00:00Z`. A
  decoder must not wrap a large integer. For example, `3b ffffffffffffffff`
  (−2^64) is a decode error, not 0.
- A float must be finite. Convert it to binary64 first. Then calculate the
  seconds and the fraction units with binary64 arithmetic. `p` is the number
  of fraction digits that the target holds:

  ```text
  seconds = floor(value)
  units   = floor((value - seconds) * 10^p + 0.5)
  if units == 10^p: seconds = seconds + 1, units = 0
  ```

  Do the multiplication and the addition as two operations. Do not fuse them
  into one multiply-add, because a fused operation can give a different
  result. Before the calculation, refuse a value below `FIRST - 1` or above
  `LAST + 1`, so that the conversion to an integer cannot overflow.
- After the conversion, `seconds` must be from `FIRST = -62135596800`
  (`0001-01-01T00:00:00Z`) to `LAST = 253402300799` (`9999-12-31T23:59:59Z`).
  This check decides the range. The first check only prevents an overflow,
  so `FIRST - 0.5` passes it and then fails this check.

**Encode.** Write tag 1. If the fraction is zero, write the seconds as the
shortest integer. Otherwise write a double-precision float (`0xfb`) with the
value `seconds + units / 10^p`. Calculate the division and then the addition
in binary64. A target that holds more digits than another can write different
bytes for the same input, because it keeps more of the fraction.
Near the present epoch, the step between two binary64 values is about
1.2e-7 s. Thus, at `p` of 7 or more, a fraction usually changes a little after
one round trip. For example, `.000500` seconds comes back as `.000499964`
seconds at `p = 9`. Use `timestamp` when the fraction must be exact.

Encode fails if the instant is outside `FIRST` to `LAST`, or if it has a
digit that is not zero after digit `p`. Encode returns the codec's normal
error. The same applies to `timestamp`. A typed JSON helper writes the same
number that the CBOR encode writes.

| Value on the wire | Decoded at 3 digits | Encoded again |
| --- | --- | --- |
| `1(951825600)` | `2000-02-29T12:00:00Z` | `1(951825600)` |
| `1(951825600.5)` | `2000-02-29T12:00:00.500Z` | `1(951825600.5)` |
| `1(951825600.0)` | `2000-02-29T12:00:00Z` | `1(951825600)` |
| `1(1.0005)` | `1970-01-01T00:00:01Z` | `1(1)` |

The binary64 value nearest to 1.0005 is a little less than 1.0005, so it
rounds down to zero milliseconds.

The `p` value of a target is the "Digits" column of the `timestamp` table.
Dart on the web uses `p = 3`. A tag 1 inside an `any` value is kept as it
is, with no check. Only a declared `cddl_time` field applies these rules. TypeScript decodes an integer and a float to
the same `number`, so it uses the float path for both. The result is the same,
because an integer has a zero fraction.
Text-based targets (C, Zig, OCaml, and Swift) use `p = 9`, and decode writes
the canonical UTC text. Encode reads that text with the `timestamp` decode
rules. The JSON Schema and OpenAPI view of a `cddl_time` is
`{"type":"number"}`.

The vectors in `tests/interop/epoch-vectors/vectors.tsv` hold these rules. The
`digits` column is `*` for a row that applies to all targets. Otherwise the row
applies only to a target with that `p`. Run
`tests/interop/epoch-vectors/check.sh <target>` to check one target.

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

OCaml and Swift have no decimal type. They keep a `decimal` as exact text
(`"123.45"`) and write tag 4 on the wire. Encode accepts an optional sign,
digits, and an optional fraction. The mantissa must fit in 64 bits. Swift
decode refuses an exponent outside -1024 to 1024, because each unit of the
exponent becomes one character of text.

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

## Named numbers

For `Code = &(busy: 7, denied: 42)`, encode `busy` as the CBOR integer
`7` (hex `07`). Encode `denied` as `42` (hex `18 2a`). The same rule
applies inside records and containers. Use the number in the schema even
if the member changes position. Reject a text label or an unknown number
during CBOR decode. A change from a quoted choice to a named number
requires a coordinated upgrade of all peers that use that field.

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

All code generators must use these rules. Preserve the declared order of the
arms, including literal arms of different types.

A union of records, including a union made with `//=`, follows the same rules: it
encodes as `[variant_index, value]`. Each `//=` arm gets the next index in the
declaration order across files that `csil-spec.md` ("Group Choice Extension")
defines. An operation input or output with an inline arm, such as
`Ev -> {* text => int} / Err`, is a named union (`<Service><Op>Output`) with this
same encoding; the reply has no `variant` (see `csil-spec.md`, "Inline arms make one
union value").

## Group references, sockets and plugs

These rules agree with the schema diagnostic (`csilgen-schema`), which is the
reference reader. Every generator applies them before it hoists
(`crates/csilgen-common/src/splice.rs`).

- **Group reference in a record.** A record entry that has no key and names a
  group rule adds the entries of that group to the record, at that position.
  `R = { shared_fields, b: bool }` with `shared_fields = (shared_id: uint)` is
  a map with the keys `shared_id` and `b`. There is no `shared_fields` key.
- **Optional or repeated group reference.** `? g` and `* g` stay as written,
  because one record field cannot hold an all-or-nothing group.
- **Tuple element.** A keyless entry in an array (`[g, M]`) is one positional
  element. It is not spliced.
- **Socket and plug.** `$name` and `$$name` refer to the rule `name`. CSIL has
  no syntax yet to add alternatives to a socket (`$name /= ...`).
- **Unfilled socket.** A `$name` with no rule `name` accepts any value
  (`any`). A `$$name` with no rule `name` adds no entries.

### Prelude constants used as types

The CDDL prelude defines some constants as types. A field of one of these
types holds only that constant.

| CSIL type          | CBOR                          |
| ------------------ | ----------------------------- |
| `true`             | simple value 21 (`0xf5`)      |
| `false`            | simple value 20 (`0xf4`)      |
| `null` / `nil`     | simple value 22 (`0xf6`)      |
| `undefined`        | written as `0xf6`; `0xf7` is read as null |

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
- **Integer out of range for the target type.** A decoder never changes an
  integer into a different value. If the target type cannot hold the value,
  decoding fails. For example, a signed 64-bit `int` refuses a major type 0
  argument of 2^63 or more, and a major type 1 argument of 2^63 or more.
  (Java keeps a `uint` in a `long` as unsigned bits. That is not a wrap,
  because it encodes back to the same value.)
- **Wrong or missing semantic tag** (`date` requires tag 1004, `timestamp` requires tag 0, `cddl_time` requires tag 1, and `decimal` requires tag 4).
- **Enum value not in the declared literal vocabulary** (see "Choices" above).
- **Union `variant_index` out of range, or a literal arm's payload not equal to
  the literal it is declared to be.**
- **Group/record missing a required (non-`?`) field**, or the wrong CBOR shape
  for the map/array.
- **A key that occurs two times** in a record or a dynamic map.
- **Dynamic map keys not in canonical order** (see "Dynamic map key order and
  duplicate keys").

This holds across all 14 code-generating targets — the point of the
per-generator codec architecture (see "Map keys are the CSIL field names,
verbatim" above) is that this validation is emitted **once, in generated code,
at the boundary**, rather than relied on ambiently from a runtime
reflection/derive library that may or may not enforce it.

### Decoder resource limits

A decoder reads input from a network peer. The input can be hostile. Every
generated decoder and every transport decoder MUST obey these limits:

- **Declared length against remaining input.** Before you convert or allocate
  for a declared array, map, byte-string, or text-string length, compare the
  length with the bytes that remain. If the length is larger, return an error.
  Each item uses a minimum of one byte, so this bound is exact.
- **Reservation clamp.** Do not reserve collection storage for more than
  **1024** elements before you read them. One input byte can become a much
  larger in-memory value. If a decoder reserves the full declared length, a
  small frame can reserve a large multiple of its size at each nesting level.
  After 1024 elements, grow the storage only as you read elements. A bump arena
  cannot resize in place, so copy into a larger block and double its size.
- **Nesting depth.** Return an error for input that has more than 64 nested
  values.
- **Text.** Return an error for a text string that is not valid UTF-8.

A frame that contains N elements still decodes to N in-memory values. The
carrier's maximum frame size is the limit for that cost (see
`csil-transport-conventions.md`, section 5).

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

For a unary operation such as `Op: Req -> Res / Refusal / Retry`, the first
type is the success type. Each subsequent type is an application error. Use
the declared type name as the CSIL-RPC response `variant`. Set `status` to
zero and encode the selected value directly in `payload` for each arm.
Receivers must reject a missing or unknown variant for these operations.
A nonzero status reports a transport failure.

To return several successful alternatives, define a named union such as
`Reply = A / B` and declare `Op: Req -> Reply`. Its payload uses the standard
union encoding `[index, value]`. Text-literal enums retain their bare text
encoding.

For an existing inline unary choice, upgrade both peers together. Older
generators can encode the choice index inside the payload.
