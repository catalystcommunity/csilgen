# Real-World API Example

This example demonstrates a realistic e-commerce API using CSIL field visibility patterns to control what data is exposed to different user roles.

## Field Visibility Patterns

### Customer vs Admin Views
- **Customer View**: Sees prices, availability, basic product info
- **Admin View**: Additionally sees costs, inventory counts, metrics, payment details

### Visibility Annotations
- `@admin-only`: Fields only visible to administrative users
- `@send-only`: Fields only sent in requests (e.g., passwords, addresses)
- `@receive-only`: Fields only returned in responses (e.g., IDs, timestamps)

## Key Patterns Demonstrated

### 1. Sensitive Data Protection
```csil
PaymentInfo = {
    @admin-only
    @receive-only
    last_four_digits: text .size (4..4),
    
    @admin-only
    @receive-only
    transaction_id: text
}
```

### 2. Role-Based Field Access
```csil
Product = {
    name: text,                    // Everyone sees this
    price: Money,                  // Everyone sees this
    
    @admin-only
    ? cost: Money,                 // Only admins see this
    
    @admin-only
    @receive-only
    ? inventory_count: int         // Only admins see this
}
```

### 3. Directional Data Flow
```csil
Address = {
    @send-only
    street: text,                  // Sent in requests only
    
    @send-only
    city: text                     // Never returned in responses
}
```

### 4. Conditional Dependencies
```csil
OrderUpdate = {
    status: OrderStatus,
    
    @depends-on(status = "shipped")
    ? tracking_number: text        // Only present when shipped
}
```

## Generated Code Benefits

When CSIL generators process this specification:

1. **Rust Generator**: 
   - Admin-only fields use `Option<T>` in customer-facing structs
   - Separate `CustomerProduct` vs `AdminProduct` types
   - Send-only fields excluded from response serialization

2. **TypeScript Generator**:
   - Different interface definitions for different user roles
   - Request vs response types automatically derived
   - Runtime validation respects visibility rules

3. **OpenAPI Generator**:
   - Customer endpoints omit admin-only fields from schemas
   - Admin endpoints include full field set
   - Proper request/response schema separation

## Try It Out

> **Note**: This example may use advanced CDDL syntax that are planned for future implementation. For working examples with current syntax, see `examples/basic-usage/` and `examples/breaking-changes/`.

```bash
# Validate the complex real-world specification  
cargo run -p csilgen validate --input examples/real-world-api/e-commerce-api.csil

# Generate role-aware code
cargo run -p csilgen generate --input examples/real-world-api/e-commerce-api.csil --target rust --output examples/real-world-api/generated/

# Generate OpenAPI spec showing different schemas for different roles
cargo run -p csilgen generate --input examples/real-world-api/e-commerce-api.csil --target openapi --output examples/real-world-api/generated/
```

This example shows how CSIL's metadata system enables sophisticated access control and data visibility patterns that go far beyond traditional IDL capabilities.