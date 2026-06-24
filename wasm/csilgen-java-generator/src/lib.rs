//! Java code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target java` from `csilgen_java_generator.wasm`.
//! Emits idiomatic Java 17 source — records for data groups, sealed interfaces for
//! choices, a typed client, a server handler interface, and verbose/compact channel
//! routers — but never wire bytes (the transport library owns the wire).

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilControlOperator, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
};

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "java-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Java code generator".to_string(),
        target: "java".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::ValidationConstraints,
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

    let files = generate_java(&input).map_err(|_| error_codes::GENERATION_ERROR)?;

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
// Generation entry
// ---------------------------------------------------------------------------

/// Which surface a (sub-)target emits: the base `java`/`java-server` produces the
/// handler interface + routers; `java-client` produces the typed client; and
/// `java-typesonly` produces the records/sealed interfaces alone.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

struct JavaConfig {
    package: String,
    surface: Surface,
}

impl JavaConfig {
    fn from_input(input: &WasmGeneratorInput) -> Result<Self, i32> {
        let package = input
            .config
            .options
            .get("java_package")
            .and_then(|v| v.as_str())
            .unwrap_or("csilgen.generated")
            .to_string();

        // An unrecognized sub-target is a hard error, never a silent fall-through,
        // mirroring the validate-early discipline of the Go/Python generators.
        let surface = match input.config.target.as_str() {
            "java" | "java-server" => Surface::Server,
            "java-client" => Surface::Client,
            "java-typesonly" => Surface::TypesOnly,
            _ => return Err(error_codes::GENERATION_ERROR),
        };
        Ok(Self { package, surface })
    }

    /// The relative file path for a top-level public class, under the package dir.
    fn path_for(&self, class: &str) -> String {
        format!("{}/{class}.java", self.package.replace('.', "/"))
    }

    /// The file preamble: the generated-code marker plus the package statement.
    fn header(&self) -> String {
        let pkg = &self.package;
        format!("// Code generated by csilgen; DO NOT EDIT.\n\npackage {pkg};\n\n")
    }
}

fn generate_java(input: &WasmGeneratorInput) -> Result<Vec<GeneratedFile>, i32> {
    let config = JavaConfig::from_input(input)?;
    let mut files = Vec::new();

    for rule in &input.csil_spec.rules {
        let doc = &rule.doc_comments;
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                files.push(generate_record(&config, &rule.name, group, doc));
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                files.push(generate_record(&config, &rule.name, group, doc));
            }
            CsilRuleType::TypeDef(type_expr) => {
                files.push(generate_alias(&config, &rule.name, type_expr, doc));
            }
            CsilRuleType::TypeChoice(choices) => {
                files.push(generate_type_choice(&config, &rule.name, choices, doc));
            }
            CsilRuleType::GroupChoice(choices) => {
                files.push(generate_group_choice(&config, &rule.name, choices, doc));
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    // Service surfaces are dispatched by sub-target.
    let services: Vec<(&str, &CsilServiceDefinition, &[String])> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::ServiceDef(def) => {
                Some((r.name.as_str(), def, r.doc_comments.as_slice()))
            }
            _ => None,
        })
        .collect();

    match config.surface {
        Surface::TypesOnly => {}
        Surface::Client => {
            if !services.is_empty() {
                files.push(generate_transport_iface(&config));
                files.push(generate_client_error(&config));
                for (name, def, doc) in &services {
                    files.push(generate_client(&config, name, def, doc));
                }
            }
        }
        Surface::Server => {
            let any_channel = services.iter().any(|(_, d, _)| service_has_channel_ops(d));
            let any_encoder = services.iter().any(|(_, d, _)| service_has_pushable_ops(d));
            if any_channel {
                files.push(generate_codec_iface(&config));
            }
            if any_encoder {
                files.push(generate_encoded_message(&config));
            }
            for (name, def, doc) in &services {
                files.push(generate_server_interface(&config, name, def, doc));
                if service_has_channel_ops(def) || def.wire_id.is_some() {
                    files.push(generate_router(&config, name, def));
                }
            }
        }
    }

    // The emit functions reference JDK types by their fully-qualified name; hoist those to
    // `import` statements and leave simple names behind, the way a Java author writes them.
    Ok(files
        .into_iter()
        .map(|mut f| {
            f.content = finalize_file(&f.content);
            f
        })
        .collect())
}

/// Hoist inline FQNs to imports and drop the blank line a per-member emit leaves before
/// the closing class brace, matching what a formatter would produce.
fn finalize_file(content: &str) -> String {
    let mut out = hoist_imports(content);
    while out.ends_with("\n\n}\n") {
        out.replace_range(out.len() - 4.., "\n}\n");
    }
    out
}

/// The fully-qualified JDK types the emit functions write inline. After a file body is
/// assembled they are lifted into a single alphabetized `import` block and referred to by
/// simple name. Each prefix is an unambiguous class name, so a plain textual replace is
/// safe (none is a substring of a generated identifier we also emit).
const KNOWN_IMPORTS: &[&str] = &[
    "java.math.BigDecimal",
    "java.time.Instant",
    "java.util.Arrays",
    "java.util.List",
    "java.util.Map",
    "java.util.Objects",
    "java.util.regex.Pattern",
];

/// Rewrite a finished file so inline FQNs become imports + simple names.
fn hoist_imports(content: &str) -> String {
    let mut used: Vec<&str> = KNOWN_IMPORTS
        .iter()
        .copied()
        .filter(|fqn| content.contains(fqn))
        .collect();
    if used.is_empty() {
        return content.to_string();
    }
    used.sort_unstable();

    let mut body = content.to_string();
    for fqn in &used {
        let simple = fqn.rsplit('.').next().unwrap();
        body = body.replace(fqn, simple);
    }

    // Splice the import block between the package statement and the first declaration.
    let Some(anchor) = body.find(";\n\n") else {
        return body;
    };
    let cut = anchor + 3;
    let imports: String = used.iter().map(|i| format!("import {i};\n")).collect();
    format!("{}{imports}\n{}", &body[..cut], &body[cut..])
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

fn generate_record(
    config: &JavaConfig,
    name: &str,
    group: &CsilGroupExpression,
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let mut code = config.header();

    let named: Vec<(&CsilGroupEntry, String)> = group
        .entries
        .iter()
        .filter_map(|e| entry_field_name(e).map(|n| (e, n)))
        .collect();

    code.push_str(&type_javadoc(doc, &named));
    code.push_str(&format!("public record {class}(\n"));
    if named.is_empty() {
        // A record needs at least an empty component list; an empty record is legal.
        code.push_str(") {\n}\n");
        return GeneratedFile {
            path: config.path_for(&class),
            content: code,
        };
    }

    let mut comps = Vec::new();
    for (entry, field) in &named {
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        // The CBOR wire keys by the CSIL field name verbatim; the camelCase Java
        // component name is purely the in-memory identifier, so the original name is
        // recorded in a comment for the reader.
        let wire = entry_wire_name(entry).unwrap_or_else(|| field.clone());
        let jtype = if optional {
            map_type_boxed(&entry.value_type)
        } else {
            map_type(&entry.value_type)
        };
        comps.push(format!("    {jtype} {field} /* wire: \"{wire}\" */"));
    }
    code.push_str(&comps.join(",\n"));
    code.push_str("\n) {\n");

    // Validation runs in the canonical constructor: throwing IllegalArgumentException
    // on a violated size/regex/bound is the idiomatic Java guard for a bad value.
    let validation = record_validation(&named);
    if !validation.is_empty() {
        code.push_str(&format!("    public {class} {{\n"));
        code.push_str(&validation);
        code.push_str("    }\n");
    }

    // A record's generated equals/hashCode compare a byte[] component by reference,
    // so a value-equal payload would falsely differ; override them to compare the
    // bytes by content whenever a byte[] component is present.
    if named.iter().any(|(e, _)| {
        !matches!(e.occurrence, Some(CsilOccurrence::Optional))
            && map_type(&e.value_type) == "byte[]"
    }) || named
        .iter()
        .any(|(e, _)| map_type_boxed(&e.value_type) == "byte[]")
    {
        code.push_str(&record_array_equality(&class, &named));
    }

    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

/// Build the canonical-constructor validation body for a record's named fields.
fn record_validation(named: &[(&CsilGroupEntry, String)]) -> String {
    let mut body = String::new();
    for (entry, field) in named {
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        let jtype = map_type(&entry.value_type);
        // Both constraint systems feed the same guards: `@`-annotations and the
        // inline `.`-control-operators on the field type.
        for meta in &entry.metadata {
            if let csilgen_common::CsilFieldMetadata::Constraint(c) = meta {
                body.push_str(&annotation_guard(field, &jtype, optional, c));
            }
        }
        if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
            for op in constraints {
                body.push_str(&control_op_guard(field, &jtype, optional, op));
            }
        }
    }
    body
}

/// The length expression for a value of the given Java type: strings use
/// `.length()`, byte arrays `.length`, and lists `.size()`.
fn len_expr(field: &str, jtype: &str) -> String {
    if jtype == "byte[]" {
        format!("{field}.length")
    } else if jtype.starts_with("java.util.List") || jtype.starts_with("java.util.Map") {
        format!("{field}.size()")
    } else {
        format!("{field}.length()")
    }
}

/// A guard short-circuited by a null-check when the field is an optional (boxed)
/// component, so an absent optional is skipped rather than dereferenced.
fn guard(field: &str, optional: bool, cond: &str, message: &str) -> String {
    let msg = java_string(message);
    let test = if optional {
        format!("{field} != null && ({cond})")
    } else {
        cond.to_string()
    };
    format!(
        "        if ({test}) {{\n            throw new IllegalArgumentException({msg});\n        }}\n"
    )
}

fn annotation_guard(
    field: &str,
    jtype: &str,
    optional: bool,
    c: &CsilValidationConstraint,
) -> String {
    let len = len_expr(field, jtype);
    match c {
        // A length/item count is never negative, so a `>= 0` floor is vacuous; skip it.
        CsilValidationConstraint::MinLength(0) => String::new(),
        CsilValidationConstraint::MinLength(n) => guard(
            field,
            optional,
            &format!("{len} < {n}"),
            &format!("field '{field}' must have length >= {n}"),
        ),
        CsilValidationConstraint::MaxLength(n) => guard(
            field,
            optional,
            &format!("{len} > {n}"),
            &format!("field '{field}' must have length <= {n}"),
        ),
        CsilValidationConstraint::MinItems(0) => String::new(),
        CsilValidationConstraint::MinItems(n) => guard(
            field,
            optional,
            &format!("{len} < {n}"),
            &format!("field '{field}' must have at least {n} items"),
        ),
        CsilValidationConstraint::MaxItems(n) => guard(
            field,
            optional,
            &format!("{len} > {n}"),
            &format!("field '{field}' must have at most {n} items"),
        ),
        CsilValidationConstraint::MinValue(v) => {
            ordered_guard(field, jtype, optional, "<", "at least", v)
        }
        CsilValidationConstraint::MaxValue(v) => {
            ordered_guard(field, jtype, optional, ">", "at most", v)
        }
        CsilValidationConstraint::Custom { .. } => String::new(),
    }
}

fn control_op_guard(field: &str, jtype: &str, optional: bool, op: &CsilControlOperator) -> String {
    match op {
        CsilControlOperator::Size(size) => size_guard(field, jtype, optional, size),
        CsilControlOperator::Regex(pattern) => guard(
            field,
            optional,
            &format!(
                "!java.util.regex.Pattern.compile({}).matcher({field}).find()",
                java_string(pattern)
            ),
            &format!("field '{field}' must match pattern {pattern}"),
        ),
        CsilControlOperator::GreaterEqual(v) => ordered_guard(field, jtype, optional, "<", ">=", v),
        CsilControlOperator::LessEqual(v) => ordered_guard(field, jtype, optional, ">", "<=", v),
        CsilControlOperator::GreaterThan(v) => ordered_guard(field, jtype, optional, "<=", ">", v),
        CsilControlOperator::LessThan(v) => ordered_guard(field, jtype, optional, ">=", "<", v),
        CsilControlOperator::Equal(v) => ordered_guard(field, jtype, optional, "!=", "==", v),
        CsilControlOperator::NotEqual(v) => ordered_guard(field, jtype, optional, "==", "!=", v),
        // Defaults and encoding-only operators are not runtime checks.
        _ => String::new(),
    }
}

fn size_guard(field: &str, jtype: &str, optional: bool, size: &CsilSizeConstraint) -> String {
    let len = len_expr(field, jtype);
    match size {
        CsilSizeConstraint::Exact(n) => guard(
            field,
            optional,
            &format!("{len} != {n}"),
            &format!("field '{field}' must have length {n}"),
        ),
        CsilSizeConstraint::Min(0) => String::new(),
        CsilSizeConstraint::Min(n) => guard(
            field,
            optional,
            &format!("{len} < {n}"),
            &format!("field '{field}' must have length >= {n}"),
        ),
        CsilSizeConstraint::Max(n) => guard(
            field,
            optional,
            &format!("{len} > {n}"),
            &format!("field '{field}' must have length <= {n}"),
        ),
        CsilSizeConstraint::Range { min, max } => {
            // A zero floor on a length is vacuous; emit only the meaningful upper bound.
            let mut out = if *min == 0 {
                String::new()
            } else {
                guard(
                    field,
                    optional,
                    &format!("{len} < {min}"),
                    &format!("field '{field}' must have length >= {min}"),
                )
            };
            out.push_str(&guard(
                field,
                optional,
                &format!("{len} > {max}"),
                &format!("field '{field}' must have length <= {max}"),
            ));
            out
        }
    }
}

/// Emit one ordered comparison honoring the field's Java type. `op` is the operator
/// whose truth means the value is invalid; `desc` is the human phrasing. Numeric
/// fields compare directly; `BigDecimal` compares via `compareTo`; an `Instant`
/// compares via `isBefore`/`isAfter`.
fn ordered_guard(
    field: &str,
    jtype: &str,
    optional: bool,
    op: &str,
    desc: &str,
    value: &CsilLiteralValue,
) -> String {
    match jtype {
        // A boolean only admits equality; comparing it to a numeric 0 would not compile,
        // and ordering (`<`/`>`) is meaningless, so only `==`/`!=` produce a guard.
        "boolean" => {
            let expected = match value {
                CsilLiteralValue::Bool(b) => *b,
                _ => return String::new(),
            };
            let cond = match op {
                "!=" => {
                    if expected {
                        format!("!{field}")
                    } else {
                        field.to_string()
                    }
                }
                "==" => {
                    if expected {
                        field.to_string()
                    } else {
                        format!("!{field}")
                    }
                }
                _ => return String::new(),
            };
            guard(
                field,
                optional,
                &cond,
                &format!("field '{field}' must be {desc} {expected}"),
            )
        }
        "java.math.BigDecimal" => {
            let Some(text) = literal_as_text(value) else {
                return String::new();
            };
            let bound = format!("new java.math.BigDecimal({})", java_string(&text));
            guard(
                field,
                optional,
                &format!("{field}.compareTo({bound}) {op} 0"),
                &format!("field '{field}' must be {desc} {text}"),
            )
        }
        "java.time.Instant" => {
            let Some(text) = literal_as_text(value) else {
                return String::new();
            };
            let bound = format!("java.time.Instant.parse({})", java_string(&text));
            let cond = match op {
                "<" => format!("{field}.isBefore({bound})"),
                ">" => format!("{field}.isAfter({bound})"),
                "<=" => format!("!{field}.isAfter({bound})"),
                ">=" => format!("!{field}.isBefore({bound})"),
                "==" => format!("{field}.equals({bound})"),
                "!=" => format!("!{field}.equals({bound})"),
                _ => return String::new(),
            };
            guard(
                field,
                optional,
                &cond,
                &format!("field '{field}' must be {desc} {text}"),
            )
        }
        _ => {
            let v = literal_as_number(value);
            guard(
                field,
                optional,
                &format!("{field} {op} {v}"),
                &format!("field '{field}' must be {desc} {v}"),
            )
        }
    }
}

/// Override `equals`/`hashCode`/`toString` so byte[] components compare by content.
fn record_array_equality(class: &str, named: &[(&CsilGroupEntry, String)]) -> String {
    let mut eq = String::new();
    let mut hashes = Vec::new();
    let mut strs = Vec::new();
    for (entry, field) in named {
        let is_bytes = map_type_boxed(&entry.value_type) == "byte[]";
        if is_bytes {
            eq.push_str(&format!(
                "            && java.util.Arrays.equals({field}, o.{field})\n"
            ));
            hashes.push(format!("java.util.Arrays.hashCode({field})"));
            strs.push(format!("\"{field}=\" + java.util.Arrays.toString({field})"));
        } else {
            eq.push_str(&format!(
                "            && java.util.Objects.equals({field}, o.{field})\n"
            ));
            hashes.push(field.to_string());
            strs.push(format!("\"{field}=\" + {field}"));
        }
    }
    let mut out = String::new();
    out.push_str("    @Override\n    public boolean equals(Object obj) {\n");
    out.push_str("        if (this == obj) return true;\n");
    out.push_str(&format!(
        "        if (!(obj instanceof {class} o)) return false;\n"
    ));
    out.push_str("        return true\n");
    out.push_str(&eq);
    out.push_str("        ;\n    }\n");
    out.push_str("    @Override\n    public int hashCode() {\n");
    out.push_str(&format!(
        "        return java.util.Objects.hash({});\n    }}\n",
        hashes.join(", ")
    ));
    out.push_str("    @Override\n    public String toString() {\n");
    out.push_str(&format!(
        "        return \"{class}[\" + {} + \"]\";\n    }}\n",
        strs.join(" + \", \" + ")
    ));
    out
}

// ---------------------------------------------------------------------------
// Aliases & choices
// ---------------------------------------------------------------------------

/// A non-group `TypeDef` becomes a single-component "newtype" record, the idiomatic
/// Java stand-in for a named scalar/map/array alias.
fn generate_alias(
    config: &JavaConfig,
    name: &str,
    type_expr: &CsilTypeExpression,
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let jtype = map_type(type_expr);
    let mut code = config.header();
    code.push_str(&javadoc("", &clean_doc(doc), &[]));
    code.push_str(&format!("public record {class}({jtype} value) {{}}\n"));
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

/// A type choice `X = A / B / C` becomes a sealed interface with a record per arm,
/// giving exhaustive `switch` at dispatch sites — the standout Java 17 idiom.
fn generate_type_choice(
    config: &JavaConfig,
    name: &str,
    choices: &[CsilTypeExpression],
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let arms: Vec<(String, String)> = choices
        .iter()
        .map(|c| (choice_arm_name(c), map_type(c)))
        .collect();
    let permits: Vec<String> = arms.iter().map(|(n, _)| format!("{class}.{n}")).collect();

    let mut code = config.header();
    code.push_str(&javadoc("", &clean_doc(doc), &[]));
    code.push_str(&format!(
        "public sealed interface {class} permits {} {{\n",
        permits.join(", ")
    ));
    for (arm, jtype) in &arms {
        code.push_str(&format!(
            "    record {arm}({jtype} value) implements {class} {{}}\n"
        ));
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

/// A group choice becomes a sealed interface with one nested record per alternative
/// group shape.
fn generate_group_choice(
    config: &JavaConfig,
    name: &str,
    choices: &[CsilGroupExpression],
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let variants: Vec<String> = (0..choices.len()).map(|i| format!("Variant{i}")).collect();
    let permits: Vec<String> = variants.iter().map(|v| format!("{class}.{v}")).collect();

    let mut code = config.header();
    code.push_str(&javadoc("", &clean_doc(doc), &[]));
    code.push_str(&format!(
        "public sealed interface {class} permits {} {{\n",
        permits.join(", ")
    ));
    for (i, group) in choices.iter().enumerate() {
        let comps: Vec<String> = group
            .entries
            .iter()
            .filter_map(|e| {
                entry_field_name(e).map(|field| {
                    let optional = matches!(e.occurrence, Some(CsilOccurrence::Optional));
                    let jtype = if optional {
                        map_type_boxed(&e.value_type)
                    } else {
                        map_type(&e.value_type)
                    };
                    format!("{jtype} {field}")
                })
            })
            .collect();
        code.push_str(&format!(
            "    record Variant{i}({}) implements {class} {{}}\n",
            comps.join(", ")
        ));
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

// ---------------------------------------------------------------------------
// Client surface
// ---------------------------------------------------------------------------

fn generate_transport_iface(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* The caller-supplied wire seam: encodes {@code req}, performs the call named by\n\
         \x20* ({@code service}, {@code method}), and decodes the response into {@code respType}.\n\
         \x20* The generator never owns the wire. Synchronous and blocking — no CompletableFuture.\n\
         \x20*/\n\
         public interface Transport {\n\
         \x20   <Resp> Resp call(String service, String method, Object req, Class<Resp> respType)\n\
         \x20           throws ClientException;\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("Transport"),
        content: code,
    }
}

fn generate_client_error(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* Wraps a transport-level failure surfaced by a generated client call. Unchecked\n\
         \x20* because it signals a protocol/transport fault, not a recoverable application\n\
         \x20* error — application errors ride inside the decoded payload, distinct from this.\n\
         \x20*/\n\
         public class ClientException extends RuntimeException {\n\
         \x20   public ClientException(String message) {\n\
         \x20       super(message);\n\
         \x20   }\n\
         \n\
         \x20   public ClientException(String message, Throwable cause) {\n\
         \x20       super(message, cause);\n\
         \x20   }\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("ClientException"),
        content: code,
    }
}

fn generate_client(
    config: &JavaConfig,
    name: &str,
    service: &CsilServiceDefinition,
    doc: &[String],
) -> GeneratedFile {
    let base = service_base(name);
    let class = format!("{base}Client");
    let wire_service = base.to_lowercase();

    let mut code = config.header();
    let mut prose = clean_doc(doc);
    prose.push(format!("A typed, blocking client for the {name} service."));
    code.push_str(&javadoc("", &prose, &[]));
    code.push_str(&format!("public final class {class} {{\n"));
    code.push_str("    private final Transport transport;\n\n");
    code.push_str(&format!(
        "    public {class}(Transport transport) {{\n        this.transport = transport;\n    }}\n"
    ));

    for op in &service.operations {
        // Only unary request/response operations belong on the RPC client; channel
        // ops ride the router/encoder surface emitted by the base `java` target.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            continue;
        }
        let method = wire_method_name(&op.name);
        let camel = op.name.to_case(Case::Camel);
        let output = map_type_boxed(&success_type(&op.output_type));
        let null_input = is_null_input(&op.input_type);
        let (params, req_arg) = if null_input {
            (String::new(), "null".to_string())
        } else {
            let input = map_type(&op.input_type);
            (format!("{input} req"), "req".to_string())
        };
        code.push('\n');
        code.push_str(&javadoc("    ", &clean_doc(&op.doc_comments), &[]));
        code.push_str(&format!(
            "    public {output} {camel}({params}) throws ClientException {{\n"
        ));
        code.push_str(&format!(
            "        return transport.call(\"{wire_service}\", \"{method}\", {req_arg}, {output}.class);\n"
        ));
        code.push_str("    }\n");
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

// ---------------------------------------------------------------------------
// Server surface
// ---------------------------------------------------------------------------

fn generate_codec_iface(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* The consumer-supplied (de)serialization layer for channel messages. The generator\n\
         \x20* is codec-agnostic; the host wires this to CBOR, JSON, or whatever its protocol\n\
         \x20* expects.\n\
         \x20*/\n\
         public interface Codec {\n\
         \x20   byte[] encode(Object value);\n\
         \n\
         \x20   <T> T decode(byte[] data, Class<T> type);\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("Codec"),
        content: code,
    }
}

fn generate_encoded_message(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* A server-pushed channel message: the wire operation name and the encoded body the\n\
         \x20* host frames onto its connection.\n\
         \x20*\n\
         \x20* @param method the wire operation name\n\
         \x20* @param data the encoded message body\n\
         \x20*/\n\
         public record EncodedMessage(String method, byte[] data) {\n\
         \x20   // A record's generated equals/hashCode compare the byte[] by reference; override\n\
         \x20   // them so two messages with equal bytes compare equal.\n\
         \x20   @Override\n\
         \x20   public boolean equals(Object obj) {\n\
         \x20       if (this == obj) {\n\
         \x20           return true;\n\
         \x20       }\n\
         \x20       return obj instanceof EncodedMessage o\n\
         \x20           && java.util.Objects.equals(method, o.method)\n\
         \x20           && java.util.Arrays.equals(data, o.data);\n\
         \x20   }\n\
         \n\
         \x20   @Override\n\
         \x20   public int hashCode() {\n\
         \x20       return java.util.Objects.hash(method, java.util.Arrays.hashCode(data));\n\
         \x20   }\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("EncodedMessage"),
        content: code,
    }
}

fn generate_server_interface(
    config: &JavaConfig,
    name: &str,
    service: &CsilServiceDefinition,
    doc: &[String],
) -> GeneratedFile {
    let iface = name.to_case(Case::Pascal);
    let mut code = config.header();
    let mut prose = clean_doc(doc);
    prose.push(format!(
        "The {name} server handler interface; the host implements it."
    ));
    code.push_str(&javadoc("", &prose, &[]));
    code.push_str(&format!("public interface {iface} {{\n"));
    let mut first = true;
    for op in &service.operations {
        let camel = op.name.to_case(Case::Camel);
        let method = match op.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_type_boxed(&success_type(&op.output_type));
                let params = if is_null_input(&op.input_type) {
                    String::new()
                } else {
                    format!("{} req", map_type(&op.input_type))
                };
                format!("    {output} {camel}({params});\n")
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the host's plumbing pulls a frame and hands
                // it to the router, which decodes and dispatches here.
                let input = map_type(&op.input_type);
                format!("    void {camel}({input} msg);\n")
            }
            // Server pushes only; no inbound method on the server side.
            CsilServiceDirection::Reverse => continue,
        };
        let jdoc = javadoc("    ", &clean_doc(&op.doc_comments), &[]);
        if !first && !jdoc.is_empty() {
            code.push('\n');
        }
        first = false;
        code.push_str(&jdoc);
        code.push_str(&method);
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&iface),
        content: code,
    }
}

fn generate_router(
    config: &JavaConfig,
    name: &str,
    service: &CsilServiceDefinition,
) -> GeneratedFile {
    let iface = name.to_case(Case::Pascal);
    let class = format!("{iface}Router");
    let mut code = config.header();
    code.push_str(&format!(
        "/**\n\
         \x20* Decodes inbound channel frames and dispatches them to a {iface} handler, and\n\
         \x20* encodes server-pushed messages. The host owns the wire; this owns dispatch.\n\
         \x20*/\n"
    ));
    code.push_str(&format!("public final class {class} {{\n"));
    code.push_str(&format!("    private {class}() {{}}\n\n"));

    // Wire-id ordinals, emitted only for a service that carries @wire-id, so a
    // wire-id-free service stays byte-identical.
    if let Some(service_id) = service.wire_id {
        code.push_str(&format!(
            "    public static final long {iface}ServiceWireId = {service_id}L;\n"
        ));
        for op in &service.operations {
            if let Some(op_id) = op.wire_id {
                let m = wire_method_name(&op.name);
                code.push_str(&format!(
                    "    public static final long {iface}Op{m}WireId = {op_id}L;\n"
                ));
            }
        }
        code.push('\n');
    }

    let has_channel = service_has_channel_ops(service);

    if has_channel {
        // Verbose router: dispatch on the wire method name.
        code.push_str(&format!(
            "    public static void route{iface}Channel({iface} handlers, Codec codec, String method, byte[] data) {{\n"
        ));
        code.push_str("        switch (method) {\n");
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let m = wire_method_name(&op.name);
            let camel = op.name.to_case(Case::Camel);
            let input = map_type(&op.input_type);
            code.push_str(&format!("            case \"{m}\" -> {{\n"));
            code.push_str(&format!(
                "                {input} msg = codec.decode(data, {input}.class);\n"
            ));
            code.push_str(&format!("                handlers.{camel}(msg);\n"));
            code.push_str("            }\n");
        }
        code.push_str(
            "            default -> throw new IllegalArgumentException(\"unknown channel method \" + method);\n",
        );
        code.push_str("        }\n    }\n\n");

        // Compact router twin: dispatch on the @wire-id ordinal. The profile is
        // negotiated on the wire (never declared in CSIL), so a host keeps both
        // routers and calls whichever the peer selected. Java `switch` rejects a
        // `long` selector, so the ordinals are matched with an if/else chain.
        if service.wire_id.is_some() {
            code.push_str(&format!(
                "    public static void route{iface}ChannelCompact({iface} handlers, Codec codec, long op, byte[] data) {{\n"
            ));
            let mut first = true;
            for op in &service.operations {
                if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                    continue;
                }
                let Some(op_id) = op.wire_id else { continue };
                let camel = op.name.to_case(Case::Camel);
                let input = map_type(&op.input_type);
                let kw = if first { "if" } else { "} else if" };
                first = false;
                code.push_str(&format!("        {kw} (op == {op_id}L) {{\n"));
                code.push_str(&format!(
                    "            {input} msg = codec.decode(data, {input}.class);\n"
                ));
                code.push_str(&format!("            handlers.{camel}(msg);\n"));
            }
            if first {
                code.push_str(
                    "        throw new IllegalArgumentException(\"unknown channel ordinal \" + op);\n",
                );
            } else {
                code.push_str("        } else {\n");
                code.push_str(
                    "            throw new IllegalArgumentException(\"unknown channel ordinal \" + op);\n",
                );
                code.push_str("        }\n");
            }
            code.push_str("    }\n\n");
        }
    }

    // Outbound encoders for server-pushed (bidirectional + reverse) operations.
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let m = wire_method_name(&op.name);
        let output = map_type(&op.output_type);
        code.push_str(&format!(
            "    public static EncodedMessage encode{iface}{m}(Codec codec, {output} msg) {{\n"
        ));
        code.push_str(&format!(
            "        return new EncodedMessage(\"{m}\", codec.encode(msg));\n"
        ));
        code.push_str("    }\n\n");
    }

    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

// ---------------------------------------------------------------------------
// Type mapping & helpers
// ---------------------------------------------------------------------------

/// Map a CSIL type to its Java form, using primitive scalars where possible.
fn map_type(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => "long".to_string(),
            "float" => "double".to_string(),
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "byte[]".to_string(),
            "bool" => "boolean".to_string(),
            // CBOR tag 0, RFC3339, always UTC — Instant is the UTC instant type.
            "timestamp" => "java.time.Instant".to_string(),
            // CBOR tag 4 exact decimal fraction — BigDecimal is Java's exact decimal.
            "decimal" => "java.math.BigDecimal".to_string(),
            "any" | "nil" | "null" => "Object".to_string(),
            other => other.to_case(Case::Pascal),
        },
        CsilTypeExpression::Reference(name) => name.to_case(Case::Pascal),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("java.util.List<{}>", map_type_boxed(element_type))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            format!(
                "java.util.Map<{}, {}>",
                map_type_boxed(key),
                map_type_boxed(value)
            )
        }
        // Java has no tuple type; a fixed-shape array becomes a List<Object>.
        CsilTypeExpression::Tuple(_) => "java.util.List<Object>".to_string(),
        CsilTypeExpression::Constrained { base_type, .. } => map_type(base_type),
        // A `text / "a" / "b"` style choice (a base scalar narrowed by string literals)
        // collapses to that one underlying scalar — the literals constrain values, not the
        // Java type. A genuine multi-type union has no single Java type, so it stays Object.
        CsilTypeExpression::Choice(choices) => {
            let non_literal: Vec<&CsilTypeExpression> = choices
                .iter()
                .filter(|c| !matches!(c, CsilTypeExpression::Literal(_)))
                .collect();
            match non_literal.as_slice() {
                [only] => map_type(only),
                _ => "Object".to_string(),
            }
        }
        _ => "Object".to_string(),
    }
}

/// Map a CSIL type to its Java form with primitive scalars boxed, for use as a
/// generic argument or a nullable (optional) component.
fn map_type_boxed(type_expr: &CsilTypeExpression) -> String {
    let mapped = map_type(type_expr);
    match mapped.as_str() {
        "long" => "Long".to_string(),
        "double" => "Double".to_string(),
        "boolean" => "Boolean".to_string(),
        other => other.to_string(),
    }
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` arm of a `Res / ServiceError` union — the error half is surfaced
/// by the transport, not part of the returned value.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !matches!(c, CsilTypeExpression::Reference(name) if name == "ServiceError"))
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

/// A push op (`-> Event`) carries a `null` input type: no request to send.
fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
}

fn service_has_pushable_ops(def: &CsilServiceDefinition) -> bool {
    def.operations.iter().any(|op| {
        matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        )
    })
}

/// Strip a trailing `Service` suffix and PascalCase the remainder, matching the
/// wire service base used across the other-language clients.
fn service_base(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// PascalCase an operation name for the wire, matching the Go/TS/Python clients so
/// all generators agree on the method string passed to the transport.
fn wire_method_name(name: &str) -> String {
    name.to_case(Case::Pascal)
}

/// The camelCase Java component name for a group entry, or `None` when no stable
/// name can be derived (e.g. a typed key).
fn entry_field_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(name.to_case(Case::Camel)),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
            Some(name.to_case(Case::Camel))
        }
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(name.to_case(Case::Camel))
            }
            _ => None,
        },
    }
}

/// The verbatim CSIL field name used as the CBOR map key on the wire.
fn entry_wire_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(name.clone()),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => Some(name.clone()),
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(name.clone())
            }
            _ => None,
        },
    }
}

/// The nested-record arm name for a type-choice alternative. The `Case` suffix
/// keeps the arm name distinct from the referenced type so the arm's component
/// type still resolves to the external type, not the arm record itself.
fn choice_arm_name(type_expr: &CsilTypeExpression) -> String {
    let base = match type_expr {
        CsilTypeExpression::Reference(name) => name.to_case(Case::Pascal),
        CsilTypeExpression::Builtin(name) => name.to_case(Case::Pascal),
        CsilTypeExpression::Array { .. } => "List".to_string(),
        CsilTypeExpression::Map { .. } => "Map".to_string(),
        _ => "Value".to_string(),
    };
    format!("{base}Case")
}

fn literal_as_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn literal_as_number(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        _ => "0".to_string(),
    }
}

/// Normalize CSIL doc-comment lines into plain prose lines: trim surrounding space and
/// any leading `;`/`/` comment punctuation the source used, dropping blanks.
fn clean_doc(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.trim()
                .trim_start_matches([';', '/', ' '])
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// The human description for a field, drawn from its doc comments and any
/// `@description(...)` metadata, for use as a Javadoc `@param`.
fn entry_description(entry: &CsilGroupEntry) -> Option<String> {
    let mut parts = clean_doc(&entry.doc_comments);
    for m in &entry.metadata {
        if let csilgen_common::CsilFieldMetadata::Description(d) = m {
            let d = d.trim();
            if !d.is_empty() {
                parts.push(d.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// A Javadoc block (no trailing newline beyond the close) from prose lines and optional
/// `@param` entries, indented by `indent`. Empty when there is nothing to say.
fn javadoc(indent: &str, prose: &[String], params: &[(String, String)]) -> String {
    if prose.is_empty() && params.is_empty() {
        return String::new();
    }
    let mut out = format!("{indent}/**\n");
    for line in prose {
        out.push_str(&format!("{indent} * {line}\n"));
    }
    if !prose.is_empty() && !params.is_empty() {
        out.push_str(&format!("{indent} *\n"));
    }
    for (name, desc) in params {
        out.push_str(&format!("{indent} * @param {name} {desc}\n"));
    }
    out.push_str(&format!("{indent} */\n"));
    out
}

/// A type-level Javadoc from a rule's doc comments plus a `@param` per documented field.
fn type_javadoc(doc: &[String], named: &[(&CsilGroupEntry, String)]) -> String {
    let prose = clean_doc(doc);
    let params: Vec<(String, String)> = named
        .iter()
        .filter_map(|(e, f)| entry_description(e).map(|d| (f.clone(), d)))
        .collect();
    javadoc("", &prose, &params)
}

/// A safely-escaped Java double-quoted string literal for arbitrary text.
fn java_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilFieldMetadata, CsilGroupExpression, CsilPosition, CsilRule, CsilServiceOperation,
        CsilSpecSerialized, GeneratorConfig,
    };
    use std::collections::HashMap;

    fn meta() -> GeneratorMetadata {
        GeneratorMetadata {
            name: "java".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            target: "java".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        }
    }

    fn input_for(rules: Vec<CsilRule>, target: &str) -> WasmGeneratorInput {
        let service_count = rules
            .iter()
            .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
            .count();
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules,
                source_content: None,
                service_count,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: meta(),
        }
    }

    fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    fn bare(name: &str, ty: CsilTypeExpression, occ: Option<CsilOccurrence>) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: occ,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    fn builtin(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Builtin(name.to_string())
    }

    fn op(
        name: &str,
        input: CsilTypeExpression,
        output: CsilTypeExpression,
        dir: CsilServiceDirection,
        wire_id: Option<u64>,
    ) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: input,
            output_type: output,
            direction: dir,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id,
        }
    }

    fn file<'a>(files: &'a [GeneratedFile], suffix: &str) -> &'a GeneratedFile {
        files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no file ending in {suffix}; got {:?}", paths(files)))
    }

    fn paths(files: &[GeneratedFile]) -> Vec<String> {
        files.iter().map(|f| f.path.clone()).collect()
    }

    #[test]
    fn record_maps_snake_fields_to_camel_keeps_wire_name() {
        let group = CsilGroupExpression {
            entries: vec![
                bare("current_state", builtin("text"), None),
                bare(
                    "retry_count",
                    builtin("int"),
                    Some(CsilOccurrence::Optional),
                ),
                bare("blob", builtin("bytes"), None),
            ],
        };
        let files = generate_java(&input_for(
            vec![rule("TaskState", CsilRuleType::GroupDef(group))],
            "java",
        ))
        .unwrap();
        let f = file(&files, "csilgen/generated/TaskState.java");
        assert!(f.content.contains("public record TaskState("));
        // snake_case -> camelCase identifier, wire key kept verbatim in a comment.
        assert!(
            f.content
                .contains("String currentState /* wire: \"current_state\" */")
        );
        // optional int becomes a nullable boxed Long.
        assert!(
            f.content
                .contains("Long retryCount /* wire: \"retry_count\" */")
        );
        // a byte[] component forces a content-aware equals override, with the JDK type
        // hoisted to an import and referenced by simple name.
        assert!(f.content.contains("byte[] blob"));
        assert!(f.content.contains("import java.util.Arrays;"));
        assert!(f.content.contains("Arrays.equals(blob, o.blob)"));
        assert!(!f.content.contains("java.util.Arrays.equals"));
        assert!(f.content.contains("@Override\n    public int hashCode()"));
    }

    #[test]
    fn timestamp_and_decimal_map_to_jdk_types() {
        let group = CsilGroupExpression {
            entries: vec![
                bare("created_at", builtin("timestamp"), None),
                bare("amount", builtin("decimal"), None),
            ],
        };
        let files = generate_java(&input_for(
            vec![rule("Money", CsilRuleType::GroupDef(group))],
            "java-typesonly",
        ))
        .unwrap();
        let f = file(&files, "Money.java");
        // JDK types are imported and used by simple name, not inline-qualified.
        assert!(f.content.contains("import java.time.Instant;"));
        assert!(f.content.contains("import java.math.BigDecimal;"));
        assert!(f.content.contains("Instant createdAt"));
        assert!(f.content.contains("BigDecimal amount"));
        assert!(!f.content.contains("java.time.Instant createdAt"));
    }

    #[test]
    fn validation_runs_in_canonical_constructor() {
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        };
        let mut entry = bare("name", constrained, None);
        entry.metadata.push(CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MaxLength(10),
        ));
        let group = CsilGroupExpression {
            entries: vec![entry],
        };
        let files = generate_java(&input_for(
            vec![rule("User", CsilRuleType::GroupDef(group))],
            "java",
        ))
        .unwrap();
        let f = file(&files, "User.java");
        assert!(f.content.contains("public User {"));
        assert!(f.content.contains("name.length() < 3"));
        assert!(f.content.contains("name.length() > 10"));
        assert!(f.content.contains("throw new IllegalArgumentException("));
    }

    #[test]
    fn type_choice_becomes_sealed_interface() {
        let files = generate_java(&input_for(
            vec![rule(
                "Result",
                CsilRuleType::TypeChoice(vec![
                    CsilTypeExpression::Reference("Ok".to_string()),
                    CsilTypeExpression::Reference("Err".to_string()),
                ]),
            )],
            "java-typesonly",
        ))
        .unwrap();
        let f = file(&files, "Result.java");
        assert!(
            f.content
                .contains("public sealed interface Result permits Result.OkCase, Result.ErrCase")
        );
        assert!(
            f.content
                .contains("record OkCase(Ok value) implements Result {}")
        );
        assert!(
            f.content
                .contains("record ErrCase(Err value) implements Result {}")
        );
    }

    #[test]
    fn client_target_emits_typed_blocking_client() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let files = generate_java(&input_for(
            vec![rule("CorndogsService", CsilRuleType::ServiceDef(svc))],
            "java-client",
        ))
        .unwrap();
        assert!(files.iter().any(|f| f.path.ends_with("Transport.java")));
        assert!(
            files
                .iter()
                .any(|f| f.path.ends_with("ClientException.java"))
        );
        let f = file(&files, "CorndogsClient.java");
        assert!(f.content.contains("public final class CorndogsClient"));
        // ServiceError stripped from the typed return; method is camelCase.
        assert!(f.content.contains(
            "public SubmitTaskResponse submitTask(SubmitTaskRequest req) throws ClientException"
        ));
        // wire service is the lowercased base; wire method is PascalCase, matching peers.
        assert!(f.content.contains(
            "transport.call(\"corndogs\", \"SubmitTask\", req, SubmitTaskResponse.class)"
        ));
        // no server interface for the client target.
        assert!(!files.iter().any(|f| f.path.ends_with("Corndogs.java")));
    }

    #[test]
    fn server_target_emits_interface_and_router_twins() {
        let svc = CsilServiceDefinition {
            operations: vec![
                op(
                    "list-events",
                    CsilTypeExpression::Reference("Q".to_string()),
                    CsilTypeExpression::Reference("R".to_string()),
                    CsilServiceDirection::Unidirectional,
                    Some(1),
                ),
                op(
                    "play",
                    CsilTypeExpression::Reference("Move".to_string()),
                    CsilTypeExpression::Reference("Ack".to_string()),
                    CsilServiceDirection::Bidirectional,
                    Some(2),
                ),
            ],
            wire_id: Some(7),
        };
        let files = generate_java(&input_for(
            vec![rule("Match", CsilRuleType::ServiceDef(svc))],
            "java",
        ))
        .unwrap();
        assert!(files.iter().any(|f| f.path.ends_with("Codec.java")));
        assert!(
            files
                .iter()
                .any(|f| f.path.ends_with("EncodedMessage.java"))
        );

        let iface = file(&files, "Match.java");
        assert!(iface.content.contains("public interface Match {"));
        assert!(iface.content.contains("R listEvents(Q req);"));
        assert!(iface.content.contains("void play(Move msg);"));

        let router = file(&files, "MatchRouter.java");
        // wire-id ordinals
        assert!(
            router
                .content
                .contains("public static final long MatchServiceWireId = 7L;")
        );
        assert!(
            router
                .content
                .contains("public static final long MatchOpPlayWireId = 2L;")
        );
        // verbose router dispatches on method name
        assert!(router
            .content
            .contains("public static void routeMatchChannel(Match handlers, Codec codec, String method, byte[] data)"));
        assert!(router.content.contains("case \"Play\" -> {"));
        assert!(router.content.contains("handlers.play(msg);"));
        // compact router twin dispatches on the ordinal
        assert!(router
            .content
            .contains("public static void routeMatchChannelCompact(Match handlers, Codec codec, long op, byte[] data)"));
        assert!(router.content.contains("if (op == 2L) {"));
        // outbound encoder for the bidi op
        assert!(
            router
                .content
                .contains("public static EncodedMessage encodeMatchPlay(Codec codec, Ack msg)")
        );
        assert!(
            router
                .content
                .contains("return new EncodedMessage(\"Play\", codec.encode(msg));")
        );
    }

    #[test]
    fn push_only_op_drops_request_parameter() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "ping",
                builtin("null"),
                CsilTypeExpression::Reference("Pong".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let files = generate_java(&input_for(
            vec![rule("Health", CsilRuleType::ServiceDef(svc))],
            "java-client",
        ))
        .unwrap();
        let f = file(&files, "HealthClient.java");
        assert!(
            f.content
                .contains("public Pong ping() throws ClientException")
        );
        assert!(
            f.content
                .contains("transport.call(\"health\", \"Ping\", null, Pong.class)")
        );
    }

    #[test]
    fn unknown_subtarget_errors() {
        let err = generate_java(&input_for(
            vec![rule(
                "M",
                CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            )],
            "java-bogus",
        ));
        assert!(err.is_err());
    }

    #[test]
    fn field_description_becomes_param_javadoc_and_zero_floor_is_skipped() {
        let mut described = bare("display_name", builtin("text"), None);
        described
            .metadata
            .push(CsilFieldMetadata::Description("The shown name".to_string()));
        // A `.size (0..40)` lower bound of zero is vacuous and must not emit a `< 0` guard.
        let bounded_ty = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                min: 0,
                max: 40,
            })],
        };
        let bio = bare("bio", bounded_ty, None);
        let group = CsilGroupExpression {
            entries: vec![described, bio],
        };
        let mut r = rule("Profile", CsilRuleType::GroupDef(group));
        r.doc_comments = vec!["A user profile.".to_string()];
        let files = generate_java(&input_for(vec![r], "java-typesonly")).unwrap();
        let f = file(&files, "Profile.java");
        assert!(f.content.contains("/**\n * A user profile.\n"));
        assert!(f.content.contains(" * @param displayName The shown name"));
        // vacuous zero floor skipped; real upper bound kept.
        assert!(!f.content.contains("bio.length() < 0"));
        assert!(f.content.contains("bio.length() > 40"));
    }

    #[test]
    fn literal_choice_collapses_to_its_scalar() {
        // `text / "a" / "b"` is a string-constrained scalar, not a multi-type union.
        let files = generate_java(&input_for(
            vec![rule(
                "Status",
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    CsilTypeExpression::Literal(CsilLiteralValue::Text("a".to_string())),
                    CsilTypeExpression::Literal(CsilLiteralValue::Text("b".to_string())),
                ])),
            )],
            "java-typesonly",
        ))
        .unwrap();
        let f = file(&files, "Status.java");
        assert!(f.content.contains("public record Status(String value) {}"));
    }

    #[test]
    fn emitted_files_use_spaces_not_tabs() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "do-thing",
                CsilTypeExpression::Reference("In".to_string()),
                CsilTypeExpression::Reference("Out".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let files = generate_java(&input_for(
            vec![rule("ThingService", CsilRuleType::ServiceDef(svc))],
            "java-client",
        ))
        .unwrap();
        // The whole surface is space-indented; a stray tab would break the house style.
        for f in &files {
            assert!(!f.content.contains('\t'), "tab found in {}", f.path);
        }
    }

    #[test]
    fn map_and_array_box_primitive_generic_args() {
        assert_eq!(
            map_type(&CsilTypeExpression::Array {
                element_type: Box::new(builtin("int")),
                occurrence: None,
            }),
            "java.util.List<Long>"
        );
        assert_eq!(
            map_type(&CsilTypeExpression::Map {
                key: Box::new(builtin("text")),
                value: Box::new(builtin("bool")),
                occurrence: None,
            }),
            "java.util.Map<String, Boolean>"
        );
    }
}
