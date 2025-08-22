# Basic Usage Example

This example demonstrates the fundamental concepts of CSIL (CBOR Service Interface Language), showing how to define simple services using CDDL types plus CSIL service extensions.

## What's Demonstrated

- **CDDL Foundation**: Basic types (int, text, bool), groups, and validation constraints
- **CSIL Services**: Service definitions with operations that have inputs, outputs, and error handling
- **Operation Types**:
  - Simple request/response (`get-user: UserID -> User`)
  - Error handling (`create-user: CreateUserRequest -> User / UserError`)
  - Bidirectional operations (`subscribe-user-updates: UserID <-> UserUpdate`)

## Try It Out

```bash
# From the csilgen root directory

# Validate the CSIL file
cargo run -p csilgen validate --input examples/basic-usage/simple-service.csil

# Generate code (generators are in development)
cargo run -p csilgen generate --input examples/basic-usage/simple-service.csil --target rust --output examples/basic-usage/generated/

# Format the file
cargo run -p csilgen format examples/basic-usage/ --dry-run

# Lint the file
cargo run -p csilgen lint examples/basic-usage/
```

## Key Concepts

1. **CDDL Types**: The foundation - `UserID = int`, `Username = text .size (3..50)`
2. **CDDL Groups**: Structured data - `User = { id: UserID, username: Username, ... }`
3. **CSIL Services**: Operations - `service UserService { get-user: UserID -> User }`
4. **Error Handling**: Operations can fail - `create-user: CreateUserRequest -> User / UserError`
5. **Bidirectional**: Real-time operations - `subscribe-user-updates: UserID <-> UserUpdate`

This serves as the foundation for understanding more complex CSIL features demonstrated in other examples.