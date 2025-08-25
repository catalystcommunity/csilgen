# CBOR Service Interface Language (CSIL) Specification

## Version 1.0 (Alpha)

## Table of Contents

1. [Introduction](#introduction)
2. [Language Overview](#language-overview)
3. [CDDL Compatibility](#cddl-compatibility)
4. [Core Syntax](#core-syntax)
5. [Type System](#type-system)
6. [Control Operators](#control-operators)
7. [Metadata Annotations](#metadata-annotations)
8. [Service Definitions](#service-definitions)
9. [Import System](#import-system)
10. [File Options](#file-options)
11. [Code Generation](#code-generation)
12. [Examples](#examples)
13. [Best Practices](#best-practices)
14. [Future Extensions](#future-extensions)

## Introduction

CBOR Service Interface Language (CSIL) is an interface definition language that extends the Concise Data Definition Language (CDDL) as defined in RFC 8610. CSIL maintains (or should eventually) strict superset compatibility with CDDL while adding service-oriented features for modern API development, including service definitions, field metadata, visibility controls, and enhanced code generation capabilities.

### Design Goals

1. **CDDL Compatibility**: Every valid CDDL file is a valid CSIL file
2. **Service-Oriented**: First-class support for defining services and operations
3. **Metadata-Rich**: Comprehensive field annotations for validation, visibility, and dependencies
4. **Code/Doc Generation**: Designed for generating idiomatic code in multiple languages or documentation targets
5. **Type Safety**: Strong typing with extensive validation constraints
6. **Modularity**: Multi-file support with imports and namespacing

## Language Overview

CSIL files use the `.csil` extension and follow CDDL's syntax with extensions. Comments use semicolons:

```csil
;; Single-line comments start with double semicolons
; Single semicolon comments are also supported

;; Basic type definition (pure CDDL)
UserID = int

;; Type with constraints (CDDL control operators)
Username = text .size (3..50)

;; Group definition with metadata (CSIL extension)
User = {
    @description("Unique user identifier")
    @receive-only
    id: UserID,
    
    username: Username,
    email: text,
    ? active: bool .default true
}

;; Service definition (CSIL extension)
service UserService {
    get-user: UserID -> User,
    create-user: User -> UserID / Error
}
```

## CDDL Compatibility

CSIL is a strict superset of CDDL as defined in RFC 8610. All CDDL features are supported:

### Supported CDDL Features

1. **Basic Types**: `int`, `uint`, `float`, `text`, `bytes`, `bool`, `nil`, `any`
2. **Type Definitions**: `name = type`
3. **Group Definitions**: `name = { ... }`
4. **Arrays**: `[* type]`, `[+ type]`, `[? type]`
5. **Maps**: `{ * key => value }`
6. **Choices**: `type1 / type2 / type3`
7. **Ranges**: `1..10`, `1...10`
8. **Occurrences**: `?` (optional), `*` (zero or more), `+` (one or more), `n*m` (range)
9. **Type Choices**: `name /= type`
10. **Group Choices**: `name //= group`
11. **Control Operators**: `.size`, `.regex`, `.default`, `.ge`, `.le`, `.gt`, `.lt`, `.eq`, `.ne`
12. **Socket/Plug**: `$name`, `$$name` (extensibility points)

### CDDL Syntax Examples

```csil
;; Basic types
age = int
name = text
data = bytes
flag = bool
nothing = nil
anything = any

;; Constrained types
port = uint .le 65535
email = text .regex "^[\\w.-]+@[\\w.-]+\\.\\w+$"
password = text .size (8..100)

;; Groups (objects/maps)
person = {
    name: text,
    age: int .ge 0 .le 120,
    ? email: text,      ;; optional field
    * text => any       ;; additional properties
}

;; Arrays
numbers = [* int]           ;; array of integers
non_empty = [+ text]        ;; non-empty array of strings
triple = [3*3 float]        ;; exactly 3 floats
ranged = [1*10 person]      ;; 1 to 10 persons

;; Choices
status = "active" / "inactive" / "pending"
result = int / text / bool

;; Type augmentation
base_user = {
    id: int,
    name: text
}

base_user //= {
    email: text
}

;; Socket and plug for extensibility
config = {
    name: text,
    $extensions        ;; socket for extensions
}

my_config = {
    $$config,         ;; plug into config
    custom: int
}
```

## Core Syntax

### Lexical Structure

CSIL uses UTF-8 encoding and follows CDDL's lexical conventions:

- **Identifiers**: Start with letter or underscore, contain letters, digits, underscores, hyphens
- **Keywords**: Reserved words include CDDL keywords plus CSIL extensions
- **Comments**: `;` or `;;` to end of line
- **Whitespace**: Spaces, tabs, newlines are generally insignificant
- **String literals**: Double-quoted with escape sequences
- **Numeric literals**: Integers, floats, hexadecimal, binary

### Grammar Overview

```ebnf
csil_spec = *import_statement [options_block] *rule

import_statement = include_import / selective_import
include_import = "include" text ["as" identifier]
selective_import = "from" text "include" identifier *("," identifier)

options_block = "options" "{" *option_entry "}"
option_entry = identifier ":" literal

rule = identifier "=" type_expr
     / identifier "/=" type_expr    ; type choice
     / identifier "//=" group_expr  ; group choice
     / service_definition

service_definition = "service" identifier "{" *operation "}"
operation = identifier ":" type_expr direction type_expr
direction = "->" / "<-" / "<->"
```

## Type System

### Primitive Types

CSIL inherits all CDDL primitive types:

| Type | Description | Example |
|------|-------------|---------|
| `int` | Signed integer | `42`, `-17` |
| `uint` | Unsigned integer | `42`, `0` |
| `float` | Floating point | `3.14`, `-0.5` |
| `text` | UTF-8 string | `"hello"` |
| `bytes` | Byte string | `h'DEADBEEF'` |
| `bool` | Boolean | `true`, `false` |
| `nil` | Null value | `nil` |
| `any` | Any valid CBOR | - |

### Composite Types

#### Groups (Objects/Records)

Groups define structured data with named fields:

```csil
User = {
    id: int,
    username: text,
    email: text,
    ? profile: Profile,      ;; optional field
    * text => any           ;; additional properties
}
```

#### Arrays

Arrays are ordered sequences of elements:

```csil
;; Array of integers
numbers = [* int]

;; Non-empty array
required_items = [+ text]

;; Fixed size array
coordinate = [2*2 float]  ;; exactly 2 floats

;; Array with mixed types
mixed = [int, text, bool]
```

#### Maps

Maps are key-value collections:

```csil
;; String to integer map
scores = {* text => int}

;; Typed key-value pairs
config = {* ConfigKey => ConfigValue}
```

#### Choices (Unions)

Choices represent alternative types:

```csil
;; Simple choice
status = "pending" / "approved" / "rejected"

;; Type union
value = int / float / text / bool

;; Complex choice
response = Success / Error / Redirect
```

## Control Operators

Control operators constrain types with additional validation rules:

### Size Constraints

```csil
;; Exact size
username = text .size 10

;; Size range
password = text .size (8..100)

;; Minimum size
description = text .size (10..)

;; Maximum size  
summary = text .size (..500)
```

### Value Constraints

```csil
;; Comparison operators
age = int .ge 0 .le 120       ;; 0 <= age <= 120
priority = int .gt 0 .lt 10    ;; 0 < priority < 10
exact = int .eq 42             ;; must equal 42
not_zero = int .ne 0           ;; must not equal 0

;; Multiple constraints can be chained
score = float .ge 0.0 .le 100.0
```

### Pattern Matching

```csil
;; Regular expression
email = text .regex "^[\\w.-]+@[\\w.-]+\\.\\w+$"
phone = text .regex "^\\+?[1-9]\\d{1,14}$"

;; Multiple patterns (must match all)
identifier = text .size (3..20) .regex "^[a-zA-Z][a-zA-Z0-9_]*$"
```

### Default Values

```csil
User = {
    name: text,
    ? active: bool .default true,
    ? role: text .default "user",
    ? timeout: int .default 30
}
```

### Encoding Constraints (Planned)

```csil
;; JSON-specific encoding
json_data = text .json

;; CBOR-specific encoding
cbor_data = bytes .cbor

;; CBOR sequence
cbor_seq = bytes .cborseq
```

## Metadata Annotations

CSIL extends CDDL with rich metadata annotations for fields and types. Annotations start with `@` and appear before field definitions.

### Visibility Annotations

Control field visibility in different contexts:

```csil
User = {
    @description("Internal database ID")
    @receive-only
    id: int,
    
    @description("Username chosen by user")
    @send-only
    username: text,
    
    @description("Email for notifications")
    @bidirectional  ;; default, can be omitted
    email: text,
    
    @description("Admin notes")
    @admin-only  ;; custom visibility
    ? notes: text
}
```

### Validation Annotations

Additional validation constraints beyond control operators:

```csil
Product = {
    @min-value(0)
    @max-value(1000000)
    price: int,
    
    @min-length(10)
    @max-length(1000)
    description: text,
    
    @min-items(1)
    @max-items(10)
    categories: [* text],
    
    @description("Stock must be non-negative")
    @min-value(0)
    stock: int
}
```

### Dependency Annotations

Express field dependencies and conditional requirements:

```csil
Document = {
    content_type: text,
    
    @depends-on(content_type = "text/plain")
    ? text_content: text,
    
    @depends-on(content_type = "application/pdf")
    ? pdf_content: bytes,
    
    @depends-on(content_type = "image/jpeg")
    ? image_data: bytes
}

Payment = {
    method: "card" / "bank" / "crypto",
    
    @depends-on(method = "card")
    ? card_number: text,
    
    @depends-on(method = "bank")
    ? account_number: text,
    
    @depends-on(method = "crypto")
    ? wallet_address: text
}
```

### Documentation Annotations

Human-readable descriptions for generated documentation:

```csil
@description("User account information")
User = {
    @description("Unique identifier assigned by the system")
    id: int,
    
    @description("Display name shown to other users")
    name: text,
    
    @description("Contact email (verified)")
    email: text
}
```

### Custom Annotations

Extensible metadata for generator or context specific hints, such as tags that should be in generator outputs:

```csil
User = {
    @custom-validator("checkUsername")
    username: text,
    
    @db-index(unique: true)
    email: text,
    
    @json-name("created_at")
    @xml-attribute
    created: int
}
```

## Service Definitions

CSIL's primary extension over CDDL is first-class service definitions for RPC and API specifications.

### Basic Service Syntax

```csil
service ServiceName {
    operation-name: InputType -> OutputType,
    another-operation: Request -> Response / Error
}
```

### Operation Directions

Three types of operation directions, which make it useful for protocols other than HTTPS:

```csil
service ExampleService {
    ;; Unidirectional (request-response)
    get-data: Request -> Response,
    
    ;; Bidirectional (streaming/real-time)
    subscribe: Topic <-> Update,
    
    ;; Reverse (rarely used, for callbacks)
    notify: Event <- Acknowledgment
}
```

### Error Handling

Operations can return multiple possible types using choices:

```csil
service UserService {
    ;; Success or error
    create-user: CreateRequest -> User / ValidationError / ServerError,
    
    ;; Multiple success types
    get-user: UserID -> User / UserProfile / NotFound,
    
    ;; Optional response
    delete-user: UserID -> Success / Error
}
```

### Complex Service Example

```csil
;; E-commerce order service
service OrderService {
    ;; Query operations
    get-order: OrderID -> Order / NotFound,
    list-orders: ListRequest -> OrderList / Error,
    search-orders: SearchQuery -> SearchResults / Error,
    
    ;; Mutations
    create-order: CreateOrderRequest -> Order / ValidationError / Error,
    update-order: UpdateOrderRequest -> Order / ValidationError / NotFound,
    cancel-order: CancelRequest -> CancelResponse / Error,
    
    ;; Real-time subscriptions
    watch-order: OrderID <-> OrderUpdate / Error,
    subscribe-events: EventFilter <-> OrderEvent
}
```

### Service Composition

Services can be modular and reference shared types:

```csil
;; Common types
include "common/types.csil"
include "common/errors.csil"

;; Authentication service
service AuthService {
    login: Credentials -> Session / AuthError,
    logout: SessionID -> Success / Error,
    refresh: RefreshToken -> Session / AuthError
}

;; User service (uses auth types)
service UserService {
    @auth-required
    get-profile: SessionID -> UserProfile / AuthError / NotFound,
    
    @auth-required  
    update-profile: UpdateProfileRequest -> UserProfile / AuthError / ValidationError
}
```

## Import System

CSIL supports modular specifications through imports.

### Include Imports

Basic file inclusion:

```csil
;; Simple include
include "types/user.csil"
include "types/product.csil"

;; Include with alias to avoid naming conflicts
include "v1/api.csil" as v1
include "v2/api.csil" as v2

;; Use aliased types
user_v1 = v1.User
user_v2 = v2.User
```

### Selective Imports

Import specific types from a file:

```csil
;; Import only what you need
from "common/types.csil" include UserID, ProductID, OrderID
from "common/errors.csil" include ValidationError, NotFound

;; The imported types are now available
order = {
    id: OrderID,
    user_id: UserID,
    product_id: ProductID
}
```

### Import Resolution

1. Relative paths are resolved from the current file's directory
2. Absolute paths are resolved from the project root
3. Circular imports are detected and reported as errors
4. Import cycles must be broken using forward declarations

### Multi-file Example

```csil
;; File: types/base.csil
ID = text .size (10..20)
Timestamp = int .ge 0

;; File: types/user.csil
include "base.csil"

User = {
    id: ID,
    username: text,
    created: Timestamp
}

;; File: services/user-service.csil
include "../types/user.csil"

service UserService {
    get-user: ID -> User
}
```

## File Options

CSIL files can include an options block for file-level configuration:

```csil
options {
    package: "com.example.api",
    version: "1.0.0",
    namespace: "UserAPI",
    go_package: "github.com/example/api/user"
}

;; Rest of the CSIL definitions
User = { ... }
```

Common options:
- `package`: Package/module name for generated code
- `version`: API version
- `namespace`: Namespace for generated types
- `*_package`: Language-specific package names
- Custom generator-specific options

## Code Generation

CSIL is designed for code generation across multiple target languages.

### Generator Capabilities

Generators can produce:

1. **Data Models**: Classes, structs, interfaces for types
2. **Serialization**: JSON, CBOR, Protocol Buffers, etc.
3. **Validation**: Runtime validation based on constraints
4. **Client SDKs**: Service clients with type safety
5. **Server Stubs**: Service interfaces and base implementations
6. **Documentation**: API docs, OpenAPI/Swagger specs
7. **Schema**: JSON Schema, XML Schema, etc.

### Language Support

Current and planned language targets:

- **Rust**: Structs with serde
- **TypeScript**: Interfaces and types with validation
- **Python**: Dataclasses with pydantic
- **Go**: Structs with tags
- **Java**: POJOs with annotations
- **C#**: Classes with attributes
- **OpenAPI**: OpenAPI 3.0 specifications
- **JSON Schema**: JSON Schema draft 7+

### Generation Examples

```bash
# Generate Rust code
csilgen generate --input api.csil --target rust --output ./src/generated/

# Generate TypeScript with validation
csilgen generate --input api.csil --target typescript --validation --output ./src/api/

# Generate OpenAPI specification
csilgen generate --input api.csil --target openapi --output ./docs/api.yaml

# Generate multiple targets
csilgen generate --input api.csil --target rust,typescript,python --output ./generated/
```

### Generator Directives

Control code generation with annotations:

```csil
@rust-derive("Debug, Clone, Serialize, Deserialize")
@typescript-readonly
User = {
    @rust-skip
    @typescript-type("string | number")
    id: ID,
    
    @python-field(validator="validate_email")
    email: text
}
```

## Examples

### Simple Data Model

```csil
;; Basic user model
UserID = int .ge 1
Email = text .regex "^[\\w.-]+@[\\w.-]+\\.\\w+$"

User = {
    id: UserID,
    email: Email,
    name: text .size (1..100),
    ? age: int .ge 0 .le 120,
    ? tags: [* text]
}
```

### REST API

```csil
;; RESTful CRUD API
service UserAPI {
    ;; GET /users
    list-users: ListRequest -> UserList,
    
    ;; GET /users/{id}
    get-user: UserID -> User / NotFound,
    
    ;; POST /users
    create-user: CreateUserRequest -> User / ValidationError,
    
    ;; PUT /users/{id}
    update-user: UpdateUserRequest -> User / ValidationError / NotFound,
    
    ;; DELETE /users/{id}
    delete-user: UserID -> Success / NotFound
}

ListRequest = {
    ? offset: int .default 0,
    ? limit: int .default 20 .le 100,
    ? sort: "name" / "created" / "email"
}

UserList = {
    users: [* User],
    total: int .ge 0,
    ? next_offset: int
}
```

### Real-time Messaging

```csil
;; WebSocket/streaming API
service ChatService {
    ;; Join a chat room
    join-room: JoinRequest -> JoinResponse / RoomError,
    
    ;; Bidirectional message stream
    messages: Message <-> Message,
    
    ;; Real-time notifications
    notifications: NotificationRequest <-> Notification
}

Message = {
    @send-only
    ? content: text .size (1..1000),
    
    @receive-only
    ? id: text,
    
    @receive-only
    ? timestamp: int,
    
    @receive-only
    ? author: text,
    
    room_id: text
}
```

### Complex Validation

```csil
;; Payment processing with complex validation
PaymentMethod = "card" / "bank" / "paypal" / "crypto"

Payment = {
    amount: int .gt 0,
    currency: text .size (3..3),
    method: PaymentMethod,
    
    @depends-on(method = "card")
    ? card: CardDetails,
    
    @depends-on(method = "bank")
    ? bank: BankDetails,
    
    @depends-on(method = "paypal")  
    ? paypal_email: Email,
    
    @depends-on(method = "crypto")
    ? crypto: CryptoDetails
}

CardDetails = {
    @description("Card number without spaces")
    @min-length(13)
    @max-length(19)
    number: text .regex "^[0-9]+$",
    
    @description("MM/YY format")
    expiry: text .regex "^(0[1-9]|1[0-2])/[0-9]{2}$",
    
    @description("3 or 4 digit CVV")
    cvv: text .regex "^[0-9]{3,4}$",
    
    holder_name: text .size (1..100)
}
```

## Best Practices

### 1. Type Organization

- Define reusable types at the top of files
- Use semantic type names (UserID vs int)
- Group related types together
- Consider creating a `common/types.csil` file for common/universal types

### 2. Constraint Usage

- Apply constraints at the type definition level when possible
- Use `.default` for optional fields with common values
- Combine constraints for complex validation
- Document why constraints exist with `@description`

### 3. Service Design

- Keep operations focused and single-purpose
- Use consistent naming conventions
- Group related operations in the same service
- Design for forward compatibility

### 4. Metadata Best Practices

- Always include `@description` for public APIs
- Use `@receive-only` for server-generated fields like IDs
- Use `@send-only` for client-only input fields
- Document field dependencies with `@depends-on`

### 5. Error Handling

- Define clear error types
- Use choices for multiple possible outcomes
- Include error codes and messages
- Consider versioning error formats

### 6. Modularity

- Split large specifications into multiple files
- Use imports to share common types
- Avoid circular dependencies
- Version your API specifications with Options metadata

### 7. Code Generation

- Test generated code regularly
- Use generator-specific annotations sparingly (describe for posterity, not just one tool)
- Maintain backward compatibility
- Document breaking changes

### 8. Documentation

- Comment complex type definitions
- Explain business logic in descriptions
- Provide examples in comments
- Maintain a changelog

## Future Extensions

Planned features for future CSIL versions:

### Advanced Control Operators

- `.ne` (not equal constraint)
- `.bits` (bit-level constraints)
- `.and` (type intersection)
- `.within` (subset constraints)
- `.json`, `.cbor`, `.cborseq` (encoding specifications)

### Enhanced Service Features

- Service inheritance and composition
- Middleware/interceptor definitions
- Rate limiting annotations
- Authentication/authorization directives
- Service versioning

### Type System Extensions

- Generic types with parameters
- Type aliases with constraints
- Enum types with values
- Discriminated unions
- Recursive type definitions

### Validation Extensions

- Cross-field validation rules
- Custom validation functions
- Context-aware validation

### Generator Improvements

- Incremental generation
- Custom generator plugins
- Template-based generation
- Multi-version support

### Tooling

- Language server protocol (LSP) support
- IDE plugins and extensions
- Interactive documentation
- Migration tools

## Conclusion

CSIL provides a powerful, extensible interface definition language that builds upon CDDL's solid foundation. By adding service definitions, metadata annotations, and enhanced code generation capabilities while maintaining CDDL compatibility, CSIL enables teams to define, validate, and generate type-safe APIs across multiple languages, protocols, and platforms.

The language continues to evolve based on real-world usage and community feedback, with a focus on maintaining backward compatibility while adding powerful new features for modern API development.

## References

- [RFC 8610](https://datatracker.ietf.org/doc/html/rfc8610) - Concise Data Definition Language (CDDL)
- [RFC 7049](https://datatracker.ietf.org/doc/html/rfc7049) - Concise Binary Object Representation (CBOR)
- [CDDL Tool](https://github.com/cbor-wg/cddl) - Reference CDDL implementation
- [CSIL Repository](https://github.com/catalystcommunity/csilgen) - CSIL implementation and tools