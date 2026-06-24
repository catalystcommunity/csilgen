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
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
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
            TypeKind::Alias(t) => {
                aliases.push_str(&format!("/* {name} is a type alias. */\n"));
                aliases.push_str(&format!(
                    "typedef {};\n\n",
                    declarator(&base_c_type(t, config), 0, name)
                ));
            }
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

// ---- client ---------------------------------------------------------------

/// The transport seam every generated call delegates to: the host implements
/// `call`, performing the wire round-trip for `(service, op)`. The generator
/// never owns the bytes.
const CLIENT_PRELUDE_C: &str = "\
/* CsilgenTransport is supplied by the caller: it encodes req (CBOR over HTTP,
 * say), performs the call named by (service, op), and writes the response
 * bytes into *resp (caller frees), returning 0 on success or a non-zero error. */
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
            "\"types.gen.h\"".to_string(),
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
        content.push_str(&doc_comment(&[
            &format!("Invoke {wire_service}/{wire_op}. The encoded request rides in req; the"),
            "decoded response bytes are written to *resp (caller frees).",
        ]));
        content.push_str(&format!(
            "static inline int {fn_name}(const CsilgenTransport *t,\n\
             \x20                       const uint8_t *req, size_t req_len,\n\
             \x20                       uint8_t **resp, size_t *resp_len) {{\n"
        ));
        content.push_str(&format!(
            "    return t->call(t->self, \"{wire_service}\", \"{wire_op}\", req, req_len, resp, resp_len);\n"
        ));
        content.push_str("}\n\n");
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
