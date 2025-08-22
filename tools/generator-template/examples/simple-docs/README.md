# CSIL Documentation Generator Example

This example demonstrates how to create a simple but useful CSIL generator that produces human-readable Markdown documentation. It's perfect for getting started with CSIL generator development or for generating API documentation.

## Features

- ✅ Clean Markdown output
- ✅ Automatic table of contents
- ✅ Data type documentation with field details
- ✅ Service operation documentation
- ✅ Field metadata display (visibility, descriptions)
- ✅ Missing documentation warnings

## Generated Output

Given this CSIL input:

```csil
User = {
    id: uint @receive-only @description("Unique user identifier"),
    name: text @bidirectional @min-length(1) @description("User's display name"),
    email: text @send-only @description("User's email address"),
}

service UserAPI {
    create-user: User -> User
    get-user: uint -> User
    list-users: nil <-> [* User]
}
```

The generator produces:

```markdown
# API Documentation

This documentation is automatically generated from CSIL specifications.

*Generated at: 1634567890*

---

## Table of Contents

- [Data Types](#data-types)
- [Services](#services)

## Data Types

### User

| Field | Type | Required | Description | Visibility |
|-------|------|----------|-------------|------------|
| `id` | `uint` | Yes | Unique user identifier | Receive only |
| `name` | `text` | Yes | User's display name | Bidirectional |
| `email` | `text` | Yes | User's email address | Send only |

## Services

### UserAPI

| Operation | Input | Output | Direction | Description |
|-----------|-------|--------|-----------|-------------|
| `create-user` | `User` | `User` | Request → Response | *No description* |
| `get-user` | `uint` | `User` | Request → Response | *No description* |
| `list-users` | `nil` | `[User]` | Streaming ↔ | *No description* |

---

*This documentation was generated automatically from CSIL specifications using the docs-generator.*
```

## Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `title` | string | "API Documentation" | Document title |
| `include_metadata` | boolean | true | Include field metadata in docs |
| `include_examples` | boolean | true | Generate example sections |

## Usage

```bash
# Build the generator
cargo build --target wasm32-unknown-unknown --release

# Generate documentation
csilgen generate --input api.csil --target docs-generator --output ./docs/ \
  --option title="My API Documentation" \
  --option include_metadata=true
```

## Key Implementation Details

### Simple Structure
This generator demonstrates a minimal but effective approach:
- Single output file (`API.md`)
- Clear section organization
- Table-based data presentation
- Warning generation for missing documentation

### Documentation Features
- **Automatic TOC**: Generates table of contents based on content
- **Field Details**: Shows type, requirement, description, and visibility
- **Service Operations**: Documents all operations with direction indicators  
- **Metadata Processing**: Extracts and displays field metadata
- **Warning System**: Alerts about missing descriptions

### Code Organization
The generator is organized into focused functions:
- `generate_header()`: Document header and metadata
- `generate_toc()`: Table of contents generation
- `generate_types_documentation()`: Data type documentation
- `generate_services_documentation()`: Service documentation
- `generate_footer()`: Document footer

### Type Formatting
The `format_type_for_docs()` function handles various CSIL type expressions:
- Basic types: `text`, `int`, `bool`
- References: `User`, `CreateRequest`
- Arrays: `[User]`
- Maps: `map<string, User>`
- Complex types: Simplified representations

This example shows how a relatively simple generator (< 400 lines) can produce valuable, professional documentation while demonstrating all the key concepts of CSIL generator development.

## Perfect for Learning

This generator is ideal for:
- Understanding CSIL generator basics
- Learning the WASM interface
- Seeing real metadata processing
- Building your first custom generator
- Generating project documentation

The code is well-commented and demonstrates best practices without overwhelming complexity.