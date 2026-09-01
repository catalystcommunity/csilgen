# CSIL Schema Descriptor `v1alpha1`

## Purpose

Use a CSIL schema descriptor to inspect CSIL payloads without generated code.
The descriptor is data. It does not contain source code.

`csilgen generate` writes one descriptor for each resolved root input. The file
name is `<entry-stem>.csil-schema.cbor`. Use `--no-schema` to stop this output.
This option also removes the descriptor at this exact path.

Use this media type when the server permits it:

```text
application/csil-schema+cbor
```

A consumer must also accept `application/octet-stream`.

## Top-Level Map

The descriptor is one CBOR map. It has these keys:

| Key | CBOR type | Meaning |
| --- | --- | --- |
| `format` | text | The value is `csil-schema`. |
| `version` | text | The value is `v1alpha1`. |
| `digest` | byte string | The value is a 32-byte SHA-256 digest. |
| `body` | map | The value is the semantic schema body. |

Reject a descriptor when `format` is not `csil-schema`. Reject a version that
the consumer does not support. A `v1alpha1` consumer can ignore an unknown map
key.

The `body` map has these keys:

| Key | CBOR type | Meaning |
| --- | --- | --- |
| `root` | text | The root input stem. |
| `rules` | array | All resolved non-service rules in declaration order. |
| `services` | array | All resolved services in declaration order. |

The body does not contain source text, source paths, source positions,
documentation, file options, or target-language configuration.

## Deterministic Encoding and Digest

Use these rules to encode the descriptor and the body:

1. Use the shortest CBOR integer encoding.
2. Use the shortest IEEE 754 width that keeps the exact float value and sign.
3. Sort each map by the lexicographic order of the encoded key bytes.
4. Keep each array in the order that this document specifies.
5. Do not use indefinite-length values.

To calculate the digest, make a body map that has exactly `root`, `rules`, and
`services`. Encode this map with these rules. Calculate SHA-256 from these bytes.
Do not include `format`, `version`, `digest`, or an unknown optional field in the
digest input.

The same resolved input gives the same body and digest for all generator
targets.

## Common Data Rules

A structure is a CBOR map. Its field names are the map keys in the tables in
this document. An optional value is the field value or CBOR `null`.

An enum with data is a one-entry map. The key is the variant name. The value is
the variant data. For example, the CSIL type `text` is:

```text
{ "Builtin": "text" }
```

An enum variant without data is a text value. For example, an optional
occurrence is:

```text
"Optional"
```

Encode a CSIL byte literal as a CBOR byte string. Encode an integer literal as a
CBOR integer. Do not convert a schema value through JSON.

## Rules

Each item in `rules` is a map with `name` and `definition`.

`definition` has one of these variants:

| Variant | Value |
| --- | --- |
| `Type` | A type expression. |
| `Group` | A group. |
| `GroupChoice` | An array of groups in declaration order. |

## Type Expressions

A type expression has one of these variants:

| Variant | Value |
| --- | --- |
| `Builtin` | The CSIL built-in name as text. |
| `Reference` | The resolved rule name as text. |
| `Array` | A map with `element` and `occurrence`. |
| `Tuple` | A group. Entries are in wire order. |
| `Map` | A map with `key`, `value`, and `occurrence`. |
| `Group` | A group. |
| `Choice` | An array of arms in declaration order. |
| `Range` | A map with `start`, `end`, and `inclusive`. |
| `Socket` | The socket rule name as text. |
| `Plug` | The plug rule name as text. |
| `Literal` | A literal value. |
| `Constrained` | A map with `base` and `constraints`. |

The choice array index is the declared arm index. Do not sort this array.

An occurrence has one of these variants:

| Variant | Value |
| --- | --- |
| `Optional` | No data. |
| `ZeroOrMore` | No data. |
| `OneOrMore` | No data. |
| `Exact` | An unsigned integer. |
| `Range` | A map with optional `min` and `max` integers. |

A literal has one of these variants: `Integer`, `Float`, `Text`, `Bytes`,
`Bool`, `Null`, or `Array`. `Null` has no data. `Array` contains literal values.

A control operator has one of these variants: `Size`, `Regex`, `Default`,
`GreaterEqual`, `LessEqual`, `GreaterThan`, `LessThan`, `Equal`, `NotEqual`,
`Bits`, `And`, `Within`, `Json`, `Cbor`, or `Cborseq`.

`Size` has one of these variants:

| Variant | Value |
| --- | --- |
| `Exact` | An unsigned integer. |
| `Range` | A map with `min` and `max`. |
| `Min` | An unsigned integer. |
| `Max` | An unsigned integer. |

## Groups and Fields

A group is a map with an `entries` array. Keep entries in declaration order.
Each entry has these fields:

| Key | Value |
| --- | --- |
| `key` | A group key or `null`. |
| `value` | A type expression. |
| `occurrence` | An occurrence or `null`. |
| `metadata` | An array of wire metadata. |

A group key has one of these variants: `Bare`, `Type`, or `Literal`.

Field metadata has one of these variants:

- `Visibility` with `SendOnly`, `ReceiveOnly`, or `Bidirectional`;
- `DependsOn` with `field` and optional `value`;
- `DependsOnExpr` with a dependency condition; or
- `Constraint` with a validation constraint.

A dependency condition has `Compare`, `All`, or `Any`. `Compare` contains a
field, an optional operation, and an optional literal value. The operation is
`Eq`, `Ne`, `Lt`, `Le`, `Gt`, or `Ge`.

A validation constraint is `MinLength`, `MaxLength`, `MinItems`, `MaxItems`,
`MinValue`, `MaxValue`, or `Custom`. A custom constraint contains `name` and
`value`.

The descriptor does not contain descriptions or custom generator hints.

## Services

Each service is a map with these fields:

| Key | Value |
| --- | --- |
| `name` | The declared service name. |
| `wire_id` | The optional service wire ID. |
| `operations` | The operations in declaration order. |

Each operation is a map with these fields:

| Key | Value |
| --- | --- |
| `name` | The declared operation name. |
| `wire_id` | The optional operation wire ID. |
| `input` | The input type expression. |
| `output` | The output type expression. This value keeps response choices. |
| `direction` | `Unidirectional`, `Bidirectional`, or `Reverse`. |

## Rust Reference API

The `csilgen-schema` crate is the reference implementation. Use
`SchemaDescriptor::decode` to parse and verify a descriptor. Use `unmarshal` for
an in-memory descriptor. Use `unmarshal_descriptor` to parse a descriptor and
inspect a payload in one call.

`RouteContext` supports these routes:

- CSIL-RPC request by service and operation name;
- CSIL-RPC response by service name, operation name, and response variant;
- verbose CSIL-Events by names; and
- compact CSIL-Events by service and operation wire IDs.

An Events route also specifies the input or output side. Each route records
whether the local tool sent or received the message.

`DiagnosticResult` always keeps the raw payload. For valid CBOR, it also keeps a
generic value with byte offsets. It can contain a partial typed value and a list
of diagnostics. A diagnostic contains the schema path, byte offset, expected
shape, and observed shape.

The generic value keeps these distinctions:

- positive and negative integers as `i128`;
- the original float width and IEEE 754 bits;
- text strings and byte strings;
- null and undefined;
- arrays;
- maps as key and value pairs;
- known and unknown semantic tags;
- tag 4 decimals as exact exponent and mantissa integers; and
- tag 0 and tag 1 timestamps with the original tag.

The decoder rejects truncated input, invalid UTF-8, unsafe declared lengths,
trailing bytes, and values deeper than 64 levels.
