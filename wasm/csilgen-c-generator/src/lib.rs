//! C code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target c` from `csilgen_c_generator.wasm`. Emits
//! idiomatic C11: transparent structs for records, enum-tagged unions for
//! variants, a conditional `CsilDecimal`/`CsilTimestamp` helper, `csil_`-prefixed
//! client call-sites over a transport seam, and server handler structs with
//! verbose + compact router twins. The WASM-boundary exports mirror the other
//! generators exactly; only `process_generation` and its helpers are C-specific.

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
        name: "c-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "C code generator with service support".to_string(),
        target: "c".to_string(),
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

/// In-memory C type selected for the CSIL `decimal` core type. The wire form is
/// CBOR tag 4 either way; this only changes the emitted struct field type and
/// whether the self-contained helper header is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper (no third-party dependency).
    Csil,
    /// The host supplies the decimal type; no helper is emitted.
    Library,
}

#[derive(Debug)]
struct CConfig {
    output_subdir: String,
    decimal_mapping: DecimalMapping,
    generate_validation: bool,
}

impl CConfig {
    /// The C type a `decimal` field maps to. Both carry the identical CBOR tag-4
    /// wire value; only the in-memory type differs.
    fn decimal_c_type(&self) -> &'static str {
        // The host-supplied (library) decimal still needs a concrete spelling in
        // generated headers; CsilDecimal is the agreed name either way, but only
        // the csil mapping ships its definition.
        "CsilDecimal"
    }

    /// Parse options. An unknown `decimal_mapping` is a hard error so a typo
    /// surfaces at generation time rather than silently degrading (the
    /// validate-early idiom the Go generator uses).
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
    let config = CConfig::from_options(&input.config.options)?;
    let surface = match input.config.target.as_str() {
        "c" | "c-server" => Surface::Server,
        "c-client" => Surface::Client,
        "c-typesonly" => Surface::TypesOnly,
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
            path: make_path("csil_decimal.gen.h"),
            content: CSIL_DECIMAL_H.to_string(),
        });
    }
    // The timestamp helper (CBOR tag-0 RFC3339 UTC) is emitted only when used.
    if spec_uses_builtin(&input, "timestamp") {
        files.push(GeneratedFile {
            path: make_path("csil_timestamp.gen.h"),
            content: CSIL_TIMESTAMP_H.to_string(),
        });
    }

    if let Some(types) = generate_types(&input, &config) {
        files.push(GeneratedFile {
            path: make_path("types.gen.h"),
            content: types,
        });
    }

    if config.generate_validation
        && let Some(validation) = generate_validation(&input)
    {
        files.push(GeneratedFile {
            path: make_path("validation.gen.h"),
            content: validation,
        });
    }

    // Per-type CBOR (de)serializers make the generated structs usable over the wire
    // without a hand-written codec; the typed client below is built on them.
    if let Some(codec) = generate_codec(&input, &config, &mut warnings) {
        files.push(GeneratedFile {
            path: make_path("codec.gen.h"),
            content: codec,
        });
    }

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_client(&input, &config) {
                    files.push(GeneratedFile {
                        path: make_path("client.gen.h"),
                        content: client,
                    });
                }
            }
            Surface::Server => {
                if let Some(server) = generate_server(&input, &config, &mut warnings) {
                    files.push(GeneratedFile {
                        path: make_path("server.gen.h"),
                        content: server,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
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

fn header_open(content: &mut String, guard: &str, summary: &str, includes: &[String]) {
    content.push_str(&format!("/* {summary} */\n"));
    content.push_str("/* Code generated by csilgen; DO NOT EDIT. */\n");
    content.push_str(&format!("#ifndef {guard}\n#define {guard}\n\n"));
    for inc in includes {
        content.push_str(&format!("#include {inc}\n"));
    }
    if !includes.is_empty() {
        content.push('\n');
    }
}

fn header_close(content: &mut String, guard: &str) {
    // One blank line before the guard close, never a ragged stack of them: each
    // emitter ends its block with its own trailing newline, so trim back to a
    // single separator rather than letting them accumulate.
    while content.ends_with('\n') {
        content.pop();
    }
    content.push_str(&format!("\n\n#endif /* {guard} */\n"));
}

/// Render a C block comment, aligning every continuation line's `*` under the
/// opening `/*` — the layout clang-format produces and a reviewer expects. A
/// single-line input collapses to `/* text */`.
fn doc_comment(lines: &[&str]) -> String {
    match lines {
        [] => String::new(),
        [only] => format!("/* {only} */\n"),
        [first, rest @ ..] => {
            let (last, middle) = rest.split_last().unwrap();
            let mut s = format!("/* {first}\n");
            for line in middle {
                s.push_str(&format!(" * {line}\n"));
            }
            s.push_str(&format!(" * {last} */\n"));
            s
        }
    }
}

// ---- types ----------------------------------------------------------------

/// How a named type rule is realized in C, used to drive emission phases (enums
/// and aliases need no forward declaration; aggregates do, and must be defined in
/// value-dependency order so a by-value member never precedes its definition).
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

/// The names this entry embeds *by value* (so its definition must precede this
/// type's). Optional, array, and map members are pointers and impose no order.
fn entry_value_dep(entry: &CsilGroupEntry) -> Option<String> {
    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
        return None;
    }
    match unwrap_constrained(&entry.value_type) {
        CsilTypeExpression::Reference(n) => Some(n.clone()),
        _ => None,
    }
}

fn generate_types(input: &WasmGeneratorInput, config: &CConfig) -> Option<String> {
    // Gather the named type rules and their by-value dependencies.
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
    let mut forwards = String::new();
    let mut aliases = String::new();
    // Definitions are keyed by name so they can be emitted in topological order.
    let mut defs: HashMap<String, String> = HashMap::new();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (name, kind) in &typed {
        match kind {
            TypeKind::Enum(variants) => emit_enum(&mut enums, name, variants),
            TypeKind::Alias(t) => match unwrap_constrained(t) {
                // A named map alias (`StringInt64Map = {* text => int}`) gets a real
                // struct carrying parallel key/value arrays and a count, so a field of
                // that type can hold entries and round-trip — not the data-less `void *`
                // a bare map reference used to collapse to. Its members are pointers, so
                // a forward declaration covers any record it references by value.
                CsilTypeExpression::Map { key, value, .. } => {
                    forwards.push_str(&format!("typedef struct {name} {name};\n"));
                    let mut s = String::new();
                    emit_map_alias_struct(&mut s, name, key, value, config);
                    defs.insert(name.to_string(), s);
                    order.push(name.to_string());
                    deps.insert(name.to_string(), Vec::new());
                }
                // A named list alias (`Tags = [* text]`) likewise becomes an items+count
                // struct rather than a count-less bare pointer.
                CsilTypeExpression::Array { element_type, .. } => {
                    forwards.push_str(&format!("typedef struct {name} {name};\n"));
                    let mut s = String::new();
                    emit_list_alias_struct(&mut s, name, element_type, config);
                    defs.insert(name.to_string(), s);
                    order.push(name.to_string());
                    deps.insert(name.to_string(), Vec::new());
                }
                _ => {
                    aliases.push_str(&format!("/* {name} is a type alias. */\n"));
                    aliases.push_str(&format!(
                        "typedef {};\n\n",
                        declarator(&base_c_type(t, config), 0, name)
                    ));
                }
            },
            TypeKind::Struct(group) => {
                forwards.push_str(&format!("typedef struct {name} {name};\n"));
                let mut s = String::new();
                emit_struct(&mut s, name, group, config);
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
                forwards.push_str(&format!("typedef struct {name} {name};\n"));
                let mut s = String::new();
                emit_choice(&mut s, name, arms, config);
                defs.insert(name.to_string(), s);
                order.push(name.to_string());
                // Union members are by value, so every Reference arm must precede.
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
                forwards.push_str(&format!("typedef struct {name} {name};\n"));
                let mut s = String::new();
                emit_group_choice(&mut s, name, arms, config);
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
    let mut includes = vec![
        "<stdbool.h>".to_string(),
        "<stddef.h>".to_string(),
        "<stdint.h>".to_string(),
    ];
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(input, "decimal") {
        includes.push("\"csil_decimal.gen.h\"".to_string());
    }
    if spec_uses_builtin(input, "timestamp") {
        includes.push("\"csil_timestamp.gen.h\"".to_string());
    }
    header_open(
        &mut content,
        "CSILGEN_TYPES_GEN_H",
        "Generated CSIL value types.",
        &includes,
    );
    // A bytes field carries an explicit length, so opaque byte ranges use one
    // small typedef rather than a bare pointer the caller must size by hand.
    content.push_str(
        "typedef struct CsilBytes {\n    uint8_t *data;\n    size_t len;\n} CsilBytes;\n\n",
    );
    // Enums and forward declarations first so by-value members and pointer cycles
    // both resolve; then aliases; then the full definitions in dependency order.
    content.push_str(&enums);
    if !forwards.is_empty() {
        content
            .push_str("/* Forward declarations (resolve mutual and out-of-order references). */\n");
        content.push_str(&forwards);
        content.push('\n');
    }
    content.push_str(&aliases);
    content.push_str(&definitions);
    header_close(&mut content, "CSILGEN_TYPES_GEN_H");
    Some(content)
}

/// Emit the struct/union definitions in value-dependency order (Kahn's
/// algorithm). A by-value member must follow its type's definition; pointer
/// members do not constrain order because the forward declarations cover them. A
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
    content.push_str(&format!("/* {name} is an enumeration. */\n"));
    content.push_str(&format!("typedef enum {name} {{\n"));
    for variant in variants {
        content.push_str(&format!(
            "    {}_{},\n",
            to_upper_snake(name),
            to_upper_snake(variant)
        ));
    }
    content.push_str(&format!("}} {name};\n\n"));
}

fn emit_struct(content: &mut String, name: &str, group: &CsilGroupExpression, config: &CConfig) {
    content.push_str(&format!("/* {name} is a structured data type. */\n"));
    content.push_str(&format!("typedef struct {name} {{\n"));
    for entry in &group.entries {
        if let Some(field) = entry_field_name(&entry.key) {
            if let Some(description) = field_description(entry) {
                content.push_str(&format!("    /* {description} */\n"));
            }
            emit_field(
                content,
                &field,
                &entry.value_type,
                &entry.occurrence,
                config,
            );
        }
    }
    content.push_str(&format!("}} {name};\n\n"));
}

/// Emit the faithful struct a named map alias resolves to: parallel `keys`/`values`
/// arrays and a `count`, mirroring the inline-map field expansion but as a reusable
/// named type so a field can carry the entries and round-trip.
fn emit_map_alias_struct(
    content: &mut String,
    name: &str,
    key: &CsilTypeExpression,
    value: &CsilTypeExpression,
    config: &CConfig,
) {
    let k = base_c_type(key, config);
    let v = base_c_type(value, config);
    content.push_str(&format!("/* {name} is a named map alias. */\n"));
    content.push_str(&format!("typedef struct {name} {{\n"));
    content.push_str(&format!("    {};\n", declarator(&k, 1, "keys")));
    content.push_str(&format!("    {};\n", declarator(&v, 1, "values")));
    content.push_str("    size_t count;\n");
    content.push_str(&format!("}} {name};\n\n"));
}

/// Emit the faithful struct a named list alias resolves to: an `items` array and a
/// `count`, so a field of the alias type carries its elements rather than dropping
/// the length the way a bare count-less pointer did.
fn emit_list_alias_struct(
    content: &mut String,
    name: &str,
    element_type: &CsilTypeExpression,
    config: &CConfig,
) {
    let e = base_c_type(element_type, config);
    content.push_str(&format!("/* {name} is a named list alias. */\n"));
    content.push_str(&format!("typedef struct {name} {{\n"));
    content.push_str(&format!("    {};\n", declarator(&e, 1, "items")));
    content.push_str("    size_t count;\n");
    content.push_str(&format!("}} {name};\n\n"));
}

/// Emit one struct member (or the member group an array/map field expands to).
/// Snake_case CSIL field names map verbatim to C member names — the wire key is
/// already idiomatic C — so no case mangling happens here.
fn emit_field(
    content: &mut String,
    field: &str,
    value_type: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
    config: &CConfig,
) {
    let base = unwrap_constrained(value_type);
    match base {
        // A list field expands to a pointer + count pair, the idiomatic C
        // representation of a dynamically sized sequence.
        CsilTypeExpression::Array { element_type, .. } => {
            let elem = base_c_type(element_type, config);
            content.push_str(&format!("    {};\n", declarator(&elem, 1, field)));
            content.push_str(&format!("    size_t {field}_count;\n"));
        }
        // A map field expands to parallel key/value arrays + a count.
        CsilTypeExpression::Map { key, value, .. } => {
            let k = base_c_type(key, config);
            let v = base_c_type(value, config);
            content.push_str(&format!(
                "    {};\n",
                declarator(&k, 1, &format!("{field}_keys"))
            ));
            content.push_str(&format!(
                "    {};\n",
                declarator(&v, 1, &format!("{field}_values"))
            ));
            content.push_str(&format!("    size_t {field}_count;\n"));
        }
        _ => {
            let c_type = base_c_type(base, config);
            let optional = matches!(occurrence, Some(CsilOccurrence::Optional));
            // An optional pointer-typed field already encodes absence as NULL; an
            // optional value-typed field becomes a pointer so NULL means absent.
            let extra_ptr = usize::from(optional && !c_type.ends_with('*'));
            content.push_str(&format!("    {};\n", declarator(&c_type, extra_ptr, field)));
        }
    }
}

/// A non-enum `TypeChoice` is a discriminated union (the idiomatic C ADT): an
/// enum tag plus a payload union. (Text-literal choices are emitted as C enums by
/// `emit_enum` instead.)
fn emit_choice(content: &mut String, name: &str, arms: &[CsilTypeExpression], config: &CConfig) {
    content.push_str(&format!("/* {name} is a tagged union. */\n"));
    content.push_str(&format!("typedef enum {name}Tag {{\n"));
    for (i, arm) in arms.iter().enumerate() {
        content.push_str(&format!(
            "    {}_{},\n",
            to_upper_snake(name),
            to_upper_snake(&arm_name(arm, i))
        ));
    }
    content.push_str(&format!("}} {name}Tag;\n\n"));
    content.push_str(&format!("typedef struct {name} {{\n"));
    content.push_str(&format!("    {name}Tag tag;\n"));
    content.push_str("    union {\n");
    for (i, arm) in arms.iter().enumerate() {
        let c_type = base_c_type(arm, config);
        content.push_str(&format!(
            "        {};\n",
            declarator(&c_type, 0, &to_snake(&arm_name(arm, i)))
        ));
    }
    content.push_str("    } u;\n");
    content.push_str(&format!("}} {name};\n\n"));
}

/// A `GroupChoice` is a union over record shapes: each arm becomes its own struct
/// `<Name>Arm<N>`, tied together by an enum-tagged union.
fn emit_group_choice(
    content: &mut String,
    name: &str,
    arms: &[CsilGroupExpression],
    config: &CConfig,
) {
    for (i, arm) in arms.iter().enumerate() {
        emit_struct(content, &format!("{name}Arm{i}"), arm, config);
    }
    content.push_str(&format!("typedef enum {name}Tag {{\n"));
    for i in 0..arms.len() {
        content.push_str(&format!("    {}_ARM{i},\n", to_upper_snake(name)));
    }
    content.push_str(&format!("}} {name}Tag;\n\n"));
    content.push_str(&format!("typedef struct {name} {{\n"));
    content.push_str(&format!("    {name}Tag tag;\n"));
    content.push_str("    union {\n");
    for i in 0..arms.len() {
        content.push_str(&format!("        {name}Arm{i} arm{i};\n"));
    }
    content.push_str("    } u;\n");
    content.push_str(&format!("}} {name};\n\n"));
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
    header_open(
        &mut content,
        "CSILGEN_VALIDATION_GEN_H",
        "Generated validation predicates.",
        &[
            "<stdbool.h>".to_string(),
            "<string.h>".to_string(),
            "\"types.gen.h\"".to_string(),
        ],
    );
    content.push_str(&body);
    header_close(&mut content, "CSILGEN_VALIDATION_GEN_H");
    Some(content)
}

/// A `static inline bool <Type>_validate(const <Type> *v)` returning false on the
/// first failed check. `static inline` keeps it header-only without an
/// unused-function warning when a translation unit ignores it. The function is
/// emitted only when at least one check line is produced, so a type with only
/// unsupported constraints does not leave `v` unused.
fn emit_validate_fn(content: &mut String, name: &str, group: &CsilGroupExpression) {
    let mut checks = String::new();
    for entry in &group.entries {
        if let Some(field) = entry_field_name(&entry.key) {
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            for metadata in &entry.metadata {
                if let CsilFieldMetadata::Constraint(constraint) = metadata {
                    emit_metadata_check(&mut checks, &field, optional, constraint);
                }
            }
            if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                for op in constraints {
                    emit_control_check(&mut checks, &field, optional, op);
                }
            }
        }
    }
    if checks.is_empty() {
        return;
    }
    content.push_str(&format!(
        "/* {name}_validate returns false on the first failed constraint. */\n"
    ));
    content.push_str(&format!(
        "static inline bool {name}_validate(const {name} *v) {{\n"
    ));
    content.push_str(&checks);
    content.push_str("    return true;\n}\n\n");
}

/// A string-length check against a NUL-terminated field; the NULL guard covers
/// both an absent optional and an unset required pointer.
fn text_check(out: &mut String, field: &str, op: &str, n: u64) {
    out.push_str(&format!(
        "    if (v->{field} != NULL && strlen(v->{field}) {op} {n}u) return false;\n"
    ));
}

/// A numeric comparison. The read is cast to int64_t so the comparison never
/// trips -Wsign-compare on an unsigned field; an optional field is a pointer, so
/// it is NULL-guarded and dereferenced.
fn numeric_check(out: &mut String, field: &str, optional: bool, op: &str, n: i64) {
    if optional {
        out.push_str(&format!(
            "    if (v->{field} != NULL && (int64_t)(*v->{field}) {op} {n}) return false;\n"
        ));
    } else {
        out.push_str(&format!(
            "    if ((int64_t)v->{field} {op} {n}) return false;\n"
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
        CsilValidationConstraint::MinLength(n) => text_check(out, field, "<", *n),
        CsilValidationConstraint::MaxLength(n) => text_check(out, field, ">", *n),
        CsilValidationConstraint::MinItems(n) => {
            out.push_str(&format!("    if (v->{field}_count < {n}u) return false;\n"))
        }
        CsilValidationConstraint::MaxItems(n) => {
            out.push_str(&format!("    if (v->{field}_count > {n}u) return false;\n"))
        }
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
        CsilControlOperator::Size(CsilSizeConstraint::Min(n)) => text_check(out, field, "<", *n),
        CsilControlOperator::Size(CsilSizeConstraint::Max(n)) => text_check(out, field, ">", *n),
        CsilControlOperator::Size(CsilSizeConstraint::Exact(n)) => text_check(out, field, "!=", *n),
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
/// at generation time, never at runtime — the wire map comes out deterministic
/// without a runtime sort.
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

/// The C string-literal length a `csilc_w_text` call passes for a key, counting
/// UTF-8 bytes (CBOR text length is byte length, not character count).
fn key_len(name: &str) -> usize {
    name.len()
}

/// What a referenced type name can resolve to during codec emission: the names with a
/// generated codec (records and enums) and the transparent aliases a reference
/// resolves through. Bundled so the recursive value emitters keep a sane arity.
struct CodecScope<'a> {
    names: &'a std::collections::HashSet<String>,
    aliases: &'a std::collections::HashMap<String, CsilTypeExpression>,
}

impl CodecScope<'_> {
    /// Whether a referenced name has a generated codec (records and enums do). A
    /// reference to anything else (a transparent alias, a union) is resolved or
    /// degraded to a warned placeholder so the output still compiles.
    fn has_codec(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a scalar / reference (NOT a record group, choice, map, or list). A field
/// referencing one carries no codec of its own, so the codec must encode it as its
/// underlying type rather than the `null` stub a bare non-record reference yields —
/// otherwise a `Uuid = text` field is silently dropped. Map and list aliases are
/// excluded here because they are now codec'd types in their own right (each gets a
/// `csilc_enc_*`/`csilc_dec_*`), reached via the record/codec-name check first.
fn codec_aliases(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match unwrap_constrained(t) {
                CsilTypeExpression::Group(_)
                | CsilTypeExpression::Choice(_)
                | CsilTypeExpression::Map { .. }
                | CsilTypeExpression::Array { .. } => None,
                _ => Some((rule.name.clone(), t.clone())),
            },
            _ => None,
        })
        .collect()
}

/// For an alias `TypeKind`, its target when that target is a codec'd aggregate (a map
/// or a list), distinguishing those — which now carry their own `csilc_enc_*`/`dec_*`
/// — from transparent scalar/reference aliases that resolve through to an underlying
/// codec.
fn alias_aggregate<'a>(kind: &'a TypeKind<'a>) -> Option<&'a CsilTypeExpression> {
    if let TypeKind::Alias(t) = kind {
        match unwrap_constrained(t) {
            agg @ (CsilTypeExpression::Map { .. } | CsilTypeExpression::Array { .. }) => Some(agg),
            _ => None,
        }
    } else {
        None
    }
}

/// Emit the statements that encode a single scalar/reference value `expr` of type
/// `ty` into the buffer `b`. Arrays and maps are handled by the field emitter, not
/// here; a nested array/map (an unrepresentable element shape) degrades to a null.
fn emit_enc_value(
    out: &mut String,
    indent: &str,
    ty: &CsilTypeExpression,
    expr: &str,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => out.push_str(&format!(
                "{indent}if (csilc_w_int(b, (int64_t)({expr}))) return -1;\n"
            )),
            "uint" => out.push_str(&format!(
                "{indent}if (csilc_w_uint(b, (uint64_t)({expr}))) return -1;\n"
            )),
            "bool" | "true" | "false" => {
                out.push_str(&format!("{indent}if (csilc_w_bool(b, ({expr}))) return -1;\n"))
            }
            "float" | "float64" | "double" => out.push_str(&format!(
                "{indent}if (csilc_w_f64(b, (double)({expr}))) return -1;\n"
            )),
            "float16" | "float32" => out.push_str(&format!(
                "{indent}if (csilc_w_f32(b, (float)({expr}))) return -1;\n"
            )),
            "text" | "tstr" => out.push_str(&format!(
                "{indent}if (csilc_w_text(b, ({expr}), ({expr}) ? strlen({expr}) : 0)) return -1;\n"
            )),
            "bytes" | "bstr" => out.push_str(&format!(
                "{indent}if (csilc_w_bytes(b, ({expr}).data, ({expr}).len)) return -1;\n"
            )),
            "timestamp" => out.push_str(&format!(
                "{indent}if (csilc_w_tag(b, 0)) return -1;\n\
                 {indent}if (csilc_w_text(b, ({expr}).rfc3339, ({expr}).rfc3339 ? strlen(({expr}).rfc3339) : 0)) return -1;\n"
            )),
            "decimal" => out.push_str(&format!(
                "{indent}if (csilc_w_tag(b, 4) || csilc_w_array_head(b, 2) ||\n\
                 {indent}    csilc_w_int(b, ({expr}).exponent) || csilc_w_int(b, ({expr}).mantissa)) return -1;\n"
            )),
            "null" | "nil" | "undefined" | "any" => {
                out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"))
            }
            other => {
                warnings.push(GeneratorWarning {
                    message: format!("c codec: unsupported builtin `{other}` encoded as null"),
                    level: WarningLevel::Warning, location: None, suggestion: None,
                });
                out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"));
            }
        },
        CsilTypeExpression::Reference(name) if scope.has_codec(name) => out.push_str(&format!(
            "{indent}if (csilc_enc_{name}(b, &({expr}))) return -1;\n"
        )),
        // A reference to a transparent alias (`Uuid = text`) has no codec of its own;
        // encode it as its underlying type. The alias typedef makes the C field token
        // identical to the underlying's, so the same `expr` flows through unchanged.
        // (An alias whose target is an array/map degrades through the catch-all below,
        // matching its lossy `void *`/no-count type emission — see `codec_aliases`.)
        CsilTypeExpression::Reference(name) if scope.aliases.contains_key(name) => {
            emit_enc_value(out, indent, &scope.aliases[name], expr, scope, warnings);
        }
        CsilTypeExpression::Reference(name) => {
            warnings.push(GeneratorWarning {
                message: format!("c codec: `{name}` has no generated codec; encoded as null"),
                level: WarningLevel::Warning, location: None, suggestion: None,
            });
            out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"));
        }
        _ => {
            warnings.push(GeneratorWarning {
                message: "c codec: unrepresentable nested value encoded as null".to_string(),
                level: WarningLevel::Warning, location: None, suggestion: None,
            });
            out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"));
        }
    }
}

/// Emit the statements that decode a single scalar/reference value from `src` (a
/// `const csilc_value *`) into the lvalue `dst` of type `ty`.
fn emit_dec_value(
    out: &mut String,
    indent: &str,
    ty: &CsilTypeExpression,
    src: &str,
    dst: &str,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => out.push_str(&format!(
                "{indent}if (!csilc_as_i64({src}, &({dst}))) return -1;\n"
            )),
            "uint" => out.push_str(&format!(
                "{indent}if (!csilc_as_u64({src}, &({dst}))) return -1;\n"
            )),
            "bool" | "true" | "false" => out.push_str(&format!(
                "{indent}if (!csilc_as_bool({src}, &({dst}))) return -1;\n"
            )),
            "float" | "float64" | "double" => out.push_str(&format!(
                "{indent}if (!csilc_as_f64({src}, &({dst}))) return -1;\n"
            )),
            "float16" | "float32" => out.push_str(&format!(
                "{indent}{{ double csilc_d; if (!csilc_as_f64({src}, &csilc_d)) return -1; ({dst}) = (float)csilc_d; }}\n"
            )),
            "text" | "tstr" => out.push_str(&format!(
                "{indent}if (!csilc_get_text({src}, &({dst}))) return -1;\n"
            )),
            "bytes" | "bstr" => out.push_str(&format!(
                "{indent}if (!csilc_get_bytes({src}, &({dst}).data, &({dst}).len)) return -1;\n"
            )),
            "timestamp" => out.push_str(&format!(
                "{indent}if (!csilc_get_tagged_text({src}, 0, &({dst}).rfc3339)) return -1;\n\
                 {indent}({dst}).epoch_seconds = 0;\n"
            )),
            "decimal" => out.push_str(&format!(
                "{indent}if (!csilc_get_decimal({src}, &({dst}).exponent, &({dst}).mantissa)) return -1;\n"
            )),
            "null" | "nil" | "undefined" | "any" => {
                out.push_str(&format!("{indent}({dst}) = NULL;\n"))
            }
            other => {
                warnings.push(GeneratorWarning {
                    message: format!("c codec: unsupported builtin `{other}` decoded as zero"),
                    level: WarningLevel::Warning, location: None, suggestion: None,
                });
                out.push_str(&format!("{indent}(void)({src});\n"));
            }
        },
        CsilTypeExpression::Reference(name) if scope.has_codec(name) => out.push_str(&format!(
            "{indent}if (csilc_dec_{name}({src}, a, &({dst}))) return -1;\n"
        )),
        // A reference to a transparent alias decodes as its underlying type; the
        // alias typedef makes `dst` the same C token the underlying decoder writes.
        CsilTypeExpression::Reference(name) if scope.aliases.contains_key(name) => {
            emit_dec_value(out, indent, &scope.aliases[name], src, dst, scope, warnings);
        }
        CsilTypeExpression::Reference(name) => {
            warnings.push(GeneratorWarning {
                message: format!("c codec: `{name}` has no generated codec; left zero on decode"),
                level: WarningLevel::Warning, location: None, suggestion: None,
            });
            out.push_str(&format!("{indent}(void)({src});\n"));
        }
        _ => {
            warnings.push(GeneratorWarning {
                message: "c codec: unrepresentable nested value left zero on decode".to_string(),
                level: WarningLevel::Warning, location: None, suggestion: None,
            });
            out.push_str(&format!("{indent}(void)({src});\n"));
        }
    }
}

/// The presence test for an optional field at encode time. Every optional scalar,
/// text, bytes, or reference field is a pointer (NULL == absent); an optional
/// array/map keeps its pointer+count shape and is "present" when its count is
/// non-zero.
fn enc_presence(field: &CodecField, member: &str) -> String {
    match unwrap_constrained(field.value_type) {
        CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. } => {
            format!("v->{member}_count")
        }
        _ => format!("v->{member}"),
    }
}

/// Emit the key + CBOR array head + per-element encode loop for a list field.
fn emit_enc_array_body(
    out: &mut String,
    indent: &str,
    member: &str,
    klen: usize,
    element_type: &CsilTypeExpression,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    out.push_str(&format!(
        "{indent}if (csilc_w_text(b, \"{member}\", {klen})) return -1;\n"
    ));
    out.push_str(&format!(
        "{indent}if (csilc_w_array_head(b, v->{member}_count)) return -1;\n"
    ));
    out.push_str(&format!(
        "{indent}for (size_t csilc_i = 0; csilc_i < v->{member}_count; csilc_i++) {{\n"
    ));
    let inner = format!("{indent}    ");
    emit_enc_value(
        out,
        &inner,
        element_type,
        &format!("v->{member}[csilc_i]"),
        scope,
        warnings,
    );
    out.push_str(&format!("{indent}}}\n"));
}

/// Emit the key + CBOR map head + per-entry encode loop for a map field. The map's
/// key and value types travel together as `kv` to keep the argument count down.
fn emit_enc_map_body(
    out: &mut String,
    indent: &str,
    member: &str,
    klen: usize,
    kv: (&CsilTypeExpression, &CsilTypeExpression),
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let (key, value) = kv;
    out.push_str(&format!(
        "{indent}if (csilc_w_text(b, \"{member}\", {klen})) return -1;\n"
    ));
    out.push_str(&format!(
        "{indent}if (csilc_w_map_head(b, v->{member}_count)) return -1;\n"
    ));
    out.push_str(&format!(
        "{indent}for (size_t csilc_i = 0; csilc_i < v->{member}_count; csilc_i++) {{\n"
    ));
    let inner = format!("{indent}    ");
    emit_enc_value(
        out,
        &inner,
        key,
        &format!("v->{member}_keys[csilc_i]"),
        scope,
        warnings,
    );
    emit_enc_value(
        out,
        &inner,
        value,
        &format!("v->{member}_values[csilc_i]"),
        scope,
        warnings,
    );
    out.push_str(&format!("{indent}}}\n"));
}

/// Emit the encode of one record field (key + value), honoring optionality and the
/// array/map pointer+count expansion.
fn emit_enc_field(
    out: &mut String,
    field: &CodecField,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let member = &field.name;
    let klen = key_len(member);
    let key_write = format!("    if (csilc_w_text(b, \"{member}\", {klen})) return -1;\n");
    let base = unwrap_constrained(field.value_type);
    match base {
        CsilTypeExpression::Array { element_type, .. } => {
            if field.optional {
                out.push_str(&format!("    if (v->{member}_count) {{\n"));
                emit_enc_array_body(out, "        ", member, klen, element_type, scope, warnings);
                out.push_str("    }\n");
            } else {
                emit_enc_array_body(out, "    ", member, klen, element_type, scope, warnings);
            }
        }
        CsilTypeExpression::Map { key, value, .. } => {
            if field.optional {
                out.push_str(&format!("    if (v->{member}_count) {{\n"));
                emit_enc_map_body(out, "        ", member, klen, (key, value), scope, warnings);
                out.push_str("    }\n");
            } else {
                emit_enc_map_body(out, "    ", member, klen, (key, value), scope, warnings);
            }
        }
        _ => {
            // Scalars and references: an optional value-typed field is a pointer to
            // its value, so the present value reads `*v->member`; a pointer-typed
            // field (text) is already the value.
            let c_type = base_c_type(base, &default_config());
            let is_ptr = c_type.ends_with('*');
            let expr = if field.optional && !is_ptr {
                format!("(*v->{member})")
            } else {
                format!("v->{member}")
            };
            if field.optional {
                out.push_str(&format!("    if (v->{member}) {{\n"));
                out.push_str(&key_write.replace("    if", "        if"));
                emit_enc_value(out, "        ", base, &expr, scope, warnings);
                out.push_str("    }\n");
            } else {
                out.push_str(&key_write);
                emit_enc_value(out, "    ", base, &expr, scope, warnings);
            }
        }
    }
}

/// Emit the decode of one record field from the decoded map `m` into `out->member`.
fn emit_dec_field(
    out: &mut String,
    field: &CodecField,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let member = &field.name;
    out.push_str(&format!("    csilc_f = csilc_map_get(m, \"{member}\");\n"));
    let base = unwrap_constrained(field.value_type);
    match base {
        CsilTypeExpression::Array { element_type, .. } => {
            let elem = base_c_type(element_type, &default_config());
            out.push_str("    if (!csilc_f || csilc_f->kind != CSILC_ARRAY) return -1;\n");
            out.push_str(&format!(
                "    out->{member}_count = csilc_f->as.array.count;\n"
            ));
            out.push_str(&format!("    out->{member} = NULL;\n"));
            out.push_str(&format!("    if (out->{member}_count) {{\n"));
            out.push_str(&format!(
                "        out->{member} = ({elem} *)csilc_arena_alloc(a, out->{member}_count * sizeof({elem}));\n"
            ));
            out.push_str(&format!("        if (!out->{member}) return -1;\n"));
            out.push_str(&format!(
                "        for (size_t csilc_i = 0; csilc_i < out->{member}_count; csilc_i++) {{\n"
            ));
            emit_dec_value(
                out,
                "            ",
                element_type,
                "&csilc_f->as.array.items[csilc_i]",
                &format!("out->{member}[csilc_i]"),
                scope,
                warnings,
            );
            out.push_str("        }\n    }\n");
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kt = base_c_type(key, &default_config());
            let vt = base_c_type(value, &default_config());
            out.push_str("    if (!csilc_f || csilc_f->kind != CSILC_MAP) return -1;\n");
            out.push_str(&format!(
                "    out->{member}_count = csilc_f->as.map.count;\n"
            ));
            out.push_str(&format!("    out->{member}_keys = NULL;\n"));
            out.push_str(&format!("    out->{member}_values = NULL;\n"));
            out.push_str(&format!("    if (out->{member}_count) {{\n"));
            out.push_str(&format!(
                "        out->{member}_keys = ({kt} *)csilc_arena_alloc(a, out->{member}_count * sizeof({kt}));\n"
            ));
            out.push_str(&format!(
                "        out->{member}_values = ({vt} *)csilc_arena_alloc(a, out->{member}_count * sizeof({vt}));\n"
            ));
            out.push_str(&format!(
                "        if (!out->{member}_keys || !out->{member}_values) return -1;\n"
            ));
            out.push_str(&format!(
                "        for (size_t csilc_i = 0; csilc_i < out->{member}_count; csilc_i++) {{\n"
            ));
            emit_dec_value(
                out,
                "            ",
                key,
                "csilc_f->as.map.pairs[csilc_i].key",
                &format!("out->{member}_keys[csilc_i]"),
                scope,
                warnings,
            );
            emit_dec_value(
                out,
                "            ",
                value,
                "csilc_f->as.map.pairs[csilc_i].val",
                &format!("out->{member}_values[csilc_i]"),
                scope,
                warnings,
            );
            out.push_str("        }\n    }\n");
        }
        _ => {
            let c_type = base_c_type(base, &default_config());
            let is_ptr = c_type.ends_with('*');
            if field.optional && is_ptr {
                // Optional text (a `char *`): absence is just NULL, no allocation.
                if matches!(base, CsilTypeExpression::Builtin(n) if n == "text" || n == "tstr") {
                    out.push_str(&format!(
                        "    out->{member} = (csilc_f && csilc_f->kind == CSILC_TEXT) ? (char *)csilc_f->as.bytes.ptr : NULL;\n"
                    ));
                } else {
                    out.push_str(&format!("    out->{member} = NULL;\n"));
                }
            } else if field.optional {
                let pointee = c_type.clone();
                out.push_str(&format!("    out->{member} = NULL;\n"));
                out.push_str("    if (csilc_f) {\n");
                out.push_str(&format!(
                    "        {pointee} *csilc_p = ({pointee} *)csilc_arena_alloc(a, sizeof({pointee}));\n"
                ));
                out.push_str("        if (!csilc_p) return -1;\n");
                emit_dec_value(
                    out,
                    "        ",
                    base,
                    "csilc_f",
                    "(*csilc_p)",
                    scope,
                    warnings,
                );
                out.push_str(&format!("        out->{member} = csilc_p;\n"));
                out.push_str("    }\n");
            } else {
                emit_dec_value(
                    out,
                    "    ",
                    base,
                    "csilc_f",
                    &format!("out->{member}"),
                    scope,
                    warnings,
                );
            }
        }
    }
}

/// A throwaway default config for the codec's type-spelling lookups, where the
/// decimal mapping does not affect the emitted C token (`CsilDecimal` either way).
fn default_config() -> CConfig {
    CConfig {
        output_subdir: String::new(),
        decimal_mapping: DecimalMapping::Csil,
        generate_validation: true,
    }
}

/// Emit the encode + decode bodies for one record type.
fn emit_record_codec(
    out: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let fields = codec_fields(group);
    let required = fields.iter().filter(|f| !f.optional).count();

    out.push_str(&format!(
        "/* csilc_enc_{name} writes {name} as a canonical CBOR map. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str(&format!("    size_t csilc_n = {required};\n"));
    for field in fields.iter().filter(|f| f.optional) {
        out.push_str(&format!(
            "    if ({}) csilc_n++;\n",
            enc_presence(field, &field.name)
        ));
    }
    out.push_str("    if (csilc_w_map_head(b, csilc_n)) return -1;\n");
    for field in &fields {
        emit_enc_field(out, field, scope, warnings);
    }
    out.push_str("    return 0;\n}\n\n");

    out.push_str(&format!(
        "/* csilc_dec_{name} reads {name} from a decoded CBOR map (arena-borrowed). */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    const csilc_value *csilc_f;\n");
    out.push_str("    (void)a;\n");
    out.push_str("    if (!m || m->kind != CSILC_MAP) return -1;\n");
    for field in &fields {
        emit_dec_field(out, field, scope, warnings);
    }
    out.push_str("    return 0;\n}\n\n");
}

/// Emit the encode + decode bodies for a named map alias. It walks the struct's
/// `keys`/`values`/`count` members the same way the inline-map field codec walks a
/// field's `_keys`/`_values`/`_count`, so a map-of-record alias recurses into the
/// value record's own `csilc_enc_*`/`csilc_dec_*` exactly as an inline map would.
fn emit_map_alias_codec(
    out: &mut String,
    name: &str,
    key: &CsilTypeExpression,
    value: &CsilTypeExpression,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let kt = base_c_type(key, &default_config());
    let vt = base_c_type(value, &default_config());

    out.push_str(&format!(
        "/* csilc_enc_{name} writes {name} as a CBOR map of text keys to encoded values. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str("    if (csilc_w_map_head(b, v->count)) return -1;\n");
    out.push_str("    for (size_t csilc_i = 0; csilc_i < v->count; csilc_i++) {\n");
    emit_enc_value(out, "        ", key, "v->keys[csilc_i]", scope, warnings);
    emit_enc_value(
        out,
        "        ",
        value,
        "v->values[csilc_i]",
        scope,
        warnings,
    );
    out.push_str("    }\n    return 0;\n}\n\n");

    out.push_str(&format!(
        "/* csilc_dec_{name} reads {name} from a decoded CBOR map (arena-borrowed). */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    if (!m || m->kind != CSILC_MAP) return -1;\n");
    out.push_str("    out->count = m->as.map.count;\n");
    out.push_str("    out->keys = NULL;\n    out->values = NULL;\n");
    out.push_str("    if (out->count) {\n");
    out.push_str(&format!(
        "        out->keys = ({kt} *)csilc_arena_alloc(a, out->count * sizeof({kt}));\n"
    ));
    out.push_str(&format!(
        "        out->values = ({vt} *)csilc_arena_alloc(a, out->count * sizeof({vt}));\n"
    ));
    out.push_str("        if (!out->keys || !out->values) return -1;\n");
    out.push_str("        for (size_t csilc_i = 0; csilc_i < out->count; csilc_i++) {\n");
    emit_dec_value(
        out,
        "            ",
        key,
        "m->as.map.pairs[csilc_i].key",
        "out->keys[csilc_i]",
        scope,
        warnings,
    );
    emit_dec_value(
        out,
        "            ",
        value,
        "m->as.map.pairs[csilc_i].val",
        "out->values[csilc_i]",
        scope,
        warnings,
    );
    out.push_str("        }\n    }\n    return 0;\n}\n\n");
}

/// Emit the encode + decode bodies for a named list alias, walking the struct's
/// `items`/`count` members the way the inline-array field codec walks a field's
/// `_items`/`_count`.
fn emit_list_alias_codec(
    out: &mut String,
    name: &str,
    element_type: &CsilTypeExpression,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let et = base_c_type(element_type, &default_config());

    out.push_str(&format!(
        "/* csilc_enc_{name} writes {name} as a CBOR array of encoded elements. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str("    if (csilc_w_array_head(b, v->count)) return -1;\n");
    out.push_str("    for (size_t csilc_i = 0; csilc_i < v->count; csilc_i++) {\n");
    emit_enc_value(
        out,
        "        ",
        element_type,
        "v->items[csilc_i]",
        scope,
        warnings,
    );
    out.push_str("    }\n    return 0;\n}\n\n");

    out.push_str(&format!(
        "/* csilc_dec_{name} reads {name} from a decoded CBOR array (arena-borrowed). */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    if (!m || m->kind != CSILC_ARRAY) return -1;\n");
    out.push_str("    out->count = m->as.array.count;\n");
    out.push_str("    out->items = NULL;\n");
    out.push_str("    if (out->count) {\n");
    out.push_str(&format!(
        "        out->items = ({et} *)csilc_arena_alloc(a, out->count * sizeof({et}));\n"
    ));
    out.push_str("        if (!out->items) return -1;\n");
    out.push_str("        for (size_t csilc_i = 0; csilc_i < out->count; csilc_i++) {\n");
    emit_dec_value(
        out,
        "            ",
        element_type,
        "&m->as.array.items[csilc_i]",
        "out->items[csilc_i]",
        scope,
        warnings,
    );
    out.push_str("        }\n    }\n    return 0;\n}\n\n");
}

/// Emit the encode + decode bodies for an enum type. The wire form of a closed CSIL
/// enum is its original literal text, so the codec maps the C ordinal to/from that
/// text through a names table.
fn emit_enum_codec(out: &mut String, name: &str, variants: &[String]) {
    out.push_str(&format!(
        "static CSILC_UNUSED const char *const csilc_{name}_names[] = {{\n"
    ));
    for variant in variants {
        out.push_str(&format!("    \"{}\",\n", c_escape(variant)));
    }
    out.push_str("};\n");
    out.push_str(&format!(
        "/* csilc_enc_{name} writes the {name} variant's wire text. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str(&format!(
        "    const char *csilc_s = csilc_{name}_names[(size_t)(*v)];\n"
    ));
    out.push_str("    return csilc_w_text(b, csilc_s, strlen(csilc_s));\n}\n\n");
    out.push_str(&format!(
        "/* csilc_dec_{name} matches the wire text back to a {name} variant. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *src, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    (void)a;\n");
    out.push_str("    if (!src || src->kind != CSILC_TEXT) return -1;\n");
    out.push_str(&format!(
        "    for (size_t csilc_i = 0; csilc_i < sizeof(csilc_{name}_names) / sizeof(csilc_{name}_names[0]); csilc_i++) {{\n"
    ));
    out.push_str(&format!(
        "        if (strlen(csilc_{name}_names[csilc_i]) == src->as.bytes.len &&\n\
         \x20           memcmp(csilc_{name}_names[csilc_i], src->as.bytes.ptr, src->as.bytes.len) == 0) {{\n"
    ));
    out.push_str(&format!("            *out = ({name})csilc_i;\n"));
    out.push_str("            return 0;\n        }\n    }\n    return -1;\n}\n\n");
}

/// Escape a string for a C string literal.
fn c_escape(s: &str) -> String {
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

fn generate_codec(
    input: &WasmGeneratorInput,
    config: &CConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Option<String> {
    // The codec covers records (CBOR maps) and enums (wire text); aliases and
    // unions are not codec'd and a field referencing one degrades to a warned null.
    let typed: Vec<(&str, TypeKind)> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| classify_rule(&r.rule_type).map(|k| (r.name.as_str(), k)))
        .collect();
    // Records, enums, and named map/list aliases all carry a generated codec, so a
    // field referencing any of them flows through the record-reference codec arm.
    let codec_names: std::collections::HashSet<String> = typed
        .iter()
        .filter(|(_, k)| {
            matches!(k, TypeKind::Struct(_) | TypeKind::Enum(_)) || alias_aggregate(k).is_some()
        })
        .map(|(n, _)| n.to_string())
        .collect();
    if codec_names.is_empty() {
        return None;
    }
    // Transparent aliases the codec resolves through so a field typed as one encodes
    // its underlying type instead of the dropped-data null stub a bare reference hits.
    let aliases = codec_aliases(input);
    let scope = CodecScope {
        names: &codec_names,
        aliases: &aliases,
    };

    let mut decls = String::new();
    let mut bodies = String::new();
    for (name, kind) in &typed {
        match kind {
            TypeKind::Struct(group) => {
                decls.push_str(&format!(
                    "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v);\n"
                ));
                decls.push_str(&format!(
                    "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out);\n"
                ));
                emit_record_codec(&mut bodies, name, group, &scope, warnings);
            }
            TypeKind::Enum(variants) => {
                decls.push_str(&format!(
                    "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v);\n"
                ));
                decls.push_str(&format!(
                    "static inline int csilc_dec_{name}(const csilc_value *src, CsilCodecArena *a, {name} *out);\n"
                ));
                emit_enum_codec(&mut bodies, name, variants);
            }
            _ => {
                if let Some(agg) = alias_aggregate(kind) {
                    decls.push_str(&format!(
                        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v);\n"
                    ));
                    decls.push_str(&format!(
                        "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out);\n"
                    ));
                    match agg {
                        CsilTypeExpression::Map { key, value, .. } => {
                            emit_map_alias_codec(&mut bodies, name, key, value, &scope, warnings)
                        }
                        CsilTypeExpression::Array { element_type, .. } => {
                            emit_list_alias_codec(&mut bodies, name, element_type, &scope, warnings)
                        }
                        // alias_aggregate only ever yields a map or a list.
                        _ => {}
                    }
                }
            }
        }
    }

    // Public, ergonomic per-type wrappers: encode to a fresh malloc'd buffer the
    // caller frees with free(); decode into a typed value backed by an arena the
    // caller frees once with csil_codec_arena_free.
    let mut public = String::new();
    for (name, kind) in &typed {
        if !matches!(kind, TypeKind::Struct(_) | TypeKind::Enum(_))
            && alias_aggregate(kind).is_none()
        {
            continue;
        }
        public.push_str(&doc_comment(&[
            &format!("Encode a {name} to CBOR. On success *out is a malloc'd buffer of"),
            "*out_len bytes the caller frees with free(); returns non-zero on failure.",
        ]));
        public.push_str(&format!(
            "static inline int csil_encode_{name}(const {name} *v, uint8_t **out, size_t *out_len) {{\n\
             \x20   csilc_buf b;\n\
             \x20   csilc_buf_init(&b);\n\
             \x20   if (csilc_enc_{name}(&b, v)) {{ csilc_buf_dispose(&b); return -1; }}\n\
             \x20   *out = b.data;\n\
             \x20   *out_len = b.len;\n\
             \x20   return 0;\n}}\n\n"
        ));
        public.push_str(&doc_comment(&[
            &format!("Decode CBOR into a {name}. On success *owner holds the backing"),
            "storage (every string/bytes/array inside *out borrows from it); free it",
            "once with csil_codec_arena_free when done. Returns non-zero on failure.",
        ]));
        public.push_str(&format!(
            "static inline int csil_decode_{name}(const uint8_t *in, size_t len, {name} *out, CsilCodecArena **owner) {{\n\
             \x20   CsilCodecArena *a;\n\
             \x20   const csilc_value *root;\n\
             \x20   if (csilc_decode(in, len, &a, &root)) return -1;\n\
             \x20   if (csilc_dec_{name}(root, a, out)) {{ csil_codec_arena_free(a); return -1; }}\n\
             \x20   *owner = a;\n\
             \x20   return 0;\n}}\n\n"
        ));
    }

    let mut content = String::new();
    let mut includes = vec![
        "<stdbool.h>".to_string(),
        "<stddef.h>".to_string(),
        "<stdint.h>".to_string(),
        "<stdlib.h>".to_string(),
        "<string.h>".to_string(),
        "\"types.gen.h\"".to_string(),
    ];
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(input, "decimal") {
        includes.push("\"csil_decimal.gen.h\"".to_string());
    }
    if spec_uses_builtin(input, "timestamp") {
        includes.push("\"csil_timestamp.gen.h\"".to_string());
    }
    header_open(
        &mut content,
        "CSILGEN_CODEC_GEN_H",
        "Generated CBOR (de)serializers for the CSIL value types.",
        &includes,
    );
    content.push_str(CODEC_RUNTIME_C);
    content.push('\n');
    content.push_str("/* ---- per-type codec forward declarations ---- */\n");
    content.push_str(&decls);
    content.push('\n');
    content.push_str(&bodies);
    content.push_str(&public);
    header_close(&mut content, "CSILGEN_CODEC_GEN_H");
    Some(content)
}

// ---- client ---------------------------------------------------------------

/// The carrier seam every generated call delegates to: the host implements `call`,
/// performing the raw byte round-trip for `(service, op)`. The generated client
/// owns serialization (it encodes the typed request and decodes the typed
/// response); the carrier only moves bytes, exactly as in the other languages.
const CLIENT_PRELUDE_C: &str = "\
/* CsilgenTransport is the caller-supplied byte carrier: it performs the call named
 * by (service, op) with the already-encoded req bytes (CBOR over HTTP, say) and
 * writes the response bytes into *resp (the generated client frees them),
 * returning 0 on success or a non-zero error. */
typedef struct CsilgenTransport {
    int (*call)(void *self, const char *service, const char *op,
                const uint8_t *req, size_t req_len,
                uint8_t **resp, size_t *resp_len);
    void *self;
} CsilgenTransport;
";

fn generate_client(input: &WasmGeneratorInput, config: &CConfig) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_calls(&mut body, &rule.name, service, config);
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = String::new();
    header_open(
        &mut content,
        "CSILGEN_CLIENT_GEN_H",
        "Generated typed RPC client call-sites.",
        &[
            "<stddef.h>".to_string(),
            "<stdint.h>".to_string(),
            "<stdlib.h>".to_string(),
            "\"types.gen.h\"".to_string(),
            "\"codec.gen.h\"".to_string(),
        ],
    );
    content.push_str(CLIENT_PRELUDE_C);
    content.push('\n');
    content.push_str(&body);
    header_close(&mut content, "CSILGEN_CLIENT_GEN_H");
    Some(content)
}

fn emit_client_calls(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CConfig,
) {
    let base = service_base(name);
    let prefix = to_snake(&base);
    let wire_service = base.to_lowercase();
    let _ = config;
    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the router surface the server target emits.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            content.push_str(&format!(
                "/* channel operation {} is not part of the RPC client */\n",
                op.name
            ));
            continue;
        }
        let fn_name = format!("csil_{prefix}_{}", to_snake(&op.name));
        let wire_op = simple_pascal(&op.name);
        let resp_type = base_c_type(&success_type(&op.output_type), config);
        let has_input = !op_input_is_null(&op.input_type);
        let req_type = base_c_type(&op.input_type, config);

        content.push_str(&doc_comment(&[
            &format!("Invoke {wire_service}/{wire_op} with a typed request and decode the typed"),
            "response. *resp_owner holds the response's backing storage; free it once",
            "with csil_codec_arena_free when done with *resp. Returns non-zero on failure.",
        ]));
        if has_input {
            content.push_str(&format!(
                "static inline int {fn_name}(const CsilgenTransport *t, const {req_type} *req,\n\
                 \x20                       {resp_type} *resp, CsilCodecArena **resp_owner) {{\n"
            ));
            content.push_str("    uint8_t *csil_reqb = NULL;\n    size_t csil_reqn = 0;\n");
            content.push_str(&format!(
                "    if (csil_encode_{}(req, &csil_reqb, &csil_reqn)) return -1;\n",
                type_codec_name(&op.input_type)
            ));
        } else {
            content.push_str(&format!(
                "static inline int {fn_name}(const CsilgenTransport *t,\n\
                 \x20                       {resp_type} *resp, CsilCodecArena **resp_owner) {{\n"
            ));
            content.push_str("    uint8_t *csil_reqb = NULL;\n    size_t csil_reqn = 0;\n");
        }
        content.push_str("    uint8_t *csil_respb = NULL;\n    size_t csil_respn = 0;\n");
        content.push_str(&format!(
            "    int csil_rc = t->call(t->self, \"{wire_service}\", \"{wire_op}\", csil_reqb, csil_reqn, &csil_respb, &csil_respn);\n"
        ));
        content.push_str("    free(csil_reqb);\n");
        content.push_str("    if (csil_rc != 0) { free(csil_respb); return csil_rc; }\n");
        content.push_str(&format!(
            "    int csil_drc = csil_decode_{}(csil_respb, csil_respn, resp, resp_owner);\n",
            type_codec_name(&success_type(&op.output_type))
        ));
        content.push_str("    free(csil_respb);\n");
        content.push_str("    return csil_drc;\n}\n\n");
    }
}

/// The codec base name for a type reference (`csil_encode_<Name>` / `csil_decode_<Name>`).
/// Operation input/output types are references to named records, so this is their name.
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
    config: &CConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_handlers_struct(&mut body, &rule.name, service, config);
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
    let _ = warnings;
    let mut content = String::new();
    header_open(
        &mut content,
        "CSILGEN_SERVER_GEN_H",
        "Generated service handler structs and routers.",
        &[
            "<stddef.h>".to_string(),
            "<stdint.h>".to_string(),
            "<string.h>".to_string(),
            "\"types.gen.h\"".to_string(),
        ],
    );
    // The codec is consumer-supplied so the runtime never owns serialization.
    content.push_str(&doc_comment(&[
        "CsilgenCodec is the consumer-supplied (de)serialization layer for channel",
        "messages. The generator is codec-agnostic; the implementer wires this to",
        "CBOR, JSON, or whatever its protocol expects. decode returns 0 on success.",
    ]));
    content.push_str(
        "typedef struct CsilgenCodec {\n\
         \x20   int (*decode)(void *self, const uint8_t *data, size_t len, void *out);\n\
         \x20   int (*encode)(void *self, const void *value, uint8_t **out, size_t *out_len);\n\
         \x20   void *self;\n\
         } CsilgenCodec;\n\n",
    );
    content.push_str(&body);
    header_close(&mut content, "CSILGEN_SERVER_GEN_H");
    Some(content)
}

fn emit_handlers_struct(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CConfig,
) {
    let base = service_base(name);
    content.push_str(&format!(
        "/* {base}Handlers is the host's implementation of the {name} service. */\n"
    ));
    content.push_str(&format!("typedef struct {base}Handlers {{\n"));
    for op in &service.operations {
        let method = to_snake(&op.name);
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let in_type = op_in_type(&op.input_type, config);
                let out_type = base_c_type(&success_type(&op.output_type), config);
                if in_type == "void" {
                    content.push_str(&format!(
                        "    int (*{method})(void *ctx, {out_type} *resp);\n"
                    ));
                } else {
                    content.push_str(&format!(
                        "    int (*{method})(void *ctx, const {in_type} *req, {out_type} *resp);\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let in_type = base_c_type(&op.input_type, config);
                // Fire-and-forget inbound: the router decodes and dispatches here.
                content.push_str(&format!(
                    "    int (*{method})(void *ctx, const {in_type} *msg);\n"
                ));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound method on the server side.
            }
        }
    }
    content.push_str(&format!("}} {base}Handlers;\n\n"));
}

/// Emit `#define` wire-id ordinals exposing the `@wire-id(N)` values. Purely
/// additive: emits nothing unless the service carries a wire-id, keeping
/// wire-id-free output byte-identical.
fn emit_wire_ids(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let prefix = to_upper_snake(&service_base(name));
    content.push_str(&format!(
        "/* Wire-id ordinals for the {name} service (transport compact profiles). */\n"
    ));
    content.push_str(&format!("#define {prefix}_SERVICE_WIRE_ID {service_id}u\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            // The OP infix keeps op ordinals distinct from the service ordinal.
            content.push_str(&format!(
                "#define {prefix}_OP_{}_WIRE_ID {op_id}u\n",
                to_upper_snake(&op.name)
            ));
        }
    }
    content.push('\n');
}

fn emit_channel_router(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    let snake = to_snake(&service_base(name));
    content.push_str(&doc_comment(&[
        &format!("route_{snake}_channel decodes one inbound channel frame and dispatches to the"),
        &format!("matching {name} method by wire op name."),
    ]));
    content.push_str(&format!(
        "static inline int route_{0}_channel(const {1}Handlers *h, void *ctx,\n\
         \x20                          const CsilgenCodec *codec, const char *method,\n\
         \x20                          const uint8_t *data, size_t len) {{\n",
        to_snake(&service_base(name)),
        service_base(name)
    ));
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = to_snake(&op.name);
        let wire_op = simple_pascal(&op.name);
        content.push_str(&format!("    if (strcmp(method, \"{wire_op}\") == 0) {{\n"));
        content.push_str("        void *msg = NULL;\n");
        content
            .push_str("        if (codec->decode(codec->self, data, len, &msg) != 0) return -1;\n");
        content.push_str(&format!("        return h->{method}(ctx, msg);\n"));
        content.push_str("    }\n");
    }
    content.push_str("    return -1; /* unknown channel method */\n");
    content.push_str("}\n\n");
}

/// The compact-profile twin: dispatch on the `@wire-id` operation ordinal rather
/// than the wire op name. Emitted only for wire-id-bearing services, so
/// wire-id-free output stays byte-identical.
fn emit_channel_router_compact(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    if service.wire_id.is_none() {
        return;
    }
    let snake = to_snake(&service_base(name));
    content.push_str(&doc_comment(&[
        &format!("route_{snake}_channel_compact dispatches by @wire-id ordinal (compact profile)."),
        "The host calls whichever twin matches the negotiated wire profile.",
    ]));
    content.push_str(&format!(
        "static inline int route_{0}_channel_compact(const {1}Handlers *h, void *ctx,\n\
         \x20                                  const CsilgenCodec *codec, uint64_t op,\n\
         \x20                                  const uint8_t *data, size_t len) {{\n",
        to_snake(&service_base(name)),
        service_base(name)
    ));
    content.push_str("    switch (op) {\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let Some(op_id) = op.wire_id else {
            continue;
        };
        let method = to_snake(&op.name);
        content.push_str(&format!("    case {op_id}u: {{\n"));
        content.push_str("        void *msg = NULL;\n");
        content
            .push_str("        if (codec->decode(codec->self, data, len, &msg) != 0) return -1;\n");
        content.push_str(&format!("        return h->{method}(ctx, msg);\n"));
        content.push_str("    }\n");
    }
    content.push_str("    default: return -1; /* unknown channel ordinal */\n");
    content.push_str("    }\n");
    content.push_str("}\n\n");
}

fn emit_push_encoders(content: &mut String, name: &str, service: &CsilServiceDefinition) {
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method = to_snake(&op.name);
        let wire_op = simple_pascal(&op.name);
        let snake = to_snake(&service_base(name));
        content.push_str(&doc_comment(&[
            &format!(
                "encode_{snake}_{method} encodes a {wire_op} message the server pushes to a peer;"
            ),
            &format!("the implementer frames (\"{wire_op}\", bytes) onto its connection."),
        ]));
        content.push_str(&format!(
            "static inline int encode_{0}_{method}(const CsilgenCodec *codec, const void *msg,\n\
             \x20                         uint8_t **out, size_t *out_len) {{\n",
            to_snake(&service_base(name))
        ));
        content.push_str("    return codec->encode(codec->self, msg, out, out_len);\n");
        content.push_str("}\n\n");
    }
}

// ---- type mapping ---------------------------------------------------------

/// The C token a CSIL type maps to for a scalar/reference field. Arrays and maps
/// are expanded by `emit_field`; this is their element/fallback spelling.
fn base_c_type(type_expr: &CsilTypeExpression, config: &CConfig) -> String {
    match unwrap_constrained(type_expr) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => "int64_t".to_string(),
            "uint" => "uint64_t".to_string(),
            "float" | "float64" | "double" => "double".to_string(),
            "float16" | "float32" => "float".to_string(),
            "text" | "tstr" => "char *".to_string(),
            "bytes" | "bstr" => "CsilBytes".to_string(),
            "bool" | "true" | "false" => "bool".to_string(),
            "timestamp" => "CsilTimestamp".to_string(),
            "decimal" => config.decimal_c_type().to_string(),
            "null" | "nil" | "undefined" | "any" => "void *".to_string(),
            other => other.to_string(),
        },
        CsilTypeExpression::Reference(name) => name.clone(),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("{} *", base_c_type(element_type, config))
        }
        _ => "void *".to_string(),
    }
}

/// The input type a unidirectional op's handler/client takes: `void` when the op
/// declares an empty (`null`/`nil`) input.
fn op_in_type(input_type: &CsilTypeExpression, config: &CConfig) -> String {
    if op_input_is_null(input_type) {
        "void".to_string()
    } else {
        base_c_type(input_type, config)
    }
}

fn op_input_is_null(input_type: &CsilTypeExpression) -> bool {
    matches!(unwrap_constrained(input_type), CsilTypeExpression::Builtin(n) if n == "null" || n == "nil")
}

/// The success arm of a `Res / ServiceError` output choice: a typed client/server
/// returns the success type, not the whole choice. A non-choice output is its own
/// success type.
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

/// Compose a C declarator for `name` of type `c_type` plus `extra_ptr` additional
/// pointer levels, using `PointerAlignment: Right` (the `*` binds to the name:
/// `int64_t n`, `char *s`, `char **list`). Counting and re-rendering the stars
/// keeps spacing canonical no matter how the incoming type string spells them, so
/// an array/map of `char *` reads `char **field` rather than `char * *field`.
fn declarator(c_type: &str, extra_ptr: usize, name: &str) -> String {
    let stars = c_type.matches('*').count() + extra_ptr;
    let base = c_type.replace('*', " ");
    let base = base.split_whitespace().collect::<Vec<_>>().join(" ");
    if stars == 0 {
        format!("{base} {name}")
    } else {
        format!("{base} {}{name}", "*".repeat(stars))
    }
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

/// A field's C member name, or None when the key is not a stable name (a typed
/// key), which is skipped consistently everywhere.
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

/// The C arm name for a choice arm: a referenced/builtin type's name, else a
/// positional `Choice<N>`.
fn arm_name(arm: &CsilTypeExpression, index: usize) -> String {
    match arm {
        CsilTypeExpression::Reference(n) | CsilTypeExpression::Builtin(n) => n.clone(),
        _ => format!("Choice{index}"),
    }
}

// ---- naming (wire names verbatim; C symbols cased) ------------------------

/// PascalCase by the same simple rule the other generators use for *wire* method
/// names, so a C client and a Go/Rust/Python/TS server agree byte-for-byte: break
/// on `_`/`-`, uppercase the letter after each break, keep the rest.
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

/// snake_case for C symbol names (PascalCase services, kebab-case operations).
/// Only C identifiers are reshaped this way; wire strings stay verbatim.
fn to_snake(s: &str) -> String {
    let mut out = String::new();
    let mut prev_alnum = false;
    for c in s.chars() {
        if c == '-' || c == '_' {
            out.push('_');
            prev_alnum = false;
        } else if c.is_uppercase() {
            if prev_alnum {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_alnum = true;
        } else {
            out.push(c);
            prev_alnum = c.is_alphanumeric();
        }
    }
    out
}

fn to_upper_snake(s: &str) -> String {
    to_snake(s).to_uppercase()
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

// ---- embedded codec runtime -----------------------------------------------

/// The self-contained canonical-CBOR runtime the per-type codecs build on. It is
/// header-only and every symbol is `static`, so it is included in exactly one
/// translation unit per linked image. It mirrors the conformance-tested transport
/// codec (`transports/c/src/cbor.c`): shortest-form heads, definite lengths, an
/// arena-backed decode tree the caller frees in one shot. `void`-cast unused params
/// keep `-Wunused` quiet when a spec exercises only part of the surface.
const CODEC_RUNTIME_C: &str = r##"/* ===== self-contained canonical CBOR codec (RFC 8949 subset) ===== */

/* The codec emits every primitive even when a spec uses only some of them; the
 * unused attribute keeps a host's -Wunused warnings quiet on the leftovers. */
#if defined(__GNUC__)
#define CSILC_UNUSED __attribute__((unused))
#else
#define CSILC_UNUSED
#endif

typedef struct csilc_buf {
    uint8_t *data;
    size_t len;
    size_t cap;
} csilc_buf;

static inline void csilc_buf_init(csilc_buf *b) {
    b->data = NULL;
    b->len = 0;
    b->cap = 0;
}

static inline void csilc_buf_dispose(csilc_buf *b) {
    free(b->data);
    b->data = NULL;
    b->len = 0;
    b->cap = 0;
}

static inline int csilc_buf_reserve(csilc_buf *b, size_t extra) {
    if (extra > SIZE_MAX - b->len) return -1;
    size_t need = b->len + extra;
    if (need <= b->cap) return 0;
    size_t cap = b->cap ? b->cap : 64;
    while (cap < need) {
        if (cap > SIZE_MAX / 2) { cap = need; break; }
        cap *= 2;
    }
    uint8_t *p = (uint8_t *)realloc(b->data, cap);
    if (!p) return -1;
    b->data = p;
    b->cap = cap;
    return 0;
}

static inline int csilc_buf_push(csilc_buf *b, uint8_t byte) {
    if (csilc_buf_reserve(b, 1)) return -1;
    b->data[b->len++] = byte;
    return 0;
}

static inline int csilc_buf_append(csilc_buf *b, const uint8_t *p, size_t n) {
    if (n == 0) return 0;
    if (csilc_buf_reserve(b, n)) return -1;
    memcpy(b->data + b->len, p, n);
    b->len += n;
    return 0;
}

/* Emit the initial byte (major type in the high three bits) plus the shortest-form
 * argument bytes for n, per deterministic encoding. */
static inline int csilc_head(csilc_buf *b, uint8_t major, uint64_t n) {
    uint8_t mt = (uint8_t)(major << 5);
    if (n < 24) return csilc_buf_push(b, (uint8_t)(mt | (uint8_t)n));
    if (n < 0x100ULL) {
        uint8_t t[2] = {(uint8_t)(mt | 24), (uint8_t)n};
        return csilc_buf_append(b, t, 2);
    }
    if (n < 0x10000ULL) {
        uint8_t t[3] = {(uint8_t)(mt | 25), (uint8_t)(n >> 8), (uint8_t)n};
        return csilc_buf_append(b, t, 3);
    }
    if (n < 0x100000000ULL) {
        uint8_t t[5] = {(uint8_t)(mt | 26), (uint8_t)(n >> 24), (uint8_t)(n >> 16),
                        (uint8_t)(n >> 8), (uint8_t)n};
        return csilc_buf_append(b, t, 5);
    }
    uint8_t t[9] = {(uint8_t)(mt | 27), (uint8_t)(n >> 56), (uint8_t)(n >> 48),
                    (uint8_t)(n >> 40), (uint8_t)(n >> 32), (uint8_t)(n >> 24),
                    (uint8_t)(n >> 16), (uint8_t)(n >> 8), (uint8_t)n};
    return csilc_buf_append(b, t, 9);
}

static inline int csilc_w_uint(csilc_buf *b, uint64_t n) { return csilc_head(b, 0, n); }

static inline int csilc_w_int(csilc_buf *b, int64_t x) {
    if (x >= 0) return csilc_head(b, 0, (uint64_t)x);
    /* CBOR negative ints encode -1-x; the magnitude is computed in unsigned to stay
     * defined even at INT64_MIN. */
    uint64_t mag = (uint64_t)(-(x + 1));
    return csilc_head(b, 1, mag);
}

static inline int csilc_w_text(csilc_buf *b, const char *s, size_t n) {
    if (csilc_head(b, 3, (uint64_t)n)) return -1;
    return csilc_buf_append(b, (const uint8_t *)s, n);
}

static inline int csilc_w_bytes(csilc_buf *b, const uint8_t *p, size_t n) {
    if (csilc_head(b, 2, (uint64_t)n)) return -1;
    return csilc_buf_append(b, p, n);
}

static inline int csilc_w_bool(csilc_buf *b, bool v) { return csilc_buf_push(b, v ? 0xf5 : 0xf4); }
static inline int csilc_w_null(csilc_buf *b) { return csilc_buf_push(b, 0xf6); }
static inline int csilc_w_array_head(csilc_buf *b, uint64_t n) { return csilc_head(b, 4, n); }
static inline int csilc_w_map_head(csilc_buf *b, uint64_t n) { return csilc_head(b, 5, n); }
static inline int csilc_w_tag(csilc_buf *b, uint64_t n) { return csilc_head(b, 6, n); }

static inline int csilc_w_f64(csilc_buf *b, double x) {
    uint64_t bits;
    memcpy(&bits, &x, 8);
    uint8_t t[9] = {0xfb, (uint8_t)(bits >> 56), (uint8_t)(bits >> 48), (uint8_t)(bits >> 40),
                    (uint8_t)(bits >> 32), (uint8_t)(bits >> 24), (uint8_t)(bits >> 16),
                    (uint8_t)(bits >> 8), (uint8_t)bits};
    return csilc_buf_append(b, t, 9);
}

static inline int csilc_w_f32(csilc_buf *b, float x) {
    uint32_t bits;
    memcpy(&bits, &x, 4);
    uint8_t t[5] = {0xfa, (uint8_t)(bits >> 24), (uint8_t)(bits >> 16),
                    (uint8_t)(bits >> 8), (uint8_t)bits};
    return csilc_buf_append(b, t, 5);
}

/* ---- bump arena (decode side) --------------------------------------------- */

typedef struct csilc_arena_block {
    struct csilc_arena_block *next;
    size_t used;
    size_t cap;
} csilc_arena_block;

typedef struct CsilCodecArena {
    csilc_arena_block *head;
} CsilCodecArena;

static inline CsilCodecArena *csilc_arena_new(void) {
    return (CsilCodecArena *)calloc(1, sizeof(CsilCodecArena));
}

static inline uint8_t *csilc_block_data(csilc_arena_block *blk) {
    return (uint8_t *)blk + sizeof(csilc_arena_block);
}

static inline void *csilc_arena_alloc(CsilCodecArena *a, size_t size) {
    if (!a) return NULL;
    size_t aligned = (size + 15u) & ~(size_t)15u;
    if (aligned < size) return NULL;
    csilc_arena_block *blk = a->head;
    if (!blk || blk->cap - blk->used < aligned) {
        size_t cap = aligned > 4096u ? aligned : 4096u;
        if (cap > SIZE_MAX - sizeof(csilc_arena_block)) return NULL;
        blk = (csilc_arena_block *)malloc(sizeof(csilc_arena_block) + cap);
        if (!blk) return NULL;
        blk->used = 0;
        blk->cap = cap;
        blk->next = a->head;
        a->head = blk;
    }
    void *p = csilc_block_data(blk) + blk->used;
    blk->used += aligned;
    return p;
}

/* Free the whole decoded value tree (and the C arrays mapped out of it) at once. */
static inline void csil_codec_arena_free(CsilCodecArena *a) {
    if (!a) return;
    csilc_arena_block *blk = a->head;
    while (blk) {
        csilc_arena_block *next = blk->next;
        free(blk);
        blk = next;
    }
    free(a);
}

/* ---- decoded value tree --------------------------------------------------- */

typedef enum csilc_kind {
    CSILC_UINT,
    CSILC_NINT,
    CSILC_TEXT,
    CSILC_BYTES,
    CSILC_ARRAY,
    CSILC_MAP,
    CSILC_TAG,
    CSILC_FLOAT,
    CSILC_BOOL,
    CSILC_NULL
} csilc_kind;

typedef struct csilc_value csilc_value;
typedef struct csilc_pair {
    csilc_value *key;
    csilc_value *val;
} csilc_pair;

struct csilc_value {
    csilc_kind kind;
    union {
        uint64_t u;
        int64_t i;
        double f;
        bool boolean;
        struct {
            const uint8_t *ptr; /* TEXT is NUL-terminated in the arena; BYTES is raw */
            size_t len;
        } bytes;
        struct {
            csilc_value *items;
            size_t count;
        } array;
        struct {
            csilc_pair *pairs;
            size_t count;
        } map;
        struct {
            uint64_t num;
            csilc_value *content;
        } tag;
    } as;
};

static inline int csilc_read_arg(const uint8_t *b, size_t len, uint8_t low, uint64_t *arg,
                          size_t *head_len) {
    if (low < 24) { *arg = low; *head_len = 1; return 0; }
    if (low == 24) {
        if (len < 2) return -1;
        *arg = b[1];
        *head_len = 2;
        return 0;
    }
    if (low == 25) {
        if (len < 3) return -1;
        *arg = ((uint64_t)b[1] << 8) | (uint64_t)b[2];
        *head_len = 3;
        return 0;
    }
    if (low == 26) {
        if (len < 5) return -1;
        *arg = ((uint64_t)b[1] << 24) | ((uint64_t)b[2] << 16) | ((uint64_t)b[3] << 8) |
               (uint64_t)b[4];
        *head_len = 5;
        return 0;
    }
    if (low == 27) {
        if (len < 9) return -1;
        uint64_t v = 0;
        for (int i = 1; i <= 8; i++) v = (v << 8) | (uint64_t)b[i];
        *arg = v;
        *head_len = 9;
        return 0;
    }
    return -1; /* 28..31 (indefinite/reserved) are forbidden */
}

static inline const uint8_t *csilc_arena_copy(CsilCodecArena *a, const uint8_t *src, size_t n,
                                       bool as_text) {
    size_t total = as_text ? n + 1 : (n ? n : 1);
    uint8_t *dst = (uint8_t *)csilc_arena_alloc(a, total);
    if (!dst) return NULL;
    if (n) memcpy(dst, src, n);
    if (as_text) dst[n] = 0;
    return dst;
}

/* Decode a half-precision float (only ever seen on decode; encode never emits one). */
static inline double csilc_half_to_double(uint16_t h) {
    uint32_t sign = (uint32_t)(h & 0x8000u) << 16;
    uint32_t exp = (h >> 10) & 0x1f;
    uint32_t mant = h & 0x3ff;
    uint32_t bits;
    if (exp == 0) {
        if (mant == 0) {
            bits = sign;
        } else {
            exp = 127 - 15 + 1;
            while ((mant & 0x400) == 0) { mant <<= 1; exp--; }
            mant &= 0x3ff;
            bits = sign | (exp << 23) | (mant << 13);
        }
    } else if (exp == 0x1f) {
        bits = sign | 0x7f800000u | (mant << 13);
    } else {
        bits = sign | ((exp + (127 - 15)) << 23) | (mant << 13);
    }
    float f;
    memcpy(&f, &bits, 4);
    return (double)f;
}

static inline int csilc_decode_value(CsilCodecArena *a, const uint8_t *b, size_t len,
                              csilc_value *out, size_t *consumed) {
    if (len == 0) return -1;
    uint8_t ib = b[0];
    uint8_t major = ib >> 5;
    uint8_t low = ib & 0x1f;
    uint64_t arg = 0;
    size_t head = 0;
    if (csilc_read_arg(b, len, low, &arg, &head)) return -1;
    switch (major) {
    case 0:
        out->kind = CSILC_UINT;
        out->as.u = arg;
        *consumed = head;
        return 0;
    case 1:
        if (arg > (uint64_t)INT64_MAX) return -1;
        out->kind = CSILC_NINT;
        out->as.i = -1 - (int64_t)arg;
        *consumed = head;
        return 0;
    case 2:
    case 3: {
        if (arg > len - head) return -1;
        bool as_text = major == 3;
        const uint8_t *copy = csilc_arena_copy(a, b + head, (size_t)arg, as_text);
        if (!copy) return -1;
        out->kind = as_text ? CSILC_TEXT : CSILC_BYTES;
        out->as.bytes.ptr = copy;
        out->as.bytes.len = (size_t)arg;
        *consumed = head + (size_t)arg;
        return 0;
    }
    case 4: {
        csilc_value *items = NULL;
        if (arg) {
            items = (csilc_value *)csilc_arena_alloc(a, (size_t)arg * sizeof(*items));
            if (!items) return -1;
        }
        size_t off = head;
        for (uint64_t i = 0; i < arg; i++) {
            size_t m = 0;
            if (csilc_decode_value(a, b + off, len - off, &items[i], &m)) return -1;
            off += m;
        }
        out->kind = CSILC_ARRAY;
        out->as.array.items = items;
        out->as.array.count = (size_t)arg;
        *consumed = off;
        return 0;
    }
    case 5: {
        csilc_pair *pairs = NULL;
        if (arg) {
            pairs = (csilc_pair *)csilc_arena_alloc(a, (size_t)arg * sizeof(*pairs));
            if (!pairs) return -1;
        }
        size_t off = head;
        for (uint64_t i = 0; i < arg; i++) {
            csilc_value *k = (csilc_value *)csilc_arena_alloc(a, sizeof(*k));
            csilc_value *v = (csilc_value *)csilc_arena_alloc(a, sizeof(*v));
            if (!k || !v) return -1;
            size_t m = 0;
            if (csilc_decode_value(a, b + off, len - off, k, &m)) return -1;
            off += m;
            if (csilc_decode_value(a, b + off, len - off, v, &m)) return -1;
            off += m;
            pairs[i].key = k;
            pairs[i].val = v;
        }
        out->kind = CSILC_MAP;
        out->as.map.pairs = pairs;
        out->as.map.count = (size_t)arg;
        *consumed = off;
        return 0;
    }
    case 6: {
        csilc_value *content = (csilc_value *)csilc_arena_alloc(a, sizeof(*content));
        if (!content) return -1;
        size_t m = 0;
        if (csilc_decode_value(a, b + head, len - head, content, &m)) return -1;
        out->kind = CSILC_TAG;
        out->as.tag.num = arg;
        out->as.tag.content = content;
        *consumed = head + m;
        return 0;
    }
    case 7:
        switch (low) {
        case 20:
            out->kind = CSILC_BOOL;
            out->as.boolean = false;
            *consumed = head;
            return 0;
        case 21:
            out->kind = CSILC_BOOL;
            out->as.boolean = true;
            *consumed = head;
            return 0;
        case 22:
        case 23:
            out->kind = CSILC_NULL;
            *consumed = head;
            return 0;
        case 25:
            out->kind = CSILC_FLOAT;
            out->as.f = csilc_half_to_double((uint16_t)arg);
            *consumed = head;
            return 0;
        case 26: {
            uint32_t bits = (uint32_t)arg;
            float f;
            memcpy(&f, &bits, 4);
            out->kind = CSILC_FLOAT;
            out->as.f = (double)f;
            *consumed = head;
            return 0;
        }
        case 27: {
            double d;
            memcpy(&d, &arg, 8);
            out->kind = CSILC_FLOAT;
            out->as.f = d;
            *consumed = head;
            return 0;
        }
        default:
            return -1;
        }
    default:
        return -1;
    }
}

static inline int csilc_decode(const uint8_t *b, size_t len, CsilCodecArena **out_arena,
                        const csilc_value **out) {
    *out_arena = NULL;
    *out = NULL;
    CsilCodecArena *a = csilc_arena_new();
    if (!a) return -1;
    csilc_value *root = (csilc_value *)csilc_arena_alloc(a, sizeof(*root));
    if (!root) {
        csil_codec_arena_free(a);
        return -1;
    }
    size_t consumed = 0;
    if (csilc_decode_value(a, b, len, root, &consumed)) {
        csil_codec_arena_free(a);
        return -1;
    }
    if (consumed != len) { /* a payload is a single self-contained item */
        csil_codec_arena_free(a);
        return -1;
    }
    *out_arena = a;
    *out = root;
    return 0;
}

static inline const csilc_value *csilc_map_get(const csilc_value *v, const char *key) {
    if (!v || v->kind != CSILC_MAP) return NULL;
    size_t klen = strlen(key);
    for (size_t i = 0; i < v->as.map.count; i++) {
        const csilc_value *k = v->as.map.pairs[i].key;
        if (k->kind == CSILC_TEXT && k->as.bytes.len == klen &&
            memcmp(k->as.bytes.ptr, key, klen) == 0) {
            return v->as.map.pairs[i].val;
        }
    }
    return NULL;
}

static inline bool csilc_as_u64(const csilc_value *v, uint64_t *out) {
    if (!v) return false;
    if (v->kind == CSILC_UINT) { *out = v->as.u; return true; }
    if (v->kind == CSILC_NINT && v->as.i >= 0) { *out = (uint64_t)v->as.i; return true; }
    return false;
}

static inline bool csilc_as_i64(const csilc_value *v, int64_t *out) {
    if (!v) return false;
    if (v->kind == CSILC_UINT) {
        if (v->as.u > (uint64_t)INT64_MAX) return false;
        *out = (int64_t)v->as.u;
        return true;
    }
    if (v->kind == CSILC_NINT) { *out = v->as.i; return true; }
    return false;
}

static inline bool csilc_as_f64(const csilc_value *v, double *out) {
    if (!v) return false;
    if (v->kind == CSILC_FLOAT) { *out = v->as.f; return true; }
    if (v->kind == CSILC_UINT) { *out = (double)v->as.u; return true; }
    if (v->kind == CSILC_NINT) { *out = (double)v->as.i; return true; }
    return false;
}

static inline bool csilc_as_bool(const csilc_value *v, bool *out) {
    if (v && v->kind == CSILC_BOOL) { *out = v->as.boolean; return true; }
    return false;
}

/* Typed field readers. Taking the value by pointer (rather than inlining a
 * `!&items[i]` test at the call site) keeps -Waddress quiet when the source is an
 * array element whose address is never NULL. */
static inline bool csilc_get_text(const csilc_value *v, char **out) {
    if (!v || v->kind != CSILC_TEXT) return false;
    *out = (char *)v->as.bytes.ptr;
    return true;
}

static inline bool csilc_get_bytes(const csilc_value *v, uint8_t **out, size_t *out_len) {
    if (!v || v->kind != CSILC_BYTES) return false;
    *out = (uint8_t *)v->as.bytes.ptr;
    *out_len = v->as.bytes.len;
    return true;
}

static inline bool csilc_get_tagged_text(const csilc_value *v, uint64_t num, const char **out) {
    if (!v || v->kind != CSILC_TAG || v->as.tag.num != num ||
        v->as.tag.content->kind != CSILC_TEXT) {
        return false;
    }
    *out = (const char *)v->as.tag.content->as.bytes.ptr;
    return true;
}

static inline bool csilc_get_decimal(const csilc_value *v, int64_t *exp, int64_t *mant) {
    if (!v || v->kind != CSILC_TAG || v->as.tag.num != 4) return false;
    const csilc_value *arr = v->as.tag.content;
    if (arr->kind != CSILC_ARRAY || arr->as.array.count != 2) return false;
    return csilc_as_i64(&arr->as.array.items[0], exp) &&
           csilc_as_i64(&arr->as.array.items[1], mant);
}
"##;

// ---- embedded helper headers ----------------------------------------------

/// CsilDecimal: the exact base-10 `decimal` core type (CBOR tag 4, a two-element
/// `[exponent, mantissa]` array). Self-contained; the host needs no decimal lib.
const CSIL_DECIMAL_H: &str = r#"/* Generated CSIL exact-decimal helper. */
/* Code generated by csilgen; DO NOT EDIT. */
#ifndef CSILGEN_CSIL_DECIMAL_GEN_H
#define CSILGEN_CSIL_DECIMAL_GEN_H

#include <stdint.h>

/* CsilDecimal is the exact, base-10 `decimal` core type. On the wire it is CBOR
 * tag 4 (decimal fraction): a two-element array [exponent, mantissa] whose value
 * is mantissa * 10^exponent. The value is kept as exact integers, never a float,
 * so no precision is lost. A host needing arbitrary precision can widen mantissa
 * to a bignum; the 64-bit form covers the common case without a dependency. */
typedef struct CsilDecimal {
    int64_t exponent;
    int64_t mantissa;
} CsilDecimal;

#endif /* CSILGEN_CSIL_DECIMAL_GEN_H */
"#;

/// CsilTimestamp: the `timestamp` core type (CBOR tag 0, RFC3339 UTC text). Kept
/// as the canonical RFC3339 string plus its epoch-seconds value for comparison.
const CSIL_TIMESTAMP_H: &str = r#"/* Generated CSIL timestamp helper. */
/* Code generated by csilgen; DO NOT EDIT. */
#ifndef CSILGEN_CSIL_TIMESTAMP_GEN_H
#define CSILGEN_CSIL_TIMESTAMP_GEN_H

#include <stdint.h>

/* CsilTimestamp is the `timestamp` core type: CBOR tag 0, an RFC3339 UTC string
 * on the wire. The canonical text is retained verbatim (so a round-trip is
 * byte-stable) alongside its epoch-seconds value for ordering comparisons. */
typedef struct CsilTimestamp {
    const char *rfc3339;
    int64_t epoch_seconds;
} CsilTimestamp;

#endif /* CSILGEN_CSIL_TIMESTAMP_GEN_H */
"#;

#[cfg(test)]
mod tests;
