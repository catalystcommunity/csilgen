# csilgen-go-generator

The built-in Go target. A `cdylib` that produces `csilgen_go_generator.wasm` and serves `--target go`.

## Features

- Go struct generation from CSIL groups, including optional-field pointer handling, arrays, maps, and choice/enum types.
- Service interface generation with the per-direction emission model (see below) — `->`, `<->`, and `<-` all supported.
- JSON tag generation with `@send-only` / `@receive-only` field-visibility handling.
- Optional `Validate()` method generation from CSIL constraints (`@min-length`, `@max-length`, etc.).
- Field metadata: `@description` becomes Go doc comments, `@bidirectional` (the *field visibility* annotation) keeps the field in both send and receive payloads.

## What it emits for each service-operation direction

The Go generator follows the cross-generator handler+router model (see `csil-spec.md` "Operation Directions"). Generators emit only typed shapes and routing — never the wire. The implementer wires those to their connection (WebSocket / TCP / whatever).

| Direction | Generated artifact (server side) |
|---|---|
| `->` | A method on the service interface: `Method(ctx, req Input) (Output, error)` |
| `<->` | A fire-and-forget inbound method on the service interface: `Method(ctx, msg Input) error`; plus `Route<Service>Channel` (dispatch by wire method name) and `Encode<Service><Method>(codec Codec, msg Output)` (returns `(string, []byte, error)`) |
| `<-` | Only `Encode<Service><Method>` for the server's outbound `Output`. No inbound method, no router case. |

A `Codec` interface (`Encode(value any) ([]byte, error)`, `Decode(data []byte, out any) error`) is emitted once per services file when any channel ops exist.

## Generated file layout

```
types.gen.go         # struct definitions
services.gen.go      # service interfaces, Codec, routers, encoders
validation.gen.go    # Validate() methods (when constraints present)
constructors.gen.go  # Constructors for defaulted fields (opt-in)
```

## Example

CSIL input:

```csil
User = {
    ;;; Unique user identifier.
    id: uint @receive-only,
    ;;; User's display name.
    name: text @bidirectional @min-length(1) @max-length(100),
    email: text @send-only,
}

CreateUserRequest = { name: text @min-length(1), email: text }

service UserAPI {
    create-user: CreateUserRequest -> User,
    get-user:    uint -> User,
    subscribe:   uint <-> User,            ;; bidirectional
}
```

`services.gen.go` (excerpt):

```go
type Codec interface {
    Encode(value any) ([]byte, error)
    Decode(data []byte, out any) error
}

type UserAPI interface {
    CreateUser(ctx context.Context, req CreateUserRequest) (User, error)
    GetUser(ctx context.Context, req uint64) (User, error)
    Subscribe(ctx context.Context, msg uint64) error  // <-> inbound is fire-and-forget
}

func RouteUserAPIChannel(handlers UserAPI, ctx context.Context, codec Codec, method string, data []byte) error {
    switch method {
    case "Subscribe":
        var msg uint64
        if err := codec.Decode(data, &msg); err != nil { return err }
        return handlers.Subscribe(ctx, msg)
    default:
        return fmt.Errorf("unknown channel method %q", method)
    }
}

func EncodeUserAPISubscribe(codec Codec, msg User) (string, []byte, error) {
    data, err := codec.Encode(msg)
    if err != nil { return "", nil, err }
    return "Subscribe", data, nil
}
```

## Configuration Options

Set inside the CSIL `options { … }` block; the CLI does **not** accept `--option` flags.

| Option | Type | Default | Description |
|---|---|---|---|
| `package_name` | string | `"api"` | Go package name (overridden by `go_package`'s last path segment if both are set) |
| `go_package` | string | `nil` | Full Go import path; the last `/`-segment becomes the package name |
| `go_module` | string | `nil` | When set together with `go_package`, the relative remainder becomes the output subdirectory inside `--output` |
| `use_json_tags` | bool | `true` | Emit `json:"…"` struct tags |
| `use_yaml_tags` | bool | `false` | Also emit `yaml:"…"` struct tags |
| `generate_validation` | bool | `true` | Emit `Validate()` methods from constraints |
| `generate_constructors` | bool | `false` | Emit `NewT()` constructors that wire up default values |
| `decimal_mapping` | string | `"csil"` | In-memory type for `decimal`: `"csil"` emits a self-contained `CsilDecimal`; `"library"` uses `github.com/shopspring/decimal`. Any other value is a hard error |
| `go_imports` | array of strings | `[]` | Extra `import` paths to inject |

### Core types: `timestamp` and `decimal`

`timestamp` maps to `time.Time` (kept in UTC) and encodes as CBOR tag 0 (RFC 3339);
`time` is imported automatically wherever a timestamp appears. `decimal` is the
exact, base-10 CBOR tag-4 decimal fraction `[exponent, mantissa]`. Under the
default `decimal_mapping: "csil"` a single self-contained `CsilDecimal` helper is
emitted to `csil_decimal.gen.go` **only when the spec actually uses `decimal`**; it
(de)serializes the tag-4 wire form and bridges to/from `shopspring` via
`String()`/`ParseCsilDecimal` without depending on it. Under `"library"` the field
is `decimal.Decimal` and `github.com/shopspring/decimal` is imported instead.

### Constraints

Both constraint systems feed the same `Validate()` method: the `@`-annotations
(`@min_length`, `@max_value`, `@regex`, …) and the `.`-control-operators carried
inline on a field's type (`.size`, `.ge`/`.le`/`.gt`/`.lt`/`.eq`/`.ne`, `.regex`).
`.default` is applied by the constructor. Encoding-only operators
(`.json`/`.cbor`/`.cborseq`/`.bits`/`.and`/`.within`) are documented but never
produce a runtime check. `regexp` is imported only when a pattern check is emitted.

## Building & Installing

This generator is bundled into the `cargo run -p xtask install-wasm` flow alongside the other built-in targets, which installs `csilgen_go_generator.wasm` to `~/.csilgen/generators/`. The dynamic-discovery layer in the runtime resolves `--target go` to this generator from its filename.

For a manual build:

```bash
cargo build --target wasm32-unknown-unknown --release -p csilgen-go-generator
cp target/wasm32-unknown-unknown/release/csilgen_go_generator.wasm ~/.csilgen/generators/

csilgen generate --input api.csil --target go --output ./generated/
```
