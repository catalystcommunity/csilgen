# Complex Metadata Example

This example demonstrates advanced CSIL features including rich field metadata, conditional dependencies, and complex service patterns.

## Advanced Features Demonstrated

### Field Metadata
- **Visibility Controls**: `@send-only`, `@receive-only`, `@bidirectional`
- **Dependencies**: `@depends-on(field = value)` for conditional requirements
- **Validation**: `@min-length`, `@max-length`, `@min-value`, `@max-value`, `@max-items`
- **Documentation**: `@description("...")` for generated docs

### Complex Service Patterns
- **Error Handling**: Operations that can return errors (`-> Document / DocumentError`)
- **Bidirectional Operations**: Real-time updates (`<->` operator)
- **Conditional Logic**: Fields that are required only when other fields have specific values
- **Optimistic Locking**: Version-based conflict resolution

### Validation Constraints
- **Size Limits**: Field length and collection size constraints
- **Regex Validation**: Email format validation, content type validation
- **Range Validation**: Numeric ranges and timestamp constraints
- **Conditional Requirements**: Content fields based on content type

## Key Examples in the File

1. **Conditional Content**: Document can have either text_content OR binary_content based on content_type
2. **Permission Dependencies**: Sharing features only available when user has share permissions
3. **Optimistic Locking**: Updates require expected_version to prevent conflicts
4. **Visibility Control**: Metadata fields are receive-only, request fields are send-only
5. **Complex Search**: Multiple optional filters with pagination

## Try It Out

> **Note**: The `advanced-api.csil` file uses advanced metadata syntax that is planned for future implementation. However, we've created a working demo of the newly implemented CDDL comparison operators:

```bash
# Try the working .ge/.le constraint demo
cargo run -p csilgen validate --input examples/complex-metadata/simple-ge-demo.csil

# The full advanced example (will show syntax errors for metadata features)
cargo run -p csilgen validate --input examples/complex-metadata/advanced-api.csil

# For fully working examples with current syntax, see:
cargo run -p csilgen validate --input examples/basic-usage/simple-service.csil
cargo run -p csilgen validate --input examples/breaking-changes/api-v1.csil
```

This example shows how CSIL metadata can drive sophisticated code generation and validation beyond what traditional IDLs provide.