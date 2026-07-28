//! C code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target c` from `csilgen_c_generator.wasm`. Emits
//! idiomatic C11: transparent structs for records, enum-tagged unions for
//! variants, a conditional `CsilDecimal`/`CsilTimestamp` helper, `csil_`-prefixed
//! client call-sites over a transport seam, and server handler structs with
//! verbose + compact router twins. The WASM-boundary exports mirror the other
//! generators exactly; only `process_generation` and its helpers are C-specific.

use csilgen_common::{
    ChoiceClass, CsilControlOperator, CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression,
    CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition,
    CsilServiceDirection, CsilSizeConstraint, CsilTypeExpression, CsilValidationConstraint,
    GeneratedFile, GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning,
    HoistOptions, WarningLevel, WasmGeneratorInput, WasmGeneratorOutput, hoist_inline_composites,
    wasm_interface::*,
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

fn process_generation(mut input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = CConfig::from_options(&input.config.options)?;
    // Rewrite every inline (anonymous) choice/group field position to a synthesized
    // named rule up front, so every downstream emitter (types/validation/codec/
    // client/server) only ever sees ordinary fully-resolved types with no lookup
    // table of its own — exactly as if the CSIL source had named the shape and
    // referenced it. Unlike TypeScript (whose field emitter can render an
    // all-literal choice as a bare enum type in place), C's `emit_field`/
    // `base_c_type` only ever route a FIELD's type through `Reference`/`Builtin`/
    // `Literal`/`Array`/`Map`/`Tuple` — there is no inline-enum-in-a-struct-member
    // rendering path — so an all-literal choice needs the same synthesized name a
    // mixed-kind or type-bearing choice does (`hoist_all_literal_choices: true`,
    // matching OCaml's setting and rationale, not TypeScript's).
    input.csil_spec = hoist_inline_composites(
        &input.csil_spec,
        HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
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

    // A package's `genquickstart.md` demonstrates both the calling side (the RPC and
    // Datagrams sections, over `client.gen.h`) and the handling side (the Events
    // section, over the `server.gen.h` channel router), so a package must carry both
    // surfaces for its own quickstart to compile against the single emitted package —
    // regardless of which surface the sub-target requested. A flat (non-package) build
    // stays byte-identical: it emits only the requested surface.
    let pkg_mode = emit_packages_includes_c(&input.config.options);
    let want_client =
        matches!(surface, Surface::Client) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    let want_server =
        matches!(surface, Surface::Server) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    if input.csil_spec.service_count > 0 {
        if want_client && let Some(client) = generate_client(&input, &config) {
            files.push(GeneratedFile {
                path: make_path("client.gen.h"),
                content: client,
            });
        }
        if want_server && let Some(server) = generate_server(&input, &config, &mut warnings) {
            files.push(GeneratedFile {
                path: make_path("server.gen.h"),
                content: server,
            });
        }
    }

    // Self-contained publishable-package mode: when `emit_packages` includes "c",
    // emit a README with a copy-paste CSIL-RPC Quickstart alongside the headers, so
    // the OUTPUT directory documents how to drive the generated client end to end.
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
            content: package_readme(&input, &config),
        });
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
    IntEnum(Vec<i64>),
    /// An all-literal choice whose vocabulary is NOT uniformly text-only or
    /// int-only (any other kind or mix, e.g. `"a" / 1`, `1 / true`) — still an
    /// `Enum` per `csilgen_common::choice`'s normative contract, just one C can't
    /// give a single bare-scalar backing to. See `emit_mixed_enum`.
    MixedEnum(Vec<&'a CsilLiteralValue>),
    Union(&'a [CsilTypeExpression]),
    GroupUnion(&'a [CsilGroupExpression]),
}

/// Classify a type-choice's arms via the shared, normative
/// `csilgen_common::classify_choice` (every arm a literal, of any kind or mix, is
/// an `Enum`; at least one non-literal arm is a `Union`) and layer C's own
/// sub-shape on an `Enum`: a uniform-text vocabulary is a bare-text `Enum`, a
/// uniform-integer vocabulary is a bare-integer `IntEnum`, and any other kind mix
/// is a `MixedEnum` — mirrors the ocaml generator's `classify_choice` wrapping
/// `classify_enum`.
fn classify_choice(arms: &[CsilTypeExpression]) -> TypeKind<'_> {
    match csilgen_common::classify_choice(arms) {
        ChoiceClass::Enum(literals) => classify_enum(literals),
        ChoiceClass::Union(_) => TypeKind::Union(arms),
    }
}

/// Sub-classify an all-literal vocabulary into the enum shape C renders: a pure
/// text or pure integer vocabulary keeps its historical bare-scalar backing
/// (`Enum`/`IntEnum`); any other kind or mix becomes a `MixedEnum`.
fn classify_enum(literals: Vec<&CsilLiteralValue>) -> TypeKind<'_> {
    if literals
        .iter()
        .all(|l| matches!(l, CsilLiteralValue::Text(_)))
    {
        TypeKind::Enum(
            literals
                .iter()
                .map(|l| match l {
                    CsilLiteralValue::Text(t) => t.clone(),
                    _ => unreachable!("filtered to Text above"),
                })
                .collect(),
        )
    } else if literals
        .iter()
        .all(|l| matches!(l, CsilLiteralValue::Integer(_)))
    {
        TypeKind::IntEnum(
            literals
                .iter()
                .map(|l| match l {
                    CsilLiteralValue::Integer(n) => *n,
                    _ => unreachable!("filtered to Integer above"),
                })
                .collect(),
        )
    } else {
        TypeKind::MixedEnum(literals)
    }
}

fn classify_rule(rule_type: &CsilRuleType) -> Option<TypeKind<'_>> {
    match rule_type {
        CsilRuleType::GroupDef(g) => Some(TypeKind::Struct(g)),
        CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(TypeKind::Struct(g)),
        // A `X = a / b / c` rule reaches the generator as a `TypeDef` wrapping a
        // `Choice` (not a `TypeChoice`), so it must be classified here too or the
        // whole enum/union collapses to a data-less `void *` alias.
        CsilRuleType::TypeDef(CsilTypeExpression::Choice(arms)) => Some(classify_choice(arms)),
        CsilRuleType::TypeDef(t) => Some(TypeKind::Alias(t)),
        CsilRuleType::TypeChoice(arms) => Some(classify_choice(arms)),
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

/// The names a record type's non-optional by-value members depend on. Optional,
/// array, and map members are pointers and impose no order. A tuple's own
/// non-optional elements are embedded by value the same way, so they are walked
/// too (closing a gap `entry_value_dep` alone leaves: it never looks inside a
/// tuple). Every field here is already a fully-resolved type — an inline
/// choice/group position was rewritten to a `Reference` by
/// `csilgen_common::hoist_inline_composites` before this ever runs (see
/// `process_generation`), so there is no lookup table to consult.
fn struct_value_deps(group: &CsilGroupExpression) -> Vec<String> {
    let mut deps = Vec::new();
    for entry in &group.entries {
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        match unwrap_constrained(&entry.value_type) {
            CsilTypeExpression::Reference(n) if !optional => deps.push(n.clone()),
            CsilTypeExpression::Tuple(tgroup) => {
                for (_, tentry) in tuple_members(tgroup) {
                    if matches!(tentry.occurrence, Some(CsilOccurrence::Optional)) {
                        continue;
                    }
                    if let CsilTypeExpression::Reference(n) = unwrap_constrained(&tentry.value_type)
                    {
                        deps.push(n.clone());
                    }
                }
            }
            _ => {}
        }
    }
    deps
}

fn generate_types(input: &WasmGeneratorInput, config: &CConfig) -> Option<String> {
    // Every named type rule, including any inline choice/group already rewritten to
    // a synthesized named rule by `csilgen_common::hoist_inline_composites` in
    // `process_generation` — from here on a hoisted type is an ordinary rule and
    // needs no special-casing.
    let typed: Vec<(String, TypeKind)> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| classify_rule(&r.rule_type).map(|k| (r.name.clone(), k)))
        .collect();
    if typed.is_empty() {
        return None;
    }
    // Only a Struct/Union/GroupUnion/MixedEnum kind ever joins `order`/`deps`/`defs`
    // below; an Enum/IntEnum/Alias dependency is always satisfied positionally
    // (enums are emitted unconditionally before any struct, aliases carry no
    // members), so including one in a `deps` filter would make it a dependency
    // Kahn's algorithm can never mark ready, needlessly deferring the depending
    // type to the best-effort remnants pass. A `MixedEnum` joins this set (unlike
    // `Enum`/`IntEnum`) because — like `Union` — it is a struct wrapping a tagged
    // union, so another struct embedding it BY VALUE needs its full definition
    // (not merely the forward declaration) to precede.
    let struct_kind_names: std::collections::HashSet<&str> = typed
        .iter()
        .filter(|(_, k)| {
            matches!(
                k,
                TypeKind::Struct(_)
                    | TypeKind::Union(_)
                    | TypeKind::GroupUnion(_)
                    | TypeKind::MixedEnum(_)
            )
        })
        .map(|(n, _)| n.as_str())
        .collect();

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
            TypeKind::IntEnum(values) => emit_int_enum(&mut enums, name, values),
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
                    struct_value_deps(group)
                        .into_iter()
                        .filter(|d| struct_kind_names.contains(d.as_str()))
                        .collect(),
                );
            }
            TypeKind::MixedEnum(literals) => {
                forwards.push_str(&format!("typedef struct {name} {name};\n"));
                let mut s = String::new();
                emit_mixed_enum(&mut s, name, literals);
                defs.insert(name.to_string(), s);
                order.push(name.to_string());
                // All-literal by definition, so no arm can ever be a `Reference` —
                // this always joins `order`/`defs` with an empty dependency list.
                deps.insert(name.to_string(), Vec::new());
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
                            CsilTypeExpression::Reference(n)
                                if struct_kind_names.contains(n.as_str()) =>
                            {
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
                        .filter(|d| struct_kind_names.contains(d.as_str()))
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
    // An `any` field carries an opaque CBOR value (the codec's own value tree). The
    // type only needs the incomplete tag here; codec.gen.h defines the full struct.
    // C11 permits the matching typedef redefinition in both headers.
    if spec_uses_builtin(input, "any") {
        content.push_str("typedef struct csilc_value csilc_value;\n\n");
    }
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

/// Suffix for an integer-enum variant's C enumerator name. A negative literal can't
/// appear in a C identifier, so it is spelled `NEG<magnitude>`.
fn int_variant_suffix(v: i64) -> String {
    if v < 0 {
        format!("NEG{}", v.unsigned_abs())
    } else {
        v.to_string()
    }
}

/// Emit an integer-literal enum (`Priority = 1 / 2 / 3`). Each enumerator is given
/// its literal value, so the bare-integer wire codec reads/writes the value directly.
fn emit_int_enum(content: &mut String, name: &str, values: &[i64]) {
    content.push_str(&format!("/* {name} is an integer enumeration. */\n"));
    content.push_str(&format!("typedef enum {name} {{\n"));
    for value in values {
        content.push_str(&format!(
            "    {}_{} = {value},\n",
            to_upper_snake(name),
            int_variant_suffix(*value)
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
                &c_member(&field),
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
/// Snake_case CSIL field names map to C member names with no case mangling — the
/// wire key is already idiomatic C — the only reshaping being the keyword escape
/// the caller applies via `c_member` (a field named `int` still has to declare).
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
        // A fixed-shape tuple (`[text, int]`, `[text, ?int, ?bool]`) becomes an
        // anonymous struct of positional members. An optional element becomes a
        // pointer so NULL means the absent (null-in-place) slot the wire carries.
        CsilTypeExpression::Tuple(group) => {
            content.push_str("    struct {\n");
            for (member, entry) in tuple_members(group) {
                let et = base_c_type(&entry.value_type, config);
                let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
                let extra_ptr = usize::from(optional && !et.ends_with('*'));
                content.push_str(&format!(
                    "        {};\n",
                    declarator(&et, extra_ptr, &member)
                ));
            }
            content.push_str(&format!("    }} {field};\n"));
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
            declarator(&c_type, 0, &arm_member(arm, i))
        ));
    }
    content.push_str("    } u;\n");
    content.push_str(&format!("}} {name};\n\n"));
}

/// A mixed-kind all-literal choice (`"a" / 1`, `1 / true`, ...) as C: the same
/// `typedef struct { <Tag> tag; union {...} u; } Name;` shape `emit_choice` uses
/// for a general tagged-sum union, but every union payload member is a literal's
/// own scalar/text/bytes C type (never a `Reference` — an all-literal choice can't
/// carry one) and the wire is the BARE literal value, no `[index, value]`
/// discriminator (see `emit_mixed_enum_codec`). This is the C rendering of the
/// `TypeKind::MixedEnum` shape the shared `csilgen_common::classify_choice`
/// contract carves out for an all-literal vocabulary that is not uniformly
/// text-only or int-only (which stay the simpler bare `emit_enum`/`emit_int_enum`
/// C enums).
fn emit_mixed_enum(content: &mut String, name: &str, literals: &[&CsilLiteralValue]) {
    let arms = mixed_arm_names(literals);
    content.push_str(&format!(
        "/* {name} is a mixed-literal enumeration (bare wire value, closed vocabulary). */\n"
    ));
    content.push_str(&format!("typedef enum {name}Tag {{\n"));
    for arm in &arms {
        content.push_str(&format!(
            "    {}_{},\n",
            to_upper_snake(name),
            to_upper_snake(arm)
        ));
    }
    content.push_str(&format!("}} {name}Tag;\n\n"));
    content.push_str(&format!("typedef struct {name} {{\n"));
    content.push_str(&format!("    {name}Tag tag;\n"));
    content.push_str("    union {\n");
    for (lit, arm) in literals.iter().zip(&arms) {
        let c_type = literal_c_type(lit);
        content.push_str(&format!(
            "        {};\n",
            declarator(&c_type, 0, &mixed_arm_member(arm))
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
    // Length checks must know whether a field is text or bytes even through a
    // transparent alias (`Key = bytes`), so the alias map rides along.
    let aliases = codec_aliases(input);
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group
            && group.entries.iter().any(entry_has_check)
        {
            emit_validate_fn(&mut body, &rule.name, group, &aliases);
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
fn emit_validate_fn(
    content: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    aliases: &HashMap<String, CsilTypeExpression>,
) {
    let mut checks = String::new();
    for entry in &group.entries {
        if let Some(field) = entry_field_name(&entry.key) {
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            let fc = FieldCheck::new(&field, &entry.value_type, optional, aliases);
            for metadata in &entry.metadata {
                if let CsilFieldMetadata::Constraint(constraint) = metadata {
                    emit_metadata_check(&mut checks, &fc, constraint);
                }
            }
            if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                for op in constraints {
                    emit_control_check(&mut checks, &fc, op);
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

/// What a length-style constraint (`.size`, minlength/maxlength) measures on a
/// field: `strlen` of a NUL-terminated text, the `len` member of a `CsilBytes` —
/// where `strlen` would be a type error and wrong for binary data anyway — or
/// nothing (a `.size` on an integer bounds its encoded width, which has no
/// in-memory length to test).
enum LenKind {
    Text,
    Bytes,
    Other,
}

/// Everything a per-field check needs to spell a correct member access: the
/// keyword-escaped member, optionality, the length kind resolved through
/// transparent aliases, and whether the member sits behind the extra pointer an
/// optional value-typed field gains (mirroring `emit_field`'s `extra_ptr`).
struct FieldCheck {
    member: String,
    optional: bool,
    kind: LenKind,
    deref: bool,
}

impl FieldCheck {
    fn new(
        field: &str,
        value_type: &CsilTypeExpression,
        optional: bool,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> Self {
        let base = unwrap_constrained(value_type);
        let mut resolved = base;
        // Bounded so a (malformed) alias cycle cannot spin the generator.
        for _ in 0..16 {
            match resolved {
                CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
                    resolved = unwrap_constrained(&aliases[name]);
                }
                _ => break,
            }
        }
        let kind = match resolved {
            CsilTypeExpression::Builtin(n) if n == "text" || n == "tstr" => LenKind::Text,
            CsilTypeExpression::Builtin(n) if n == "bytes" || n == "bstr" => LenKind::Bytes,
            _ => LenKind::Other,
        };
        FieldCheck {
            member: c_member(field),
            optional,
            deref: optional && !base_c_type(base, &default_config()).ends_with('*'),
            kind,
        }
    }
}

/// A length check dispatched on what the field actually is; a kind with no
/// runtime length emits nothing rather than a check that cannot compile.
fn len_check(out: &mut String, fc: &FieldCheck, op: &str, n: u64) {
    match fc.kind {
        LenKind::Text => text_check(out, fc, op, n),
        LenKind::Bytes => bytes_check(out, fc, op, n),
        LenKind::Other => {}
    }
}

/// A string-length check against a NUL-terminated field; the NULL guard covers
/// both an absent optional and an unset required pointer.
fn text_check(out: &mut String, fc: &FieldCheck, op: &str, n: u64) {
    let member = &fc.member;
    let val = if fc.deref {
        format!("(*v->{member})")
    } else {
        format!("v->{member}")
    };
    out.push_str(&format!(
        "    if (v->{member} != NULL && strlen({val}) {op} {n}u) return false;\n"
    ));
}

/// A byte-length check against a `CsilBytes` field's `len` member. A required
/// bytes field is held by value, so its unset state is a NULL `data` pointer; an
/// optional one is behind the extra pointer, NULL when absent.
fn bytes_check(out: &mut String, fc: &FieldCheck, op: &str, n: u64) {
    let member = &fc.member;
    if fc.deref {
        out.push_str(&format!(
            "    if (v->{member} != NULL && v->{member}->len {op} {n}u) return false;\n"
        ));
    } else {
        out.push_str(&format!(
            "    if (v->{member}.data != NULL && v->{member}.len {op} {n}u) return false;\n"
        ));
    }
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

fn emit_metadata_check(out: &mut String, fc: &FieldCheck, constraint: &CsilValidationConstraint) {
    let member = &fc.member;
    match constraint {
        CsilValidationConstraint::MinLength(n) => len_check(out, fc, "<", *n),
        CsilValidationConstraint::MaxLength(n) => len_check(out, fc, ">", *n),
        CsilValidationConstraint::MinItems(n) => out.push_str(&format!(
            "    if (v->{member}_count < {n}u) return false;\n"
        )),
        CsilValidationConstraint::MaxItems(n) => out.push_str(&format!(
            "    if (v->{member}_count > {n}u) return false;\n"
        )),
        CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, member, fc.optional, "<", *n)
        }
        CsilValidationConstraint::MaxValue(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, member, fc.optional, ">", *n)
        }
        _ => {}
    }
}

fn emit_control_check(out: &mut String, fc: &FieldCheck, op: &CsilControlOperator) {
    let member = &fc.member;
    match op {
        CsilControlOperator::Size(CsilSizeConstraint::Min(n)) => len_check(out, fc, "<", *n),
        CsilControlOperator::Size(CsilSizeConstraint::Max(n)) => len_check(out, fc, ">", *n),
        CsilControlOperator::Size(CsilSizeConstraint::Exact(n)) => len_check(out, fc, "!=", *n),
        CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, member, fc.optional, "<", *n)
        }
        CsilControlOperator::LessEqual(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, member, fc.optional, ">", *n)
        }
        CsilControlOperator::GreaterThan(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, member, fc.optional, "<=", *n)
        }
        CsilControlOperator::LessThan(CsilLiteralValue::Integer(n)) => {
            numeric_check(out, member, fc.optional, ">=", *n)
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
struct CodecField {
    name: String,
    // The C struct member the value lives in: the wire name with the keyword
    // escape applied, matching what `emit_field` declared.
    member: String,
    key_bytes: Vec<u8>,
    value_type: CsilTypeExpression,
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
fn codec_fields(group: &CsilGroupExpression) -> Vec<CodecField> {
    let mut fields: Vec<CodecField> = group
        .entries
        .iter()
        .filter_map(|entry| {
            entry_field_name(&entry.key).map(|name| CodecField {
                key_bytes: cbor_text_key_bytes(&name),
                member: c_member(&name),
                name,
                value_type: entry.value_type.clone(),
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
            "null" | "nil" | "undefined" => {
                out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"))
            }
            // `any` is an opaque CBOR value tree; re-emit it verbatim.
            "any" => out.push_str(&format!(
                "{indent}if (csilc_w_value(b, ({expr}))) return -1;\n"
            )),
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
        CsilTypeExpression::Literal(value) => emit_enc_literal(out, indent, value, warnings),
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
            "null" | "nil" | "undefined" => {
                out.push_str(&format!("{indent}({dst}) = NULL;\n"))
            }
            // `any` keeps a borrowed pointer into the decode arena's value tree, valid
            // for the lifetime of the owning decoded value.
            "any" => out.push_str(&format!("{indent}({dst}) = {src};\n")),
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
        CsilTypeExpression::Literal(value) => {
            emit_dec_literal(out, indent, value, src, dst, warnings);
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

fn emit_enc_literal(
    out: &mut String,
    indent: &str,
    value: &CsilLiteralValue,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match value {
        CsilLiteralValue::Integer(i) if *i >= 0 => out.push_str(&format!(
            "{indent}if (csilc_w_uint(b, (uint64_t){i})) return -1;\n"
        )),
        CsilLiteralValue::Integer(i) => out.push_str(&format!(
            "{indent}if (csilc_w_int(b, (int64_t){i})) return -1;\n"
        )),
        CsilLiteralValue::Float(f) => out.push_str(&format!(
            "{indent}if (csilc_w_f64(b, (double){f})) return -1;\n"
        )),
        CsilLiteralValue::Text(s) => out.push_str(&format!(
            "{indent}if (csilc_w_text(b, \"{}\", {})) return -1;\n",
            c_escape(s),
            s.len()
        )),
        CsilLiteralValue::Bool(b) => {
            out.push_str(&format!("{indent}if (csilc_w_bool(b, {b})) return -1;\n"))
        }
        CsilLiteralValue::Null => {
            out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"));
        }
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| format!("0x{b:02x}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "{indent}{{ static const uint8_t csilc_lit[] = {{ {values} }}; if (csilc_w_bytes(b, csilc_lit, {})) return -1; }}\n",
                bytes.len()
            ));
        }
        CsilLiteralValue::Array(_) => {
            warnings.push(GeneratorWarning {
                message: "c codec: array literal encoded as null".to_string(),
                level: WarningLevel::Warning,
                location: None,
                suggestion: None,
            });
            out.push_str(&format!("{indent}if (csilc_w_null(b)) return -1;\n"));
        }
    }
}

fn emit_dec_literal(
    out: &mut String,
    indent: &str,
    value: &CsilLiteralValue,
    src: &str,
    dst: &str,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match value {
        CsilLiteralValue::Integer(i) if *i >= 0 => out.push_str(&format!(
            "{indent}{{ uint64_t csilc_lit; if (!csilc_as_u64({src}, &csilc_lit) || csilc_lit != (uint64_t){i}) return -1; ({dst}) = (int64_t){i}; }}\n"
        )),
        CsilLiteralValue::Integer(i) => out.push_str(&format!(
            "{indent}{{ int64_t csilc_lit; if (!csilc_as_i64({src}, &csilc_lit) || csilc_lit != (int64_t){i}) return -1; ({dst}) = (int64_t){i}; }}\n"
        )),
        CsilLiteralValue::Float(f) => out.push_str(&format!(
            "{indent}{{ double csilc_lit; if (!csilc_as_f64({src}, &csilc_lit) || csilc_lit != (double){f}) return -1; ({dst}) = (double){f}; }}\n"
        )),
        CsilLiteralValue::Text(s) => out.push_str(&format!(
            "{indent}{{ char *csilc_lit; if (!csilc_get_text({src}, &csilc_lit) || strcmp(csilc_lit, \"{}\") != 0) return -1; ({dst}) = csilc_lit; }}\n",
            c_escape(s)
        )),
        CsilLiteralValue::Bool(b) => out.push_str(&format!(
            "{indent}{{ bool csilc_lit; if (!csilc_as_bool({src}, &csilc_lit) || csilc_lit != {b}) return -1; ({dst}) = {b}; }}\n"
        )),
        CsilLiteralValue::Null => out.push_str(&format!(
            "{indent}if (!({src}) || ({src})->kind != CSILC_NULL) return -1; ({dst}) = NULL;\n"
        )),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| format!("0x{b:02x}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "{indent}{{ static const uint8_t csilc_lit[] = {{ {values} }}; uint8_t *csilc_bytes; size_t csilc_len; if (!csilc_get_bytes({src}, &csilc_bytes, &csilc_len) || csilc_len != {} || memcmp(csilc_bytes, csilc_lit, {}) != 0) return -1; ({dst}).data = csilc_bytes; ({dst}).len = csilc_len; }}\n",
                bytes.len(),
                bytes.len()
            ));
        }
        CsilLiteralValue::Array(_) => {
            warnings.push(GeneratorWarning {
                message: "c codec: array literal left zero on decode".to_string(),
                level: WarningLevel::Warning,
                location: None,
                suggestion: None,
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
    match unwrap_constrained(&field.value_type) {
        CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. } => {
            format!("v->{member}_count")
        }
        _ => format!("v->{member}"),
    }
}

/// Emit the key + CBOR array head + per-element encode loop for a list field. The
/// wire key and the C member travel separately: a keyword-named field escapes only
/// its member, never its key.
fn emit_enc_array_body(
    out: &mut String,
    indent: &str,
    field: &CodecField,
    element_type: &CsilTypeExpression,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let member = &field.member;
    out.push_str(&format!(
        "{indent}if (csilc_w_text(b, \"{}\", {})) return -1;\n",
        field.name,
        key_len(&field.name)
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
    field: &CodecField,
    kv: (&CsilTypeExpression, &CsilTypeExpression),
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let (key, value) = kv;
    let member = &field.member;
    out.push_str(&format!(
        "{indent}if (csilc_w_text(b, \"{}\", {})) return -1;\n",
        field.name,
        key_len(&field.name)
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
    let member = &field.member;
    let klen = key_len(&field.name);
    let key_write = format!(
        "    if (csilc_w_text(b, \"{}\", {klen})) return -1;\n",
        field.name
    );
    let base = unwrap_constrained(&field.value_type);
    match base {
        CsilTypeExpression::Array { element_type, .. } => {
            if field.optional {
                out.push_str(&format!("    if (v->{member}_count) {{\n"));
                emit_enc_array_body(out, "        ", field, element_type, scope, warnings);
                out.push_str("    }\n");
            } else {
                emit_enc_array_body(out, "    ", field, element_type, scope, warnings);
            }
        }
        CsilTypeExpression::Map { key, value, .. } => {
            if field.optional {
                out.push_str(&format!("    if (v->{member}_count) {{\n"));
                emit_enc_map_body(out, "        ", field, (key, value), scope, warnings);
                out.push_str("    }\n");
            } else {
                emit_enc_map_body(out, "    ", field, (key, value), scope, warnings);
            }
        }
        // A tuple writes a fixed-length positional CBOR array; an absent optional
        // element is held in place as null so the array length never changes.
        CsilTypeExpression::Tuple(group) => {
            out.push_str(&key_write);
            out.push_str(&format!(
                "    if (csilc_w_array_head(b, {})) return -1;\n",
                group.entries.len()
            ));
            for (tmember, entry) in tuple_members(group) {
                let place = format!("v->{member}.{tmember}");
                let et = base_c_type(&entry.value_type, &default_config());
                let is_ptr = et.ends_with('*');
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    out.push_str(&format!("    if ({place}) {{\n"));
                    let val = if is_ptr {
                        place.clone()
                    } else {
                        format!("(*{place})")
                    };
                    emit_enc_value(out, "        ", &entry.value_type, &val, scope, warnings);
                    out.push_str("    } else {\n");
                    out.push_str("        if (csilc_w_null(b)) return -1;\n");
                    out.push_str("    }\n");
                } else {
                    emit_enc_value(out, "    ", &entry.value_type, &place, scope, warnings);
                }
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
    let member = &field.member;
    out.push_str(&format!(
        "    csilc_f = csilc_map_get(m, \"{}\");\n",
        field.name
    ));
    let base = unwrap_constrained(&field.value_type);
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
        // A tuple reads a fixed-length positional array; an optional slot is the
        // borrowed null marker (pointer left NULL) or a decoded, arena-held value.
        CsilTypeExpression::Tuple(group) => {
            let n = group.entries.len();
            out.push_str(&format!(
                "    if (!csilc_f || csilc_f->kind != CSILC_ARRAY || csilc_f->as.array.count != {n}) return -1;\n"
            ));
            for (i, (tmember, entry)) in tuple_members(group).iter().enumerate() {
                let dst = format!("out->{member}.{tmember}");
                let src = format!("&csilc_f->as.array.items[{i}]");
                let et = base_c_type(&entry.value_type, &default_config());
                let is_ptr = et.ends_with('*');
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    out.push_str(&format!("    if (({src})->kind == CSILC_NULL) {{\n"));
                    out.push_str(&format!("        {dst} = NULL;\n"));
                    out.push_str("    } else {\n");
                    if is_ptr {
                        emit_dec_value(
                            out,
                            "        ",
                            &entry.value_type,
                            &src,
                            &dst,
                            scope,
                            warnings,
                        );
                    } else {
                        out.push_str(&format!(
                            "        {et} *csilc_tp = ({et} *)csilc_arena_alloc(a, sizeof({et}));\n"
                        ));
                        out.push_str("        if (!csilc_tp) return -1;\n");
                        emit_dec_value(
                            out,
                            "        ",
                            &entry.value_type,
                            &src,
                            "(*csilc_tp)",
                            scope,
                            warnings,
                        );
                        out.push_str(&format!("        {dst} = csilc_tp;\n"));
                    }
                    out.push_str("    }\n");
                } else {
                    emit_dec_value(out, "    ", &entry.value_type, &src, &dst, scope, warnings);
                }
            }
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
            enc_presence(field, &field.member)
        ));
    }
    out.push_str("    if (csilc_w_map_head(b, csilc_n)) return -1;\n");
    // An empty record (`{}`) reads no fields, so `v` would otherwise be an unused
    // parameter under -Wextra; the cast keeps the codec warning-clean.
    if fields.is_empty() {
        out.push_str("    (void)v;\n");
    }
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
    out.push_str("    (void)a;\n");
    // `csilc_f` and `out` are only touched when there are fields to read; an empty
    // record leaves them unused, so declare/cast them to match the field count.
    if fields.is_empty() {
        out.push_str("    (void)out;\n");
    } else {
        out.push_str("    const csilc_value *csilc_f;\n");
    }
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

/// Emit the codec for an integer-literal enum. The wire form is the bare integer
/// literal, and each C enumerator already equals its literal, so encode writes the
/// value directly and decode matches it back, rejecting an out-of-set integer.
fn emit_int_enum_codec(out: &mut String, name: &str, values: &[i64]) {
    out.push_str(&format!(
        "/* csilc_enc_{name} writes the {name} variant's bare integer literal. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str("    return csilc_w_int(b, (int64_t)(*v));\n}\n\n");
    out.push_str(&format!(
        "/* csilc_dec_{name} matches the wire integer back to a {name} variant. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *src, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    (void)a;\n    int64_t csilc_v;\n");
    out.push_str("    if (!csilc_as_i64(src, &csilc_v)) return -1;\n");
    out.push_str("    switch (csilc_v) {\n");
    for value in values {
        out.push_str(&format!("    case {value}:\n"));
    }
    out.push_str(&format!(
        "        *out = ({name})csilc_v;\n        return 0;\n"
    ));
    out.push_str("    default: return -1;\n    }\n}\n\n");
}

/// Emit the codec for a tagged-sum union. The wire form is a 2-element CBOR array
/// `[variant_index, value]`, the index being the 0-based declaration order, so any
/// arm types round-trip unambiguously (matching rust/go/python).
fn emit_union_codec(
    out: &mut String,
    name: &str,
    arms: &[CsilTypeExpression],
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    out.push_str(&format!(
        "/* csilc_enc_{name} writes the union as a tagged sum [variant_index, value]. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str("    if (csilc_w_array_head(b, 2)) return -1;\n");
    out.push_str("    switch (v->tag) {\n");
    for (i, arm) in arms.iter().enumerate() {
        let tag = format!(
            "{}_{}",
            to_upper_snake(name),
            to_upper_snake(&arm_name(arm, i))
        );
        let member = arm_member(arm, i);
        out.push_str(&format!("    case {tag}:\n"));
        out.push_str(&format!("        if (csilc_w_uint(b, {i})) return -1;\n"));
        emit_enc_value(
            out,
            "        ",
            arm,
            &format!("v->u.{member}"),
            scope,
            warnings,
        );
        out.push_str("        break;\n");
    }
    out.push_str("    default: return -1;\n    }\n    return 0;\n}\n\n");

    out.push_str(&format!(
        "/* csilc_dec_{name} reads a tagged sum [variant_index, value] into the union. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    (void)a;\n    uint64_t csilc_idx;\n");
    out.push_str("    if (!m || m->kind != CSILC_ARRAY || m->as.array.count != 2) return -1;\n");
    out.push_str("    if (!csilc_as_u64(&m->as.array.items[0], &csilc_idx)) return -1;\n");
    out.push_str("    switch (csilc_idx) {\n");
    for (i, arm) in arms.iter().enumerate() {
        let tag = format!(
            "{}_{}",
            to_upper_snake(name),
            to_upper_snake(&arm_name(arm, i))
        );
        let member = arm_member(arm, i);
        out.push_str(&format!("    case {i}:\n"));
        out.push_str(&format!("        out->tag = {tag};\n"));
        emit_dec_value(
            out,
            "        ",
            arm,
            "&m->as.array.items[1]",
            &format!("out->u.{member}"),
            scope,
            warnings,
        );
        out.push_str("        return 0;\n");
    }
    out.push_str("    default: return -1;\n    }\n}\n\n");
}

/// One membership-scan arm of `emit_mixed_enum_codec`'s decode: a `{ ...; if
/// (<src matches this literal>) { commit tag + payload; return 0; } }` block.
/// Unlike `emit_dec_literal` (which unconditionally rejects the WHOLE field on a
/// mismatch, correct for a single required literal), a mixed-enum arm must fall
/// through to try the NEXT literal on a mismatch, so the match test and the
/// commit are gated behind one `if` instead of an early `return -1`; the caller
/// appends a final `return -1;` once every arm has had its turn.
fn emit_mixed_dec_arm(
    out: &mut String,
    tag: &str,
    member: &str,
    lit: &CsilLiteralValue,
    warnings: &mut Vec<GeneratorWarning>,
) {
    match lit {
        CsilLiteralValue::Integer(i) if *i >= 0 => out.push_str(&format!(
            "    {{ uint64_t csilc_v; if (csilc_as_u64(src, &csilc_v) && csilc_v == (uint64_t){i}) {{ out->tag = {tag}; out->u.{member} = (int64_t){i}; return 0; }} }}\n"
        )),
        CsilLiteralValue::Integer(i) => out.push_str(&format!(
            "    {{ int64_t csilc_v; if (csilc_as_i64(src, &csilc_v) && csilc_v == (int64_t){i}) {{ out->tag = {tag}; out->u.{member} = (int64_t){i}; return 0; }} }}\n"
        )),
        CsilLiteralValue::Float(f) => out.push_str(&format!(
            "    {{ double csilc_v; if (csilc_as_f64(src, &csilc_v) && csilc_v == (double){f}) {{ out->tag = {tag}; out->u.{member} = (double){f}; return 0; }} }}\n"
        )),
        CsilLiteralValue::Text(s) => out.push_str(&format!(
            "    {{ char *csilc_v; if (csilc_get_text(src, &csilc_v) && strcmp(csilc_v, \"{}\") == 0) {{ out->tag = {tag}; out->u.{member} = csilc_v; return 0; }} }}\n",
            c_escape(s)
        )),
        CsilLiteralValue::Bool(b) => out.push_str(&format!(
            "    {{ bool csilc_v; if (csilc_as_bool(src, &csilc_v) && csilc_v == {b}) {{ out->tag = {tag}; out->u.{member} = {b}; return 0; }} }}\n"
        )),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| format!("0x{b:02x}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "    {{ static const uint8_t csilc_lit[] = {{ {values} }}; uint8_t *csilc_b; size_t csilc_len; if (csilc_get_bytes(src, &csilc_b, &csilc_len) && csilc_len == {n} && memcmp(csilc_b, csilc_lit, {n}) == 0) {{ out->tag = {tag}; out->u.{member}.data = csilc_b; out->u.{member}.len = csilc_len; return 0; }} }}\n",
                n = bytes.len()
            ));
        }
        CsilLiteralValue::Null => out.push_str(&format!(
            "    if (src && src->kind == CSILC_NULL) {{ out->tag = {tag}; out->u.{member} = NULL; return 0; }}\n"
        )),
        // An array literal has no CBOR-tree membership test this codec can express
        // (see `emit_enc_literal`'s matching warned degrade); the arm simply never
        // matches on decode, same as it never round-trips on encode.
        CsilLiteralValue::Array(_) => {
            warnings.push(GeneratorWarning {
                message: "c codec: array literal in mixed enum never matches on decode"
                    .to_string(),
                level: WarningLevel::Warning,
                location: None,
                suggestion: None,
            });
        }
    }
}

/// Emit the codec for a `TypeKind::MixedEnum` (see `emit_mixed_enum`). The wire
/// form is the BARE literal value — no `[index, value]` wrapper, unlike
/// `emit_union_codec` — so encode writes the tag's own known literal directly via
/// `emit_enc_literal` (the same per-kind writer a literal choice arm uses
/// anywhere else in this codec) and decode reads the one bare CBOR value and
/// scans every declared literal (`emit_mixed_dec_arm`) for a kind-and-value
/// match, rejecting anything outside the declared vocabulary — mirroring
/// `emit_int_enum_codec`'s switch, generalized across literal kinds since no
/// single `csilc_as_*` accessor spans them all.
fn emit_mixed_enum_codec(
    out: &mut String,
    name: &str,
    literals: &[&CsilLiteralValue],
    warnings: &mut Vec<GeneratorWarning>,
) {
    let arms = mixed_arm_names(literals);
    out.push_str(&format!(
        "/* csilc_enc_{name} writes the {name} variant's own bare literal value (no [index, value] wrapper). */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v) {{\n"
    ));
    out.push_str("    switch (v->tag) {\n");
    for (lit, arm) in literals.iter().zip(&arms) {
        out.push_str(&format!(
            "    case {}_{}:\n",
            to_upper_snake(name),
            to_upper_snake(arm)
        ));
        emit_enc_literal(out, "        ", lit, warnings);
        out.push_str("        return 0;\n");
    }
    out.push_str("    default: return -1;\n    }\n}\n\n");

    out.push_str(&format!(
        "/* csilc_dec_{name} matches the bare wire value back to a {name} variant, rejecting anything outside the declared vocabulary. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *src, CsilCodecArena *a, {name} *out) {{\n"
    ));
    out.push_str("    (void)a;\n");
    for (lit, arm) in literals.iter().zip(&arms) {
        let tag = format!("{}_{}", to_upper_snake(name), to_upper_snake(arm));
        let member = mixed_arm_member(arm);
        emit_mixed_dec_arm(out, &tag, &member, lit, warnings);
    }
    out.push_str("    return -1;\n}\n\n");
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
    // Every named type rule, including any inline choice/group already rewritten
    // to a synthesized named rule by `csilgen_common::hoist_inline_composites` in
    // `process_generation`, gets exactly the same codec treatment.
    let typed: Vec<(String, TypeKind)> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| classify_rule(&r.rule_type).map(|k| (r.name.clone(), k)))
        .collect();
    // Records, enums, and named map/list aliases all carry a generated codec, so a
    // field referencing any of them flows through the record-reference codec arm.
    let codec_names = codec_type_names(input);
    // A service with unidirectional ops still needs this header even when no named
    // type is codec'd, because the client's per-op boundary wrappers live here; only
    // a spec with neither codec'd types nor such ops leaves the header unemitted.
    if codec_names.is_empty() && !spec_has_unidirectional_ops(input) {
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
            TypeKind::IntEnum(values) => {
                decls.push_str(&format!(
                    "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v);\n"
                ));
                decls.push_str(&format!(
                    "static inline int csilc_dec_{name}(const csilc_value *src, CsilCodecArena *a, {name} *out);\n"
                ));
                emit_int_enum_codec(&mut bodies, name, values);
            }
            TypeKind::MixedEnum(literals) => {
                decls.push_str(&format!(
                    "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v);\n"
                ));
                decls.push_str(&format!(
                    "static inline int csilc_dec_{name}(const csilc_value *src, CsilCodecArena *a, {name} *out);\n"
                ));
                emit_mixed_enum_codec(&mut bodies, name, literals, warnings);
            }
            TypeKind::Union(arms) => {
                decls.push_str(&format!(
                    "static inline int csilc_enc_{name}(csilc_buf *b, const {name} *v);\n"
                ));
                decls.push_str(&format!(
                    "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {name} *out);\n"
                ));
                emit_union_codec(&mut bodies, name, arms, &scope, warnings);
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
        if !matches!(
            kind,
            TypeKind::Struct(_)
                | TypeKind::Enum(_)
                | TypeKind::IntEnum(_)
                | TypeKind::MixedEnum(_)
                | TypeKind::Union(_)
        ) && alias_aggregate(kind).is_none()
        {
            continue;
        }
        emit_public_wrappers(&mut public, name, name);
    }

    // Synthetic codecs for the op boundaries that are not codec'd named types: a
    // scalar/transparent-alias request like `get-house: HouseID -> House` and a bare
    // `[* House]` response. Without these the typed client would call a
    // `csil_encode_HouseID`/`csil_decode_<list>` that no record or alias ever defined.
    // Each name is emitted once across the whole spec; a bare array reuses the
    // named-list machinery (an items+count struct plus its list codec) so the wire
    // form is a plain CBOR array, identical to what a named list alias produces.
    let mut synth_structs = String::new();
    let mut synth_bodies = String::new();
    let mut synth_public = String::new();
    let mut synth_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            for ty in [&op.input_type, &success] {
                let boundary = classify_boundary(ty, &codec_names, config);
                let Some(synth) = boundary.synth else {
                    continue;
                };
                if !synth_seen.insert(boundary.codec_name.clone()) {
                    continue;
                }
                match synth {
                    SynthBoundary::Scalar(scalar_ty) => emit_scalar_boundary_codec(
                        &mut synth_bodies,
                        &boundary.codec_name,
                        &boundary.c_type,
                        &scalar_ty,
                        &scope,
                        warnings,
                    ),
                    SynthBoundary::Array(element) => {
                        emit_list_alias_struct(
                            &mut synth_structs,
                            &boundary.codec_name,
                            &element,
                            config,
                        );
                        emit_list_alias_codec(
                            &mut synth_bodies,
                            &boundary.codec_name,
                            &element,
                            &scope,
                            warnings,
                        );
                    }
                }
                emit_public_wrappers(&mut synth_public, &boundary.codec_name, &boundary.c_type);
            }
        }
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
    // A synthetic boundary struct precedes its codec, which precedes its public
    // wrapper, so each name is defined before use without a forward-declaration pass.
    content.push_str(&synth_structs);
    content.push_str(&synth_bodies);
    content.push_str(&public);
    content.push_str(&synth_public);
    header_close(&mut content, "CSILGEN_CODEC_GEN_H");
    Some(content)
}

/// Emit the public encode/decode wrappers for a codec type: `csil_encode_<name>`
/// fills a malloc'd buffer the caller frees with `free()`; `csil_decode_<name>`
/// fills a value backed by an arena the caller frees once with
/// `csil_codec_arena_free`. `c_type` is the C token of the value (it equals `name`
/// for records/enums/aliases, and differs only for a bare-builtin op boundary).
fn emit_public_wrappers(public: &mut String, name: &str, c_type: &str) {
    public.push_str(&doc_comment(&[
        &format!("Encode a {name} to CBOR. On success *out is a malloc'd buffer of"),
        "*out_len bytes the caller frees with free(); returns non-zero on failure.",
    ]));
    public.push_str(&format!(
        "static inline int csil_encode_{name}(const {c_type} *v, uint8_t **out, size_t *out_len) {{\n\
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
        "static inline int csil_decode_{name}(const uint8_t *in, size_t len, {c_type} *out, CsilCodecArena **owner) {{\n\
         \x20   CsilCodecArena *a;\n\
         \x20   const csilc_value *root;\n\
         \x20   if (csilc_decode(in, len, &a, &root)) return -1;\n\
         \x20   if (csilc_dec_{name}(root, a, out)) {{ csil_codec_arena_free(a); return -1; }}\n\
         \x20   *owner = a;\n\
         \x20   return 0;\n}}\n\n"
    ));
}

/// The named types that carry a generated codec: records, enums, and named map/list
/// aliases. A reference to any of these resolves to its `csil_encode_*`/`csil_decode_*`.
fn codec_type_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| {
            let kind = classify_rule(&rule.rule_type)?;
            let codecd = matches!(
                kind,
                TypeKind::Struct(_)
                    | TypeKind::Enum(_)
                    | TypeKind::IntEnum(_)
                    | TypeKind::MixedEnum(_)
                    | TypeKind::Union(_)
            ) || alias_aggregate(&kind).is_some();
            codecd.then(|| rule.name.clone())
        })
        .collect()
}

fn spec_has_unidirectional_ops(input: &WasmGeneratorInput) -> bool {
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::ServiceDef(service) => service
                .operations
                .iter()
                .any(|op| matches!(op.direction, CsilServiceDirection::Unidirectional)),
            _ => false,
        })
}

/// A synthetic per-op-boundary codec the client needs but no named type provides:
/// a scalar/transparent-alias value (one CBOR value, no map wrapper) or a bare
/// `[* T]` list (a CBOR array, built on the named-list machinery).
enum SynthBoundary {
    Scalar(CsilTypeExpression),
    Array(CsilTypeExpression),
}

/// How one operation request/response type is (de)serialized by the typed client.
/// `codec_name` is the `csil_encode_<X>`/`csil_decode_<X>` suffix; `c_type` is the C
/// token the client declares. `synth` is set when the codec must additionally emit
/// those functions because no named type already carries them.
struct OpBoundary {
    is_null: bool,
    codec_name: String,
    c_type: String,
    synth: Option<SynthBoundary>,
}

/// Classify an op boundary type. A reference to a codec'd named type reuses its
/// existing `csil_encode_*`/`csil_decode_*`; a `null`/`nil` input carries no body;
/// everything else (a scalar/transparent alias, a bare builtin, a bare `[* T]`)
/// needs a synthetic codec so every operation round-trips, not only record ones.
fn classify_boundary(
    ty: &CsilTypeExpression,
    codec_names: &std::collections::HashSet<String>,
    config: &CConfig,
) -> OpBoundary {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(n) if n == "null" || n == "nil" => OpBoundary {
            is_null: true,
            codec_name: String::new(),
            c_type: "void".to_string(),
            synth: None,
        },
        CsilTypeExpression::Reference(n) if codec_names.contains(n) => OpBoundary {
            is_null: false,
            codec_name: n.clone(),
            c_type: n.clone(),
            synth: None,
        },
        // A reference to a transparent scalar alias (`HouseID = text`): its typedef
        // makes the C token identical to the alias name, and the value emitters
        // resolve the reference to its underlying codec.
        reference @ CsilTypeExpression::Reference(n) => OpBoundary {
            is_null: false,
            codec_name: n.clone(),
            c_type: n.clone(),
            synth: Some(SynthBoundary::Scalar(reference.clone())),
        },
        CsilTypeExpression::Array { element_type, .. } => {
            let name = array_boundary_name(element_type);
            OpBoundary {
                is_null: false,
                codec_name: name.clone(),
                c_type: name,
                synth: Some(SynthBoundary::Array((**element_type).clone())),
            }
        }
        builtin @ CsilTypeExpression::Builtin(n) => OpBoundary {
            is_null: false,
            codec_name: n.clone(),
            c_type: base_c_type(builtin, config),
            synth: Some(SynthBoundary::Scalar(builtin.clone())),
        },
        // A bare map or other unrepresentable boundary degrades to a value the codec
        // warns about and encodes as null, so the output still compiles.
        other => OpBoundary {
            is_null: false,
            codec_name: "CsilUnsupportedBoundary".to_string(),
            c_type: "void *".to_string(),
            synth: Some(SynthBoundary::Scalar(other.clone())),
        },
    }
}

/// The synthesized codec/struct name for a bare `[* T]` op boundary. A reference
/// element keeps its name, a builtin element is capitalized, and anything else
/// collapses to `Value`; the `Csil…List` shape avoids colliding with user types.
fn array_boundary_name(element: &CsilTypeExpression) -> String {
    let token = match unwrap_constrained(element) {
        CsilTypeExpression::Reference(n) => n.clone(),
        CsilTypeExpression::Builtin(n) => {
            let mut chars = n.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => "Value".to_string(),
            }
        }
        _ => "Value".to_string(),
    };
    format!("Csil{token}List")
}

/// Emit the encode + decode bodies for a synthetic scalar op boundary: a single
/// CBOR value with no map wrapper, so the wire form matches what a record field of
/// the same type would write.
fn emit_scalar_boundary_codec(
    out: &mut String,
    name: &str,
    c_type: &str,
    ty: &CsilTypeExpression,
    scope: &CodecScope,
    warnings: &mut Vec<GeneratorWarning>,
) {
    out.push_str(&format!(
        "/* csilc_enc_{name} writes a bare {name} value. */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_enc_{name}(csilc_buf *b, const {c_type} *v) {{\n"
    ));
    emit_enc_value(out, "    ", ty, "(*v)", scope, warnings);
    out.push_str("    return 0;\n}\n\n");
    out.push_str(&format!(
        "/* csilc_dec_{name} reads a bare {name} value (arena-borrowed). */\n"
    ));
    out.push_str(&format!(
        "static inline int csilc_dec_{name}(const csilc_value *m, CsilCodecArena *a, {c_type} *out) {{\n"
    ));
    out.push_str("    (void)a;\n");
    emit_dec_value(out, "    ", ty, "m", "(*out)", scope, warnings);
    out.push_str("    return 0;\n}\n\n");
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
    // The client classifies each op boundary against the codec's named types so a
    // record reuses its `csil_encode_*`/`csil_decode_*` while a scalar/alias/array
    // boundary calls the synthetic wrapper the codec emits for it.
    let codec_names = codec_type_names(input);
    let mut body = String::new();
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_calls(&mut body, &rule.name, service, &codec_names, config);
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
    codec_names: &std::collections::HashSet<String>,
    config: &CConfig,
) {
    let base = service_base(name);
    let prefix = to_snake(&base);
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
        let wire_op = &op.name;
        let success = success_type(&op.output_type);
        let resp = classify_boundary(&success, codec_names, config);
        let req = classify_boundary(&op.input_type, codec_names, config);
        let resp_type = resp.c_type;
        let has_input = !req.is_null;
        let req_type = req.c_type;

        content.push_str(&doc_comment(&[
            &format!("Invoke {name}/{wire_op} with a typed request and decode the typed"),
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
                req.codec_name
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
            "    int csil_rc = t->call(t->self, \"{name}\", \"{wire_op}\", csil_reqb, csil_reqn, &csil_respb, &csil_respn);\n"
        ));
        content.push_str("    free(csil_reqb);\n");
        content.push_str("    if (csil_rc != 0) { free(csil_respb); return csil_rc; }\n");
        content.push_str(&format!(
            "    int csil_drc = csil_decode_{}(csil_respb, csil_respn, resp, resp_owner);\n",
            resp.codec_name
        ));
        content.push_str("    free(csil_respb);\n");
        content.push_str("    return csil_drc;\n}\n\n");
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
        let wire_op = &op.name;
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
        let wire_op = &op.name;
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
            "null" | "nil" | "undefined" => "void *".to_string(),
            // `any` carries an opaque CBOR value through verbatim, held as the codec's
            // own decoded value-tree node so it re-encodes byte-identically.
            "any" => "const csilc_value *".to_string(),
            other => other.to_string(),
        },
        CsilTypeExpression::Literal(value) => literal_c_type(value),
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

/// The union payload member for a choice arm — the single derivation shared by the
/// type emitter and both codec directions, so an arm named after a C keyword
/// (`int` / `float`) is escaped identically everywhere it is declared or accessed.
fn arm_member(arm: &CsilTypeExpression, index: usize) -> String {
    c_member(&to_snake(&arm_name(arm, index)))
}

/// The C arm-name fragment for one arm of a `TypeKind::MixedEnum` — the literal's
/// own value rendered as an identifier, so a tag like `Priority_PENDING` or
/// `Priority_NEG1` reads back to its wire value (mirrors `arm_name`'s use of a
/// reference/builtin's own name; `int_variant_suffix` gives the same negative-safe
/// spelling `emit_int_enum`'s tags already use). A kind with no meaningful
/// identifier spelling (float/bytes/null/array) falls back to a positional
/// `Value<N>`, matching `arm_name`'s `Choice<N>` fallback for a non-nameable arm.
fn mixed_arm_name(lit: &CsilLiteralValue, index: usize) -> String {
    match lit {
        CsilLiteralValue::Text(s) => s.clone(),
        CsilLiteralValue::Integer(n) => int_variant_suffix(*n),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Float(_)
        | CsilLiteralValue::Bytes(_)
        | CsilLiteralValue::Null
        | CsilLiteralValue::Array(_) => format!("Value{index}"),
    }
}

/// The unique per-arm names for a `TypeKind::MixedEnum`'s literals, disambiguating
/// a spelling collision (e.g. text `"1"` and integer `1` both naming their arm
/// `1`) with the same positional fallback `mixed_arm_name` uses for an unnameable
/// kind, so every tag/union-member name this type declares is guaranteed unique.
fn mixed_arm_names(literals: &[&CsilLiteralValue]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    literals
        .iter()
        .enumerate()
        .map(|(i, lit)| {
            let base = mixed_arm_name(lit, i);
            if seen.insert(base.clone()) {
                base
            } else {
                let fallback = format!("Value{i}");
                seen.insert(fallback.clone());
                fallback
            }
        })
        .collect()
}

/// The union payload member for one already-disambiguated `TypeKind::MixedEnum`
/// arm name, escaped exactly like `arm_member` so a name colliding with a C
/// keyword still declares. Unlike a choice arm's `Reference`/`Builtin` name (never
/// digit-leading), a positive-integer literal's arm name IS its bare digits (e.g.
/// `1`) — a valid ENUM TAG SUFFIX (always prefixed by the type name, `..._1`) but
/// not a valid bare C identifier on its own, so a digit-leading result here gets a
/// `v` prefix.
fn mixed_arm_member(arm_name: &str) -> String {
    let member = c_member(&to_snake(arm_name));
    if member.starts_with(|c: char| c.is_ascii_digit()) {
        format!("v{member}")
    } else {
        member
    }
}

/// The C type one literal value's union payload member carries — the same mapping
/// `base_c_type`'s `Literal` arm uses for a bare literal-typed field, factored out
/// so `emit_mixed_enum`'s union members and `base_c_type` agree exactly.
fn literal_c_type(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(_) => "int64_t".to_string(),
        CsilLiteralValue::Float(_) => "double".to_string(),
        CsilLiteralValue::Text(_) => "char *".to_string(),
        CsilLiteralValue::Bytes(_) => "CsilBytes".to_string(),
        CsilLiteralValue::Bool(_) => "bool".to_string(),
        CsilLiteralValue::Null | CsilLiteralValue::Array(_) => "void *".to_string(),
    }
}

/// The positional members of a tuple, paired with their entries. A keyed tuple entry
/// (`[tag: text, value: any]`) keeps its name; an unnamed positional element becomes
/// `f<index>`. The same naming is used by the struct, encoder, and decoder.
fn tuple_members(group: &CsilGroupExpression) -> Vec<(String, &CsilGroupEntry)> {
    group
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            // Tuple members are positional on the wire, so the keyword escape
            // costs nothing in wire fidelity.
            let name = entry_field_name(&entry.key)
                .map(|n| c_member(&n))
                .unwrap_or_else(|| format!("f{i}"));
            (name, entry)
        })
        .collect()
}

// ---- naming (wire names verbatim; C symbols cased) ------------------------

/// PascalCase by a simple rule — break on `_`/`-`, uppercase the letter after
/// each break, keep the rest — used only to shape C identifiers (via
/// `service_base`). Wire strings carry the verbatim CSIL names instead.
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

/// C keywords (plus the `<stdbool.h>` macros, which macro-expand inside any
/// declaration that names them) that can collide with a snake_case member name
/// derived from CSIL. The `_X`-spelled C11 keywords are omitted: `to_snake`
/// can never produce a leading-underscore-capital spelling.
const C_RESERVED_MEMBER_NAMES: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void", "volatile", "while", "bool", "true", "false",
];

/// A C member identifier for a derived name: reserved words take a trailing
/// underscore (`int` -> `int_`) so a CSIL field or choice arm named after a C
/// keyword still declares. Wire keys are never routed through here — they stay
/// verbatim.
fn c_member(name: &str) -> String {
    if C_RESERVED_MEMBER_NAMES.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// Strip a trailing `Service` suffix and PascalCase the remainder, used only for
/// C identifiers (handler structs, function prefixes, wire-id macros). Wire
/// strings carry the verbatim CSIL service name instead (csil-rpc-transport.md §1.1).
fn service_base(name: &str) -> String {
    let pascal = simple_pascal(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

// ---- self-contained package (README + Quickstart) -------------------------

/// True only when the `emit_packages` generation option is an array containing the
/// `"c"` token. Parsed defensively against an arbitrary `serde_json::Value`: a
/// missing option, a non-array value, or an array without `"c"` all leave the
/// output as source-only. The match is case-insensitive to be forgiving.
fn emit_packages_includes_c(options: &HashMap<String, serde_json::Value>) -> bool {
    options
        .get("emit_packages")
        .and_then(|v| v.as_array())
        .is_some_and(|tokens| {
            tokens
                .iter()
                .filter_map(|v| v.as_str())
                .any(|token| token.eq_ignore_ascii_case("c"))
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
        // A path-style `package_name` is the cross-ecosystem source of truth; C wants
        // only its tail. See `package_name_last_segment`.
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

/// The first service (in declaration order) that has a `->` operation, reduced to
/// an example call. `None` for a serviceless / types-only package.
struct CUnaryExample {
    fn_name: String,
    wire_service: String,
    wire_op: String,
    req_type: String,
    resp_type: String,
    has_request: bool,
    req_literal: String,
    resp_print_field: Option<String>,
    /// The request/response record names for the per-type codec helpers
    /// (`csil_encode_<X>` / `csil_decode_<X>`) the datagram section calls; `None`
    /// when the payload is not a record reference the codec covers.
    req_codec: Option<String>,
    resp_codec: Option<String>,
    /// The op's datagram ordinal: its `@wire-id` when present, else a placeholder.
    op_ord: u64,
}

fn first_unary_example(input: &WasmGeneratorInput, config: &CConfig) -> Option<CUnaryExample> {
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        let Some(op) = service
            .operations
            .iter()
            .find(|op| matches!(op.direction, CsilServiceDirection::Unidirectional))
        else {
            continue;
        };
        let base = service_base(&rule.name);
        let prefix = to_snake(&base);
        let success = success_type(&op.output_type);
        let has_request = !op_input_is_null(&op.input_type);
        return Some(CUnaryExample {
            fn_name: format!("csil_{prefix}_{}", to_snake(&op.name)),
            wire_service: rule.name.clone(),
            wire_op: op.name.clone(),
            req_type: base_c_type(&op.input_type, config),
            resp_type: base_c_type(&success, config),
            has_request,
            req_literal: if has_request {
                c_request_literal(input, &op.input_type, config)
            } else {
                String::new()
            },
            resp_print_field: first_text_field(input, &success),
            req_codec: if has_request {
                record_ref_name(input, &op.input_type)
            } else {
                None
            },
            resp_codec: record_ref_name(input, &success),
            op_ord: op.wire_id.unwrap_or(1),
        });
    }
    None
}

/// The record name a type reference names, if it resolves to a record the codec
/// covers; `None` otherwise. Used to gate the codec-driven datagram payload.
fn record_ref_name(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> Option<String> {
    let CsilTypeExpression::Reference(name) = unwrap_constrained(ty) else {
        return None;
    };
    find_record(input, name).map(|_| name.clone())
}

/// The pieces the Events session needs: the generated handlers struct + channel router +
/// outbound encoder names, the inbound (op input) and outbound (op success output) record
/// type names, the handler method, the outbound wire op + sample literal, and the wire
/// service.
struct CChannelExample {
    handlers_struct: String,
    service_snake: String,
    wire_service: String,
    route_fn: String,
    encode_fn: String,
    handler_method: String,
    inbound_type: String,
    outbound_type: String,
    outbound_wire_op: String,
    outbound_sample: String,
}

/// The first service (declaration order) with a `<->` op whose input and success output
/// are both records (so the generated router, encoder, and per-type codec helpers exist).
/// `None` when no service has a usable channel op — the Events section then shows the
/// handshake/heartbeat without dispatch wiring.
fn first_channel_example(input: &WasmGeneratorInput, config: &CConfig) -> Option<CChannelExample> {
    let _ = config;
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let (Some(inbound), Some(outbound)) = (
                record_ref_name(input, &op.input_type),
                record_ref_name(input, &success),
            ) else {
                continue;
            };
            let base = service_base(&rule.name);
            let snake = to_snake(&base);
            let method = to_snake(&op.name);
            return Some(CChannelExample {
                handlers_struct: format!("{base}Handlers"),
                service_snake: snake.clone(),
                wire_service: rule.name.clone(),
                route_fn: format!("route_{snake}_channel"),
                encode_fn: format!("encode_{snake}_{method}"),
                handler_method: method,
                inbound_type: inbound,
                outbound_type: outbound,
                outbound_wire_op: op.name.clone(),
                outbound_sample: c_request_literal(input, &success, config),
            });
        }
    }
    None
}

/// A compiling C designated-initializer for the request record's required fields:
/// real values for scalars, `"example"` for text, and `{0}` for shapes a generic
/// sample can't fabricate, so the snippet always compiles even where a user must
/// fill a value in.
fn c_request_literal(
    input: &WasmGeneratorInput,
    ty: &CsilTypeExpression,
    config: &CConfig,
) -> String {
    let CsilTypeExpression::Reference(name) = unwrap_constrained(ty) else {
        return "{0}".to_string();
    };
    let Some(group) = find_record(input, name) else {
        return "{0}".to_string();
    };
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .filter_map(|e| {
            entry_field_name(&e.key).map(|field| {
                format!(
                    ".{} = {}",
                    c_member(&field),
                    c_sample_value(&e.value_type, config)
                )
            })
        })
        .collect();
    if fields.is_empty() {
        "{0}".to_string()
    } else {
        format!("{{ {} }}", fields.join(", "))
    }
}

/// A single C value literal for `ty`, used inside a request initializer.
fn c_sample_value(ty: &CsilTypeExpression, config: &CConfig) -> String {
    match unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" | "true" | "false" => "false".to_string(),
            "int" | "nint" | "uint" => "0".to_string(),
            "float" | "float16" | "float32" | "float64" | "double" => "0.0".to_string(),
            _ => "{0}".to_string(),
        },
        _ => {
            let _ = config;
            "{0}".to_string()
        }
    }
}

/// The first required text field of a record type reference, so the example can
/// print a typed response value rather than just announcing success.
fn first_text_field(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> Option<String> {
    let CsilTypeExpression::Reference(name) = unwrap_constrained(ty) else {
        return None;
    };
    let group = find_record(input, name)?;
    group.entries.iter().find_map(|e| {
        let is_text = matches!(unwrap_constrained(&e.value_type), CsilTypeExpression::Builtin(n) if n == "text" || n == "tstr");
        if is_text && !matches!(e.occurrence, Some(CsilOccurrence::Optional)) {
            entry_field_name(&e.key).map(|n| c_member(&n))
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

/// Which of the three transport sections to render. The `genquickstart_transports`
/// option is a JSON array subset of `["rpc","events","datagrams"]`; unknown entries are
/// ignored, and an absent or all-unknown value means "all three" so the document always
/// renders something coherent.
fn wanted_transports(options: &HashMap<String, serde_json::Value>) -> (bool, bool, bool) {
    let Some(items) = options
        .get("genquickstart_transports")
        .and_then(|v| v.as_array())
    else {
        return (true, true, true);
    };
    let names: std::collections::BTreeSet<&str> = items.iter().filter_map(|v| v.as_str()).collect();
    let any_known = ["rpc", "events", "datagrams"]
        .iter()
        .any(|t| names.contains(t));
    if any_known {
        (
            names.contains("rpc"),
            names.contains("events"),
            names.contains("datagrams"),
        )
    } else {
        (true, true, true)
    }
}

/// The package README: a transport-by-transport Quickstart over the official `csil`
/// reference library (`transports/c`). The generated codec owns CBOR (de)serialization
/// and the library owns the envelope/framing/lifecycle; the consumer supplies only a
/// *carrier* that moves bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events
/// over TLS, CSIL-Datagrams over UDP) is a complete, copy-paste example built on the lib.
fn package_readme(input: &WasmGeneratorInput, config: &CConfig) -> String {
    let name = package_name(input);
    let mut out = format!(
        "# {name}\n\n\
         Generated by csilgen. A typed CSIL client in C: the generated codec owns CBOR\n\
         (de)serialization and the `csil` transport library (`transports/c`) owns the\n\
         envelope, framing, and connection lifecycle. You supply only a *carrier* that\n\
         moves bytes, so the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw\n\
         UDP unchanged.\n\n\
         ## Consume\n\n\
         C has no package manager, so vendor the generated headers into your project and\n\
         put this directory on your include path. The transport library is not yet\n\
         published; vendor `transports/c` (its `include/` on your include path, its `src/`\n\
         in your build) for now. A single translation unit that `#include`s `client.gen.h`\n\
         pulls in the types and the self-contained CBOR codec; `#include <csil/csil.h>`\n\
         pulls in the transport envelopes. Build with any C11 compiler:\n\n\
         ```sh\n\
         cc -I. -Ipath/to/transports/c/include main.c path/to/transports/c/src/*.c -o demo\n\
         ```\n\n\
         > This package ships both surfaces: `client.gen.h` (RPC + Datagrams) and\n\
         > `server.gen.h` (the Events channel router), plus the shared `codec.gen.h` /\n\
         > `types.gen.h` — so all three sections below compile against this one directory.\n\n"
    );

    let (rpc, events, datagrams) = wanted_transports(&input.config.options);
    let unary = first_unary_example(input, config);
    let channel = first_channel_example(input, config);
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

/// CSIL-RPC over HTTP: a carrier implementing the generated `CsilgenTransport` byte seam
/// that builds the envelope with the library's `csil_rpc_request_*` and decodes the
/// library's `csil_rpc_response_*` (never hand-rolled), POSTing to `{host}:{port}/csil/v1/rpc`
/// over raw POSIX sockets. The typed client decodes the success payload; a non-zero
/// transport status and the `ServiceError` arm are surfaced distinctly.
fn rpc_section(ex: Option<&CUnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`csil_rpc_request` /\n\
         `csil_rpc_response`); you bring a carrier that moves bytes. The carrier below\n\
         implements the generated `CsilgenTransport` byte seam and POSTs over raw POSIX\n\
         sockets — swap it for libcurl or any HTTP client.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```c\n");
    out.push_str(RPC_CARRIER_C);
    out.push('\n');
    // The example call: build the carrier, wire the seam, call the first op.
    out.push_str("int main(void) {\n");
    out.push_str("    CsilRpcCarrier carrier = { .host = \"127.0.0.1\", .port = \"5080\" };\n");
    out.push_str(
        "    CsilgenTransport transport = { .call = csil_rpc_call, .self = &carrier };\n\n",
    );
    out.push_str("    CsilCodecArena *owner = NULL;\n");
    let resp_decl = declarator(&ex.resp_type, 0, "resp");
    out.push_str(&format!("    {resp_decl};\n"));
    if ex.has_request {
        let req_decl = declarator(&ex.req_type, 0, "req");
        out.push_str(&format!("    {req_decl} = {};\n", ex.req_literal));
        out.push_str(&format!(
            "    if ({}(&transport, &req, &resp, &owner) != 0) {{\n",
            ex.fn_name
        ));
    } else {
        out.push_str(&format!(
            "    if ({}(&transport, &resp, &owner) != 0) {{\n",
            ex.fn_name
        ));
    }
    out.push_str(&format!(
        "        fprintf(stderr, \"csil-rpc {}/{} failed\\n\");\n",
        ex.wire_service, ex.wire_op
    ));
    out.push_str("        return 1;\n    }\n");
    match &ex.resp_print_field {
        Some(field) => out.push_str(&format!(
            "    printf(\"{}/{} -> %s\\n\", resp.{field});\n",
            ex.wire_service, ex.wire_op
        )),
        None => out.push_str(&format!(
            "    printf(\"{}/{} ok\\n\");\n",
            ex.wire_service, ex.wire_op
        )),
    }
    out.push_str("    csil_codec_arena_free(owner); // frees everything `resp` borrows\n");
    out.push_str("    return 0;\n}\n");
    out.push_str("```\n\n");
    out
}

/// CSIL-Events over TLS: a full session example. A TLS `csil_stream` (OpenSSL) is wrapped
/// in the library's `csil_stream_carrier` (length-prefix framing); the session does the
/// `$hello`/`$hello-ack` handshake, sends one outbound event via the generated
/// `encode_<svc>_<op>`, and runs a recv loop that decodes each frame to a `csil_event`,
/// answers `$ping` with `$pong`, and dispatches typed events to the generated
/// `route_<svc>_channel`. With no channel op the dispatch wiring becomes a note.
fn events_section(ch: Option<&CChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns\n\
         the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and length-\n\
         prefix framing (`csil_stream_carrier` over a `csil_stream`); the generated router\n\
         dispatches typed events. The TLS carrier below uses OpenSSL (link `-lssl\n\
         -lcrypto`) — swap it for any byte stream (a WebSocket, a QUIC stream).\n\n",
    );
    out.push_str("```c\n");
    out.push_str(EVENTS_CARRIER_C);
    out.push('\n');
    match ch {
        Some(ch) => out.push_str(&events_session(ch)),
        None => out.push_str(EVENTS_NO_CHANNEL_SESSION_C),
    }
    out.push_str("```\n\n");
    out
}

/// The channel session body for an Events connection that has a `<->` op: a `CsilgenCodec`
/// backed by the op's generated per-type helpers, the handshake, one outbound event via
/// the generated encoder, and the recv loop that heartbeats and dispatches into the
/// generated router.
fn events_session(ch: &CChannelExample) -> String {
    let CChannelExample {
        handlers_struct,
        service_snake,
        wire_service,
        route_fn,
        encode_fn,
        handler_method,
        inbound_type,
        outbound_type,
        outbound_wire_op,
        outbound_sample,
    } = ch;
    let _ = service_snake;
    format!(
        r#"// Back the generated router's codec with the per-type CBOR helpers. decode heap-allocs
// the typed message (its arena backs the strings; the host frees it after dispatch);
// encode writes a fresh buffer the caller frees.
static int channel_decode(void *self, const uint8_t *data, size_t len, void *out) {{
    (void)self;
    {inbound_type} *msg = ({inbound_type} *)calloc(1, sizeof *msg);
    CsilCodecArena *owner = NULL;
    if (!msg || csil_decode_{inbound_type}(data, len, msg, &owner)) {{ free(msg); return -1; }}
    *(void **)out = msg; /* owner backs msg; free with csil_codec_arena_free post-dispatch */
    return 0;
}}
static int channel_encode(void *self, const void *value, uint8_t **out, size_t *out_len) {{
    (void)self;
    return csil_encode_{outbound_type}((const {outbound_type} *)value, out, out_len);
}}

// The host's handler implementation; dispatch lands here.
static int on_{handler_method}(void *ctx, const {inbound_type} *msg) {{
    (void)ctx;
    (void)msg;
    printf("event {outbound_wire_op}\n");
    return 0;
}}

/* The max-frame guard is a carrier setting, not a generated constant: raise it when a
   peer accepts payloads larger than the 16 MiB default (the envelope adds framing and
   request metadata around the payload, so the limit must exceed the largest payload), or
   lower it to harden an exposed listener. Valid limits are 1..CSIL_MAX_FRAME_LIMIT; an
   invalid one yields a carrier with NULL send_frame/recv_frame, checked below. */
#define MAX_FRAME CSIL_MAX_FRAME_DEFAULT

static int session(SSL *ssl) {{
    csil_stream stream = {{ .read = tls_read, .write = tls_write, .userdata = ssl }};
    csil_frame_carrier carrier = csil_stream_carrier(&stream, MAX_FRAME);
    if (!carrier.send_frame) {{ return -1; }} /* invalid max-frame limit */
    CsilgenCodec codec = {{ .decode = channel_decode, .encode = channel_encode, .self = NULL }};
    {handlers_struct} handlers = {{ .{handler_method} = on_{handler_method} }};

    // $hello / $hello-ack handshake (control plane); the ack pins the wire profile for
    // the connection's lifetime.
    const uint64_t versions[] = {{ CSIL_VERSION }};
    const char *profiles[] = {{ "verbose" }};
    csil_hello hello = {{ .versions = versions, .versions_len = 1,
                         .profiles = profiles, .profiles_len = 1, .service = "{wire_service}" }};
    uint8_t *hb = NULL; size_t hbn = 0;
    if (csil_hello_encode(&hello, &hb, &hbn)
        || carrier.send_frame(carrier.userdata, hb, hbn)) {{
        csil_free(hb); csil_stream_carrier_dispose(&carrier); return -1;
    }}
    csil_free(hb);

    uint8_t *ackf = NULL; size_t ackn = 0;
    if (carrier.recv_frame(carrier.userdata, &ackf, &ackn) || !ackf) {{
        csil_stream_carrier_dispose(&carrier); return -1;
    }}
    csil_hello_ack ack;
    csil_profile profile;
    if (csil_hello_ack_decode(ackf, ackn, &ack)
        || !csil_profile_parse(ack.profile, &profile)) {{
        csil_hello_ack_free(&ack); csil_free(ackf);
        csil_stream_carrier_dispose(&carrier); return -1;
    }}
    csil_hello_ack_free(&ack);
    csil_free(ackf);

    // Send one outbound event via the generated encoder, framed as a verbose Event.
    {outbound_type} out_msg = {outbound_sample};
    uint8_t *outb = NULL; size_t outn = 0;
    if ({encode_fn}(&codec, &out_msg, &outb, &outn) == 0) {{
        csil_event ev;
        csil_event_init_verbose(&ev, "{wire_service}", "{outbound_wire_op}", outb, outn);
        uint8_t *evb = NULL; size_t evn = 0;
        if (csil_event_encode(&ev, profile, &evb, &evn) == 0) {{
            carrier.send_frame(carrier.userdata, evb, evn);
            csil_free(evb);
        }}
        free(outb);
    }}

    // Recv loop: decode each frame to an Event, answer $ping with $pong, dispatch the
    // rest to the generated router.
    for (;;) {{
        uint8_t *frame = NULL; size_t flen = 0;
        if (carrier.recv_frame(carrier.userdata, &frame, &flen) || !frame) break;
        csil_event ev;
        if (csil_event_decode(frame, flen, profile, &ev)) {{ csil_free(frame); break; }}
        if (ev.event && strcmp(ev.event, CSIL_PING_NAME) == 0) {{
            csil_heartbeat ping;
            if (csil_heartbeat_decode(ev.payload, ev.payload_len, &ping) == 0) {{
                csil_heartbeat pong = {{ .nonce = ping.nonce }};
                uint8_t *pb = NULL; size_t pn = 0;
                if (csil_heartbeat_encode(&pong, &pb, &pn) == 0) {{
                    csil_event pe;
                    csil_event_init_verbose(&pe, NULL, CSIL_PONG_NAME, pb, pn);
                    uint8_t *peb = NULL; size_t pen = 0;
                    if (csil_event_encode(&pe, profile, &peb, &pen) == 0) {{
                        carrier.send_frame(carrier.userdata, peb, pen);
                        csil_free(peb);
                    }}
                    csil_free(pb);
                }}
                csil_heartbeat_free(&ping);
            }}
        }} else if (ev.event) {{
            {route_fn}(&handlers, NULL, &codec, ev.event, ev.payload, ev.payload_len);
        }}
        csil_event_free(&ev);
        csil_free(frame);
    }}
    csil_stream_carrier_dispose(&carrier);
    return 0;
}}
"#,
    )
}

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in the
/// library's `csil_datagram`, and send it fire-and-forget over a POSIX UDP socket. The
/// recv path decodes an inbound `csil_datagram` and decodes its payload with the generated
/// codec into the RESPONSE type — there is NO synchronous response.
fn datagrams_section(ex: Option<&CUnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `csil_datagram`\n\
         envelope; you bring a datagram carrier. The UDP carrier below uses raw POSIX\n\
         sockets (libc only) — QUIC datagrams or a WebRTC channel drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_codec), Some(resp_codec)) = (&ex.req_codec, &ex.resp_codec) else {
        out.push_str(
            "This package's `->` operations have non-record payloads, so there is no codec-driven\n\
             datagram payload to encode; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```c\n");
    out.push_str(DATAGRAMS_CARRIER_C);
    out.push('\n');
    out.push_str(&format!(
        "// The operation's datagram ordinal — its @wire-id, or a channel-agreed number.\n#define OP_ORD {}u\n\n",
        ex.op_ord
    ));
    out.push_str("int main(void) {\n");
    out.push_str("    int fd = udp_connect(\"127.0.0.1\", \"9000\");\n");
    out.push_str("    if (fd < 0) return 1;\n");
    out.push_str("    UdpCarrier u = { .fd = fd };\n");
    out.push_str(
        "    csil_datagram_carrier carrier = { .send_datagram = udp_send,\n\
         \x20                                     .recv_datagram = udp_recv, .userdata = &u };\n\n",
    );
    out.push_str(
        "    // Fire-and-forget: encode the `->` request via the generated codec, wrap it in\n\
         \x20   // the library's Datagram, and send it. seq 0 marks an unsequenced datagram.\n",
    );
    let req_decl = declarator(&ex.req_type, 0, "req");
    out.push_str(&format!("    {req_decl} = {};\n", ex.req_literal));
    out.push_str("    uint8_t *payload = NULL; size_t payload_len = 0;\n");
    out.push_str(&format!(
        "    if (csil_encode_{req_codec}(&req, &payload, &payload_len)) {{ close(fd); return 1; }}\n"
    ));
    out.push_str("    csil_datagram dg;\n");
    out.push_str("    csil_datagram_init(&dg, OP_ORD, 0, payload, payload_len);\n");
    out.push_str("    uint8_t *frame = NULL; size_t frame_len = 0;\n");
    out.push_str("    if (csil_datagram_encode(&dg, &frame, &frame_len) == 0) {\n");
    out.push_str("        carrier.send_datagram(carrier.userdata, frame, frame_len);\n");
    out.push_str("        csil_free(frame);\n    }\n");
    out.push_str("    free(payload);\n\n");
    out.push_str(
        "    // Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There\n\
         \x20   // is NO synchronous response; the caller must tolerate loss and reordering and\n\
         \x20   // handle a reply whenever (if ever) it shows up.\n",
    );
    out.push_str("    uint8_t *inb = NULL; size_t inn = 0;\n");
    out.push_str(
        "    if (carrier.recv_datagram(carrier.userdata, &inb, &inn) == CSIL_OK && inb) {\n",
    );
    out.push_str("        csil_datagram in_dg;\n");
    out.push_str("        if (csil_datagram_decode(inb, inn, &in_dg) == 0) {\n");
    let resp_decl = declarator(&ex.resp_type, 0, "resp");
    out.push_str(&format!("            {resp_decl};\n"));
    out.push_str("            CsilCodecArena *owner = NULL;\n");
    out.push_str(&format!(
        "            if (csil_decode_{resp_codec}(in_dg.payload, in_dg.payload_len, &resp, &owner) == 0) {{\n"
    ));
    out.push_str("                printf(\"late response\\n\");\n");
    out.push_str("                csil_codec_arena_free(owner);\n            }\n");
    out.push_str("            csil_datagram_free(&in_dg);\n        }\n");
    out.push_str("        csil_free(inb);\n    }\n");
    out.push_str("    close(fd);\n    return 0;\n}\n");
    out.push_str("```\n\n");
    out
}

/// The CSIL-RPC HTTP carrier — spec-independent, so a constant. It builds the request
/// envelope with the library's `csil_rpc_request_*`, POSTs it to `{host}:{port}/csil/v1/rpc`
/// over a raw POSIX socket (libc only), and decodes the library's `csil_rpc_response_*`;
/// a non-zero transport status (`csil_rpc_response_is_transport_error`) and the typed
/// `ServiceError` variant each become a non-zero rc.
const RPC_CARRIER_C: &str = r##"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (csil_rpc_request / csil_rpc_response); the carrier owns only the transport. The HTTP
// POST is hand-rolled over POSIX sockets (libc only) — swap it for libcurl or any client.
// Drop this in a .c file next to the generated headers and the vendored transport lib.
//
// The feature-test macro exposes getaddrinfo/socket under a strict `-std=c11`; it must
// precede the first system header, so it leads the file.
#define _POSIX_C_SOURCE 200112L
#include "client.gen.h"
#include <csil/csil.h> // the csil reference transport library (transports/c)

#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

typedef struct CsilRpcCarrier {
    const char *host; // e.g. "127.0.0.1"
    const char *port; // e.g. "5080"
} CsilRpcCarrier;

// Read the whole socket to EOF (the server replies with Connection: close).
static int csil_read_all(int fd, uint8_t **out, size_t *out_len) {
    size_t cap = 4096, len = 0;
    uint8_t *buf = (uint8_t *)malloc(cap);
    if (!buf) return -1;
    for (;;) {
        if (len == cap) {
            uint8_t *grown = (uint8_t *)realloc(buf, cap * 2);
            if (!grown) { free(buf); return -1; }
            buf = grown;
            cap *= 2;
        }
        ssize_t n = read(fd, buf + len, cap - len);
        if (n < 0) { free(buf); return -1; }
        if (n == 0) break;
        len += (size_t)n;
    }
    *out = buf;
    *out_len = len;
    return 0;
}

// The CsilgenTransport seam: build the CSIL-RPC envelope with the library, POST it, and
// decode the library's response — never hand-rolling the envelope.
static int csil_rpc_call(void *self, const char *service, const char *op,
                         const uint8_t *req, size_t req_len,
                         uint8_t **resp, size_t *resp_len) {
    CsilRpcCarrier *c = (CsilRpcCarrier *)self;

    // 1. Encode the request envelope with the library (tag-24 payload, canonical CBOR).
    csil_rpc_request rq;
    csil_rpc_request_init(&rq, service, op, req, req_len);
    uint8_t *env = NULL;
    size_t env_len = 0;
    if (csil_rpc_request_encode(&rq, &env, &env_len)) return -1;

    // 2. POST it to {host}:{port}/csil/v1/rpc over a raw socket (libc only).
    struct addrinfo hints, *ai;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(c->host, c->port, &hints, &ai)) { csil_free(env); return -1; }
    int fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
    if (fd < 0 || connect(fd, ai->ai_addr, ai->ai_addrlen)) {
        if (fd >= 0) close(fd);
        freeaddrinfo(ai);
        csil_free(env);
        return -1;
    }
    freeaddrinfo(ai);

    char header[256];
    int hn = snprintf(header, sizeof(header),
                      "POST /csil/v1/rpc HTTP/1.1\r\nHost: %s\r\n"
                      "Content-Type: application/cbor\r\nContent-Length: %zu\r\n"
                      "Connection: close\r\n\r\n",
                      c->host, env_len);
    if (hn < 0 || hn >= (int)sizeof(header)
        || write(fd, header, (size_t)hn) < 0
        || (env_len && write(fd, env, env_len) < 0)) {
        close(fd);
        csil_free(env);
        return -1;
    }
    csil_free(env);

    uint8_t *raw = NULL;
    size_t raw_len = 0;
    if (csil_read_all(fd, &raw, &raw_len)) { close(fd); return -1; }
    close(fd);

    // 3. Split HTTP headers from the CBOR body; require a 200 status line.
    uint8_t *body = NULL;
    size_t body_len = 0;
    for (size_t i = 0; i + 4 <= raw_len; i++) {
        if (memcmp(raw + i, "\r\n\r\n", 4) == 0) {
            body = raw + i + 4;
            body_len = raw_len - i - 4;
            break;
        }
    }
    if (!body || raw_len < 12 || memcmp(raw + 9, "200", 3) != 0) { free(raw); return -1; }

    // 4. Decode the response envelope with the library.
    csil_rpc_response rsp;
    if (csil_rpc_response_decode(body, body_len, &rsp)) { free(raw); return -1; }

    // A non-zero transport status is a transport failure (no typed payload).
    if (csil_rpc_response_is_transport_error(&rsp)) {
        csil_rpc_response_free(&rsp);
        free(raw);
        return -1;
    }
    // A typed application error rides as a status-0 "ServiceError" variant — surface it
    // so the typed client decodes success only.
    if (rsp.variant && strcmp(rsp.variant, "ServiceError") == 0) {
        fprintf(stderr, "csil-rpc %s/%s: service error\n", service, op);
        csil_rpc_response_free(&rsp);
        free(raw);
        return 1;
    }

    // 5. Hand a malloc'd copy of the success payload to the generated client.
    *resp = (uint8_t *)malloc(rsp.payload_len ? rsp.payload_len : 1);
    if (!*resp) { csil_rpc_response_free(&rsp); free(raw); return -1; }
    memcpy(*resp, rsp.payload, rsp.payload_len);
    *resp_len = rsp.payload_len;

    csil_rpc_response_free(&rsp);
    free(raw);
    return 0;
}
"##;

/// The CSIL-Events TLS carrier prelude — spec-independent. A `csil_stream` over an OpenSSL
/// `SSL*`; the per-spec session wraps it in the library's `csil_stream_carrier` (length-
/// prefix framing). Read/write are synchronous so the host owns a simple blocking I/O loop.
const EVENTS_CARRIER_C: &str = r##"// One example carrier: a TLS byte stream (OpenSSL — link -lssl -lcrypto) the library
// frames with its 4-byte length prefix via csil_stream_carrier. Swap OpenSSL for any byte
// stream (a WebSocket, a QUIC stream) by filling in csil_stream.
#include "server.gen.h" // the generated handlers + channel router surface (--target c-server)
#include "codec.gen.h"  // the per-type CBOR (de)serializers
#include <csil/csil.h>  // the csil reference transport library (transports/c)

#include <openssl/ssl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// csil_stream read/write over an OpenSSL SSL*; the host owns the I/O loop and threads.
static long tls_read(void *self, uint8_t *buf, size_t len) {
    int n = SSL_read((SSL *)self, buf, (int)len);
    return n > 0 ? (long)n : (n == 0 ? 0 : -1);
}
static int tls_write(void *self, const uint8_t *buf, size_t len) {
    return SSL_write((SSL *)self, buf, (int)len) == (int)len ? 0 : -1;
}
"##;

/// The Events session body when the spec declares no `<->` op: the handshake and heartbeat
/// still apply, so they are shown, with a note where the dispatch would go.
const EVENTS_NO_CHANNEL_SESSION_C: &str = r##"/* The max-frame guard is a carrier setting an
   operator can raise or lower; valid limits are 1..CSIL_MAX_FRAME_LIMIT, and an invalid one
   yields a carrier with NULL send_frame/recv_frame, checked below. */
#define MAX_FRAME CSIL_MAX_FRAME_DEFAULT

static int session(SSL *ssl) {
    csil_stream stream = { .read = tls_read, .write = tls_write, .userdata = ssl };
    csil_frame_carrier carrier = csil_stream_carrier(&stream, MAX_FRAME);
    if (!carrier.send_frame) { return -1; } /* invalid max-frame limit */

    // $hello / $hello-ack handshake (control plane).
    const uint64_t versions[] = { CSIL_VERSION };
    const char *profiles[] = { "verbose" };
    csil_hello hello = { .versions = versions, .versions_len = 1,
                         .profiles = profiles, .profiles_len = 1 };
    uint8_t *hb = NULL; size_t hbn = 0;
    if (csil_hello_encode(&hello, &hb, &hbn)
        || carrier.send_frame(carrier.userdata, hb, hbn)) {
        csil_free(hb); csil_stream_carrier_dispose(&carrier); return -1;
    }
    csil_free(hb);

    uint8_t *ackf = NULL; size_t ackn = 0;
    if (carrier.recv_frame(carrier.userdata, &ackf, &ackn) || !ackf) {
        csil_stream_carrier_dispose(&carrier); return -1;
    }
    csil_hello_ack ack;
    csil_profile profile;
    if (csil_hello_ack_decode(ackf, ackn, &ack)
        || !csil_profile_parse(ack.profile, &profile)) {
        csil_hello_ack_free(&ack); csil_free(ackf);
        csil_stream_carrier_dispose(&carrier); return -1;
    }
    csil_hello_ack_free(&ack);
    csil_free(ackf);

    // Recv loop: answer $ping with $pong. This package declares no <->/<- operations, so
    // there is no generated channel router to dispatch typed events into.
    for (;;) {
        uint8_t *frame = NULL; size_t flen = 0;
        if (carrier.recv_frame(carrier.userdata, &frame, &flen) || !frame) break;
        csil_event ev;
        if (csil_event_decode(frame, flen, profile, &ev)) { csil_free(frame); break; }
        if (ev.event && strcmp(ev.event, CSIL_PING_NAME) == 0) {
            csil_heartbeat ping;
            if (csil_heartbeat_decode(ev.payload, ev.payload_len, &ping) == 0) {
                csil_heartbeat pong = { .nonce = ping.nonce };
                uint8_t *pb = NULL; size_t pn = 0;
                if (csil_heartbeat_encode(&pong, &pb, &pn) == 0) {
                    csil_event pe;
                    csil_event_init_verbose(&pe, NULL, CSIL_PONG_NAME, pb, pn);
                    uint8_t *peb = NULL; size_t pen = 0;
                    if (csil_event_encode(&pe, profile, &peb, &pen) == 0) {
                        carrier.send_frame(carrier.userdata, peb, pen);
                        csil_free(peb);
                    }
                    csil_free(pb);
                }
                csil_heartbeat_free(&ping);
            }
        }
        csil_event_free(&ev);
        csil_free(frame);
    }
    csil_stream_carrier_dispose(&carrier);
    return 0;
}
"##;

/// The CSIL-Datagrams UDP carrier prelude — spec-independent. A `csil_datagram_carrier`
/// over a connected POSIX UDP socket; `udp_send` writes one packet and `udp_recv` reads
/// the next (it never waits for or correlates a reply).
const DATAGRAMS_CARRIER_C: &str = r##"// One example carrier: UDP via POSIX sockets (libc only). Datagrams are unreliable and
// unordered, so the carrier never waits for or correlates a reply.
#define _POSIX_C_SOURCE 200112L
#include "client.gen.h"
#include <csil/csil.h> // the csil reference transport library (transports/c)

#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

typedef struct UdpCarrier { int fd; } UdpCarrier;

static csil_err udp_send(void *self, const uint8_t *data, size_t len) {
    UdpCarrier *u = (UdpCarrier *)self;
    return send(u->fd, data, len, 0) == (ssize_t)len ? CSIL_OK : CSIL_ERR_CARRIER;
}
static csil_err udp_recv(void *self, uint8_t **out, size_t *out_len) {
    UdpCarrier *u = (UdpCarrier *)self;
    uint8_t buf[CSIL_MAX_DATAGRAM_DEFAULT];
    ssize_t n = recv(u->fd, buf, sizeof buf, 0);
    if (n < 0) return CSIL_ERR_CARRIER;
    uint8_t *copy = (uint8_t *)malloc((size_t)n ? (size_t)n : 1);
    if (!copy) return CSIL_ERR_OOM;
    memcpy(copy, buf, (size_t)n);
    *out = copy;
    *out_len = (size_t)n;
    return CSIL_OK;
}

static int udp_connect(const char *host, const char *port) {
    struct addrinfo hints, *ai;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_DGRAM;
    if (getaddrinfo(host, port, &hints, &ai)) return -1;
    int fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
    if (fd >= 0 && connect(fd, ai->ai_addr, ai->ai_addrlen)) { close(fd); fd = -1; }
    freeaddrinfo(ai);
    return fd;
}
"##;

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

/* Re-emit a decoded value tree as CBOR, used by `any` fields that carry an opaque
 * value through verbatim. A value decoded from canonical input re-encodes to
 * identical bytes (maps keep their already-canonical input order). */
static inline int csilc_w_value(csilc_buf *b, const csilc_value *v) {
    if (!v) return csilc_w_null(b);
    switch (v->kind) {
    case CSILC_UINT: return csilc_w_uint(b, v->as.u);
    case CSILC_NINT: return csilc_w_int(b, v->as.i);
    case CSILC_TEXT: return csilc_w_text(b, (const char *)v->as.bytes.ptr, v->as.bytes.len);
    case CSILC_BYTES: return csilc_w_bytes(b, v->as.bytes.ptr, v->as.bytes.len);
    case CSILC_BOOL: return csilc_w_bool(b, v->as.boolean);
    case CSILC_NULL: return csilc_w_null(b);
    case CSILC_FLOAT: return csilc_w_f64(b, v->as.f);
    case CSILC_ARRAY:
        if (csilc_w_array_head(b, v->as.array.count)) return -1;
        for (size_t csilc_i = 0; csilc_i < v->as.array.count; csilc_i++) {
            if (csilc_w_value(b, &v->as.array.items[csilc_i])) return -1;
        }
        return 0;
    case CSILC_MAP:
        if (csilc_w_map_head(b, v->as.map.count)) return -1;
        for (size_t csilc_i = 0; csilc_i < v->as.map.count; csilc_i++) {
            if (csilc_w_value(b, v->as.map.pairs[csilc_i].key)) return -1;
            if (csilc_w_value(b, v->as.map.pairs[csilc_i].val)) return -1;
        }
        return 0;
    case CSILC_TAG:
        if (csilc_w_tag(b, v->as.tag.num)) return -1;
        return csilc_w_value(b, v->as.tag.content);
    }
    return -1;
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
