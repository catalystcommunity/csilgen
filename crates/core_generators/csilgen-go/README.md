# CSIL Go Generator Example

This example demonstrates how to create a fully functional CSIL generator that produces Go code with struct definitions and service interfaces.

## Features

- ✅ Go struct generation from CSIL groups
- ✅ Service interface generation
- ✅ JSON tags with field visibility support
- ✅ Input validation generation
- ✅ Bidirectional streaming support
- ✅ Complete field metadata processing

## Generated Code Structure

The generator produces three main files:

### `types.go` - Data Type Definitions
```go
package api

import (
    "encoding/json"
)

// User represents a structured data type
type User struct {
    // User's display name
    Name string `json:"name"`
    // User's email address (send-only)
    Email string `json:"-" # send-only`
    // Unique user identifier
    ID uint64 `json:"id"`
}
```

### `services.go` - Service Interface Definitions
```go
package api

import (
    "context"
)

// UserAPI defines the service interface
type UserAPI interface {
    CreateUser(ctx context.Context, req CreateUserRequest) (User, error)
    GetUser(ctx context.Context, req uint64) (User, error)
    UpdateUserStream(ctx context.Context) (UpdateUserStream, error)
}

// UpdateUserStream handles bidirectional streaming
type UpdateUserStream interface {
    Send(User) error
    Recv() (User, error)
    Close() error
}
```

### `validation.go` - Input Validation
```go
package api

import (
    "errors"
    "fmt"
)

// ValidateUser validates the User struct
func (v *User) Validate() error {
    if len(v.Name) < 1 {
        return fmt.Errorf("field 'Name' must have at least 1 characters")
    }
    if len(v.Name) > 100 {
        return fmt.Errorf("field 'Name' must have at most 100 characters")
    }
    return nil
}
```

## Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `package_name` | string | "api" | Go package name |
| `use_json_tags` | boolean | true | Generate JSON struct tags |
| `generate_validation` | boolean | true | Generate validation methods |

## Example CSIL Input

```csil
User = {
    id: uint @receive-only @description("Unique user identifier"),
    name: text @bidirectional @min-length(1) @max-length(100) @description("User's display name"),
    email: text @send-only @description("User's email address"),
}

CreateUserRequest = {
    name: text @min-length(1),
    email: text,
}

service UserAPI {
    create-user: CreateUserRequest -> User
    get-user: uint -> User
    update-user: User <-> User
}
```

## Usage

```bash
# Build the generator
cargo build --target wasm32-unknown-unknown --release

# Use with csilgen CLI
csilgen generate --input api.csil --target go-generator --output ./generated/ \
  --option package_name=myapi \
  --option use_json_tags=true
```

## Key Implementation Features

### Type Mapping
- Maps CDDL types to appropriate Go types
- Handles optional fields with pointers
- Supports arrays and maps
- Custom type references

### Service Generation  
- Creates Go interfaces for services
- Handles unidirectional operations as methods
- Bidirectional operations become streaming interfaces
- Reverse operations become event handlers

### Field Metadata Processing
- `@send-only`: Excludes field from JSON serialization
- `@receive-only`: Includes helpful comments
- `@description`: Generates Go comments
- `@min-length/@max-length`: Creates validation logic

### Validation Generation
- Generates `Validate()` methods for structs
- Implements constraint checking
- Returns descriptive error messages
- Optional feature via configuration

This example demonstrates the complete workflow of building a production-ready CSIL generator with comprehensive feature support.