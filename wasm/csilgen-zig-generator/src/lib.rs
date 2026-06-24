//! Zig code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target zig` from `csilgen_zig_generator.wasm`.
//! Emits idiomatic Zig 0.14: `struct` records, `union(enum)` variants, `enum`
//! closed sets, snake_case fields/functions with `@"..."`-quoted reserved words,
//! a conditional `CsilDecimal`/`CsilTimestamp` helper, a typed client struct over
//! a transport seam, and server handler structs with verbose + compact router
//! twins. The WASM-boundary exports mirror the other generators exactly; only
//! `process_generation` and its helpers are Zig-specific.

use csilgen_common::{
    CsilControlOperator, CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilSizeConstraint, CsilTypeExpression, CsilValidationConstraint, GeneratedFile,
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::HashMap;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "zig-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Zig code generator with service support".to_string(),
        target: "zig".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen".to_string()),
    };
    write_json(&metadata) as *const u8
}

#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let input = match deserialize_input(input_ptr, input_len) {
        Ok(input) => input,
        Err(_) => return std::ptr::null_mut(),
    };
    match process_generation(input) {
        Ok(output) => write_json(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn deserialize_input(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorInput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

fn write_json<T: serde::Serialize>(value: &T) -> *mut u8 {
    let json = match serde_json::to_string(value) {
        Ok(j) => j,
        Err(_) => return std::ptr::null_mut(),
    };
    let bytes = json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::write(ptr as *mut u32, bytes.len() as u32);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }
    ptr
}

/// In-memory Zig type selected for the CSIL `decimal` core type. The wire form is
/// CBOR tag 4 either way; this only changes whether the self-contained helper file
/// is emitted (the in-memory spelling is `CsilDecimal` for both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper (no third-party dependency).
    Csil,
    /// The host supplies the decimal type; no helper is emitted.
    Library,
}

#[derive(Debug)]
struct ZigConfig {
    output_subdir: String,
    decimal_mapping: DecimalMapping,
    generate_validation: bool,
}

impl ZigConfig {
    /// Parse options. An unknown `decimal_mapping` is a hard error so a typo
    /// surfaces at generation time rather than silently degrading (the
    /// validate-early idiom the Go and C generators use).
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
        let decimal_mapping = match options.get("decimal_mapping") {
            None => DecimalMapping::Csil,
            Some(v) => match v.as_str() {
                Some("csil") => DecimalMapping::Csil,
                Some("library") => DecimalMapping::Library,
                _ => return Err(error_codes::GENERATION_ERROR),
            },
        };
        Ok(Self {
            output_subdir: options
                .get("output_subdir")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            decimal_mapping,
            generate_validation: options
                .get("generate_validation")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        })
    }
}

/// Which generated surface the sub-target selects. An unrecognized sub-target is
/// a hard error, never a silent fall-through.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

fn process_generation(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = ZigConfig::from_options(&input.config.options)?;
    let _ = &config;
    let surface = match input.config.target.as_str() {
        "zig" | "zig-server" => Surface::Server,
        "zig-client" => Surface::Client,
        "zig-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let mut warnings: Vec<GeneratorWarning> = Vec::new();
    let mut files = Vec::new();
    let make_path = |name: &str| -> String {
        if config.output_subdir.is_empty() {
            name.to_string()
        } else {
            format!("{}/{name}", config.output_subdir)
        }
    };

    // The exact-decimal helper is self-contained and only worth emitting when the
    // spec uses `decimal` under the default mapping; the library mapping expects
    // the host to provide the type.
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(&input, "decimal") {
        files.push(GeneratedFile {
            path: make_path("csil_decimal.gen.zig"),
            content: CSIL_DECIMAL_ZIG.to_string(),
        });
    }
    // The timestamp helper (CBOR tag-0 RFC3339 UTC) is emitted only when used.
    if spec_uses_builtin(&input, "timestamp") {
        files.push(GeneratedFile {
            path: make_path("csil_timestamp.gen.zig"),
            content: CSIL_TIMESTAMP_ZIG.to_string(),
        });
    }

    if let Some(types) = generate_types(&input, &config) {
        files.push(GeneratedFile {
            path: make_path("types.gen.zig"),
            content: types,
        });
    }

    if config.generate_validation
        && let Some(validation) = generate_validation(&input)
    {
        files.push(GeneratedFile {
            path: make_path("validation.gen.zig"),
            content: validation,
        });
    }

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_client(&input) {
                    files.push(GeneratedFile {
                        path: make_path("client.gen.zig"),
                        content: client,
                    });
                }
            }
            Surface::Server => {
                if let Some(server) = generate_server(&input, &mut warnings) {
                    files.push(GeneratedFile {
                        path: make_path("server.gen.zig"),
                        content: server,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
    }

    // `zig fmt` keeps exactly one trailing newline; emit that so the output is
    // already formatter-clean and a `zig fmt --check` in CI stays quiet.
    for file in &mut files {
        let trimmed = file.content.trim_end_matches('\n');
        file.content = format!("{trimmed}\n");
    }

    let total_size: usize = files.iter().map(|f| f.content.len()).sum();
    Ok(WasmGeneratorOutput {
        stats: GenerationStats {
            files_generated: files.len(),
            total_size_bytes: total_size,
            services_count: input.csil_spec.service_count,
            fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
            generation_time_ms: 0,
            peak_memory_bytes: None,
        },
        files,
        warnings,
    })
}

// ---- file headers ---------------------------------------------------------

fn file_header(content: &mut String, summary: &str) {
    content.push_str(&format!("// {summary}\n"));
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n\n");
}

// ---- types ----------------------------------------------------------------

/// How a named type rule is realized in Zig, used to drive emission phases.
/// Enums and aliases need no dependency ordering; aggregates (structs / unions)
/// are emitted in by-value dependency order so a reader sees a member's type
/// before the type that embeds it (Zig itself resolves container-level decls
/// lazily, but ordered output keeps the file readable and self-documenting).
enum TypeKind<'a> {
    Struct(&'a CsilGroupExpression),
    Alias(&'a CsilTypeExpression),
    Enum(Vec<String>),
    Union(&'a [CsilTypeExpression]),
    GroupUnion(&'a [CsilGroupExpression]),
}

fn classify_rule(rule_type: &CsilRuleType) -> Option<TypeKind<'_>> {
    match rule_type {
        CsilRuleType::GroupDef(g) => Some(TypeKind::Struct(g)),
        CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(TypeKind::Struct(g)),
        // The parser routes a named choice (`X = A / B`) to a `TypeDef(Choice)`, so
        // this is where real specs land; classify it into the same closed-enum /
        // open-string / tagged-union shapes a hand-built `TypeChoice` would.
        CsilRuleType::TypeDef(t @ CsilTypeExpression::Choice(arms)) => {
            Some(classify_choice(arms, t))
        }
        CsilRuleType::TypeDef(t) => Some(TypeKind::Alias(t)),
        CsilRuleType::TypeChoice(arms) => {
            let literals: Option<Vec<String>> = arms
                .iter()
                .map(|a| match a {
                    CsilTypeExpression::Literal(CsilLiteralValue::Text(t)) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            Some(match literals {
                Some(names) => TypeKind::Enum(names),
                None => TypeKind::Union(arms),
            })
        }
        CsilRuleType::GroupChoice(arms) => Some(TypeKind::GroupUnion(arms)),
        CsilRuleType::ServiceDef(_) => None,
    }
}

/// Classify the arms of a choice rule. A choice of only text literals is a closed
/// `enum`; a choice that mixes the `text` builtin with literals is an open string
/// (`[]const u8` — any text is allowed, the literals are merely suggested values);
/// anything else (referenced types) is a tagged `union(enum)`. The `whole`
/// expression is carried so the open-string case can alias the choice straight to
/// its `map_zig_type` result.
fn classify_choice<'a>(
    arms: &'a [CsilTypeExpression],
    whole: &'a CsilTypeExpression,
) -> TypeKind<'a> {
    let literals: Option<Vec<String>> = arms
        .iter()
        .map(|a| match a {
            CsilTypeExpression::Literal(CsilLiteralValue::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect();
    match literals {
        Some(names) if !names.is_empty() => TypeKind::Enum(names),
        _ if arms.iter().all(is_text_like) => TypeKind::Alias(whole),
        _ => TypeKind::Union(arms),
    }
}

/// The names this entry embeds *by value* (so its definition is ordered after
/// that type's). An optional `?T`, a slice `[]T`, or a map member does not impose
/// an ordering edge — matching the cross-language canary rule — keeping the topo
/// sort identical to the C generator's even though Zig's `?T` is inline.
fn entry_value_dep(entry: &CsilGroupEntry) -> Option<String> {
    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
        return None;
    }
    match unwrap_constrained(&entry.value_type) {
        CsilTypeExpression::Reference(n) => Some(n.clone()),
        _ => None,
    }
}

fn generate_types(input: &WasmGeneratorInput, config: &ZigConfig) -> Option<String> {
    let typed: Vec<(&str, TypeKind)> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| classify_rule(&r.rule_type).map(|k| (r.name.as_str(), k)))
        .collect();
    if typed.is_empty() {
        return None;
    }
    let names: std::collections::HashSet<&str> = typed.iter().map(|(n, _)| *n).collect();

    let mut enums = String::new();
    let mut aliases = String::new();
    // Definitions keyed by name so they can be emitted in topological order.
    let mut defs: HashMap<String, String> = HashMap::new();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (name, kind) in &typed {
        match kind {
            TypeKind::Enum(variants) => emit_enum(&mut enums, name, variants),
            TypeKind::Alias(t) => {
                aliases.push_str(&format!("/// {name} is a type alias.\n"));
                aliases.push_str(&format!("pub const {name} = {};\n\n", map_zig_type(t, "")));
            }
            TypeKind::Struct(group) => {
                let mut s = String::new();
                emit_struct(&mut s, name, group, "");
                defs.insert(name.to_string(), s);
                order.push(name.to_string());
                deps.insert(
                    name.to_string(),
                    group
                        .entries
                        .iter()
                        .filter_map(entry_value_dep)
                        .filter(|d| names.contains(d.as_str()))
                        .collect(),
                );
            }
            TypeKind::Union(arms) => {
                let mut s = String::new();
                emit_choice(&mut s, name, arms, "");
                defs.insert(name.to_string(), s);
                order.push(name.to_string());
                // Union arms are embedded by value, so every Reference arm is a dep.
                deps.insert(
                    name.to_string(),
                    arms.iter()
                        .filter_map(|a| match a {
                            CsilTypeExpression::Reference(n) if names.contains(n.as_str()) => {
                                Some(n.clone())
                            }
                            _ => None,
                        })
                        .collect(),
                );
            }
            TypeKind::GroupUnion(arms) => {
                let mut s = String::new();
                emit_group_choice(&mut s, name, arms, "");
                defs.insert(name.to_string(), s);
                order.push(name.to_string());
                deps.insert(
                    name.to_string(),
                    arms.iter()
                        .flat_map(|g| g.entries.iter().filter_map(entry_value_dep))
                        .filter(|d| names.contains(d.as_str()))
                        .collect(),
                );
            }
        }
    }

    let definitions = topo_emit(&order, &deps, &defs);

    let mut content = String::new();
    file_header(&mut content, "Generated CSIL value types.");
    // A map field maps onto a std HashMap, which lives in std; import it only when
    // a map is present so a map-free spec needs no std import.
    if spec_uses_map(input) {
        content.push_str("const std = @import(\"std\");\n\n");
    }
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(input, "decimal") {
        content
            .push_str("pub const CsilDecimal = @import(\"csil_decimal.gen.zig\").CsilDecimal;\n");
    }
    if spec_uses_builtin(input, "timestamp") {
        content.push_str(
            "pub const CsilTimestamp = @import(\"csil_timestamp.gen.zig\").CsilTimestamp;\n",
        );
    }
    if (config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(input, "decimal"))
        || spec_uses_builtin(input, "timestamp")
    {
        content.push('\n');
    }
    // Zig resolves container-level declarations lazily, so types may reference each
    // other regardless of source order — no C-style forward declarations exist or
    // are needed. The definitions below are still emitted in by-value dependency
    // order so a by-value member's type is always seen first.
    content.push_str(&enums);
    content.push_str(&aliases);
    content.push_str(&definitions);
    Some(content)
}

/// Emit the struct/union definitions in value-dependency order (Kahn's
/// algorithm). A by-value member is ordered after its type's definition. A
/// dependency cycle through by-value members would be an ill-formed (infinitely
/// sized) spec, so any leftover nodes are appended in their original order.
fn topo_emit(
    order: &[String],
    deps: &HashMap<String, Vec<String>>,
    defs: &HashMap<String, String>,
) -> String {
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = String::new();
    let mut progress = true;
    while progress {
        progress = false;
        for name in order {
            if emitted.contains(name) {
                continue;
            }
            let ready = deps
                .get(name)
                .map(|d| d.iter().all(|dep| dep == name || emitted.contains(dep)))
                .unwrap_or(true);
            if ready {
                out.push_str(&defs[name]);
                emitted.insert(name.clone());
                progress = true;
            }
        }
    }
    // Append any cycle remnants so output is still complete (best effort).
    for name in order {
        if !emitted.contains(name) {
            out.push_str(&defs[name]);
        }
    }
    out
}

fn emit_enum(content: &mut String, name: &str, variants: &[String]) {
    content.push_str(&format!("/// {name} is an enumeration.\n"));
    content.push_str(&format!("pub const {name} = enum {{\n"));
    for variant in variants {
        content.push_str(&format!("    {},\n", zig_ident(&to_snake(variant))));
    }
    // The wire form of a closed CSIL enum is the original literal text, which may
    // differ from the snake_case Zig tag; wire_name maps each tag back verbatim.
    content.push_str(&format!(
        "\n    pub fn wire_name(self: {name}) []const u8 {{\n"
    ));
    content.push_str("        return switch (self) {\n");
    for variant in variants {
        content.push_str(&format!(
            "            .{} => \"{}\",\n",
            zig_ident(&to_snake(variant)),
            zig_escape(variant)
        ));
    }
    content.push_str("        };\n");
    content.push_str("    }\n");
    content.push_str("};\n\n");
}

fn emit_struct(content: &mut String, name: &str, group: &CsilGroupExpression, type_prefix: &str) {
    content.push_str(&format!("/// {name} is a structured data type.\n"));
    content.push_str(&format!("pub const {name} = struct {{\n"));
    for entry in &group.entries {
        if let Some(field) = entry_field_name(&entry.key) {
            if let Some(description) = field_description(entry) {
                content.push_str(&format!("    /// {description}\n"));
            }
            emit_field(
                content,
                &field,
                &entry.value_type,
                &entry.occurrence,
                type_prefix,
            );
        }
    }
    content.push_str("};\n\n");
}

/// Emit one struct field. Snake_case CSIL field names map verbatim to Zig field
/// names (the wire key is already idiomatic Zig); a name colliding with a Zig
/// keyword is `@"..."`-quoted. An optional field becomes `?T = null`.
fn emit_field(
    content: &mut String,
    field: &str,
    value_type: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    type_prefix: &str,
) {
    let ident = zig_ident(field);
    let zt = map_zig_type(value_type, type_prefix);
    if matches!(occurrence, Some(CsilOccurrence::Optional)) {
        // A type that is already nullable (the `?*anyopaque` dynamic fallback) is not
        // wrapped a second time; `??T` is never what an optional field wants.
        if zt.starts_with('?') {
            content.push_str(&format!("    {ident}: {zt} = null,\n"));
        } else {
            content.push_str(&format!("    {ident}: ?{zt} = null,\n"));
        }
    } else {
        content.push_str(&format!("    {ident}: {zt},\n"));
    }
}

/// A non-enum `TypeChoice` is a `union(enum)` (the idiomatic Zig sum type): one
/// tag per arm plus a `variant_name` mapping each tag to its CSIL type name (the
/// `variant` string on the RPC response wire).
fn emit_choice(content: &mut String, name: &str, arms: &[CsilTypeExpression], type_prefix: &str) {
    content.push_str(&format!("/// {name} is a tagged union.\n"));
    content.push_str(&format!("pub const {name} = union(enum) {{\n"));
    for (i, arm) in arms.iter().enumerate() {
        let tag = zig_ident(&to_snake(&arm_name(arm, i)));
        content.push_str(&format!("    {tag}: {},\n", map_zig_type(arm, type_prefix)));
    }
    content.push_str(&format!(
        "\n    pub fn variant_name(self: {name}) []const u8 {{\n"
    ));
    content.push_str("        return switch (self) {\n");
    for (i, arm) in arms.iter().enumerate() {
        let tag = zig_ident(&to_snake(&arm_name(arm, i)));
        content.push_str(&format!(
            "            .{tag} => \"{}\",\n",
            zig_escape(&arm_name(arm, i))
        ));
    }
    content.push_str("        };\n");
    content.push_str("    }\n");
    content.push_str("};\n\n");
}

/// A `GroupChoice` is a union over record shapes: each arm becomes its own struct
/// `<Name>Arm<N>`, tied together by a `union(enum)`.
fn emit_group_choice(
    content: &mut String,
    name: &str,
    arms: &[CsilGroupExpression],
    type_prefix: &str,
) {
    for (i, arm) in arms.iter().enumerate() {
        emit_struct(content, &format!("{name}Arm{i}"), arm, type_prefix);
    }
    content.push_str(&format!("/// {name} is a union over record shapes.\n"));
    content.push_str(&format!("pub const {name} = union(enum) {{\n"));
    for i in 0..arms.len() {
        content.push_str(&format!("    arm{i}: {name}Arm{i},\n"));
    }
    content.push_str("};\n\n");
}

// ---- validation -----------------------------------------------------------

fn generate_validation(input: &WasmGeneratorInput) -> Option<String> {
    let mut body = String::new();
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group
            && group.entries.iter().any(entry_has_check)
        {
            emit_validate_fn(&mut body, &rule.name, group);
        }
    }
    if body.is_empty() {
        return None;
    }
    let mut content = String::new();
    file_header(&mut content, "Generated validation predicates.");
    content.push_str("const types = @import(\"types.gen.zig\");\n\n");
    content.push_str(&body);
    Some(content)
}

/// A `pub fn validate_<type>(v: *const types.<Type>) bool` returning false on the
/// first failed check. Emitted only when at least one check line is produced.
fn emit_validate_fn(content: &mut String, name: &str, group: &CsilGroupExpression) {
    let mut checks = String::new();
    for entry in &group.entries {
        if let Some(field) = entry_field_name(&entry.key) {
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            let ident = zig_ident(&field);
            for metadata in &entry.metadata {
                if let CsilFieldMetadata::Constraint(constraint) = metadata {
                    emit_metadata_check(&mut checks, &ident, optional, constraint);
                }
            }
            if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                for op in constraints {
                    emit_control_check(&mut checks, &ident, optional, op);
                }
            }
        }
    }
    if checks.is_empty() {
        return;
    }
    content.push_str(&format!(
        "/// validate_{0} returns false on the first failed constraint.\n",
        to_snake(name)
    ));
    content.push_str(&format!(
        "pub fn validate_{0}(v: *const types.{name}) bool {{\n",
        to_snake(name)
    ));
    content.push_str(&checks);
    content.push_str("    return true;\n}\n\n");
}

/// A length check against a text/byte slice or list field. An optional field is a
/// `?T`, captured behind an `if (...) |x|` guard so an absent value is skipped.
fn len_check(out: &mut String, field: &str, optional: bool, op: &str, n: u64) {
    if optional {
        out.push_str(&format!(
            "    if (v.{field}) |x| {{\n        if (x.len {op} {n}) return false;\n    }}\n"
        ));
    } else {
        out.push_str(&format!("    if (v.{field}.len {op} {n}) return false;\n"));
    }
}

/// A numeric comparison. The read is cast to `i64` so a negative bound compared
/// against an unsigned field never trips Zig's signedness mismatch; an optional
/// field is captured behind a guard.
fn numeric_check(out: &mut String, field: &str, optional: bool, op: &str, n: i64) {
    if optional {
        out.push_str(&format!(
            "    if (v.{field}) |x| {{\n        if (@as(i64, @intCast(x)) {op} {n}) return false;\n    }}\n"
        ));
    } else {
        out.push_str(&format!(
            "    if (@as(i64, @intCast(v.{field})) {op} {n}) return false;\n"
        ));
    }
}

fn emit_metadata_check(
    out: &mut String,
    field: &str,
    optional: bool,
    constraint: &CsilValidationConstraint,
) {
    match constraint {
        CsilValidationConstraint::MinLength(n) => len_check(out, field, optional, "<", *n),
        CsilValidationConstraint::MaxLength(n) => len_check(out, field, optional, ">", *n),
        CsilValidationConstraint::MinItems(n) => len_check(out, field, optional, "<", *n),
        CsilValidationConstraint::MaxItems(n) => len_check(out, field, optional, ">", *n),
        CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, field, optional, "<", *n)
        }
        CsilValidationConstraint::MaxValue(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, field, optional, ">", *n)
        }
        _ => {}
    }
}

fn emit_control_check(out: &mut String, field: &str, optional: bool, op: &CsilControlOperator) {
    match op {
        CsilControlOperator::Size(CsilSizeConstraint::Min(n)) => {
            len_check(out, field, optional, "<", *n)
        }
        CsilControlOperator::Size(CsilSizeConstraint::Max(n)) => {
            len_check(out, field, optional, ">", *n)
        }
        CsilControlOperator::Size(CsilSizeConstraint::Exact(n)) => {
            len_check(out, field, optional, "!=", *n)
        }
        CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, field, optional, "<", *n)
        }
        CsilControlOperator::LessEqual(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, field, optional, ">", *n)
        }
        CsilControlOperator::GreaterThan(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, field, optional, "<=", *n)
        }
        CsilControlOperator::LessThan(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, field, optional, ">=", *n)
        }
        // Encoding-only and structural operators carry no runtime predicate.
        _ => {}
    }
}

// ---- client ---------------------------------------------------------------

/// The transport seam every generated call delegates to: the host implements
/// `call`, performing the wire round-trip for `(service, op)`. The generator
/// never owns the bytes.
const CLIENT_PRELUDE_ZIG: &str = "\
/// CsilgenTransport is supplied by the caller: it encodes req (CBOR over HTTP,
/// say), performs the call named by (service, op), and returns the response
/// bytes (the caller owns/frees them), or an error. The generator never owns the
/// wire.
pub const CsilgenTransport = struct {
    ptr: *anyopaque,
    call: *const fn (ptr: *anyopaque, service: []const u8, op: []const u8, req: []const u8) anyerror![]u8,
};
";

fn generate_client(input: &WasmGeneratorInput) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_struct(&mut body, &rule.name, service);
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = String::new();
    file_header(&mut content, "Generated typed RPC client call-sites.");
    content.push_str(CLIENT_PRELUDE_ZIG);
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

fn emit_client_struct(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let base = service_base(name);
    let client = format!("{base}Client");
    let wire_service = base.to_lowercase();
    content.push_str(&format!(
        "/// {client} is a typed client for the {name} service over a CsilgenTransport.\n"
    ));
    content.push_str(&format!("pub const {client} = struct {{\n"));
    content.push_str("    transport: CsilgenTransport,\n\n");
    content.push_str(&format!(
        "    pub fn init(transport: CsilgenTransport) {client} {{\n"
    ));
    content.push_str("        return .{ .transport = transport };\n");
    content.push_str("    }\n");
    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the router surface the server target emits.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            content.push_str(&format!(
                "\n    // channel operation {} is not part of the RPC client\n",
                op.name
            ));
            continue;
        }
        let method = zig_ident(&to_snake(&op.name));
        let wire_op = simple_pascal(&op.name);
        content.push_str(&format!(
            "\n    /// Invoke {wire_service}/{wire_op}. The encoded request rides in req; the\n\
             \x20   /// decoded response bytes are returned (caller owns).\n"
        ));
        content.push_str(&format!(
            "    pub fn {method}(self: {client}, req: []const u8) anyerror![]u8 {{\n"
        ));
        content.push_str(&format!(
            "        return self.transport.call(self.transport.ptr, \"{wire_service}\", \"{wire_op}\", req);\n"
        ));
        content.push_str("    }\n");
    }
    content.push_str("};\n\n");
}

// ---- server ---------------------------------------------------------------

fn generate_server(
    input: &WasmGeneratorInput,
    _warnings: &mut [GeneratorWarning],
) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_handlers_struct(&mut body, &rule.name, service);
            emit_wire_ids(&mut body, &rule.name, service);
            if service_has_channel_ops(service) {
                emit_channel_router(&mut body, &rule.name, service);
                emit_channel_router_compact(&mut body, &rule.name, service);
                emit_push_encoders(&mut body, &rule.name, service);
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = String::new();
    file_header(
        &mut content,
        "Generated service handler structs and routers.",
    );
    content.push_str("const std = @import(\"std\");\n");
    content.push_str("const types = @import(\"types.gen.zig\");\n\n");
    // The codec is consumer-supplied so the runtime never owns serialization.
    content.push_str(
        "/// CsilgenCodec is the consumer-supplied (de)serialization layer for channel\n\
         /// messages. The generator is codec-agnostic; the implementer wires this to\n\
         /// CBOR, JSON, or whatever its protocol expects. decode writes into out.\n\
         pub const CsilgenCodec = struct {\n\
         \x20   ptr: *anyopaque,\n\
         \x20   decode: *const fn (ptr: *anyopaque, data: []const u8, out: *anyopaque) anyerror!void,\n\
         \x20   encode: *const fn (ptr: *anyopaque, value: *const anyopaque) anyerror![]u8,\n\
         };\n\n",
    );
    content.push_str(&body);
    Some(content)
}

fn emit_handlers_struct(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let base = service_base(name);
    content.push_str(&format!(
        "/// {base}Handlers is the host's implementation of the {name} service.\n"
    ));
    content.push_str(&format!("pub const {base}Handlers = struct {{\n"));
    for op in &service.operations {
        let method = zig_ident(&to_snake(&op.name));
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let out_type = map_zig_type(&success_type(&op.output_type), "types.");
                if op_input_is_null(&op.input_type) {
                    content.push_str(&format!(
                        "    {method}: *const fn (ctx: *anyopaque, resp: *{out_type}) anyerror!void,\n"
                    ));
                } else {
                    let in_type = map_zig_type(&op.input_type, "types.");
                    content.push_str(&format!(
                        "    {method}: *const fn (ctx: *anyopaque, req: *const {in_type}, resp: *{out_type}) anyerror!void,\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let in_type = map_zig_type(&op.input_type, "types.");
                // Fire-and-forget inbound: the router decodes and dispatches here.
                content.push_str(&format!(
                    "    {method}: *const fn (ctx: *anyopaque, msg: *const {in_type}) anyerror!void,\n"
                ));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound method on the server side.
            }
        }
    }
    content.push_str("};\n\n");
}

/// Emit `pub const` wire-id ordinals exposing the `@wire-id(N)` values. Purely
/// additive: emits nothing unless the service carries a wire-id, keeping
/// wire-id-free output byte-identical.
fn emit_wire_ids(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let prefix = to_snake(&service_base(name));
    content.push_str(&format!(
        "/// Wire-id ordinals for the {name} service (transport compact profiles).\n"
    ));
    content.push_str(&format!(
        "pub const {prefix}_service_wire_id: u64 = {service_id};\n"
    ));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            // The op_ infix keeps op ordinals distinct from the service ordinal.
            content.push_str(&format!(
                "pub const {prefix}_op_{}_wire_id: u64 = {op_id};\n",
                to_snake(&op.name)
            ));
        }
    }
    content.push('\n');
}

fn emit_channel_router(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let base = service_base(name);
    let prefix = to_snake(&base);
    content.push_str(&format!(
        "/// route_{prefix}_channel decodes one inbound channel frame and dispatches to\n\
         /// the matching {name} method by wire op name.\n"
    ));
    content.push_str(&format!(
        "pub fn route_{prefix}_channel(h: *const {base}Handlers, ctx: *anyopaque, codec: CsilgenCodec, method: []const u8, data: []const u8) anyerror!void {{\n"
    ));
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = zig_ident(&to_snake(&op.name));
        let wire_op = simple_pascal(&op.name);
        let in_type = map_zig_type(&op.input_type, "types.");
        content.push_str(&format!(
            "    if (std.mem.eql(u8, method, \"{wire_op}\")) {{\n"
        ));
        content.push_str(&format!("        var msg: {in_type} = undefined;\n"));
        content.push_str("        try codec.decode(codec.ptr, data, &msg);\n");
        content.push_str(&format!("        return h.{method}(ctx, &msg);\n"));
        content.push_str("    }\n");
    }
    content.push_str("    return error.UnknownChannelMethod;\n");
    content.push_str("}\n\n");
}

/// The compact-profile twin: dispatch on the `@wire-id` operation ordinal rather
/// than the wire op name. Emitted only for wire-id-bearing services, so
/// wire-id-free output stays byte-identical.
fn emit_channel_router_compact(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    if service.wire_id.is_none() {
        return;
    }
    let base = service_base(name);
    let prefix = to_snake(&base);
    content.push_str(&format!(
        "/// route_{prefix}_channel_compact dispatches by @wire-id ordinal (compact\n\
         /// profile). The host calls whichever twin matches the negotiated wire profile.\n"
    ));
    content.push_str(&format!(
        "pub fn route_{prefix}_channel_compact(h: *const {base}Handlers, ctx: *anyopaque, codec: CsilgenCodec, op: u64, data: []const u8) anyerror!void {{\n"
    ));
    content.push_str("    switch (op) {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let Some(op_id) = operation.wire_id else {
            continue;
        };
        let method = zig_ident(&to_snake(&operation.name));
        let in_type = map_zig_type(&operation.input_type, "types.");
        content.push_str(&format!("        {op_id} => {{\n"));
        content.push_str(&format!("            var msg: {in_type} = undefined;\n"));
        content.push_str("            try codec.decode(codec.ptr, data, &msg);\n");
        content.push_str(&format!("            return h.{method}(ctx, &msg);\n"));
        content.push_str("        },\n");
    }
    content.push_str("        else => return error.UnknownChannelOrdinal,\n");
    content.push_str("    }\n");
    content.push_str("}\n\n");
}

fn emit_push_encoders(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let prefix = to_snake(&service_base(name));
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method = to_snake(&op.name);
        let wire_op = simple_pascal(&op.name);
        content.push_str(&format!(
            "/// encode_{prefix}_{method} encodes a {wire_op} message the server pushes to a\n\
             /// peer; the implementer frames (\"{wire_op}\", bytes) onto its connection.\n"
        ));
        content.push_str(&format!(
            "pub fn encode_{prefix}_{method}(codec: CsilgenCodec, msg: *const anyopaque) anyerror![]u8 {{\n"
        ));
        content.push_str("    return codec.encode(codec.ptr, msg);\n");
        content.push_str("}\n\n");
    }
}

// ---- type mapping ---------------------------------------------------------

/// The Zig type a CSIL type maps to. References are prefixed with `type_prefix`
/// (`""` inside types.gen.zig, `"types."` for the server/validation files that
/// `@import` it).
fn map_zig_type(type_expr: &CsilTypeExpression, type_prefix: &str) -> String {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => "i64".to_string(),
            "uint" => "u64".to_string(),
            "float" | "float64" | "double" => "f64".to_string(),
            "float16" | "float32" => "f32".to_string(),
            "text" | "tstr" => "[]const u8".to_string(),
            "bytes" | "bstr" => "[]const u8".to_string(),
            "bool" | "true" | "false" => "bool".to_string(),
            "timestamp" => format!("{type_prefix}CsilTimestamp"),
            "decimal" => format!("{type_prefix}CsilDecimal"),
            "null" | "nil" | "undefined" | "any" => "?*anyopaque".to_string(),
            other => format!("{type_prefix}{other}"),
        },
        CsilTypeExpression::Reference(name) => format!("{type_prefix}{name}"),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("[]{}", map_zig_type(element_type, type_prefix))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let v = map_zig_type(value, type_prefix);
            // A text-keyed map uses StringHashMap; any other key uses AutoHashMap.
            // The unmanaged forms keep the field plain-old-data (no embedded
            // allocator), matching the explicit-allocator house style.
            if matches!(unwrap_constrained(key), CsilTypeExpression::Builtin(n) if n == "text" || n == "tstr")
            {
                format!("std.StringHashMapUnmanaged({v})")
            } else {
                let k = map_zig_type(key, type_prefix);
                format!("std.AutoHashMapUnmanaged({k}, {v})")
            }
        }
        // A `text / "a" / "b"` choice is a string with a suggested value set; on the
        // wire it is just a text string, so `[]const u8` is both idiomatic and the
        // only type that can actually hold the value (an opaque pointer cannot).
        CsilTypeExpression::Choice(arms) if arms.iter().all(is_text_like) => {
            "[]const u8".to_string()
        }
        _ => "?*anyopaque".to_string(),
    }
}

/// Whether a choice arm is text-like (the `text` builtin or a text literal), used to
/// recognize the `text / "lit" / "lit"` string-enum pattern.
fn is_text_like(arm: &CsilTypeExpression) -> bool {
    match unwrap_constrained(arm) {
        CsilTypeExpression::Builtin(n) => n == "text" || n == "tstr",
        CsilTypeExpression::Literal(CsilLiteralValue::Text(_)) => true,
        _ => false,
    }
}

fn op_input_is_null(input_type: &CsilTypeExpression) -> bool {
    matches!(unwrap_constrained(input_type), CsilTypeExpression::Builtin(n) if n == "null" || n == "nil")
}

/// The success arm of a `Res / ServiceError` output choice: a typed client/server
/// returns the success type, not the whole choice.
fn success_type(output_type: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(arms) = output_type {
        for arm in arms {
            let is_error = matches!(arm, CsilTypeExpression::Reference(n) if n.ends_with("Error"));
            if !is_error {
                return arm.clone();
            }
        }
    }
    output_type.clone()
}

fn unwrap_constrained(type_expr: &CsilTypeExpression) -> &CsilTypeExpression {
    match type_expr {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    }
}

// ---- spec scans -----------------------------------------------------------

fn spec_uses_builtin(input: &WasmGeneratorInput, builtin: &str) -> bool {
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group)
            | CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => group
                .entries
                .iter()
                .any(|e| type_uses_builtin(&e.value_type, builtin)),
            CsilRuleType::TypeDef(t) => type_uses_builtin(t, builtin),
            CsilRuleType::TypeChoice(arms) => arms.iter().any(|t| type_uses_builtin(t, builtin)),
            CsilRuleType::GroupChoice(arms) => arms.iter().any(|g| {
                g.entries
                    .iter()
                    .any(|e| type_uses_builtin(&e.value_type, builtin))
            }),
            CsilRuleType::ServiceDef(svc) => svc.operations.iter().any(|op| {
                type_uses_builtin(&op.input_type, builtin)
                    || type_uses_builtin(&op.output_type, builtin)
            }),
        })
}

fn spec_uses_map(input: &WasmGeneratorInput) -> bool {
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group)
            | CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                group.entries.iter().any(|e| type_is_map(&e.value_type))
            }
            CsilRuleType::TypeDef(t) => type_is_map(t),
            CsilRuleType::GroupChoice(arms) => arms
                .iter()
                .any(|g| g.entries.iter().any(|e| type_is_map(&e.value_type))),
            _ => false,
        })
}

fn type_is_map(type_expr: &CsilTypeExpression) -> bool {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Map { .. } => true,
        CsilTypeExpression::Array { element_type, .. } => type_is_map(element_type),
        _ => false,
    }
}

fn type_uses_builtin(type_expr: &CsilTypeExpression, builtin: &str) -> bool {
    match type_expr {
        CsilTypeExpression::Builtin(n) => n == builtin,
        CsilTypeExpression::Reference(_) => false,
        CsilTypeExpression::Array { element_type, .. } => type_uses_builtin(element_type, builtin),
        CsilTypeExpression::Map { key, value, .. } => {
            type_uses_builtin(key, builtin) || type_uses_builtin(value, builtin)
        }
        CsilTypeExpression::Group(g) | CsilTypeExpression::Tuple(g) => g
            .entries
            .iter()
            .any(|e| type_uses_builtin(&e.value_type, builtin)),
        CsilTypeExpression::Choice(arms) => arms.iter().any(|t| type_uses_builtin(t, builtin)),
        CsilTypeExpression::Constrained { base_type, .. } => type_uses_builtin(base_type, builtin),
        _ => false,
    }
}

fn service_has_channel_ops(service: &CsilServiceDefinition) -> bool {
    service
        .operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

// ---- field helpers --------------------------------------------------------

fn entry_field_name(key: &Option<CsilGroupKey>) -> Option<String> {
    match key {
        Some(CsilGroupKey::Bare(name)) => Some(name.clone()),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => Some(name.clone()),
        _ => None,
    }
}

fn field_description(entry: &CsilGroupEntry) -> Option<String> {
    entry.metadata.iter().find_map(|m| match m {
        CsilFieldMetadata::Description(d) => Some(d.clone()),
        _ => None,
    })
}

fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let metadata_check = entry.metadata.iter().any(|m| {
        matches!(
            m,
            CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinLength(_)
                    | CsilValidationConstraint::MaxLength(_)
                    | CsilValidationConstraint::MinItems(_)
                    | CsilValidationConstraint::MaxItems(_)
                    | CsilValidationConstraint::MinValue(_)
                    | CsilValidationConstraint::MaxValue(_)
            )
        )
    });
    let control_check = matches!(
        &entry.value_type,
        CsilTypeExpression::Constrained { constraints, .. }
            if constraints.iter().any(|c| matches!(
                c,
                CsilControlOperator::Size(_)
                    | CsilControlOperator::GreaterEqual(_)
                    | CsilControlOperator::LessEqual(_)
                    | CsilControlOperator::GreaterThan(_)
                    | CsilControlOperator::LessThan(_)
            ))
    );
    metadata_check || control_check
}

/// The arm name for a choice arm: a referenced/builtin type's name, else a
/// positional `Choice<N>`.
fn arm_name(arm: &CsilTypeExpression, index: usize) -> String {
    match arm {
        CsilTypeExpression::Reference(n) | CsilTypeExpression::Builtin(n) => n.clone(),
        _ => format!("Choice{index}"),
    }
}

// ---- naming (wire names verbatim; Zig symbols cased) ----------------------

/// PascalCase by the same simple rule the other generators use for *wire* method
/// names, so a Zig client and a Go/Rust/Python/TS server agree byte-for-byte:
/// break on `_`/`-`, uppercase the letter after each break, keep the rest.
fn simple_pascal(s: &str) -> String {
    let mut out = String::new();
    for word in s.split(['_', '-']) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// snake_case for Zig symbol names (PascalCase services, kebab-case operations).
/// Only Zig identifiers are reshaped this way; wire strings stay verbatim. A run of
/// capitals is treated as a single acronym word so `APIError` becomes `api_error`
/// and `ECommerceAPI` becomes `e_commerce_api` rather than `a_p_i_error` — the form
/// a Zig reader expects.
fn to_snake(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' || c == '_' {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            continue;
        }
        if c.is_uppercase() {
            // A word boundary precedes this capital when the previous char was
            // lower/digit (camel hump) or when it ends an acronym run that is
            // immediately followed by a lowercase letter (e.g. the `E` in `APIError`).
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                None => false,
                Some(p) if p == '-' || p == '_' => false,
                Some(p) if p.is_lowercase() || p.is_ascii_digit() => true,
                Some(p) if p.is_uppercase() => next.is_some_and(char::is_lowercase),
                _ => false,
            };
            if boundary && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Strip a trailing `Service` suffix and PascalCase the remainder, matching the
/// wire service base used across the other clients.
fn service_base(name: &str) -> String {
    let pascal = simple_pascal(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// Zig reserved words that cannot be a bare identifier. A field or tag colliding
/// with one is emitted in the `@"..."` quoted-identifier form so the generated
/// source still compiles.
const ZIG_KEYWORDS: &[&str] = &[
    "addrspace",
    "align",
    "allowzero",
    "and",
    "anyframe",
    "anytype",
    "asm",
    "async",
    "await",
    "break",
    "callconv",
    "catch",
    "comptime",
    "const",
    "continue",
    "defer",
    "else",
    "enum",
    "errdefer",
    "error",
    "export",
    "extern",
    "fn",
    "for",
    "if",
    "inline",
    "linksection",
    "noalias",
    "noinline",
    "nosuspend",
    "opaque",
    "or",
    "orelse",
    "packed",
    "pub",
    "resume",
    "return",
    "struct",
    "suspend",
    "switch",
    "test",
    "threadlocal",
    "try",
    "union",
    "unreachable",
    "usingnamespace",
    "var",
    "volatile",
    "while",
    // Primitive type / value names are not keywords, but used as a bare field or
    // tag identifier they shadow or collide with the primitive, so they are quoted
    // too (the brief calls out `type` specifically).
    "type",
    "void",
    "bool",
    "anyerror",
    "anyopaque",
    "noreturn",
    "comptime_int",
    "comptime_float",
    "null",
    "undefined",
    "true",
    "false",
];

/// Render an identifier safe for Zig: a reserved word (or a name that is not a
/// valid bare identifier) is wrapped in the `@"..."` quoted form.
fn zig_ident(name: &str) -> String {
    let valid_bare = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().unwrap().is_ascii_digit();
    if ZIG_KEYWORDS.contains(&name) || !valid_bare {
        format!("@\"{}\"", zig_escape(name))
    } else {
        name.to_string()
    }
}

/// Escape a string for inclusion inside a Zig double-quoted literal.
fn zig_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

// ---- embedded helper files ------------------------------------------------

/// CsilDecimal: the exact base-10 `decimal` core type (CBOR tag 4, a two-element
/// `[exponent, mantissa]` array). Self-contained; the host needs no decimal lib.
const CSIL_DECIMAL_ZIG: &str = "\
// Generated CSIL exact-decimal helper.
// Code generated by csilgen; DO NOT EDIT.

/// CsilDecimal is the exact, base-10 `decimal` core type. On the wire it is CBOR
/// tag 4 (decimal fraction): a two-element array [exponent, mantissa] whose value
/// is mantissa * 10^exponent. The value is kept as exact integers, never a float,
/// so no precision is lost. A host needing arbitrary precision can widen mantissa
/// to a bignum; the 64-bit form covers the common case without a dependency.
pub const CsilDecimal = struct {
    exponent: i64,
    mantissa: i64,
};
";

/// CsilTimestamp: the `timestamp` core type (CBOR tag 0, RFC3339 UTC text). Kept
/// as the canonical RFC3339 string plus its epoch-seconds value for comparison.
const CSIL_TIMESTAMP_ZIG: &str = "\
// Generated CSIL timestamp helper.
// Code generated by csilgen; DO NOT EDIT.

/// CsilTimestamp is the `timestamp` core type: CBOR tag 0, an RFC3339 UTC string
/// on the wire. The canonical text is retained verbatim (so a round-trip is
/// byte-stable) alongside its epoch-seconds value for ordering comparisons.
pub const CsilTimestamp = struct {
    rfc3339: []const u8,
    epoch_seconds: i64,
};
";

#[cfg(test)]
mod tests;
