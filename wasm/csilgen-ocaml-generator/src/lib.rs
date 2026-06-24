//! OCaml code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target ocaml` from `csilgen_ocaml_generator.wasm`.
//! Emits idiomatic OCaml *source* — records and Capitalized-constructor variants
//! behind a `types.mli` interface, services as modules with verbose + compact
//! routers, and a transport seam — but never the wire bytes (those live in
//! `transports/ocaml/`).

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSpecSerialized,
    CsilTypeExpression, GeneratedFile, GenerationStats, GeneratorCapability, GeneratorConfig,
    GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "ocaml-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "OCaml code generator".to_string(),
        target: "ocaml".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
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
    match process_generation(input_ptr, input_len) {
        Ok(output) => write_json(&output),
        Err(_) => std::ptr::null_mut(),
    }
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

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = generate_ocaml(&input.csil_spec, &input.config)
        .map_err(|_| error_codes::GENERATION_ERROR)?;

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: files.iter().map(|f| f.content.len()).sum(),
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };
    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// Sub-target surface
// ---------------------------------------------------------------------------

/// Which surface a sub-target emits. `ocaml`/`ocaml-server` emit handler records
/// and routers; `ocaml-client` emits transport-agnostic call functions;
/// `ocaml-typesonly` emits only the type declarations.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

fn resolve_surface(target: &str) -> Result<Surface, i32> {
    match target {
        "ocaml" | "ocaml-server" => Ok(Surface::Server),
        "ocaml-client" => Ok(Surface::Client),
        "ocaml-typesonly" => Ok(Surface::TypesOnly),
        _ => Err(error_codes::GENERATION_ERROR),
    }
}

fn generate_ocaml(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<Vec<GeneratedFile>, i32> {
    let surface = resolve_surface(config.target.as_str())?;
    let mut files = Vec::new();

    // Types are shared by every surface (the client/server reference them), so the
    // typesonly surface is simply "stop after types".
    let (types_ml, types_mli) = generate_types(spec);
    if !types_ml.trim().is_empty() {
        files.push(GeneratedFile {
            path: "types.ml".to_string(),
            content: types_ml,
        });
        files.push(GeneratedFile {
            path: "types.mli".to_string(),
            content: types_mli,
        });
    }

    let has_services = spec
        .rules
        .iter()
        .any(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)));

    match surface {
        Surface::TypesOnly => {}
        Surface::Client if has_services => {
            files.push(GeneratedFile {
                path: "client.ml".to_string(),
                content: generate_client(spec),
            });
        }
        Surface::Server if has_services => {
            files.push(GeneratedFile {
                path: "services.ml".to_string(),
                content: generate_services(spec),
            });
        }
        _ => {}
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// Identifier mapping
// ---------------------------------------------------------------------------

/// OCaml keywords that cannot be used as a value/type identifier; a clashing CSIL
/// name gets a trailing `_` so the emitted source stays valid.
const OCAML_KEYWORDS: &[&str] = &[
    "and",
    "as",
    "assert",
    "asr",
    "begin",
    "class",
    "constraint",
    "do",
    "done",
    "downto",
    "else",
    "end",
    "exception",
    "external",
    "false",
    "for",
    "fun",
    "function",
    "functor",
    "if",
    "in",
    "include",
    "inherit",
    "initializer",
    "land",
    "lazy",
    "let",
    "lor",
    "lsl",
    "lsr",
    "lxor",
    "match",
    "method",
    "mod",
    "module",
    "mutable",
    "new",
    "nonrec",
    "object",
    "of",
    "open",
    "or",
    "private",
    "rec",
    "sig",
    "struct",
    "then",
    "to",
    "true",
    "try",
    "type",
    "val",
    "virtual",
    "when",
    "while",
    "with",
];

fn is_keyword(s: &str) -> bool {
    OCAML_KEYWORDS.contains(&s)
}

/// A lowercase OCaml value / record-label / type identifier. CSIL field names are
/// snake_case already, kebab operation names become snake_case, and a name that
/// collides with a keyword or starts with a digit is made legal.
fn ocaml_ident(name: &str) -> String {
    let snake = name.to_case(Case::Snake);
    let snake = if snake.is_empty() {
        "field".to_string()
    } else {
        snake
    };
    let first = snake.chars().next().unwrap();
    let snake = if first.is_ascii_digit() {
        format!("v_{snake}")
    } else {
        snake
    };
    if is_keyword(&snake) {
        format!("{snake}_")
    } else {
        snake
    }
}

/// A type name (lowercase snake_case): OCaml type names are lowercase, so a
/// PascalCase CSIL type maps to snake_case, e.g. `DepositClaimRequest` →
/// `deposit_claim_request`.
fn ocaml_type_name(name: &str) -> String {
    ocaml_ident(name)
}

/// A Capitalized OCaml variant constructor, e.g. `not-found` → `Not_found`,
/// `DepositClaimResponse` → `Deposit_claim_response`. A leading digit (a literal
/// like `"404"` used as an enum member) is prefixed so the constructor is legal.
fn ocaml_ctor_name(name: &str) -> String {
    let snake = name.to_case(Case::Snake);
    let snake = if snake.is_empty() {
        "arm".to_string()
    } else {
        snake
    };
    let snake = if snake.chars().next().unwrap().is_ascii_digit() {
        format!("v_{snake}")
    } else {
        snake
    };
    capitalize(&snake)
}

/// A Capitalized OCaml module name for a service, e.g. `attestation-service` →
/// `Attestation_service`.
fn ocaml_module_name(name: &str) -> String {
    capitalize(&name.to_case(Case::Snake))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Strip a trailing `Service`/`-service` suffix from a service name for the wire
/// service string, matching the other generators' convention.
fn wire_service_name(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// The OCaml type for a CSIL type expression. CSIL integers map to `int64` to
/// dodge OCaml's 63-bit native `int` (a `u64`/large wire-id would silently lose
/// its high bit as a native `int`).
fn map_type(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => "int64".to_string(),
            "float" => "float".to_string(),
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`.
            "text" | "tstr" => "string".to_string(),
            "bytes" | "bstr" => "bytes".to_string(),
            "bool" => "bool".to_string(),
            // CBOR tag 0 RFC3339 text and CBOR tag 4 exact decimal: OCaml's stdlib
            // has neither a datetime nor a decimal type, so both are carried as
            // their exact wire text and documented at the field.
            "timestamp" => "string".to_string(),
            "decimal" => "string".to_string(),
            "nil" | "null" => "unit".to_string(),
            "any" => "Csilgen_transport.Cbor.t".to_string(),
            other => ocaml_type_name(other),
        },
        CsilTypeExpression::Reference(name) => ocaml_type_name(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("{} list", map_type(element_type))
        }
        // OCaml maps are association lists in the generated surface so the codec
        // can preserve canonical key order without a comparator functor.
        CsilTypeExpression::Map { key, value, .. } => {
            format!("({} * {}) list", map_type(key), map_type(value))
        }
        CsilTypeExpression::Tuple(group) => {
            if group.entries.is_empty() {
                "unit".to_string()
            } else {
                let parts: Vec<String> = group
                    .entries
                    .iter()
                    .map(|e| map_type(&e.value_type))
                    .collect();
                format!("({})", parts.join(" * "))
            }
        }
        CsilTypeExpression::Constrained { base_type, .. } => map_type(base_type),
        // A choice that is not a named rule collapses to the opaque CBOR value; a
        // named choice gets its own variant type via `generate_type_choice`.
        CsilTypeExpression::Choice(_) => "Csilgen_transport.Cbor.t".to_string(),
        _ => "Csilgen_transport.Cbor.t".to_string(),
    }
}

/// The OCaml type for a field, wrapping optionals as `t option`. No extra parens
/// are needed: the postfix `list`/`option`/`t` constructors are left-associative
/// (`string list option` already means `(string list) option`), and the only
/// compound bases `map_type` can produce — tuples and map assoc-lists — are
/// already parenthesized at their source.
fn map_field_type(entry: &CsilGroupEntry) -> String {
    let base = map_type(&entry.value_type);
    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
        format!("{base} option")
    } else {
        base
    }
}

/// The OCaml record label for a group entry, or `None` for a keyless / typed-key
/// entry that has no stable field name.
fn entry_field_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(ocaml_ident(name)),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => Some(ocaml_ident(name)),
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(ocaml_ident(name))
            }
            _ => None,
        },
    }
}

/// A constructor name and optional payload type for one arm of a (named) type
/// choice. A text literal becomes a nullary constructor named after the literal
/// (the string-enum case); a reference/builtin carries its mapped type.
fn choice_ctor(type_expr: &CsilTypeExpression) -> (String, Option<String>) {
    match type_expr {
        CsilTypeExpression::Reference(name) => (ocaml_ctor_name(name), Some(ocaml_type_name(name))),
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "nil" | "null" => (capitalize(&ocaml_ident(name)), None),
            _ => (ocaml_ctor_name(name), Some(map_type(type_expr))),
        },
        CsilTypeExpression::Literal(CsilLiteralValue::Text(text)) => (ocaml_ctor_name(text), None),
        _ => ("Other".to_string(), Some(map_type(type_expr))),
    }
}

/// Disambiguate a constructor name against the ones already emitted in this
/// variant (two literals can snake to the same identifier, and the opaque
/// fallback is always `Other`). The first use keeps the bare name; later ones get
/// a numeric suffix so the variant stays a set of distinct constructors.
fn unique_ctor(seen: &mut Vec<String>, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while seen.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    seen.push(candidate.clone());
    candidate
}

// ---------------------------------------------------------------------------
// Types file
// ---------------------------------------------------------------------------

/// Build `types.ml` and `types.mli` together so the implementation and its
/// interface always agree on the same declarations (the `.mli` is the
/// abstraction boundary the research mandates).
fn generate_types(spec: &CsilSpecSerialized) -> (String, String) {
    let header = "(* Code generated by csilgen; DO NOT EDIT. *)\n\n";
    let mut decls: Vec<String> = Vec::new();

    for rule in &spec.rules {
        match &rule.rule_type {
            CsilRuleType::TypeDef(type_expr) => match type_expr {
                CsilTypeExpression::Group(group) => {
                    decls.push(generate_record(&rule.name, group));
                }
                CsilTypeExpression::Choice(choices) => {
                    decls.push(generate_type_choice(&rule.name, choices));
                }
                _ => {
                    decls.push(format!(
                        "type {} = {}",
                        ocaml_type_name(&rule.name),
                        map_type(type_expr)
                    ));
                }
            },
            CsilRuleType::GroupDef(group) => {
                decls.push(generate_record(&rule.name, group));
            }
            CsilRuleType::TypeChoice(choices) => {
                decls.push(generate_type_choice(&rule.name, choices));
            }
            CsilRuleType::GroupChoice(groups) => {
                decls.push(generate_group_choice(&rule.name, groups));
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    if decls.is_empty() {
        return (String::new(), String::new());
    }

    // OCaml record labels share one module-level namespace, so distinct request /
    // response records that each legitimately carry e.g. a `username` field would
    // trip warning 30 (and fail under dune's warnings-as-errors). Field uses are
    // type-directed-disambiguated, so the collision is harmless here; silence the
    // warning for the generated type surface rather than mangling field names.
    let preamble = format!("{header}[@@@warning \"-30\"]\n\n");

    // Successive type declarations after the first are joined with `and` so they
    // form one mutually-recursive `type` group — generated records reference each
    // other regardless of source order, so a non-recursive `type` would fail to
    // resolve a forward reference.
    let joined = join_type_decls(&decls);
    let ml = format!("{preamble}{joined}\n");
    // The interface re-exposes exactly the same declarations; with no functions to
    // hide, the `.mli` simply publishes the type surface.
    let mli = format!("{preamble}{joined}\n");
    (ml, mli)
}

/// Join `type ...` declarations into a single `type ... and ... and ...` group.
/// The formatter packs adjacent single-line clauses with no blank between them and
/// surrounds a multi-line (record) clause with a blank line; matching that keeps
/// the generated `types.ml` stable under `ocamlformat`.
fn join_type_decls(decls: &[String]) -> String {
    let mut out = String::new();
    for (i, decl) in decls.iter().enumerate() {
        if i > 0 {
            let either_multiline = decls[i - 1].contains('\n') || decl.contains('\n');
            out.push_str(if either_multiline { "\n\n" } else { "\n" });
            // Each subsequent decl already begins with `type `; swap it for `and `.
            let body = decl.strip_prefix("type ").unwrap_or(decl);
            out.push_str("and ");
            out.push_str(body);
        } else {
            out.push_str(decl);
        }
    }
    out
}

fn generate_record(name: &str, group: &CsilGroupExpression) -> String {
    let type_name = ocaml_type_name(name);
    if group.entries.is_empty() {
        // An empty group has no fields; OCaml has no empty record, so it is a unit
        // alias which still round-trips as an empty CBOR map.
        return format!("type {type_name} = unit");
    }

    // Single-line fast path: the formatter collapses a comment-free record that
    // fits the 80-column margin onto one line, so emit that shape directly to keep
    // the generated source format-stable. `collect()` yields `None` if any entry
    // is skipped (no field name) or carries a doc comment — either forces the
    // multi-line form below.
    let simple: Option<Vec<String>> = group
        .entries
        .iter()
        .map(|entry| match entry_field_name(entry) {
            Some(field) if field_doc(entry).is_empty() => {
                Some(format!("{field} : {}", map_field_type(entry)))
            }
            _ => None,
        })
        .collect();
    if let Some(parts) = simple {
        let single = format!("type {type_name} = {{ {} }}", parts.join("; "));
        if single.len() <= MARGIN {
            return single;
        }
    }

    let mut lines = vec![format!("type {type_name} = {{")];
    for entry in &group.entries {
        let Some(field) = entry_field_name(entry) else {
            lines.push("  (* group-spread entry skipped (no field name) *)".to_string());
            continue;
        };
        for doc in field_doc(entry) {
            lines.push(format!("  (* {doc} *)"));
        }
        lines.push(format!("  {field} : {};", map_field_type(entry)));
    }
    lines.push("}".to_string());
    lines.join("\n")
}

/// The formatter's line-width margin; a declaration whose one-line form fits is
/// emitted on a single line so `ocamlformat` leaves the generated source untouched.
const MARGIN: usize = 80;

/// Render a variant type as a single line when its arms fit the margin (the
/// formatter's choice for a short sum), else one `| Arm` per line.
fn render_variant(type_name: &str, arms: &[String]) -> String {
    let single = format!("type {type_name} = {}", arms.join(" | "));
    if single.len() <= MARGIN {
        return single;
    }
    let mut lines = vec![format!("type {type_name} =")];
    for arm in arms {
        lines.push(format!("  | {arm}"));
    }
    lines.join("\n")
}

fn generate_type_choice(name: &str, choices: &[CsilTypeExpression]) -> String {
    let type_name = ocaml_type_name(name);

    // A CSIL string enum — every arm a text literal, optionally led by a single
    // `text`/`tstr` base — is the idiomatic OCaml variant: one nullary
    // Capitalized constructor per literal. A leading base means any string is
    // valid on the wire, so an extra `Other of string` arm keeps an unknown value
    // round-trippable rather than collapsing the whole type to an opaque blob.
    let literals: Vec<&str> = choices
        .iter()
        .filter_map(|c| match c {
            CsilTypeExpression::Literal(CsilLiteralValue::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let non_literals: Vec<&CsilTypeExpression> = choices
        .iter()
        .filter(|c| !matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Text(_))))
        .collect();
    let open_base = matches!(
        non_literals.as_slice(),
        [CsilTypeExpression::Builtin(n)] if n == "text" || n == "tstr"
    );
    if !literals.is_empty() && (non_literals.is_empty() || open_base) {
        let mut seen: Vec<String> = Vec::new();
        let mut arms: Vec<String> = Vec::new();
        for lit in &literals {
            arms.push(unique_ctor(&mut seen, &ocaml_ctor_name(lit)));
        }
        if open_base {
            arms.push(format!("{} of string", unique_ctor(&mut seen, "Other")));
        }
        return render_variant(&type_name, &arms);
    }

    // Otherwise a union of (named) alternatives: one constructor per arm carrying
    // its mapped payload, with any name clash disambiguated.
    let mut seen: Vec<String> = Vec::new();
    let arms: Vec<String> = choices
        .iter()
        .map(|choice| {
            let (base, payload) = choice_ctor(choice);
            let ctor = unique_ctor(&mut seen, &base);
            match payload {
                Some(ty) => format!("{ctor} of {ty}"),
                None => ctor,
            }
        })
        .collect();
    render_variant(&type_name, &arms)
}

/// A group choice (`A // B`) becomes a variant whose arms each carry an inline
/// record-shaped tuple; here each alternative is exposed as a constructor wrapping
/// a reference to its own generated record is not possible (the groups are
/// anonymous), so each arm carries the opaque CBOR value with a documented index.
fn generate_group_choice(name: &str, groups: &[CsilGroupExpression]) -> String {
    let type_name = ocaml_type_name(name);
    let arms: Vec<String> = (1..=groups.len())
        .map(|n| format!("Variant_{n} of Csilgen_transport.Cbor.t"))
        .collect();
    render_variant(&type_name, &arms)
}

/// Field documentation: the `@description` text and a wire-form note for the
/// core types whose OCaml mapping is lossy text (`timestamp`/`decimal`).
fn field_doc(entry: &CsilGroupEntry) -> Vec<String> {
    let mut out = Vec::new();
    for meta in &entry.metadata {
        if let csilgen_common::CsilFieldMetadata::Description(desc) = meta {
            out.push(desc.clone());
        }
    }
    if type_uses_builtin(&entry.value_type, "timestamp") {
        out.push("wire: CBOR tag 0 RFC3339 UTC timestamp text".to_string());
    }
    if type_uses_builtin(&entry.value_type, "decimal") {
        out.push("wire: CBOR tag 4 exact decimal text".to_string());
    }
    out
}

fn type_uses_builtin(type_expr: &CsilTypeExpression, builtin: &str) -> bool {
    match type_expr {
        CsilTypeExpression::Builtin(name) => name == builtin,
        CsilTypeExpression::Array { element_type, .. } => type_uses_builtin(element_type, builtin),
        CsilTypeExpression::Map { key, value, .. } => {
            type_uses_builtin(key, builtin) || type_uses_builtin(value, builtin)
        }
        CsilTypeExpression::Choice(choices) => {
            choices.iter().any(|c| type_uses_builtin(c, builtin))
        }
        CsilTypeExpression::Constrained { base_type, .. } => type_uses_builtin(base_type, builtin),
        CsilTypeExpression::Group(group) | CsilTypeExpression::Tuple(group) => group
            .entries
            .iter()
            .any(|e| type_uses_builtin(&e.value_type, builtin)),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Operation helpers
// ---------------------------------------------------------------------------

/// A push op (`-> Event`) carries a `null` input type: there is no request to
/// send, so the generated call/handler takes no request value.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Whether a choice arm is the conventional error half of a `Response / …Error`
/// union — the canonical `ServiceError` or any reference whose name ends in
/// `Error`/`_error`. The success type drops it so the client's typed reply is the
/// response record, not an opaque union; the error is surfaced via the transport.
fn is_error_arm(type_expr: &CsilTypeExpression) -> bool {
    matches!(
        type_expr,
        CsilTypeExpression::Reference(name)
            if name == "ServiceError" || name.ends_with("Error") || name.ends_with("_error")
    )
}

/// Reduce an operation output to its success type by dropping a top-level error
/// arm — the error half is surfaced as the `result`'s `Error`.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !is_error_arm(c))
            .cloned()
            .collect();
        match kept.len() {
            1 => kept.into_iter().next().unwrap(),
            0 => type_expr.clone(),
            _ => CsilTypeExpression::Choice(kept),
        }
    } else {
        type_expr.clone()
    }
}

/// The builtins that map to an OCaml primitive (or the opaque CBOR value), i.e.
/// everything that does NOT resolve to a generated `Types` declaration.
fn is_primitive_builtin(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "uint"
            | "float"
            | "text"
            | "tstr"
            | "bytes"
            | "bstr"
            | "bool"
            | "timestamp"
            | "decimal"
            | "nil"
            | "null"
            | "any"
    )
}

/// Whether a type expression resolves (anywhere within it) to a name declared in
/// the generated `Types` module — a reference, or a non-primitive builtin.
fn expr_uses_named_type(type_expr: &CsilTypeExpression) -> bool {
    match type_expr {
        CsilTypeExpression::Reference(_) => true,
        CsilTypeExpression::Builtin(name) => !is_primitive_builtin(name),
        CsilTypeExpression::Array { element_type, .. } => expr_uses_named_type(element_type),
        CsilTypeExpression::Map { key, value, .. } => {
            expr_uses_named_type(key) || expr_uses_named_type(value)
        }
        CsilTypeExpression::Tuple(group) => group
            .entries
            .iter()
            .any(|e| expr_uses_named_type(&e.value_type)),
        CsilTypeExpression::Constrained { base_type, .. } => expr_uses_named_type(base_type),
        CsilTypeExpression::Choice(choices) => choices.iter().any(expr_uses_named_type),
        _ => false,
    }
}

/// Whether the emitted client will reference any `Types` declaration, so the
/// `open Types` is actually used (a unary op's request or its success reply).
fn client_uses_types(spec: &CsilSpecSerialized) -> bool {
    spec.rules.iter().any(|rule| match &rule.rule_type {
        CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
            matches!(op.direction, CsilServiceDirection::Unidirectional)
                && ((!op_input_is_null(&op.input_type) && expr_uses_named_type(&op.input_type))
                    || expr_uses_named_type(&success_type(&op.output_type)))
        }),
        _ => false,
    })
}

// ---------------------------------------------------------------------------
// Client surface
// ---------------------------------------------------------------------------

/// `client.ml`: one module per service exposing typed call functions. A call
/// takes the transport client plus per-operation `encode`/`decode` closures (the
/// codec is the consumer's — the generator never owns serialization) and returns a
/// `result`.
fn generate_client(spec: &CsilSpecSerialized) -> String {
    let mut out = String::new();
    out.push_str("(* Code generated by csilgen; DO NOT EDIT. *)\n\n");
    // `open Types` only when a typed call actually names a generated type — an
    // unused `open` is a hard error under dune's default warnings-as-errors.
    if client_uses_types(spec) {
        out.push_str("open Types\n\n");
    }
    out.push_str(CLIENT_PRELUDE);

    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            out.push_str(&emit_client_module(&rule.name, service));
        }
    }
    // A single trailing newline (the formatter's end-of-file convention).
    format!("{}\n", out.trim_end())
}

const CLIENT_PRELUDE: &str = "\
(* The transport seam is supplied by the consumer: [call] performs the RPC named
   by (service, op) over its carrier and returns the raw reply payload, or an
   error string. The generator owns neither the wire nor the codec. *)
type client = {
  call : service:string -> op:string -> payload:bytes -> (bytes, string) result;
}

let make_client ~call = { call }

";

fn emit_client_module(name: &str, service: &CsilServiceDefinition) -> String {
    let module = ocaml_module_name(name);
    let wire_service = wire_service_name(name);
    let mut out = String::new();
    out.push_str(&format!("module {module} = struct\n"));
    out.push_str(&emit_wire_ids(service));

    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the server router surface.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            out.push_str(&format!(
                "  (* channel operation {} is not part of the RPC client *)\n",
                op.name
            ));
            continue;
        }
        let fn_name = ocaml_ident(&op.name);
        let out_type = map_type(&success_type(&op.output_type));
        let null_input = op_input_is_null(&op.input_type);
        if null_input {
            out.push_str(&format!(
                "  let {fn_name} (c : client)\n      ~(decode_response : bytes -> {out_type}) :\n      ({out_type}, string) result =\n"
            ));
            out.push_str(&format!(
                "    match c.call ~service:\"{wire_service}\" ~op:\"{}\" ~payload:Bytes.empty with\n",
                op.name
            ));
        } else {
            let in_type = map_type(&op.input_type);
            out.push_str(&format!(
                "  let {fn_name} (c : client)\n      ~(encode_request : {in_type} -> bytes)\n      ~(decode_response : bytes -> {out_type})\n      (req : {in_type}) : ({out_type}, string) result =\n"
            ));
            out.push_str(&format!(
                "    match c.call ~service:\"{wire_service}\" ~op:\"{}\" ~payload:(encode_request req) with\n",
                op.name
            ));
        }
        out.push_str("    | Ok payload -> Ok (decode_response payload)\n");
        out.push_str("    | Error _ as e -> e\n\n");
    }

    // No blank line directly before `end` (the formatter's struct convention).
    format!("{}\nend\n\n", out.trim_end())
}

// ---------------------------------------------------------------------------
// Server surface
// ---------------------------------------------------------------------------

/// `services.ml`: one module per service with a handler record (one function per
/// operation), a verbose router (dispatch by op name) and — for wire-id-bearing
/// services — a compact router (dispatch by `@wire-id` ordinal).
fn generate_services(spec: &CsilSpecSerialized) -> String {
    let mut out = String::new();
    out.push_str("(* Code generated by csilgen; DO NOT EDIT. *)\n\n");
    // The server surface is codec-agnostic: handlers receive opaque payload bytes
    // and decode them with the consumer's codec, so it never names a [Types]
    // declaration (an `open Types` here would be an unused-open warning).
    out.push_str(SERVER_PRELUDE);

    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            out.push_str(&emit_service_module(&rule.name, service));
        }
    }
    // A single trailing newline (the formatter's end-of-file convention).
    format!("{}\n", out.trim_end())
}

const SERVER_PRELUDE: &str = "\
(* A handler returns either a typed reply (the [variant] names the chosen output
   arm and [payload] is its encoded bytes) or a transport-level failure. *)
type outcome =
  | Reply of { variant : string; payload : bytes }
  | Transport_error of { status : int64; message : string }

let reply ~variant ~payload = Reply { variant; payload }
let transport_error ~status ~message = Transport_error { status; message }

";

fn emit_service_module(name: &str, service: &CsilServiceDefinition) -> String {
    let module = ocaml_module_name(name);
    let mut out = String::new();
    out.push_str(&format!("module {module} = struct\n"));
    out.push_str(&emit_wire_ids(service));

    // Handler record: one field per inbound operation. The field receives the
    // opaque request payload bytes; the handler decodes them with the generated
    // [Types] (the codec is the consumer's, never the generator's). A unary op
    // returns an [outcome]; a fire-and-forget channel op returns [unit]; a
    // push-only (reverse) op has no inbound handler. The typed request shape for
    // each op is documented alongside its field.
    out.push_str("  type handler = {\n");
    for op in &service.operations {
        if matches!(op.direction, CsilServiceDirection::Reverse) {
            continue;
        }
        let field = ocaml_ident(&op.name);
        let in_ty = if op_input_is_null(&op.input_type) {
            "unit".to_string()
        } else {
            map_type(&op.input_type)
        };
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                out.push_str(&format!(
                    "    (* request payload decodes to: {in_ty} *)\n    {field} : bytes -> outcome;\n"
                ));
            }
            CsilServiceDirection::Bidirectional => {
                out.push_str(&format!(
                    "    (* channel message decodes to: {in_ty} *)\n    {field} : bytes -> unit;\n"
                ));
            }
            CsilServiceDirection::Reverse => {}
        }
    }
    out.push_str("  }\n\n");

    out.push_str(&emit_router_verbose(service));
    if service.wire_id.is_some() {
        out.push_str(&emit_router_compact(service));
    }

    // No blank line directly before `end` (the formatter's struct convention).
    format!("{}\nend\n\n", out.trim_end())
}

/// Emit `@wire-id` ordinal bindings (as `int64`, the safe width for a possibly
/// large ordinal). Emits nothing for a wire-id-free service so its output stays
/// byte-identical.
fn emit_wire_ids(service: &CsilServiceDefinition) -> String {
    let Some(service_id) = service.wire_id else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str("  (* Wire-id ordinals (transport compact profiles). *)\n");
    out.push_str(&format!("  let service_wire_id = {service_id}L\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            out.push_str(&format!(
                "  let op_{}_wire_id = {op_id}L\n",
                ocaml_ident(&op.name)
            ));
        }
    }
    out.push('\n');
    out
}

/// The verbose router dispatches one decoded request by its operation name to the
/// matching handler field. Request/reply decoding is the consumer's codec, passed
/// as `decode_<op>` closures so the router stays codec-agnostic.
fn emit_router_verbose(service: &CsilServiceDefinition) -> String {
    let mut out = String::new();
    out.push_str(
        "  (* Verbose router: dispatch one request by operation name to its handler\n     field, which decodes the opaque payload with the generated [Types]. *)\n",
    );
    // The return type is inferred as [outcome]; an explicit annotation would
    // overflow the formatter margin and read as Rust/Go ceremony, not OCaml.
    out.push_str("  let route (h : handler) ~(op : string) ~(payload : bytes) =\n");
    out.push_str("    match op with\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            continue;
        }
        let field = ocaml_ident(&op.name);
        if op_input_is_null(&op.input_type) {
            out.push_str(&format!(
                "    | \"{}\" -> ignore payload; h.{field} Bytes.empty\n",
                op.name
            ));
        } else {
            out.push_str(&format!("    | \"{}\" -> h.{field} payload\n", op.name));
        }
    }
    out.push_str("    | other -> transport_error ~status:2L ~message:(\"unknown op: \" ^ other)\n");
    out.push('\n');
    out
}

/// The compact-profile router: dispatch by the operation's `@wire-id` ordinal
/// (`int64`) rather than its name. Emitted only for wire-id-bearing services.
fn emit_router_compact(service: &CsilServiceDefinition) -> String {
    let mut out = String::new();
    out.push_str(
        "  (* Compact router: dispatch one request by its @wire-id ordinal. The\n     verbose twin is [route]; the host calls whichever the negotiated profile\n     selected. *)\n",
    );
    out.push_str("  let route_compact (h : handler) ~(op_ord : int64) ~(payload : bytes) =\n");
    out.push_str("    match op_ord with\n");
    for op in &service.operations {
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            continue;
        }
        let Some(op_id) = op.wire_id else {
            continue;
        };
        let field = ocaml_ident(&op.name);
        if op_input_is_null(&op.input_type) {
            out.push_str(&format!(
                "    | {op_id}L -> ignore payload; h.{field} Bytes.empty\n"
            ));
        } else {
            out.push_str(&format!("    | {op_id}L -> h.{field} payload\n"));
        }
    }
    // Pre-wrapped to the formatter's layout (the one-line form exceeds the margin).
    out.push_str(
        "    | other ->\n        transport_error ~status:2L\n          ~message:(Printf.sprintf \"unknown op ordinal: %Ld\" other)\n",
    );
    out.push('\n');
    out
}

#[cfg(test)]
mod tests;
