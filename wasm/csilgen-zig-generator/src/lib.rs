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
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, WarningLevel,
    WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
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

    // Per-type CBOR (de)serializers make the generated structs usable over the wire
    // without a hand-written codec; the typed client below is built on them.
    if let Some(codec) = generate_codec(&input, &config, &mut warnings) {
        files.push(GeneratedFile {
            path: make_path("codec.gen.zig"),
            content: codec,
        });
    }

    // A package's `genquickstart.md` demonstrates both the calling side (the RPC and
    // Datagrams sections, over `client.gen.zig`) and the handling side (the Events
    // section, over the `server.gen.zig` channel router), so a package must carry both
    // surfaces for its own quickstart to compile against the single emitted package —
    // regardless of which surface the sub-target requested. A flat (non-package) build
    // stays byte-identical: it emits only the requested surface.
    let pkg_mode = emit_packages_includes_zig(&input.config.options);
    let want_client =
        matches!(surface, Surface::Client) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    let want_server =
        matches!(surface, Surface::Server) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    if input.csil_spec.service_count > 0 {
        if want_client && let Some(client) = generate_client(&input) {
            files.push(GeneratedFile {
                path: make_path("client.gen.zig"),
                content: client,
            });
        }
        if want_server && let Some(server) = generate_server(&input, &mut warnings) {
            files.push(GeneratedFile {
                path: make_path("server.gen.zig"),
                content: server,
            });
        }
    }

    // Self-contained publishable-package mode: when `emit_packages` includes "zig",
    // emit a README with a copy-paste CSIL-RPC Quickstart alongside the source, so the
    // OUTPUT directory documents how to drive the generated client end to end.
    // `emit_readme` defaults to true; only an explicit `false` suppresses the
    // README, so a typo or missing value never silently drops the docs.
    if pkg_mode
        && input
            .config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false)
    {
        files.push(GeneratedFile {
            path: "genquickstart.md".to_string(),
            content: package_readme(&input),
        });
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

// ---- codec ----------------------------------------------------------------

/// A record field the codec emits, paired with the byte form of its CBOR text key
/// so fields are ordered canonically (by the bytewise order of their encoded keys)
/// at generation time rather than at runtime.
struct CodecField<'a> {
    name: String,
    key_bytes: Vec<u8>,
    value_type: &'a CsilTypeExpression,
    optional: bool,
}

/// The CBOR encoding of a text key (major type 3 head + UTF-8 bytes). Comparing
/// these byte vectors lexicographically is exactly RFC 8949 §4.2.1 key ordering.
fn cbor_text_key_bytes(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let n = bytes.len() as u64;
    let mt = 3u8 << 5;
    let mut head = Vec::new();
    if n < 24 {
        head.push(mt | n as u8);
    } else if n < 0x100 {
        head.push(mt | 24);
        head.push(n as u8);
    } else if n < 0x10000 {
        head.push(mt | 25);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        head.push(mt | 26);
        head.extend_from_slice(&(n as u32).to_be_bytes());
    }
    head.extend_from_slice(bytes);
    head
}

/// The record fields a codec emits, in canonical key order. Entries with a
/// non-name key (a typed map key) are skipped, exactly as the struct emitter does.
fn codec_fields(group: &CsilGroupExpression) -> Vec<CodecField<'_>> {
    let mut fields: Vec<CodecField> = group
        .entries
        .iter()
        .filter_map(|entry| {
            entry_field_name(&entry.key).map(|name| CodecField {
                key_bytes: cbor_text_key_bytes(&name),
                name,
                value_type: &entry.value_type,
                optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
            })
        })
        .collect();
    fields.sort_by(|a, b| a.key_bytes.cmp(&b.key_bytes));
    fields
}

/// Whether a referenced name has a generated codec (records and enums do).
fn has_codec(name: &str, codec_names: &std::collections::HashSet<String>) -> bool {
    codec_names.contains(name)
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference (NOT a group or a choice, which are realized
/// as records, enums, or unions with their own handling). A field referencing one of
/// these has no codec of its own and would otherwise fall through to the null stub;
/// resolving it to its target lets the map/array/scalar branches code it correctly.
fn codec_aliases(input: &WasmGeneratorInput) -> HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((rule.name.clone(), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Follow transparent-alias references to the underlying type expression so a field or
/// value typed as a named alias (`StringInt64Map = {* text => int}`) is coded as its
/// target. Records/enums have a codec and are absent from `aliases`, so a reference to
/// one is returned unchanged for the `has_codec` arms to pick up.
fn resolve_alias<'a>(
    ty: &'a CsilTypeExpression,
    aliases: &'a HashMap<String, CsilTypeExpression>,
) -> &'a CsilTypeExpression {
    let mut cur = unwrap_constrained(ty);
    while let CsilTypeExpression::Reference(name) = cur {
        match aliases.get(name) {
            Some(next) => cur = unwrap_constrained(next),
            None => break,
        }
    }
    cur
}

/// Emit a statement that encodes the scalar/reference value `expr` of type `ty`.
fn emit_enc_value(
    out: &mut String,
    indent: &str,
    ty: &CsilTypeExpression,
    expr: &str,
    codec_names: &std::collections::HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => out.push_str(&format!("{indent}try w_int(out, {expr});\n")),
            "uint" => out.push_str(&format!("{indent}try w_uint(out, {expr});\n")),
            "bool" | "true" | "false" => {
                out.push_str(&format!("{indent}try w_bool(out, {expr});\n"))
            }
            "float" | "float64" | "double" => {
                out.push_str(&format!("{indent}try w_f64(out, {expr});\n"))
            }
            "float16" | "float32" => out.push_str(&format!("{indent}try w_f32(out, {expr});\n")),
            "text" | "tstr" => out.push_str(&format!("{indent}try w_text(out, {expr});\n")),
            "bytes" | "bstr" => out.push_str(&format!("{indent}try w_bytes(out, {expr});\n")),
            "timestamp" => out.push_str(&format!(
                "{indent}try w_tag(out, 0);\n{indent}try w_text(out, ({expr}).rfc3339);\n"
            )),
            "decimal" => out.push_str(&format!(
                "{indent}try w_tag(out, 4);\n{indent}try w_array_head(out, 2);\n\
                 {indent}try w_int(out, ({expr}).exponent);\n{indent}try w_int(out, ({expr}).mantissa);\n"
            )),
            "null" | "nil" | "undefined" | "any" => {
                out.push_str(&format!("{indent}try w_null(out);\n"))
            }
            other => {
                warnings.push(codec_warning(format!(
                    "zig codec: unsupported builtin `{other}` encoded as null"
                )));
                out.push_str(&format!("{indent}try w_null(out);\n"));
            }
        },
        CsilTypeExpression::Reference(name) if has_codec(name, codec_names) => {
            out.push_str(&format!("{indent}try enc_{name}(out, &({expr}));\n"))
        }
        // A reference to a transparent alias (`Uuid = text`) has no codec of its own;
        // encode it as its target. The named Zig type aliases the same underlying type
        // the scalar encoder expects, so the same `expr` flows through.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => emit_enc_value(
            out,
            indent,
            resolve_alias(ty, aliases),
            expr,
            codec_names,
            aliases,
            warnings,
        ),
        CsilTypeExpression::Reference(name) => {
            warnings.push(codec_warning(format!(
                "zig codec: `{name}` has no generated codec; encoded as null"
            )));
            out.push_str(&format!("{indent}try w_null(out);\n"));
        }
        // A `text / "a" / "b"` choice is just a text string on the wire.
        CsilTypeExpression::Choice(arms) if arms.iter().all(is_text_like) => {
            out.push_str(&format!("{indent}try w_text(out, {expr});\n"))
        }
        _ => {
            warnings.push(codec_warning(
                "zig codec: unrepresentable nested value encoded as null".to_string(),
            ));
            out.push_str(&format!("{indent}try w_null(out);\n"));
        }
    }
}

/// Emit a statement that decodes the scalar/reference value `src` (a `Value`) into
/// the lvalue `dst` of type `ty`.
// A buffer-writing emitter inherently carries positional context (sink, indent, the
// source/destination expressions) on top of the type and the two codec name sets, so
// it sits one over the lint's argument ceiling; bundling them would only obscure call
// sites that already pass each piece explicitly.
#[allow(clippy::too_many_arguments)]
fn emit_dec_value(
    out: &mut String,
    indent: &str,
    ty: &CsilTypeExpression,
    src: &str,
    dst: &str,
    codec_names: &std::collections::HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => out.push_str(&format!("{indent}{dst} = try as_i64({src});\n")),
            "uint" => out.push_str(&format!("{indent}{dst} = try as_u64({src});\n")),
            "bool" | "true" | "false" => {
                out.push_str(&format!("{indent}{dst} = try as_bool({src});\n"))
            }
            "float" | "float64" | "double" => {
                out.push_str(&format!("{indent}{dst} = try as_f64({src});\n"))
            }
            "float16" | "float32" => {
                out.push_str(&format!("{indent}{dst} = @floatCast(try as_f64({src}));\n"))
            }
            "text" | "tstr" => out.push_str(&format!("{indent}{dst} = try as_text({src});\n")),
            "bytes" | "bstr" => out.push_str(&format!("{indent}{dst} = try as_bytes({src});\n")),
            "timestamp" => out.push_str(&format!(
                "{indent}{dst} = .{{ .rfc3339 = try as_tagged_text({src}, 0), .epoch_seconds = 0 }};\n"
            )),
            "decimal" => out.push_str(&format!(
                "{indent}{{\n{indent}    const csil_d = try as_decimal({src});\n\
                 {indent}    {dst} = .{{ .exponent = csil_d.exp, .mantissa = csil_d.mant }};\n{indent}}}\n"
            )),
            "null" | "nil" | "undefined" | "any" => {
                out.push_str(&format!("{indent}{dst} = null;\n"))
            }
            other => {
                warnings.push(codec_warning(format!(
                    "zig codec: unsupported builtin `{other}` left default on decode"
                )));
                out.push_str(&format!("{indent}_ = {src};\n"));
            }
        },
        CsilTypeExpression::Reference(name) if has_codec(name, codec_names) => {
            out.push_str(&format!("{indent}try dec_{name}(alloc, {src}, &({dst}));\n"))
        }
        // A reference to a transparent alias decodes as its target; the unnamed value
        // the scalar decoder yields is assignable to the named alias-typed lvalue.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => emit_dec_value(
            out,
            indent,
            resolve_alias(ty, aliases),
            src,
            dst,
            codec_names,
            aliases,
            warnings,
        ),
        CsilTypeExpression::Reference(name) => {
            warnings.push(codec_warning(format!(
                "zig codec: `{name}` has no generated codec; left default on decode"
            )));
            out.push_str(&format!("{indent}_ = {src};\n"));
        }
        CsilTypeExpression::Choice(arms) if arms.iter().all(is_text_like) => {
            out.push_str(&format!("{indent}{dst} = try as_text({src});\n"))
        }
        _ => {
            warnings.push(codec_warning(
                "zig codec: unrepresentable nested value left default on decode".to_string(),
            ));
            out.push_str(&format!("{indent}_ = {src};\n"));
        }
    }
}

/// A generation-time codec warning (an unsupported field shape degraded to a null).
fn codec_warning(message: String) -> GeneratorWarning {
    GeneratorWarning {
        level: WarningLevel::Warning,
        message,
        location: None,
        suggestion: None,
    }
}

/// Emit the encode of one record field (key + value), honoring optionality and the
/// slice/map expansion. `member` is the (possibly quoted) Zig field identifier.
fn emit_enc_field(
    out: &mut String,
    field: &CodecField,
    codec_names: &std::collections::HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let member = zig_ident(&field.name);
    let key = zig_escape(&field.name);
    // Resolve transparent aliases so a field typed as a named map/array/scalar alias
    // is encoded through the same branch its inline form would take, not the stub.
    let base = resolve_alias(field.value_type, aliases);
    match base {
        CsilTypeExpression::Array { element_type, .. } => {
            if field.optional {
                out.push_str(&format!("    if (v.{member}) |csil_arr| {{\n"));
                out.push_str(&format!("        try w_text(out, \"{key}\");\n"));
                out.push_str("        try w_array_head(out, csil_arr.len);\n");
                out.push_str("        for (csil_arr) |csil_it| {\n");
                emit_enc_value(
                    out,
                    "            ",
                    element_type,
                    "csil_it",
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("        }\n    }\n");
            } else {
                out.push_str(&format!("    try w_text(out, \"{key}\");\n"));
                out.push_str(&format!("    try w_array_head(out, v.{member}.len);\n"));
                out.push_str(&format!("    for (v.{member}) |csil_it| {{\n"));
                emit_enc_value(
                    out,
                    "        ",
                    element_type,
                    "csil_it",
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("    }\n");
            }
        }
        CsilTypeExpression::Map { key: k, value, .. } => {
            if field.optional {
                out.push_str(&format!("    if (v.{member}) |csil_hm| {{\n"));
                out.push_str(&format!("        try w_text(out, \"{key}\");\n"));
                out.push_str("        try w_map_head(out, csil_hm.count());\n");
                out.push_str("        var csil_mi = csil_hm.iterator();\n");
                out.push_str("        while (csil_mi.next()) |csil_e| {\n");
                emit_enc_value(
                    out,
                    "            ",
                    k,
                    "csil_e.key_ptr.*",
                    codec_names,
                    aliases,
                    warnings,
                );
                emit_enc_value(
                    out,
                    "            ",
                    value,
                    "csil_e.value_ptr.*",
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("        }\n    }\n");
            } else {
                out.push_str("    {\n");
                out.push_str(&format!("        try w_text(out, \"{key}\");\n"));
                out.push_str(&format!(
                    "        try w_map_head(out, v.{member}.count());\n"
                ));
                out.push_str(&format!("        var csil_mi = v.{member}.iterator();\n"));
                out.push_str("        while (csil_mi.next()) |csil_e| {\n");
                emit_enc_value(
                    out,
                    "            ",
                    k,
                    "csil_e.key_ptr.*",
                    codec_names,
                    aliases,
                    warnings,
                );
                emit_enc_value(
                    out,
                    "            ",
                    value,
                    "csil_e.value_ptr.*",
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("        }\n    }\n");
            }
        }
        _ => {
            if field.optional {
                out.push_str(&format!("    if (v.{member}) |csil_x| {{\n"));
                out.push_str(&format!("        try w_text(out, \"{key}\");\n"));
                emit_enc_value(
                    out,
                    "        ",
                    base,
                    "csil_x",
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("    }\n");
            } else {
                out.push_str(&format!("    try w_text(out, \"{key}\");\n"));
                emit_enc_value(
                    out,
                    "    ",
                    base,
                    &format!("v.{member}"),
                    codec_names,
                    aliases,
                    warnings,
                );
            }
        }
    }
}

/// Emit the per-entry value computation for a map decode, binding `csil_val` to the
/// decoded value of type `value` so the loop can `put` it into the hashmap.
fn emit_map_value_decode(
    out: &mut String,
    indent: &str,
    value: &CsilTypeExpression,
    codec_names: &std::collections::HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match unwrap_constrained(value) {
        CsilTypeExpression::Reference(name) if has_codec(name, codec_names) => {
            out.push_str(&format!(
                "{indent}var csil_val: {} = undefined;\n",
                map_zig_type(value, "types.")
            ));
            out.push_str(&format!(
                "{indent}try dec_{name}(alloc, csil_kv.val, &csil_val);\n"
            ));
        }
        _ => {
            out.push_str(&format!(
                "{indent}var csil_val: {} = undefined;\n",
                map_zig_type(value, "types.")
            ));
            emit_dec_value(
                out,
                indent,
                value,
                "csil_kv.val",
                "csil_val",
                codec_names,
                aliases,
                warnings,
            );
        }
    }
}

/// Emit the decode of one record field into `out.<member>`, each wrapped in a block
/// so its locals (`csil_fv`, `csil_it`, `csil_kv`, …) never collide across fields.
fn emit_dec_field(
    out: &mut String,
    field: &CodecField,
    codec_names: &std::collections::HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let member = zig_ident(&field.name);
    let key = zig_escape(&field.name);
    // Resolve transparent aliases so a field typed as a named map/array/scalar alias
    // is decoded through the same branch its inline form would take, not the stub.
    let base = resolve_alias(field.value_type, aliases);
    match base {
        CsilTypeExpression::Array { element_type, .. } => {
            let elem = map_zig_type(element_type, "types.");
            out.push_str("    {\n");
            if field.optional {
                out.push_str(&format!("        if (mget(m, \"{key}\")) |csil_fv| {{\n"));
                out.push_str("            if (csil_fv != .array) return error.WrongType;\n");
                out.push_str(&format!(
                    "            const csil_tmp = try alloc.alloc({elem}, csil_fv.array.len);\n"
                ));
                out.push_str("            for (csil_fv.array, 0..) |csil_it, csil_i| {\n");
                emit_dec_value(
                    out,
                    "                ",
                    element_type,
                    "csil_it",
                    "csil_tmp[csil_i]",
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("            }\n");
                out.push_str(&format!("            out.{member} = csil_tmp;\n"));
                out.push_str(&format!(
                    "        }} else {{\n            out.{member} = null;\n        }}\n    }}\n"
                ));
            } else {
                out.push_str(&format!("        const csil_fv = try req(m, \"{key}\");\n"));
                out.push_str("        if (csil_fv != .array) return error.WrongType;\n");
                out.push_str(&format!(
                    "        out.{member} = try alloc.alloc({elem}, csil_fv.array.len);\n"
                ));
                out.push_str("        for (csil_fv.array, 0..) |csil_it, csil_i| {\n");
                emit_dec_value(
                    out,
                    "            ",
                    element_type,
                    "csil_it",
                    &format!("out.{member}[csil_i]"),
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("        }\n    }\n");
            }
        }
        CsilTypeExpression::Map { value, .. } => {
            let map_ty = map_zig_type(base, "types.");
            out.push_str("    {\n");
            if field.optional {
                out.push_str(&format!("        if (mget(m, \"{key}\")) |csil_fv| {{\n"));
                out.push_str("            if (csil_fv != .map) return error.WrongType;\n");
                out.push_str(&format!("            var csil_tmp: {map_ty} = .{{}};\n"));
                out.push_str("            for (csil_fv.map) |csil_kv| {\n");
                out.push_str("                const csil_k = try as_text(csil_kv.key);\n");
                emit_map_value_decode(
                    out,
                    "                ",
                    value,
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("                try csil_tmp.put(alloc, csil_k, csil_val);\n");
                out.push_str("            }\n");
                out.push_str(&format!("            out.{member} = csil_tmp;\n"));
                out.push_str(&format!(
                    "        }} else {{\n            out.{member} = null;\n        }}\n    }}\n"
                ));
            } else {
                out.push_str(&format!("        const csil_fv = try req(m, \"{key}\");\n"));
                out.push_str("        if (csil_fv != .map) return error.WrongType;\n");
                out.push_str(&format!("        out.{member} = .{{}};\n"));
                out.push_str("        for (csil_fv.map) |csil_kv| {\n");
                out.push_str("            const csil_k = try as_text(csil_kv.key);\n");
                emit_map_value_decode(out, "            ", value, codec_names, aliases, warnings);
                out.push_str(&format!(
                    "            try out.{member}.put(alloc, csil_k, csil_val);\n"
                ));
                out.push_str("        }\n    }\n");
            }
        }
        CsilTypeExpression::Reference(name) if field.optional && has_codec(name, codec_names) => {
            out.push_str("    {\n");
            out.push_str(&format!("        if (mget(m, \"{key}\")) |csil_fv| {{\n"));
            out.push_str(&format!(
                "            var csil_tmp: {} = undefined;\n",
                map_zig_type(base, "types.")
            ));
            out.push_str(&format!(
                "            try dec_{name}(alloc, csil_fv, &csil_tmp);\n"
            ));
            out.push_str(&format!("            out.{member} = csil_tmp;\n"));
            out.push_str(&format!(
                "        }} else {{\n            out.{member} = null;\n        }}\n    }}\n"
            ));
        }
        _ => {
            out.push_str("    {\n");
            if field.optional {
                out.push_str(&format!("        if (mget(m, \"{key}\")) |csil_fv| {{\n"));
                emit_dec_value(
                    out,
                    "            ",
                    base,
                    "csil_fv",
                    &format!("out.{member}"),
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str(&format!(
                    "        }} else {{\n            out.{member} = null;\n        }}\n    }}\n"
                ));
            } else {
                out.push_str(&format!("        const csil_fv = try req(m, \"{key}\");\n"));
                emit_dec_value(
                    out,
                    "        ",
                    base,
                    "csil_fv",
                    &format!("out.{member}"),
                    codec_names,
                    aliases,
                    warnings,
                );
                out.push_str("    }\n");
            }
        }
    }
}

/// Emit the encode + decode functions for one record type.
fn emit_record_codec(
    out: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    codec_names: &std::collections::HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let fields = codec_fields(group);
    let required = fields.iter().filter(|f| !f.optional).count();
    let optionals: Vec<&CodecField> = fields.iter().filter(|f| f.optional).collect();
    // Zig rejects an unused parameter: `v` is unused only for a field-less record,
    // and `alloc` only when no field needs heap (an all-scalar record). Aliases are
    // resolved first so a named map/array alias field is recognized as heap-using.
    let dec_uses_alloc = fields.iter().any(|f| {
        let resolved = resolve_alias(f.value_type, aliases);
        matches!(
            resolved,
            CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. }
        ) || matches!(resolved, CsilTypeExpression::Reference(n) if has_codec(n, codec_names))
    });

    out.push_str(&format!(
        "fn enc_{name}(out: *std.ArrayList(u8), v: *const types.{name}) CodecError!void {{\n"
    ));
    if fields.is_empty() {
        out.push_str("    _ = v;\n");
    }
    if optionals.is_empty() {
        out.push_str(&format!("    try w_map_head(out, {required});\n"));
    } else {
        out.push_str(&format!("    var csil_n: usize = {required};\n"));
        for field in &optionals {
            out.push_str(&format!(
                "    if (v.{} != null) csil_n += 1;\n",
                zig_ident(&field.name)
            ));
        }
        out.push_str("    try w_map_head(out, csil_n);\n");
    }
    for field in &fields {
        emit_enc_field(out, field, codec_names, aliases, warnings);
    }
    out.push_str("}\n\n");

    out.push_str(&format!(
        "fn dec_{name}(alloc: std.mem.Allocator, m: Value, out: *types.{name}) CodecError!void {{\n"
    ));
    if !dec_uses_alloc {
        out.push_str("    _ = alloc;\n");
    }
    // A field-less record never populates `out`, and Zig rejects the unused
    // parameter; discard it the same way `alloc` is handled above.
    if fields.is_empty() {
        out.push_str("    _ = out;\n");
    }
    out.push_str("    if (m != .map) return error.WrongType;\n");
    for field in &fields {
        emit_dec_field(out, field, codec_names, aliases, warnings);
    }
    out.push_str("}\n\n");
}

/// Emit the encode + decode for an enum type. The wire form is the variant's
/// original literal text, mapped through the generated `wire_name` on encode and an
/// explicit table on decode.
fn emit_enum_codec(out: &mut String, name: &str, variants: &[String]) {
    out.push_str(&format!(
        "fn enc_{name}(out: *std.ArrayList(u8), v: *const types.{name}) CodecError!void {{\n"
    ));
    out.push_str("    try w_text(out, v.wire_name());\n}\n\n");
    out.push_str(&format!(
        "fn dec_{name}(alloc: std.mem.Allocator, src: Value, out: *types.{name}) CodecError!void {{\n"
    ));
    out.push_str("    _ = alloc;\n");
    out.push_str("    const csil_s = try as_text(src);\n");
    for variant in variants {
        out.push_str(&format!(
            "    if (std.mem.eql(u8, csil_s, \"{}\")) {{\n        out.* = .{};\n        return;\n    }}\n",
            zig_escape(variant),
            zig_ident(&to_snake(variant))
        ));
    }
    out.push_str("    return error.WrongType;\n}\n\n");
}

fn generate_codec(
    input: &WasmGeneratorInput,
    config: &ZigConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Option<String> {
    let typed: Vec<(&str, TypeKind)> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| classify_rule(&r.rule_type).map(|k| (r.name.as_str(), k)))
        .collect();
    let codec_names: std::collections::HashSet<String> = typed
        .iter()
        .filter(|(_, k)| matches!(k, TypeKind::Struct(_) | TypeKind::Enum(_)))
        .map(|(n, _)| n.to_string())
        .collect();
    if codec_names.is_empty() {
        return None;
    }
    let aliases = codec_aliases(input);

    let mut bodies = String::new();
    let mut public = String::new();
    for (name, kind) in &typed {
        match kind {
            TypeKind::Struct(group) => {
                emit_record_codec(&mut bodies, name, group, &codec_names, &aliases, warnings)
            }
            TypeKind::Enum(variants) => emit_enum_codec(&mut bodies, name, variants),
            _ => continue,
        }
        // Public, ergonomic per-type wrappers: encode to a caller-freed slice; decode
        // into a typed value whose strings/slices live in the caller's allocator (use
        // an arena and free everything in one shot).
        public.push_str(&format!(
            "/// Encode a {name} to CBOR. The returned slice is owned by the caller\n\
             /// (free it with alloc.free).\n"
        ));
        public.push_str(&format!(
            "pub fn encode_{name}(alloc: std.mem.Allocator, v: *const types.{name}) CodecError![]u8 {{\n\
             \x20   var out = std.ArrayList(u8).init(alloc);\n\
             \x20   errdefer out.deinit();\n\
             \x20   try enc_{name}(&out, v);\n\
             \x20   return out.toOwnedSlice();\n}}\n\n"
        ));
        public.push_str(&format!(
            "/// Decode CBOR into a {name}. Every string/slice/map inside `out` is\n\
             /// allocated from `alloc`; pass an arena and free it all at once.\n"
        ));
        public.push_str(&format!(
            "pub fn decode_{name}(alloc: std.mem.Allocator, bytes: []const u8, out: *types.{name}) CodecError!void {{\n\
             \x20   const root = try decode(alloc, bytes);\n\
             \x20   try dec_{name}(alloc, root, out);\n}}\n\n"
        ));
    }

    let mut content = String::new();
    file_header(
        &mut content,
        "Generated CBOR (de)serializers for the CSIL value types.",
    );
    content.push_str("const std = @import(\"std\");\n");
    content.push_str("const types = @import(\"types.gen.zig\");\n\n");
    let _ = config;
    content.push_str(CSIL_CODEC_RUNTIME_ZIG);
    content.push('\n');
    content.push_str(&bodies);
    content.push_str(&public);
    Some(content)
}

// ---- client ---------------------------------------------------------------

/// The carrier seam every generated call delegates to: the host implements `call`,
/// performing the raw byte round-trip for `(service, op)`. The generated client
/// owns serialization (it encodes the typed request and decodes the typed
/// response); the carrier only moves bytes, exactly as in the other languages. The
/// carrier allocates the response with the passed allocator so the client frees it.
const CLIENT_PRELUDE_ZIG: &str = "\
/// CsilgenTransport is the caller-supplied byte carrier: it performs the call named
/// by (service, op) with the already-encoded req bytes (CBOR over HTTP, say) and
/// returns the response bytes, allocated with `alloc` so the generated client frees
/// them. The generator owns serialization; the carrier only moves bytes.
pub const CsilgenTransport = struct {
    ptr: *anyopaque,
    call: *const fn (ptr: *anyopaque, alloc: std.mem.Allocator, service: []const u8, op: []const u8, req: []const u8) anyerror![]u8,
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
    content.push_str("const std = @import(\"std\");\n");
    content.push_str("const types = @import(\"types.gen.zig\");\n");
    content.push_str("const codec = @import(\"codec.gen.zig\");\n\n");
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
        let resp_type = map_zig_type(&success_type(&op.output_type), "types.");
        let resp_codec = type_codec_name(&success_type(&op.output_type));
        let has_input = !op_input_is_null(&op.input_type);
        let req_type = map_zig_type(&op.input_type, "types.");
        let req_codec = type_codec_name(&op.input_type);

        content.push_str(&format!(
            "\n    /// Invoke {wire_service}/{wire_op} with a typed request, returning the decoded\n\
             \x20   /// typed response. Everything in `out` is allocated from `alloc`; pass an arena\n\
             \x20   /// and free it once when done.\n"
        ));
        if has_input {
            content.push_str(&format!(
                "    pub fn {method}(self: {client}, alloc: std.mem.Allocator, req: *const {req_type}, out: *{resp_type}) anyerror!void {{\n"
            ));
            content.push_str(&format!(
                "        const csil_reqb = try codec.encode_{req_codec}(alloc, req);\n"
            ));
            content.push_str("        defer alloc.free(csil_reqb);\n");
        } else {
            content.push_str(&format!(
                "    pub fn {method}(self: {client}, alloc: std.mem.Allocator, out: *{resp_type}) anyerror!void {{\n"
            ));
            content.push_str("        const csil_reqb: []const u8 = &.{};\n");
        }
        content.push_str(&format!(
            "        const csil_respb = try self.transport.call(self.transport.ptr, alloc, \"{wire_service}\", \"{wire_op}\", csil_reqb);\n"
        ));
        content.push_str("        defer alloc.free(csil_respb);\n");
        content.push_str(&format!(
            "        try codec.decode_{resp_codec}(alloc, csil_respb, out);\n"
        ));
        content.push_str("    }\n");
    }
    content.push_str("};\n\n");
}

/// The codec base name for an operation input/output type reference.
fn type_codec_name(type_expr: &CsilTypeExpression) -> String {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Reference(name) => name.clone(),
        CsilTypeExpression::Builtin(name) => name.clone(),
        _ => "void".to_string(),
    }
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

// ---- self-contained package (README + Quickstart) -------------------------

/// True only when the `emit_packages` generation option is an array containing the
/// `"zig"` token. Parsed defensively against an arbitrary `serde_json::Value`: a
/// missing option, a non-array value, or an array without `"zig"` all leave the
/// output as source-only. The match is case-insensitive to be forgiving.
fn emit_packages_includes_zig(options: &HashMap<String, serde_json::Value>) -> bool {
    options
        .get("emit_packages")
        .and_then(|v| v.as_array())
        .is_some_and(|tokens| {
            tokens
                .iter()
                .filter_map(|v| v.as_str())
                .any(|token| token.eq_ignore_ascii_case("zig"))
        })
}

/// The package display name: an explicit `package_name` option wins; otherwise it
/// is derived from the first service's wire base (`AuthService` -> `auth-client`),
/// falling back to a generic client name for a service-less spec.
fn package_name(input: &WasmGeneratorInput) -> String {
    if let Some(name) = input
        .config
        .options
        .get("package_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // A path-style `package_name` is the cross-ecosystem source of truth; Zig
        // wants only its tail. See `package_name_last_segment`.
        return csilgen_common::package_name_last_segment(name).to_string();
    }
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(_) = &rule.rule_type {
            let base = service_base(&rule.name).to_lowercase();
            if !base.is_empty() {
                return format!("{base}-client");
            }
        }
    }
    "csilgen-client".to_string()
}

/// Which transport sections to render in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or
/// empty value (or one naming none of the three) means "all three". Sections always
/// render in a fixed order so the document reads the same regardless of the subset.
fn wanted_transports(input: &WasmGeneratorInput) -> (bool, bool, bool) {
    let listed = match input.config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
            let any_known = ["rpc", "events", "datagrams"]
                .iter()
                .any(|t| names.contains(t));
            if any_known {
                Some((
                    names.contains("rpc"),
                    names.contains("events"),
                    names.contains("datagrams"),
                ))
            } else {
                None
            }
        }
        _ => None,
    };
    listed.unwrap_or((true, true, true))
}

/// The pieces a unary (`->`) example call needs: the typed client + method, a
/// compiling request literal, the response type, and — for the datagram section —
/// the request/response codec names and the op's datagram ordinal.
struct ZigUnaryExample {
    client_type: String,
    method: String,
    wire_service: String,
    wire_op: String,
    resp_type: String,
    has_request: bool,
    req_literal: String,
    resp_print_field: Option<String>,
    /// The input record's codec name (`encode_<X>`), or `None` for a null/non-record
    /// input — the datagram section needs a record request to encode.
    req_codec: Option<String>,
    /// The success output record's codec name (`decode_<X>`).
    res_codec: String,
    /// The op's datagram ordinal: its `@wire-id`, or `1` as a channel-agreed default.
    op_ord: u64,
}

/// The first service (declaration order) with a unidirectional op whose success
/// output is a record and whose input is null-or-record (matching the typed client's
/// own gating). `None` for a serviceless / non-record-op package.
fn first_unary_example(input: &WasmGeneratorInput) -> Option<ZigUnaryExample> {
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let null_input = op_input_is_null(&op.input_type);
            let res_codec = match record_ref_name(input, &success) {
                Some(name) => name,
                None => continue,
            };
            if !null_input && record_ref_name(input, &op.input_type).is_none() {
                continue;
            }
            let base = service_base(&rule.name);
            let has_request = !null_input;
            return Some(ZigUnaryExample {
                client_type: format!("{base}Client"),
                method: zig_ident(&to_snake(&op.name)),
                wire_service: base.to_lowercase(),
                wire_op: simple_pascal(&op.name),
                resp_type: map_zig_type(&success, "types."),
                has_request,
                req_literal: if has_request {
                    zig_request_literal(input, &op.input_type)
                } else {
                    String::new()
                },
                resp_print_field: first_text_field(input, &success),
                req_codec: record_ref_name(input, &op.input_type),
                res_codec,
                op_ord: op.wire_id.unwrap_or(1),
            });
        }
    }
    None
}

/// The pieces the Events session needs: the generated channel router, handler struct,
/// and push-encoder names, the inbound/outbound record types and their codec names,
/// the wire service/op, a sample outbound literal, and the inbound print field.
struct ZigChannelExample {
    service_wire: String,
    handlers_type: String,
    route_fn: String,
    encode_fn: String,
    method: String,
    wire_op: String,
    in_type: String,
    in_codec: String,
    out_type: String,
    out_codec: String,
    out_literal: String,
    in_print_field: Option<String>,
}

/// The first service (declaration order) with a `<->` op whose input and success
/// output are both records (so the generated router + push encoder + per-type codec
/// helpers exist). `None` when no service has a usable channel op — the Events section
/// then shows the handshake/heartbeat without dispatch wiring.
fn first_channel_example(input: &WasmGeneratorInput) -> Option<ZigChannelExample> {
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let (Some(in_codec), Some(out_codec)) = (
                record_ref_name(input, &op.input_type),
                record_ref_name(input, &success),
            ) else {
                continue;
            };
            let base = service_base(&rule.name);
            let prefix = to_snake(&base);
            let method = to_snake(&op.name);
            return Some(ZigChannelExample {
                service_wire: base.to_lowercase(),
                handlers_type: format!("{base}Handlers"),
                route_fn: format!("route_{prefix}_channel"),
                encode_fn: format!("encode_{prefix}_{method}"),
                method: zig_ident(&method),
                wire_op: simple_pascal(&op.name),
                in_type: map_zig_type(&op.input_type, "types."),
                in_codec,
                out_type: map_zig_type(&success, "types."),
                out_codec,
                out_literal: zig_request_literal(input, &success),
                in_print_field: first_text_field(input, &op.input_type),
            });
        }
    }
    None
}

/// The CSIL rule name a type reference names *if it resolves to a record*, else
/// `None` (a builtin, collection, or unknown reference has no per-type codec).
fn record_ref_name(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> Option<String> {
    let CsilTypeExpression::Reference(name) = unwrap_constrained(ty) else {
        return None;
    };
    find_record(input, name).map(|_| name.clone())
}

/// A compiling Zig struct literal for the request record's required fields: real
/// values for scalars, `"example"` for text, and `undefined` for shapes a generic
/// sample can't fabricate, so the snippet always compiles even where a user must
/// fill a value in.
fn zig_request_literal(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> String {
    let CsilTypeExpression::Reference(name) = unwrap_constrained(ty) else {
        return format!("{} = undefined", map_zig_type(ty, "types."));
    };
    let type_name = map_zig_type(ty, "types.");
    let Some(group) = find_record(input, name) else {
        return format!("{type_name}{{}}");
    };
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            entry_field_name(&e.key).map(|field| {
                format!(
                    ".{} = {}",
                    zig_ident(&field),
                    zig_sample_value(&e.value_type)
                )
            })
        })
        .collect();
    if fields.is_empty() {
        format!("{type_name}{{}}")
    } else {
        format!("{type_name}{{ {} }}", fields.join(", "))
    }
}

/// A single Zig value literal for `ty`, used inside a request struct literal.
fn zig_sample_value(ty: &CsilTypeExpression) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" | "true" | "false" => "false".to_string(),
            "int" | "nint" | "uint" => "0".to_string(),
            "float" | "float16" | "float32" | "float64" | "double" => "0.0".to_string(),
            _ => "undefined".to_string(),
        },
        _ => "undefined".to_string(),
    }
}

/// The first required text field of a record type reference, so an example can print
/// a typed value rather than just announcing success.
fn first_text_field(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> Option<String> {
    let CsilTypeExpression::Reference(name) = unwrap_constrained(ty) else {
        return None;
    };
    let group = find_record(input, name)?;
    group.entries.iter().find_map(|e| {
        let is_text = matches!(unwrap_constrained(&e.value_type), CsilTypeExpression::Builtin(n) if n == "text" || n == "tstr");
        if is_text && !matches!(e.occurrence, Some(CsilOccurrence::Optional)) {
            entry_field_name(&e.key).map(|f| zig_ident(&f))
        } else {
            None
        }
    })
}

/// The record a type reference names, if any. A `Name = { ... }` rule parses as
/// `TypeDef(Group(..))`, while a bare group rule is `GroupDef(..)`; both are records.
fn find_record<'a>(input: &'a WasmGeneratorInput, name: &str) -> Option<&'a CsilGroupExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter(|r| r.name == name)
        .find_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        })
}

/// The package README: a transport-by-transport Quickstart over the official
/// `csilgen_transport` Zig library. The generated codec owns CBOR (de)serialization
/// and the library owns the envelope/framing/lifecycle; the example supplies only a
/// *carrier* that moves bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events
/// over TLS, CSIL-Datagrams over UDP) is a complete example built on the library.
fn package_readme(input: &WasmGeneratorInput) -> String {
    let name = package_name(input);
    let mut out = format!(
        "# {name}\n\n\
         Generated by csilgen. A typed CSIL client in Zig: the generated codec owns CBOR\n\
         (de)serialization and the official `csilgen_transport` library owns the envelope,\n\
         framing, and connection lifecycle. You supply only a *carrier* that moves bytes, so\n\
         the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw UDP unchanged.\n\n\
         ## Install\n\n\
         Expose the transport library as the `csilgen_transport` module in your\n\
         `build.zig.zon` (not yet published — vendor `transports/zig/` or add it as a git\n\
         dependency for now), then vendor the generated `.zig` files alongside your code.\n\
         This package ships both surfaces: `client.gen.zig` (the RPC section) and\n\
         `server.gen.zig` (the Events channel router); both pull in `codec.gen.zig` +\n\
         `types.gen.zig` — so all three sections below build against this one directory.\n\n\
         ```sh\n\
         zig build\n\
         ```\n\n"
    );

    let (rpc, events, datagrams) = wanted_transports(input);
    let unary = first_unary_example(input);
    let channel = first_channel_example(input);
    if rpc {
        out.push_str(&rpc_section(unary.as_ref()));
    }
    if events {
        out.push_str(&events_section(channel.as_ref()));
    }
    if datagrams {
        out.push_str(&datagrams_section(unary.as_ref()));
    }
    out
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `CsilgenTransport` byte
/// seam that builds the request with the library's `RpcRequest` and decodes its
/// `RpcResponse` (never hand-rolled), POSTing to `{base_url}/csil/v1/rpc` over
/// `std.http`. A non-zero transport status and the typed `ServiceError` arm are
/// surfaced distinctly; the typed client decodes success only.
fn rpc_section(ex: Option<&ZigUnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`); you\n\
         bring a carrier that moves bytes. The HTTP carrier below is just one example — swap\n\
         `std.http` for any client (it implements the generated `CsilgenTransport` seam).\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no record `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```zig\n");
    out.push_str(RPC_CARRIER_ZIG);
    out.push('\n');
    out.push_str(
        "pub fn main() !void {\n\
         \x20   var gpa = std.heap.GeneralPurposeAllocator(.{}){};\n\
         \x20   defer _ = gpa.deinit();\n\
         \x20   const alloc = gpa.allocator();\n\n\
         \x20   var carrier = HttpRpcCarrier{ .base_url = \"http://127.0.0.1:5080\" };\n",
    );
    out.push_str(&format!(
        "    const svc = client.{}.init(carrier.transport());\n\n",
        ex.client_type
    ));
    out.push_str(
        "    // Everything in `resp` is allocated from the arena; free it all at once.\n\
         \x20   var arena = std.heap.ArenaAllocator.init(alloc);\n\
         \x20   defer arena.deinit();\n\n",
    );
    out.push_str(&format!("    var resp: {} = undefined;\n", ex.resp_type));
    if ex.has_request {
        out.push_str(&format!("    const req = {};\n", ex.req_literal));
        out.push_str(&format!(
            "    try svc.{}(arena.allocator(), &req, &resp);\n",
            ex.method
        ));
    } else {
        out.push_str(&format!(
            "    try svc.{}(arena.allocator(), &resp);\n",
            ex.method
        ));
    }
    match &ex.resp_print_field {
        Some(field) => out.push_str(&format!(
            "    std.debug.print(\"{}/{} -> {{s}}\\n\", .{{resp.{field}}});\n",
            ex.wire_service, ex.wire_op
        )),
        None => out.push_str(&format!(
            "    std.debug.print(\"{}/{} ok\\n\", .{{}});\n",
            ex.wire_service, ex.wire_op
        )),
    }
    out.push_str("}\n```\n\n");
    out
}

/// The HTTP carrier preamble — spec-independent, so a constant. It builds the request
/// envelope with the library's `RpcRequest`, POSTs it to `{base_url}/csil/v1/rpc` with
/// `std.http.Client`, decodes the `RpcResponse` with the library, and hands the typed
/// payload bytes back to the generated client. A non-zero transport status and the
/// typed `ServiceError` arm become distinct errors.
const RPC_CARRIER_ZIG: &str = r#"const std = @import("std");
const csil = @import("csilgen_transport");
const client = @import("client.gen.zig");
const types = @import("types.gen.zig");

const rpc = csil.rpc;

// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (RpcRequest/RpcResponse); the carrier owns only the transport. Swap std.http for
// any HTTP client.
const HttpRpcCarrier = struct {
    base_url: []const u8, // e.g. "http://127.0.0.1:5080"

    fn transport(self: *HttpRpcCarrier) client.CsilgenTransport {
        return .{ .ptr = self, .call = call };
    }

    fn call(ptr: *anyopaque, alloc: std.mem.Allocator, service: []const u8, op: []const u8, req: []const u8) anyerror![]u8 {
        const self: *HttpRpcCarrier = @ptrCast(@alignCast(ptr));

        // Build the request envelope with the library (NOT hand-rolled).
        const body = try rpc.RpcRequest.init(service, op, req).encode(alloc);
        defer alloc.free(body);

        // POST it to {base_url}/csil/v1/rpc with the stdlib HTTP client.
        var http = std.http.Client{ .allocator = alloc };
        defer http.deinit();
        var url_buf: [512]u8 = undefined;
        const url = try std.fmt.bufPrint(&url_buf, "{s}/csil/v1/rpc", .{self.base_url});
        var resp_body = std.ArrayList(u8).init(alloc);
        defer resp_body.deinit();
        const result = try http.fetch(.{
            .location = .{ .url = url },
            .method = .POST,
            .payload = body,
            .extra_headers = &.{.{ .name = "content-type", .value = "application/cbor" }},
            .response_storage = .{ .dynamic = &resp_body },
        });
        if (result.status != .ok) return error.CsilRpcHttpStatus;

        // Decode the response envelope with the library; surface a non-zero transport
        // status and the typed ServiceError arm distinctly.
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const resp = try rpc.decode_rpc_response(arena.allocator(), resp_body.items);
        resp.as_transport_error() catch return error.CsilRpcTransportStatus;
        if (resp.variant) |variant| {
            if (std.mem.eql(u8, variant, "ServiceError")) return error.CsilRpcServiceError;
        }
        // Hand the typed payload bytes back to the generated client (it frees them).
        return alloc.dupe(u8, resp.payload);
    }
};
"#;

/// CSIL-Events over TLS: a full session. A TLS byte stream is wrapped as the library's
/// `FrameCarrier` (CSIL length-prefix framing); the session does the `$hello`/
/// `$hello-ack` handshake, sends one outbound event via the generated push encoder,
/// and runs a recv loop that decodes each frame to an `Event`, answers `$ping` with
/// `$pong`, and dispatches typed events into the generated channel router. With no
/// channel op the dispatch wiring is replaced by a note (handshake + heartbeat stay).
fn events_section(ch: Option<&ZigChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns\n\
         the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and framing; the\n\
         generated router dispatches typed events. The TLS carrier below is just one example —\n\
         a WebSocket/WebTransport/QUIC carrier drops in unchanged. (The Zig TLS setup is a\n\
         little longer than other languages because the cert bundle and handshake are\n\
         explicit.)\n\n",
    );
    out.push_str("```zig\n");
    match ch {
        Some(ch) => {
            out.push_str(EVENTS_PRELUDE_CHANNEL);
            out.push('\n');
            out.push_str(EVENTS_TLS_CARRIER_ZIG);
            out.push('\n');
            out.push_str(&events_channel_session(ch));
        }
        None => {
            out.push_str(EVENTS_PRELUDE_NOCHANNEL);
            out.push('\n');
            out.push_str(EVENTS_TLS_CARRIER_ZIG);
            out.push('\n');
            out.push_str(EVENTS_NOCHANNEL_SESSION_ZIG);
        }
    }
    out.push_str("```\n\n");
    out
}

/// The channel session body: a `CsilgenCodec` backed by the op's generated per-type
/// helpers, the typed handler, one outbound event via the generated push encoder, and
/// the recv loop that heartbeats and dispatches into the generated router.
fn events_channel_session(ch: &ZigChannelExample) -> String {
    let print = match &ch.in_print_field {
        Some(field) => format!(
            "    std.debug.print(\"event {} {{s}}\\n\", .{{msg.{field}}});\n",
            ch.method
        ),
        None => format!("    std.debug.print(\"event {}\\n\", .{{}});\n", ch.method),
    };
    format!(
        r#"// Back the generated router's CsilgenCodec with the per-type helpers: decode the
// inbound {in_type}, encode the outbound {out_type}.
const ChannelCodec = struct {{
    alloc: std.mem.Allocator,

    fn codec(self: *ChannelCodec) server.CsilgenCodec {{
        return .{{ .ptr = self, .decode = decode, .encode = encode }};
    }}
    fn decode(ptr: *anyopaque, data: []const u8, out: *anyopaque) anyerror!void {{
        const self: *ChannelCodec = @ptrCast(@alignCast(ptr));
        const typed: *{in_type} = @ptrCast(@alignCast(out));
        return codec_gen.decode_{in_codec}(self.alloc, data, typed);
    }}
    fn encode(ptr: *anyopaque, value: *const anyopaque) anyerror![]u8 {{
        const self: *ChannelCodec = @ptrCast(@alignCast(ptr));
        const typed: *const {out_type} = @ptrCast(@alignCast(value));
        return codec_gen.encode_{out_codec}(self.alloc, typed);
    }}
}};

fn onEvent(ctx: *anyopaque, msg: *const {in_type}) anyerror!void {{
    _ = ctx;
{print}}}

pub fn main() !void {{
    var gpa = std.heap.GeneralPurposeAllocator(.{{}}){{}};
    defer _ = gpa.deinit();
    const alloc = gpa.allocator();

    var tls_carrier = try openTlsCarrier(alloc, "localhost", 7443);
    const carrier = tls_carrier.carrier();
    defer carrier.close();

    // $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the
    // wire profile for the connection's lifetime.
    const versions = [_]u64{{csil.VERSION}};
    const profiles = [_][]const u8{{"verbose"}};
    const hello_frame = try (events.Hello{{ .versions = &versions, .profiles = &profiles, .service = "{service}" }}).encode(alloc);
    defer alloc.free(hello_frame);
    try carrier.send(hello_frame);

    const ack_frame = (try carrier.recv(alloc)) orelse return error.ClosedDuringHandshake;
    defer alloc.free(ack_frame);
    var ack_arena = std.heap.ArenaAllocator.init(alloc);
    defer ack_arena.deinit();
    const ack = try events.decode_hello_ack(ack_arena.allocator(), ack_frame);
    const profile = events.Profile.parse(ack.profile) orelse return error.UnknownProfile;

    // The router needs a CsilgenCodec for this channel's types; back it with the
    // generated per-type helpers.
    var channel_codec = ChannelCodec{{ .alloc = alloc }};
    const codec = channel_codec.codec();
    const handlers = server.{handlers_type}{{ .{method} = onEvent }};
    var ctx: u8 = 0;

    // Send one outbound event via the generated encoder, framed as a verbose Event.
    const out_msg = {out_literal};
    const out_bytes = try server.{encode_fn}(codec, &out_msg);
    defer alloc.free(out_bytes);
    const out_frame = try (events.Event.verbose("{service}", "{wire_op}", out_bytes)).encode(alloc, profile);
    defer alloc.free(out_frame);
    try carrier.send(out_frame);

    // Recv loop: decode each frame to an Event, answer $ping with $pong, dispatch the
    // rest to the generated router.
    while (try carrier.recv(alloc)) |frame| {{
        defer alloc.free(frame);
        var frame_arena = std.heap.ArenaAllocator.init(alloc);
        defer frame_arena.deinit();
        const ev = try events.decode_event(frame_arena.allocator(), frame, profile);
        const name = ev.event orelse continue;
        if (std.mem.eql(u8, name, events.PING_NAME)) {{
            const ping = try events.decode_heartbeat(frame_arena.allocator(), ev.payload);
            const pong_payload = try (events.Heartbeat{{ .nonce = ping.nonce }}).encode(alloc);
            defer alloc.free(pong_payload);
            const pong_frame = try (events.Event.verbose("{service}", events.PONG_NAME, pong_payload)).encode(alloc, profile);
            defer alloc.free(pong_frame);
            try carrier.send(pong_frame);
            continue;
        }}
        try server.{route_fn}(&handlers, &ctx, codec, name, ev.payload);
    }}
}}
"#,
        in_type = ch.in_type,
        out_type = ch.out_type,
        in_codec = ch.in_codec,
        out_codec = ch.out_codec,
        print = print,
        service = ch.service_wire,
        handlers_type = ch.handlers_type,
        method = ch.method,
        out_literal = ch.out_literal,
        encode_fn = ch.encode_fn,
        wire_op = ch.wire_op,
        route_fn = ch.route_fn,
    )
}

/// The Events imports when the spec has a channel op (the router + push encoder live
/// in `server.gen.zig`; the per-type helpers in `codec.gen.zig`).
const EVENTS_PRELUDE_CHANNEL: &str = r#"const std = @import("std");
const csil = @import("csilgen_transport");
const server = @import("server.gen.zig");
const codec_gen = @import("codec.gen.zig");
const types = @import("types.gen.zig");

const events = csil.events;
const carrier_seam = csil.carrier;
"#;

/// The Events imports when the spec has no channel op (no router/codec needed — the
/// session shows only handshake + heartbeat).
const EVENTS_PRELUDE_NOCHANNEL: &str = r#"const std = @import("std");
const csil = @import("csilgen_transport");

const events = csil.events;
const carrier_seam = csil.carrier;
"#;

/// The TLS `FrameCarrier` adapter — spec-independent. It length-prefixes each outbound
/// frame and reads one length-prefixed frame per `recvFrame`, exposing the library's
/// `FrameCarrier` seam over a `std.crypto.tls.Client` so the session logic is
/// transport-agnostic. The lib's stream-carrier helpers take a plain `std.net.Stream`,
/// so the TLS variant re-implements the same 4-byte framing over `tls.Client`.
const EVENTS_TLS_CARRIER_ZIG: &str = r#"// One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
const TlsFrameCarrier = struct {
    stream: std.net.Stream,
    tls_client: std.crypto.tls.Client,
    ca: std.crypto.Certificate.Bundle,
    max_frame: usize = csil.conventions.MAX_FRAME_DEFAULT,

    fn carrier(self: *TlsFrameCarrier) carrier_seam.FrameCarrier {
        return .{ .ptr = self, .vtable = &vtable };
    }

    const vtable = carrier_seam.FrameCarrier.VTable{ .send_frame = sendFrame, .recv_frame = recvFrame, .close = closeFn };

    fn sendFrame(ptr: *anyopaque, frame: []const u8) carrier_seam.CarrierError!void {
        const self: *TlsFrameCarrier = @ptrCast(@alignCast(ptr));
        if (frame.len > self.max_frame) return error.FrameTooLarge;
        var prefix: [4]u8 = undefined;
        std.mem.writeInt(u32, &prefix, @intCast(frame.len), .big);
        self.tls_client.writeAll(self.stream, &prefix) catch return error.Carrier;
        self.tls_client.writeAll(self.stream, frame) catch return error.Carrier;
    }
    fn recvFrame(ptr: *anyopaque, alloc: std.mem.Allocator) carrier_seam.CarrierError!?[]u8 {
        const self: *TlsFrameCarrier = @ptrCast(@alignCast(ptr));
        var prefix: [4]u8 = undefined;
        var got: usize = 0;
        while (got < prefix.len) {
            const n = self.tls_client.read(self.stream, prefix[got..]) catch return error.Carrier;
            if (n == 0) break;
            got += n;
        }
        if (got == 0) return null;
        if (got < prefix.len) return error.Carrier;
        const length: usize = @intCast(std.mem.readInt(u32, &prefix, .big));
        if (length > self.max_frame) return error.FrameTooLarge;
        const buf = try alloc.alloc(u8, length);
        errdefer alloc.free(buf);
        var off: usize = 0;
        while (off < buf.len) {
            const n = self.tls_client.read(self.stream, buf[off..]) catch return error.Carrier;
            if (n == 0) break;
            off += n;
        }
        if (off < buf.len) return error.Carrier;
        return buf;
    }
    fn closeFn(ptr: *anyopaque) void {
        const self: *TlsFrameCarrier = @ptrCast(@alignCast(ptr));
        self.stream.close();
    }
};

fn openTlsCarrier(alloc: std.mem.Allocator, host: []const u8, port: u16) !TlsFrameCarrier {
    const stream = try std.net.tcpConnectToHost(alloc, host, port);
    var ca = std.crypto.Certificate.Bundle{};
    try ca.rescan(alloc);
    const tls_client = try std.crypto.tls.Client.init(stream, .{
        .host = .{ .explicit = host },
        .ca = .{ .bundle = ca },
    });
    return .{ .stream = stream, .tls_client = tls_client, .ca = ca };
}
"#;

/// The Events session body when the spec declares no channel op: the handshake and
/// heartbeat still apply, so they are shown, with a note where the dispatch would go.
const EVENTS_NOCHANNEL_SESSION_ZIG: &str = r#"pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const alloc = gpa.allocator();

    var tls_carrier = try openTlsCarrier(alloc, "localhost", 7443);
    const carrier = tls_carrier.carrier();
    defer carrier.close();

    // $hello / $hello-ack handshake (control plane).
    const versions = [_]u64{csil.VERSION};
    const profiles = [_][]const u8{"verbose"};
    const hello_frame = try (events.Hello{ .versions = &versions, .profiles = &profiles }).encode(alloc);
    defer alloc.free(hello_frame);
    try carrier.send(hello_frame);

    const ack_frame = (try carrier.recv(alloc)) orelse return error.ClosedDuringHandshake;
    defer alloc.free(ack_frame);
    var ack_arena = std.heap.ArenaAllocator.init(alloc);
    defer ack_arena.deinit();
    const ack = try events.decode_hello_ack(ack_arena.allocator(), ack_frame);
    const profile = events.Profile.parse(ack.profile) orelse return error.UnknownProfile;

    // Recv loop: answer $ping with $pong. This package declares no <->/<- operations,
    // so there is no generated channel router to dispatch typed events into.
    while (try carrier.recv(alloc)) |frame| {
        defer alloc.free(frame);
        var frame_arena = std.heap.ArenaAllocator.init(alloc);
        defer frame_arena.deinit();
        const ev = try events.decode_event(frame_arena.allocator(), frame, profile);
        const name = ev.event orelse continue;
        if (std.mem.eql(u8, name, events.PING_NAME)) {
            const ping = try events.decode_heartbeat(frame_arena.allocator(), ev.payload);
            const pong_payload = try (events.Heartbeat{ .nonce = ping.nonce }).encode(alloc);
            defer alloc.free(pong_payload);
            const pong_frame = try (events.Event.verbose(null, events.PONG_NAME, pong_payload)).encode(alloc, profile);
            defer alloc.free(pong_frame);
            try carrier.send(pong_frame);
        }
    }
}
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in
/// the library's `Datagram`, and send it fire-and-forget over UDP. The recv path
/// decodes an inbound `Datagram` and decodes its payload into the RESPONSE type — with
/// an explicit note that there is NO synchronous response.
fn datagrams_section(ex: Option<&ZigUnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram` envelope;\n\
         you bring a datagram carrier. The UDP carrier below is one example — a WebRTC\n\
         unreliable DataChannel or QUIC datagrams drop in unchanged.\n\n",
    );
    let needs_record = ex.and_then(|e| e.req_codec.as_ref().map(|c| (e, c)));
    let Some((ex, req_codec)) = needs_record else {
        out.push_str(
            "This package declares no record `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    out.push_str("```zig\n");
    out.push_str(DATAGRAMS_UDP_CARRIER_ZIG);
    out.push('\n');
    let print = match &ex.resp_print_field {
        Some(field) => {
            format!("        std.debug.print(\"late response {{s}}\\n\", .{{resp.{field}}});\n")
        }
        None => "        std.debug.print(\"late response\\n\", .{});\n".to_string(),
    };
    out.push_str(&format!(
        r#"// The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
const OP_ORD: u64 = {op_ord};

pub fn main() !void {{
    var gpa = std.heap.GeneralPurposeAllocator(.{{}}){{}};
    defer _ = gpa.deinit();
    const alloc = gpa.allocator();

    var udp = try UdpDatagramCarrier.open("127.0.0.1", 9000);
    const carrier = udp.carrier();
    defer carrier.close();

    // Fire-and-forget: encode the `->` request and send it. seq 0 marks an
    // unsequenced datagram.
    const req = {req_literal};
    const payload = try codec_gen.encode_{req_codec}(alloc, &req);
    defer alloc.free(payload);
    const datagram = try datagrams.Datagram.init(OP_ORD, 0, payload).encode(alloc);
    defer alloc.free(datagram);
    try carrier.send(datagram);

    // Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is
    // NO synchronous response; the caller must tolerate loss and reordering and handle
    // a reply whenever (if ever) it shows up.
    var arena = std.heap.ArenaAllocator.init(alloc);
    defer arena.deinit();
    if (try carrier.recv(arena.allocator())) |inbound| {{
        const dg = try datagrams.decode_datagram(arena.allocator(), inbound);
        var resp: {resp_type} = undefined;
        try codec_gen.decode_{res_codec}(arena.allocator(), dg.payload, &resp);
{print}    }}
}}
"#,
        op_ord = ex.op_ord,
        req_literal = ex.req_literal,
        req_codec = req_codec,
        resp_type = ex.resp_type,
        res_codec = ex.res_codec,
        print = print,
    ));
    out.push_str("```\n\n");
    out
}

/// The UDP `DatagramCarrier` preamble — spec-independent. `send` writes one UDP packet;
/// `recv` reads the next inbound packet (or null). Datagrams are unreliable and
/// unordered, so the carrier never waits for or correlates a reply.
const DATAGRAMS_UDP_CARRIER_ZIG: &str = r#"const std = @import("std");
const csil = @import("csilgen_transport");
const codec_gen = @import("codec.gen.zig");
const types = @import("types.gen.zig");

const datagrams = csil.datagrams;
const carrier_seam = csil.carrier;

// One example carrier: CSIL-Datagrams over UDP (std.posix). Datagrams are unreliable
// and unordered, so the carrier never waits for or correlates a reply.
const UdpDatagramCarrier = struct {
    sock: std.posix.socket_t,
    peer: std.net.Address,

    fn open(host: []const u8, port: u16) !UdpDatagramCarrier {
        const addr = try std.net.Address.parseIp(host, port);
        const sock = try std.posix.socket(addr.any.family, std.posix.SOCK.DGRAM, std.posix.IPPROTO.UDP);
        return .{ .sock = sock, .peer = addr };
    }

    fn carrier(self: *UdpDatagramCarrier) carrier_seam.DatagramCarrier {
        return .{ .ptr = self, .vtable = &vtable };
    }

    const vtable = carrier_seam.DatagramCarrier.VTable{ .send_datagram = send, .recv_datagram = recv, .close = closeFn };

    fn send(ptr: *anyopaque, datagram: []const u8) carrier_seam.CarrierError!void {
        const self: *UdpDatagramCarrier = @ptrCast(@alignCast(ptr));
        _ = std.posix.sendto(self.sock, datagram, 0, &self.peer.any, self.peer.getOsSockLen()) catch return error.Carrier;
    }
    fn recv(ptr: *anyopaque, alloc: std.mem.Allocator) carrier_seam.CarrierError!?[]u8 {
        const self: *UdpDatagramCarrier = @ptrCast(@alignCast(ptr));
        var buf: [datagrams.MAX_DATAGRAM_DEFAULT]u8 = undefined;
        const n = std.posix.recv(self.sock, &buf, 0) catch return error.Carrier;
        if (n == 0) return null;
        const out = try alloc.alloc(u8, n);
        @memcpy(out, buf[0..n]);
        return out;
    }
    fn closeFn(ptr: *anyopaque) void {
        const self: *UdpDatagramCarrier = @ptrCast(@alignCast(ptr));
        std.posix.close(self.sock);
    }
};
"#;

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

// ---- embedded codec runtime -----------------------------------------------

/// The self-contained canonical-CBOR runtime the per-type codecs build on. Modeled
/// on the conformance-tested transport codec (`transports/zig/src/cbor.zig`), it
/// adds the float/bool/null items a payload may carry and the typed accessors the
/// generated decoders call. Unused private decls are legal in Zig, so emitting every
/// primitive regardless of which a spec uses costs nothing.
const CSIL_CODEC_RUNTIME_ZIG: &str = r#"// ===== self-contained canonical CBOR codec (RFC 8949 subset) =====

const CodecError = error{ Malformed, UnexpectedEof, TrailingBytes, WrongType, MissingField } || std.mem.Allocator.Error;

const Pair = struct { key: Value, val: Value };
const Tag = struct { num: u64, content: *Value };

const Value = union(enum) {
    uint: u64,
    int: i64,
    float: f64,
    boolean: bool,
    null: void,
    bytes: []const u8,
    text: []const u8,
    array: []Value,
    map: []Pair,
    tag: Tag,
};

fn write_head(out: *std.ArrayList(u8), major: u8, n: u64) std.mem.Allocator.Error!void {
    const mt: u8 = major << 5;
    if (n < 24) {
        try out.append(mt | @as(u8, @intCast(n)));
    } else if (n < (1 << 8)) {
        try out.append(mt | 24);
        try out.append(@intCast(n));
    } else if (n < (1 << 16)) {
        try out.append(mt | 25);
        var b: [2]u8 = undefined;
        std.mem.writeInt(u16, &b, @intCast(n), .big);
        try out.appendSlice(&b);
    } else if (n < (1 << 32)) {
        try out.append(mt | 26);
        var b: [4]u8 = undefined;
        std.mem.writeInt(u32, &b, @intCast(n), .big);
        try out.appendSlice(&b);
    } else {
        try out.append(mt | 27);
        var b: [8]u8 = undefined;
        std.mem.writeInt(u64, &b, n, .big);
        try out.appendSlice(&b);
    }
}

fn w_uint(out: *std.ArrayList(u8), n: u64) std.mem.Allocator.Error!void {
    try write_head(out, 0, n);
}

fn w_int(out: *std.ArrayList(u8), x: i64) std.mem.Allocator.Error!void {
    if (x >= 0) {
        try write_head(out, 0, @intCast(x));
    } else {
        const mag: u64 = @intCast(-(x + 1));
        try write_head(out, 1, mag);
    }
}

fn w_text(out: *std.ArrayList(u8), s: []const u8) std.mem.Allocator.Error!void {
    try write_head(out, 3, s.len);
    try out.appendSlice(s);
}

fn w_bytes(out: *std.ArrayList(u8), s: []const u8) std.mem.Allocator.Error!void {
    try write_head(out, 2, s.len);
    try out.appendSlice(s);
}

fn w_bool(out: *std.ArrayList(u8), v: bool) std.mem.Allocator.Error!void {
    try out.append(if (v) 0xf5 else 0xf4);
}

fn w_null(out: *std.ArrayList(u8)) std.mem.Allocator.Error!void {
    try out.append(0xf6);
}

fn w_array_head(out: *std.ArrayList(u8), n: usize) std.mem.Allocator.Error!void {
    try write_head(out, 4, @as(u64, @intCast(n)));
}

fn w_map_head(out: *std.ArrayList(u8), n: usize) std.mem.Allocator.Error!void {
    try write_head(out, 5, @as(u64, @intCast(n)));
}

fn w_tag(out: *std.ArrayList(u8), n: u64) std.mem.Allocator.Error!void {
    try write_head(out, 6, n);
}

fn w_f64(out: *std.ArrayList(u8), x: f64) std.mem.Allocator.Error!void {
    try out.append(0xfb);
    var b: [8]u8 = undefined;
    std.mem.writeInt(u64, &b, @bitCast(x), .big);
    try out.appendSlice(&b);
}

fn w_f32(out: *std.ArrayList(u8), x: f32) std.mem.Allocator.Error!void {
    try out.append(0xfa);
    var b: [4]u8 = undefined;
    std.mem.writeInt(u32, &b, @bitCast(x), .big);
    try out.appendSlice(&b);
}

const Decoded = struct { value: Value, consumed: usize };
const Arg = struct { arg: u64, head: usize };

fn read_arg(b: []const u8, low: u8) CodecError!Arg {
    if (low < 24) return .{ .arg = low, .head = 1 };
    switch (low) {
        24 => {
            if (b.len < 2) return error.UnexpectedEof;
            return .{ .arg = b[1], .head = 2 };
        },
        25 => {
            if (b.len < 3) return error.UnexpectedEof;
            return .{ .arg = std.mem.readInt(u16, b[1..][0..2], .big), .head = 3 };
        },
        26 => {
            if (b.len < 5) return error.UnexpectedEof;
            return .{ .arg = std.mem.readInt(u32, b[1..][0..4], .big), .head = 5 };
        },
        27 => {
            if (b.len < 9) return error.UnexpectedEof;
            return .{ .arg = std.mem.readInt(u64, b[1..][0..8], .big), .head = 9 };
        },
        else => return error.Malformed,
    }
}

fn half_to_f64(h: u16) f64 {
    const sign: u32 = @as(u32, h & 0x8000) << 16;
    var exp: u32 = (h >> 10) & 0x1f;
    var mant: u32 = h & 0x3ff;
    var bits: u32 = undefined;
    if (exp == 0) {
        if (mant == 0) {
            bits = sign;
        } else {
            exp = 127 - 15 + 1;
            while ((mant & 0x400) == 0) {
                mant <<= 1;
                exp -= 1;
            }
            mant &= 0x3ff;
            bits = sign | (exp << 23) | (mant << 13);
        }
    } else if (exp == 0x1f) {
        bits = sign | 0x7f800000 | (mant << 13);
    } else {
        bits = sign | ((exp + (127 - 15)) << 23) | (mant << 13);
    }
    return @as(f64, @as(f32, @bitCast(bits)));
}

fn decode_value(alloc: std.mem.Allocator, b: []const u8) CodecError!Decoded {
    if (b.len == 0) return error.UnexpectedEof;
    const ib = b[0];
    const major: u8 = ib >> 5;
    const low: u8 = ib & 0x1f;
    const h = try read_arg(b, low);
    const arg = h.arg;
    const n = h.head;
    switch (major) {
        0 => return .{ .value = .{ .uint = arg }, .consumed = n },
        1 => {
            if (arg > std.math.maxInt(i64)) return error.Malformed;
            return .{ .value = .{ .int = -1 - @as(i64, @intCast(arg)) }, .consumed = n };
        },
        2, 3 => {
            if (arg > b.len - n) return error.UnexpectedEof;
            const end = n + @as(usize, @intCast(arg));
            const slice = try alloc.dupe(u8, b[n..end]);
            const value: Value = if (major == 2) .{ .bytes = slice } else .{ .text = slice };
            return .{ .value = value, .consumed = end };
        },
        4 => {
            const count: usize = @intCast(arg);
            const items = try alloc.alloc(Value, count);
            var off = n;
            var i: usize = 0;
            while (i < count) : (i += 1) {
                const d = try decode_value(alloc, b[off..]);
                items[i] = d.value;
                off += d.consumed;
            }
            return .{ .value = .{ .array = items }, .consumed = off };
        },
        5 => {
            const count: usize = @intCast(arg);
            const pairs = try alloc.alloc(Pair, count);
            var off = n;
            var i: usize = 0;
            while (i < count) : (i += 1) {
                const k = try decode_value(alloc, b[off..]);
                off += k.consumed;
                const v = try decode_value(alloc, b[off..]);
                off += v.consumed;
                pairs[i] = .{ .key = k.value, .val = v.value };
            }
            return .{ .value = .{ .map = pairs }, .consumed = off };
        },
        6 => {
            const inner = try decode_value(alloc, b[n..]);
            const content = try alloc.create(Value);
            content.* = inner.value;
            return .{ .value = .{ .tag = .{ .num = arg, .content = content } }, .consumed = n + inner.consumed };
        },
        7 => switch (low) {
            20 => return .{ .value = .{ .boolean = false }, .consumed = n },
            21 => return .{ .value = .{ .boolean = true }, .consumed = n },
            22, 23 => return .{ .value = .{ .null = {} }, .consumed = n },
            25 => return .{ .value = .{ .float = half_to_f64(@intCast(arg)) }, .consumed = n },
            26 => return .{ .value = .{ .float = @as(f64, @as(f32, @bitCast(@as(u32, @intCast(arg))))) }, .consumed = n },
            27 => return .{ .value = .{ .float = @bitCast(arg) }, .consumed = n },
            else => return error.Malformed,
        },
        else => return error.Malformed,
    }
}

fn decode(alloc: std.mem.Allocator, b: []const u8) CodecError!Value {
    const d = try decode_value(alloc, b);
    if (d.consumed != b.len) return error.TrailingBytes;
    return d.value;
}

fn mget(m: Value, key: []const u8) ?Value {
    if (m != .map) return null;
    for (m.map) |p| {
        if (p.key == .text and std.mem.eql(u8, p.key.text, key)) return p.val;
    }
    return null;
}

fn req(m: Value, key: []const u8) CodecError!Value {
    return mget(m, key) orelse error.MissingField;
}

fn as_i64(v: Value) CodecError!i64 {
    return switch (v) {
        .int => |x| x,
        .uint => |x| if (x > std.math.maxInt(i64)) error.WrongType else @intCast(x),
        else => error.WrongType,
    };
}

fn as_u64(v: Value) CodecError!u64 {
    return switch (v) {
        .uint => |x| x,
        .int => |x| if (x < 0) error.WrongType else @intCast(x),
        else => error.WrongType,
    };
}

fn as_f64(v: Value) CodecError!f64 {
    return switch (v) {
        .float => |x| x,
        .int => |x| @floatFromInt(x),
        .uint => |x| @floatFromInt(x),
        else => error.WrongType,
    };
}

fn as_bool(v: Value) CodecError!bool {
    return switch (v) {
        .boolean => |x| x,
        else => error.WrongType,
    };
}

fn as_text(v: Value) CodecError![]const u8 {
    return switch (v) {
        .text => |s| s,
        else => error.WrongType,
    };
}

fn as_bytes(v: Value) CodecError![]const u8 {
    return switch (v) {
        .bytes => |s| s,
        else => error.WrongType,
    };
}

fn as_tagged_text(v: Value, num: u64) CodecError![]const u8 {
    if (v != .tag or v.tag.num != num or v.tag.content.* != .text) return error.WrongType;
    return v.tag.content.text;
}

const Decimal = struct { exp: i64, mant: i64 };

fn as_decimal(v: Value) CodecError!Decimal {
    if (v != .tag or v.tag.num != 4 or v.tag.content.* != .array or v.tag.content.array.len != 2) {
        return error.WrongType;
    }
    return .{
        .exp = try as_i64(v.tag.content.array[0]),
        .mant = try as_i64(v.tag.content.array[1]),
    };
}
"#;

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
