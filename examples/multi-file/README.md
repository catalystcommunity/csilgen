# Multi-File CSIL Examples

This directory contains examples demonstrating how csilgen handles multi-file CSIL projects with dependency analysis.

## Example Scenarios

### 1. Entry Points vs Dependencies (`entry-points/` and `dependencies/`)

This example demonstrates the classic pattern where service definitions (entry points) import shared type definitions (dependencies).

**Structure:**
```
entry-points/
├── user-api.csil     # Entry point - defines UserAPI service
└── admin-api.csil    # Entry point - defines AdminAPI service

dependencies/
├── user-types.csil   # Dependency - user-related types
└── common-types.csil # Dependency - shared types and errors
```

**Test it:**
```bash
# Generate from entry points only
cd examples/multi-file
csilgen generate --input entry-points/ --target noop --output /tmp/output-entry

# See dependency analysis with verbose output
CSIL_VERBOSE=1 csilgen generate --input entry-points/ --target noop --output /tmp/output-verbose
```

**Expected behavior:**
- Only processes `user-api.csil` and `admin-api.csil` (entry points)
- Automatically includes `user-types.csil` and `common-types.csil` via import resolution
- No duplicate type definitions in generated code

### 2. Mixed Entry Points and Dependencies (`mixed/`)

This example shows a directory with both entry points and dependencies mixed together.

**Structure:**
```
mixed/
├── main.csil         # Entry point - imports shared types
├── standalone.csil   # Entry point - no imports
└── shared/
    ├── types.csil    # Dependency - imported by main.csil
    └── errors.csil   # Dependency - imported by main.csil
```

**Test it:**
```bash
# Generate from the mixed directory
cd examples/multi-file
csilgen generate --input mixed/ --target noop --output /tmp/output-mixed

# See detailed dependency analysis
CSIL_VERBOSE=1 csilgen generate --input mixed/ --target noop --output /tmp/output-mixed-verbose
```

**Expected behavior:**
- Identifies `main.csil` and `standalone.csil` as entry points
- Treats `shared/types.csil` and `shared/errors.csil` as dependencies
- Generates code from 2 entry points, avoiding 2 dependency files

## Testing Different Scenarios

### Single File (Legacy Behavior)
```bash
csilgen generate --input mixed/standalone.csil --target noop --output /tmp/output-single
```

### Glob Pattern
```bash
csilgen generate --input "mixed/*.csil" --target noop --output /tmp/output-glob
```

### Directory Processing
```bash
csilgen generate --input mixed/ --target noop --output /tmp/output-dir
```

## Understanding the Output

When processing multiple files, csilgen will show:

1. **Dependency Analysis Summary**:
   ```
   📊 Dependency analysis completed:
      Entry points: X files
      Dependencies: Y files
      Generating code from entry points only to avoid duplicates.
   ```

2. **Processing Strategy**:
   ```
   🔄 Processing X entry points from Y total files:
      📄 file1.csil
      📄 file2.csil
      (Skipping Z dependency files to avoid duplicates)
   ```

3. **Verbose Analysis** (with `CSIL_VERBOSE=1`):
   - Hierarchical dependency tree
   - List of dependency files with their importers
   - Detailed analysis of import relationships

## Best Practices

1. **Organize by Purpose**: Keep service definitions (entry points) separate from shared types (dependencies)
2. **Clear Naming**: Use descriptive names that indicate the file's role (e.g., `user-api.csil`, `shared-types.csil`)
3. **Avoid Circular Dependencies**: Structure imports to flow in one direction
4. **Test Your Structure**: Use `CSIL_VERBOSE=1` to verify the dependency analysis matches your expectations