# Breaking Change Evolution Example

This example demonstrates how to use CSIL's breaking change detection to manage API evolution safely.

## Files

- `api-v1.csil` - Original API version
- `api-v2.csil` - Evolved API with breaking and non-breaking changes

## Testing Breaking Change Detection

```bash
# Detect changes between versions
cargo run -p csilgen breaking --current api-v1.csil --new api-v2.csil

# Expected output should identify:
# BREAKING CHANGES:
# - Email type: Added regex validation constraint
# - User.email: Changed from 'text' to 'Email' (stricter validation)
# - CreateUserRequest: Renamed to CreateUserRequestV2
# - CreateUserRequest.password: Added required field
# - UserAPI.update-user: Operation removed
# - UserError.error_type: Added required field
#
# NON-BREAKING CHANGES:
# - User.created_at: Added new field
# - User.profile: Added optional field
# - UserProfile: Added new type
# - UserAPI.update-profile: Added new operation
# - UserAPI.get-user-profile: Added new operation
# - UserError.details: Added optional field
```

## Types of Changes Demonstrated

### Breaking Changes
1. **Type Constraint Changes**: Adding regex validation to Email
2. **Required Field Changes**: Adding required `password` field, `error_type` field
3. **Operation Removal**: Removing `update-user` operation
4. **Type Renaming**: `CreateUserRequest` → `CreateUserRequestV2`

### Non-Breaking Changes
1. **Adding Optional Fields**: New optional fields in existing types
2. **Adding New Types**: Completely new `UserProfile` type
3. **Adding New Operations**: New service operations
4. **Adding Optional Error Details**: Optional error context

## Migration Strategies

When breaking changes are detected, consider:

1. **Versioned APIs**: Keep v1 and v2 running simultaneously
2. **Gradual Migration**: Deprecation warnings before removal
3. **Backward Compatibility**: Wrapper operations that transform old requests
4. **Client Libraries**: Generate migration guides for client code

## Integration with CI/CD

```bash
# In your CI pipeline, fail on breaking changes to main branch
if cargo run -p csilgen breaking --current main-branch.csil --new current-branch.csil | grep "BREAKING"; then
    echo "Breaking changes detected! Consider API versioning."
    exit 1
fi
```

This example helps teams manage API evolution responsibly by understanding the impact of their changes.