# Tagged core types & CBOR constraints

`orders.csil` demonstrates the two tagged CSIL core types and the full constraint
surface, generated consistently across every target.

## The tagged types

| CSIL        | CBOR     | Meaning                                              |
| ----------- | -------- | --------------------------------------------------- |
| `timestamp` | tag 0    | RFC3339 text instant, **always serialized in UTC**  |
| `decimal`   | tag 4    | exact base-10 value (decimal fraction), never a float |

The normative wire form is in [`docs/cbor-wire-contract.md`](../../docs/cbor-wire-contract.md).

## Constraints

Both constraint systems are honored by every generator and compose freely:

- **Control operators** (`.`-form): `.size`, `.regex`, `.default`,
  `.ge`/`.le`/`.gt`/`.lt`/`.eq`/`.ne`, `.bits`, `.and`, `.within`, `.json`,
  `.cbor`, `.cborseq`.
- **`@`-annotations**: `@min-length`/`@max-length`, `@min-items`/`@max-items`,
  `@min-value`/`@max-value`.

Bounds on a `decimal` or `timestamp` are written as text so no precision is lost
(`unit_price: decimal .ge "0.00"`, `not_before: timestamp .ge "2000-01-01T00:00:00Z"`).
Code generators emit type-correct comparisons (e.g. `Decimal("0.00")`, `new Date(...)`);
schema generators carry string-typed bounds as `x-csil-minimum` / `x-csil-maximum`
vendor extensions (a numeric `minimum`/`maximum` only applies to numeric fields).

## Choosing how `decimal` maps

The `options { decimal_mapping: ... }` block selects the in-memory decimal type.
The CBOR wire form (tag 4) is identical either way.

| value (default `"csil"`) | Rust                    | Go                              | TypeScript            | Python            |
| ------------------------ | ----------------------- | ------------------------------- | --------------------- | ----------------- |
| `"csil"`                 | generated `CsilDecimal` | generated `CsilDecimal`         | generated `CsilDecimal` | `decimal.Decimal` |
| `"library"`              | `rust_decimal::Decimal` | `shopspring/decimal.Decimal`    | `Decimal` (decimal.js) | `decimal.Decimal` |

The generated `CsilDecimal` is self-contained (no third-party dependency) and is
losslessly convertible to/from the popular library via its string form, so you can
start with `"csil"` and adopt a library later without changing the wire format.

## Generate

```sh
# default: generated CsilDecimal helper
csilgen generate --input orders.csil --target rust --output ./gen/rust

# map decimal straight to the language library instead (edit the options block,
# or keep a second spec) — here rust_decimal / shopspring / decimal.js
csilgen generate --input orders.csil --target go --output ./gen/go
```

Targets: `rust`, `go`, `typescript`, `python`, `json`, `openapi` (plus their
sub-targets, e.g. `rust-client`, `typescript-server`).
