//! Kotlin (JVM) code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target kotlin` from `csilgen_kotlin_generator.wasm`.
//! Emits idiomatic Kotlin source: `data class` records, `sealed interface` choices,
//! typed client call-sites, server handler interfaces, and verbose/compact routers.
//! It never emits wire bytes — the transport library owns the wire. Structure mirrors
//! `wasm/csilgen-go-generator`; feature coverage mirrors `wasm/csilgen-python-generator`.

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
        name: "kotlin-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Kotlin (JVM) code generator with service support".to_string(),
        target: "kotlin".to_string(),
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
    serialize_and_return_ptr(&metadata)
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
    let result = match deserialize_input(input_ptr, input_len) {
        Ok(input) => process_generation(input),
        Err(error_code) => return create_error_result(error_code),
    };
    match result {
        Ok(output) => serialize_and_return_ptr(&output),
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

/// Which surface a (sub-)target selects, parallel to the Go generator's dispatch.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

fn process_generation(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = KotlinConfig::from_options(&input.config.options);
    let mut warnings = Vec::new();
    let mut files = Vec::new();

    let surface = match input.config.target.as_str() {
        "kotlin" | "kotlin-server" => Surface::Server,
        "kotlin-client" => Surface::Client,
        "kotlin-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let dir = config.package.replace('.', "/");
    let make_path = |filename: &str| -> String { format!("{dir}/{filename}") };

    if let Some(types_content) = generate_types(&input, &config, &mut warnings) {
        files.push(GeneratedFile {
            path: make_path("Types.kt"),
            content: types_content,
        });
    }

    if let Some(validation_content) = generate_validation(&input, &config) {
        files.push(GeneratedFile {
            path: make_path("Validation.kt"),
            content: validation_content,
        });
    }

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client_content) = generate_client(&input, &config) {
                    files.push(GeneratedFile {
                        path: make_path("Client.kt"),
                        content: client_content,
                    });
                }
            }
            Surface::Server => {
                if let Some(services_content) = generate_services(&input, &config) {
                    files.push(GeneratedFile {
                        path: make_path("Services.kt"),
                        content: services_content,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
    }

    let total_size: usize = files.iter().map(|f| f.content.len()).sum();
    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: total_size,
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings,
        stats,
    })
}

#[derive(Debug)]
struct KotlinConfig {
    package: String,
    package_description: String,
}

impl KotlinConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Self {
        let package = options
            .get("kotlin_package")
            .and_then(|v| v.as_str())
            .unwrap_or("community.catalyst.csilgen.generated")
            .to_string();
        let package_description = options
            .get("package_description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Self {
            package,
            package_description,
        }
    }
}

/// Standard file header: the doc block, the generated-code marker, and the
/// `package` declaration every emitted Kotlin file shares.
fn file_header(config: &KotlinConfig, summary: &str) -> String {
    let mut out = String::new();
    if config.package_description.is_empty() {
        out.push_str(&format!("// {summary}\n"));
    } else {
        for line in config.package_description.lines() {
            out.push_str(&format!("// {line}\n"));
        }
    }
    out.push_str("// Code generated by csilgen; DO NOT EDIT.\n\n");
    out.push_str(&format!("package {}\n\n", config.package));
    out
}

fn generate_types(
    input: &WasmGeneratorInput,
    config: &KotlinConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Option<String> {
    let mut body = String::new();
    let mut has_types = false;

    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                has_types = true;
                emit_data_class(&mut body, &rule.name, group, warnings);
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                has_types = true;
                emit_data_class(&mut body, &rule.name, group, warnings);
            }
            CsilRuleType::TypeDef(type_expr) => {
                has_types = true;
                let kt = map_csil_type_to_kotlin(type_expr, &None);
                body.push_str(&format!("/** Type alias for {}. */\n", rule.name));
                body.push_str(&format!(
                    "typealias {} = {}\n\n",
                    pascal_case(&rule.name),
                    kt
                ));
            }
            CsilRuleType::TypeChoice(choices) => {
                has_types = true;
                emit_type_choice(&mut body, &rule.name, choices);
            }
            CsilRuleType::GroupChoice(choices) => {
                has_types = true;
                emit_group_choice(&mut body, &rule.name, choices, warnings);
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    if !has_types {
        return None;
    }
    let mut content = file_header(config, "Generated types.");
    content.push_str(&body);
    Some(content)
}

/// A record (`group`) becomes a Kotlin `data class`. Optional fields are nullable
/// with a `null` default; a `@default`/`.default` becomes the constructor default.
/// A `ByteArray` member breaks `data class` structural equality (array identity),
/// so when one is present we override `equals`/`hashCode` with content semantics —
/// without this the conformance/value comparisons silently compare by reference.
fn emit_data_class(
    body: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    _warnings: &mut Vec<GeneratorWarning>,
) {
    let class_name = pascal_case(name);
    body.push_str(&format!("/** {name} record. */\n"));

    let mut has_byte_array = false;
    let fields: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();
    // A Kotlin `data class` must have at least one primary-constructor property; an empty
    // record degrades to a plain (singleton-friendly) class so it still compiles.
    if fields.is_empty() {
        body.push_str(&format!("class {class_name}\n\n"));
        return;
    }
    body.push_str(&format!("data class {class_name}(\n"));
    for (idx, entry) in fields.iter().enumerate() {
        let key = entry.key.as_ref().unwrap();
        let prop = kotlin_prop_name(key, &entry.metadata);
        let wire = wire_name_from_key(key);
        let kt_type = type_override(&entry.metadata)
            .unwrap_or_else(|| map_csil_type_to_kotlin(&entry.value_type, &entry.occurrence));
        if kt_type.contains("ByteArray") {
            has_byte_array = true;
        }
        if let Some(desc) = field_description(&entry.metadata) {
            body.push_str(&format!("    // {desc}\n"));
        }
        // The wire key is the CSIL field name verbatim; surface it so a reader can
        // see the camelCase property maps to the snake_case CBOR map key.
        if prop != wire {
            body.push_str(&format!("    // wire key: {wire}\n"));
        }
        let default = field_default(entry);
        let trailing = if idx + 1 < fields.len() { "," } else { "" };
        match default {
            Some(d) => body.push_str(&format!("    val {prop}: {kt_type} = {d}{trailing}\n")),
            None => body.push_str(&format!("    val {prop}: {kt_type}{trailing}\n")),
        }
    }

    if has_byte_array {
        body.push_str(") {\n");
        emit_byte_array_equality(body, &class_name, &fields);
        body.push_str("}\n\n");
    } else {
        body.push_str(")\n\n");
    }
}

/// `equals`/`hashCode` overrides that compare `ByteArray` members by content. Every
/// other property keeps value semantics; the array members use `contentEquals` /
/// `contentHashCode` so two records with equal bytes compare equal.
fn emit_byte_array_equality(body: &mut String, class_name: &str, fields: &[&CsilGroupEntry]) {
    // Each member: (property, is-bytes, is-nullable). A non-null `ByteArray` uses a plain
    // `contentEquals`; the null-safe form on it would warn "unnecessary safe call".
    let props: Vec<(String, bool, bool)> = fields
        .iter()
        .map(|e| {
            let key = e.key.as_ref().unwrap();
            let prop = kotlin_prop_name(key, &e.metadata);
            let kt = type_override(&e.metadata)
                .unwrap_or_else(|| map_csil_type_to_kotlin(&e.value_type, &e.occurrence));
            (prop, kt.contains("ByteArray"), kt.ends_with('?'))
        })
        .collect();

    body.push_str("    override fun equals(other: Any?): Boolean {\n");
    body.push_str("        if (this === other) return true\n");
    body.push_str(&format!(
        "        if (other !is {class_name}) return false\n"
    ));
    for (prop, is_bytes, nullable) in &props {
        match (is_bytes, nullable) {
            (true, true) => body.push_str(&format!(
                "        if (!({prop}?.contentEquals(other.{prop}) ?: (other.{prop} == null))) return false\n"
            )),
            (true, false) => body.push_str(&format!(
                "        if (!{prop}.contentEquals(other.{prop})) return false\n"
            )),
            _ => body.push_str(&format!(
                "        if ({prop} != other.{prop}) return false\n"
            )),
        }
    }
    body.push_str("        return true\n");
    body.push_str("    }\n\n");

    body.push_str("    override fun hashCode(): Int {\n");
    let mut first = true;
    for (prop, is_bytes, nullable) in &props {
        let expr = match (is_bytes, nullable) {
            (true, true) => format!("({prop}?.contentHashCode() ?: 0)"),
            (true, false) => format!("{prop}.contentHashCode()"),
            _ => format!("{prop}.hashCode()"),
        };
        if first {
            body.push_str(&format!("        var result = {expr}\n"));
            first = false;
        } else {
            body.push_str(&format!("        result = 31 * result + {expr}\n"));
        }
    }
    if first {
        body.push_str("        return 0\n");
    } else {
        body.push_str("        return result\n");
    }
    body.push_str("    }\n");
}

/// A type-choice (`A / B / C`) becomes a `sealed interface` whose arms are wrapper
/// `data class`es — Kotlin has no anonymous union, and a sealed hierarchy gives the
/// host an exhaustive `when`.
fn emit_type_choice(body: &mut String, name: &str, choices: &[CsilTypeExpression]) {
    let iface = pascal_case(name);
    body.push_str(&format!("/** {name}: one of {} arms. */\n", choices.len()));
    body.push_str(&format!("sealed interface {iface}\n"));
    for (i, choice) in choices.iter().enumerate() {
        let kt = map_csil_type_to_kotlin(choice, &None);
        let arm = format!("{iface}Arm{}", i + 1);
        body.push_str(&format!("data class {arm}(val value: {kt}) : {iface}\n"));
    }
    body.push('\n');
}

/// A group-choice becomes a `sealed interface` whose arms are full `data class`
/// records, each implementing the interface (exhaustive `when`).
fn emit_group_choice(
    body: &mut String,
    name: &str,
    choices: &[CsilGroupExpression],
    warnings: &mut Vec<GeneratorWarning>,
) {
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/** {name}: one of {} group arms. */\n",
        choices.len()
    ));
    body.push_str(&format!("sealed interface {iface}\n\n"));
    for (i, choice) in choices.iter().enumerate() {
        let arm_name = format!("{name}Choice{}", i + 1);
        emit_data_class_impl(body, &arm_name, choice, &iface, warnings);
    }
}

/// Like `emit_data_class`, but the class implements `iface`. Kept separate so the
/// plain record path stays the common case and free of an unused supertype.
fn emit_data_class_impl(
    body: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    iface: &str,
    _warnings: &mut Vec<GeneratorWarning>,
) {
    let class_name = pascal_case(name);
    let fields: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();
    // An empty arm still needs to implement the interface, but a `data class` cannot be
    // empty, so emit a `data object` (the idiomatic unit-arm shape for a sealed interface).
    if fields.is_empty() {
        body.push_str(&format!("data object {class_name} : {iface}\n\n"));
        return;
    }
    body.push_str(&format!("data class {class_name}(\n"));
    for (idx, entry) in fields.iter().enumerate() {
        let key = entry.key.as_ref().unwrap();
        let prop = kotlin_prop_name(key, &entry.metadata);
        let kt_type = type_override(&entry.metadata)
            .unwrap_or_else(|| map_csil_type_to_kotlin(&entry.value_type, &entry.occurrence));
        let default = field_default(entry);
        let trailing = if idx + 1 < fields.len() { "," } else { "" };
        match default {
            Some(d) => body.push_str(&format!("    val {prop}: {kt_type} = {d}{trailing}\n")),
            None => body.push_str(&format!("    val {prop}: {kt_type}{trailing}\n")),
        }
    }
    body.push_str(&format!(") : {iface}\n\n"));
}

/// Client scaffolding emitted once at the top of `Client.kt`: the error type and
/// the caller-supplied `Transport` every generated client delegates to.
const CLIENT_PRELUDE_KT: &str = "\
/**
 * ClientError is thrown by a generated client call: a structured error the service
 * returned (code/message), or a transport-level failure (cause).
 */
class ClientError(
    val code: Long = 0,
    override val message: String = \"\",
    cause: Throwable? = null,
) : RuntimeException(message, cause)

/**
 * Transport is supplied by the caller: it encodes the request (CBOR over some
 * carrier), performs the call named by (service, op), and returns the typed
 * response, or throws. The generator never owns the wire. Synchronous by design —
 * no coroutines; the host owns its own threads.
 */
interface Transport {
    fun call(service: String, op: String, request: Any?): Any?
}
";

fn generate_client(input: &WasmGeneratorInput, config: &KotlinConfig) -> Option<String> {
    let mut body = String::new();
    body.push_str(CLIENT_PRELUDE_KT);
    body.push('\n');

    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_class(&mut body, &rule.name, service);
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = file_header(config, "Generated service clients.");
    content.push_str(&body);
    Some(content)
}

fn emit_client_class(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let base = service_base(name);
    let client = format!("{base}Client");
    // The wire service string is the base name verbatim (no case transform leaks
    // onto the wire); the host and other-language clients must agree on it.
    let wire_service = &base;

    body.push_str(&format!("/** Typed client for the {name} service. */\n"));
    body.push_str(&format!(
        "class {client}(private val transport: Transport) {{\n"
    ));

    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            body.push_str(&format!(
                "    // channel operation '{}' is not part of the RPC client\n",
                operation.name
            ));
            continue;
        }
        let method = kotlin_method_name(&operation.name);
        // The wire op name stays verbatim (kebab-case) — case transforms never
        // reach the wire.
        let wire_op = &operation.name;
        let output_type = map_csil_type_to_kotlin(&success_type(&operation.output_type), &None);
        if op_input_is_null(&operation.input_type) {
            body.push_str(&format!("    fun {method}(): {output_type} {{\n"));
            body.push_str(&format!(
                "        return transport.call(\"{wire_service}\", \"{wire_op}\", null) as {output_type}\n"
            ));
        } else {
            let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
            body.push_str(&format!(
                "    fun {method}(request: {input_type}): {output_type} {{\n"
            ));
            body.push_str(&format!(
                "        return transport.call(\"{wire_service}\", \"{wire_op}\", request) as {output_type}\n"
            ));
        }
        body.push_str("    }\n");
    }
    body.push_str("}\n\n");
}

/// Codec + handler-outcome prelude emitted once at the top of `Services.kt` when any
/// service has channel ops. Codec is consumer-supplied so the runtime never owns
/// serialization, matching the Go/Python generators.
const SERVICES_CHANNEL_PRELUDE_KT: &str = "\
/**
 * Codec is the consumer-supplied (de)serialization layer for channel messages. The
 * generator is codec-agnostic; the implementer wires this to CBOR, JSON, or anything
 * its protocol expects.
 */
interface Codec {
    fun encode(value: Any?): ByteArray
    fun <T> decode(data: ByteArray, type: Class<T>): T
}
";

fn generate_services(input: &WasmGeneratorInput, config: &KotlinConfig) -> Option<String> {
    let mut body = String::new();
    let needs_channel = spec_has_channel_ops(input);
    if needs_channel {
        body.push_str(SERVICES_CHANNEL_PRELUDE_KT);
        body.push('\n');
    }

    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_service_interface(&mut body, &rule.name, service);
            emit_wire_ids(&mut body, &rule.name, service);
            if service_has_channel_ops(service) {
                emit_channel_router(&mut body, &rule.name, service);
                emit_channel_router_compact(&mut body, &rule.name, service);
                emit_channel_encoders(&mut body, &rule.name, service);
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }
    let mut content = file_header(config, "Generated service interfaces and routers.");
    content.push_str(&body);
    Some(content)
}

fn emit_service_interface(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/** {name} service interface (host implements). */\n"
    ));
    body.push_str(&format!("interface {iface} {{\n"));
    for operation in &service.operations {
        let method = kotlin_method_name(&operation.name);
        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                let output_type =
                    map_csil_type_to_kotlin(&success_type(&operation.output_type), &None);
                if op_input_is_null(&operation.input_type) {
                    body.push_str(&format!("    fun {method}(): {output_type}\n"));
                } else {
                    let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
                    body.push_str(&format!(
                        "    fun {method}(request: {input_type}): {output_type}\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
                // Fire-and-forget inbound: the host's plumbing pulls a frame off the
                // wire and hands it to the router, which decodes and dispatches here.
                body.push_str(&format!("    fun {method}(message: {input_type})\n"));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound method on the server side.
            }
        }
    }
    body.push_str("}\n\n");
}

/// Emit `const`-style wire-id ordinals (as a top-level `object`) exposing the
/// `@wire-id(N)` values. Purely additive: nothing is emitted for a wire-id-free
/// service, keeping that output byte-identical.
fn emit_wire_ids(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let prefix = pascal_case(name);
    body.push_str(&format!(
        "/** Wire-id ordinals for {name} (transport compact profiles). */\n"
    ));
    body.push_str(&format!("object {prefix}WireIds {{\n"));
    body.push_str(&format!("    const val SERVICE: ULong = {service_id}uL\n"));
    for operation in &service.operations {
        if let Some(op_id) = operation.wire_id {
            let op_const = screaming_snake(&operation.name);
            body.push_str(&format!("    const val OP_{op_const}: ULong = {op_id}uL\n"));
        }
    }
    body.push_str("}\n\n");
}

fn emit_channel_router(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/**\n * Decode one inbound channel frame and dispatch to the matching {name}\n * method by its verbose wire op name. The host feeds bytes from its connection.\n */\n"
    ));
    body.push_str(&format!(
        "fun route{iface}Channel(handlers: {iface}, codec: Codec, op: String, data: ByteArray) {{\n"
    ));
    body.push_str("    when (op) {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = kotlin_method_name(&operation.name);
        let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
        // The wire op name is verbatim (kebab-case CSIL operation name).
        body.push_str(&format!("        \"{}\" -> {{\n", operation.name));
        body.push_str(&format!(
            "            val message = codec.decode(data, {input_type}::class.java)\n"
        ));
        body.push_str(&format!("            handlers.{method}(message)\n"));
        body.push_str("        }\n");
    }
    body.push_str("        else -> throw IllegalArgumentException(\"unknown channel op '$op'\")\n");
    body.push_str("    }\n");
    body.push_str("}\n\n");
}

/// Compact-profile twin: dispatch on the operation's `@wire-id` ordinal instead of
/// the verbose name. Emitted only for wire-id-bearing services so wire-id-free
/// output stays byte-identical.
fn emit_channel_router_compact(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    if service.wire_id.is_none() {
        return;
    }
    let iface = pascal_case(name);
    body.push_str(&format!(
        "/**\n * Decode one inbound channel frame by its @wire-id ordinal (compact profile)\n * and dispatch to the matching {name} method. The verbose twin is route{iface}Channel;\n * the host calls whichever matches the profile negotiated on the wire.\n */\n"
    ));
    body.push_str(&format!(
        "fun route{iface}ChannelCompact(handlers: {iface}, codec: Codec, op: ULong, data: ByteArray) {{\n"
    ));
    body.push_str("    when (op) {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let Some(op_id) = operation.wire_id else {
            continue;
        };
        let method = kotlin_method_name(&operation.name);
        let input_type = map_csil_type_to_kotlin(&operation.input_type, &None);
        body.push_str(&format!("        {op_id}uL -> {{\n"));
        body.push_str(&format!(
            "            val message = codec.decode(data, {input_type}::class.java)\n"
        ));
        body.push_str(&format!("            handlers.{method}(message)\n"));
        body.push_str("        }\n");
    }
    body.push_str(
        "        else -> throw IllegalArgumentException(\"unknown channel ordinal $op\")\n",
    );
    body.push_str("    }\n");
    body.push_str("}\n\n");
}

/// Outbound encoders for server-pushed (bidirectional/reverse) messages: the host
/// frames (op, bytes) onto its connection.
fn emit_channel_encoders(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let iface = pascal_case(name);
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let output_type = map_csil_type_to_kotlin(&operation.output_type, &None);
        let fn_name = format!("encode{iface}{}", pascal_case(&operation.name));
        body.push_str(&format!(
            "/** Encode a `{}` message the server pushes to a peer. */\n",
            operation.name
        ));
        body.push_str(&format!(
            "fun {fn_name}(codec: Codec, message: {output_type}): Pair<String, ByteArray> {{\n"
        ));
        body.push_str(&format!(
            "    return Pair(\"{}\", codec.encode(message))\n",
            operation.name
        ));
        body.push_str("}\n\n");
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn generate_validation(input: &WasmGeneratorInput, config: &KotlinConfig) -> Option<String> {
    let mut body = String::new();

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        let Some(group) = group else { continue };
        if !group.entries.iter().any(entry_has_check) {
            continue;
        }

        let class_name = pascal_case(&rule.name);
        body.push_str(&format!(
            "/** Validate a {class_name}; throws IllegalArgumentException on a constraint breach. */\n"
        ));
        body.push_str(&format!("fun {class_name}.validate() {{\n"));
        for entry in &group.entries {
            if let Some(key) = &entry.key {
                let prop = kotlin_prop_name(key, &entry.metadata);
                let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
                let field = FieldRef {
                    prop: &prop,
                    optional,
                };
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Constraint(constraint) = metadata {
                        emit_metadata_constraint(&mut body, field, &entry.value_type, constraint);
                    }
                }
                if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                    for op in constraints {
                        emit_control_op_check(&mut body, field, &entry.value_type, op);
                    }
                }
            }
        }
        body.push_str("}\n\n");
    }

    if body.is_empty() {
        return None;
    }
    let mut content = file_header(config, "Generated validation functions.");
    content.push_str(&body);
    Some(content)
}

/// A field's Kotlin property name plus whether it is nullable (optional). An
/// optional field's checks are guarded behind a `?.let { ... }` so a null value is
/// skipped rather than throwing on access.
#[derive(Clone, Copy)]
struct FieldRef<'a> {
    prop: &'a str,
    optional: bool,
}

/// Emit a `require(...)` guarded by optionality. `cond` is the *valid* condition; an
/// optional field only checks when present. The value is bound to `v` inside the
/// guard so `cond`/`message` reference `v`.
fn push_require(body: &mut String, field: FieldRef, cond: &str, message: &str) {
    if field.optional {
        // The value is bound to `v` inside the `?.let`, so an absent (null) optional
        // is skipped rather than dereferenced.
        body.push_str(&format!("    {}?.let {{ v ->\n", field.prop));
        body.push_str(&format!("        require({cond}) {{ \"{message}\" }}\n"));
        body.push_str("    }\n");
    } else {
        // A required field inlines its property name where the guard would bind `v`.
        // Replace only the standalone `v` placeholder token, never a literal `v` inside a
        // regex pattern or string bound, which a blanket char replace would corrupt.
        body.push_str(&format!(
            "    require({}) {{ \"{message}\" }}\n",
            substitute_placeholder(cond, field.prop)
        ));
    }
}

/// Replace the standalone `v` binding placeholder in a guard condition with `prop`,
/// leaving any `v` that is part of a larger identifier or inside a quoted string literal
/// (e.g. a regex pattern like `"^v[0-9]+$"`) untouched.
fn substitute_placeholder(cond: &str, prop: &str) -> String {
    let chars: Vec<char> = cond.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(cond.len());
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        let prev_ident = i > 0 && is_ident(chars[i - 1]);
        let next_ident = i + 1 < chars.len() && is_ident(chars[i + 1]);
        if c == 'v' && !prev_ident && !next_ident {
            out.push_str(prop);
        } else {
            out.push(c);
        }
    }
    out
}

fn emit_metadata_constraint(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
) {
    let name = field.prop;
    match constraint {
        CsilValidationConstraint::MinLength(n) => push_require(
            body,
            field,
            &format!("v.length >= {n}"),
            &format!("field '{name}' must have at least {n} characters"),
        ),
        CsilValidationConstraint::MaxLength(n) => push_require(
            body,
            field,
            &format!("v.length <= {n}"),
            &format!("field '{name}' must have at most {n} characters"),
        ),
        CsilValidationConstraint::MinItems(n) => push_require(
            body,
            field,
            &format!("v.size >= {n}"),
            &format!("field '{name}' must have at least {n} items"),
        ),
        CsilValidationConstraint::MaxItems(n) => push_require(
            body,
            field,
            &format!("v.size <= {n}"),
            &format!("field '{name}' must have at most {n} items"),
        ),
        CsilValidationConstraint::MinValue(v) => {
            emit_ordered_check(body, field, value_type, (">=", "at least"), v)
        }
        CsilValidationConstraint::MaxValue(v) => {
            emit_ordered_check(body, field, value_type, ("<=", "at most"), v)
        }
        CsilValidationConstraint::Custom { name: cname, value } => {
            if cname == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                emit_regex_check(body, field, pattern);
            }
        }
    }
}

fn emit_control_op_check(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
) {
    match op {
        CsilControlOperator::GreaterEqual(v) => {
            emit_ordered_check(body, field, value_type, (">=", "at least"), v)
        }
        CsilControlOperator::LessEqual(v) => {
            emit_ordered_check(body, field, value_type, ("<=", "at most"), v)
        }
        CsilControlOperator::GreaterThan(v) => {
            emit_ordered_check(body, field, value_type, (">", "greater than"), v)
        }
        CsilControlOperator::LessThan(v) => {
            emit_ordered_check(body, field, value_type, ("<", "less than"), v)
        }
        CsilControlOperator::Equal(v) => {
            emit_ordered_check(body, field, value_type, ("==", "equal to"), v)
        }
        CsilControlOperator::NotEqual(v) => {
            emit_ordered_check(body, field, value_type, ("!=", "not equal to"), v)
        }
        CsilControlOperator::Size(size) => emit_size_check(body, field, value_type, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(body, field, pattern),
        // Applied as a constructor default, not validated.
        CsilControlOperator::Default(_) => {}
        // Encoding-only operators carry no runtime check.
        _ => {}
    }
}

/// Emit one ordered comparison. Numeric/decimal/timestamp fields share one shape:
/// `decimal` (BigDecimal) and `timestamp` (Instant) both have `compareTo`, so the
/// emitted `v <op> bound` works for all three once the bound is the right type.
fn emit_ordered_check(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
) {
    let (kt_op, desc) = op;
    let name = field.prop;
    let kind = ordered_field_kind(value_type);
    let (bound_expr, shown) = match kind {
        OrderedKind::Numeric => {
            let s = literal_to_kotlin(value);
            (s.clone(), s)
        }
        OrderedKind::Decimal => {
            let Some(text) = literal_as_text(value) else {
                return;
            };
            (
                format!("java.math.BigDecimal(\"{text}\")"),
                kotlin_escape(&text),
            )
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_text(value) else {
                return;
            };
            (
                format!("java.time.Instant.parse(\"{text}\")"),
                kotlin_escape(&text),
            )
        }
    };
    // BigDecimal/Instant compare through compareTo; the `<op>` operator desugars to
    // it, but `==`/`!=` on those types is structural-but-scale-sensitive, so use an
    // explicit compareTo for the equality forms to match value ordering.
    let cond = match (kind, kt_op) {
        (OrderedKind::Numeric, _) => format!("v {kt_op} {bound_expr}"),
        (_, "==") => format!("v.compareTo({bound_expr}) == 0"),
        (_, "!=") => format!("v.compareTo({bound_expr}) != 0"),
        (_, _) => format!("v {kt_op} {bound_expr}"),
    };
    let message = format!("field '{name}' must be {desc} {shown}");
    push_require(body, field, &cond, &message);
}

fn emit_size_check(
    body: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    size: &CsilSizeConstraint,
) {
    let name = field.prop;
    // Kotlin's `String` exposes `.length`; `ByteArray`/`List` expose `.size`. A `.size`
    // constraint on a text field would not compile against `.size`, so pick the accessor
    // (and the noun) from the field's base type.
    let (accessor, unit) = match size_accessor(value_type) {
        SizeAccessor::Length => ("length", "characters"),
        SizeAccessor::Size => ("size", "elements"),
    };
    let mut one = |kt_op: &str, n: u64, word: &str| {
        push_require(
            body,
            field,
            &format!("v.{accessor} {kt_op} {n}"),
            &format!("field '{name}' must have {word} {n} {unit}"),
        );
    };
    match size {
        CsilSizeConstraint::Exact(n) => one("==", *n, "exactly"),
        CsilSizeConstraint::Min(n) => one(">=", *n, "at least"),
        CsilSizeConstraint::Max(n) => one("<=", *n, "at most"),
        CsilSizeConstraint::Range { min, max } => {
            one(">=", *min, "at least");
            one("<=", *max, "at most");
        }
    }
}

fn emit_regex_check(body: &mut String, field: FieldRef, pattern: &str) {
    let name = field.prop;
    let escaped = kotlin_escape(pattern);
    push_require(
        body,
        field,
        &format!("Regex(\"{escaped}\").containsMatchIn(v)"),
        &format!("field '{name}' must match pattern '{escaped}'"),
    );
}

fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let meta = entry.metadata.iter().any(|m| match m {
        CsilFieldMetadata::Constraint(c) => constraint_is_check(c),
        _ => false,
    });
    let op = match &entry.value_type {
        CsilTypeExpression::Constrained { constraints, .. } => {
            constraints.iter().any(control_op_is_check)
        }
        _ => false,
    };
    meta || op
}

fn constraint_is_check(constraint: &CsilValidationConstraint) -> bool {
    match constraint {
        CsilValidationConstraint::Custom { name, .. } => name == "regex",
        _ => true,
    }
}

fn control_op_is_check(op: &CsilControlOperator) -> bool {
    matches!(
        op,
        CsilControlOperator::Size(_)
            | CsilControlOperator::Regex(_)
            | CsilControlOperator::GreaterEqual(_)
            | CsilControlOperator::LessEqual(_)
            | CsilControlOperator::GreaterThan(_)
            | CsilControlOperator::LessThan(_)
            | CsilControlOperator::Equal(_)
            | CsilControlOperator::NotEqual(_)
    )
}

// ---------------------------------------------------------------------------
// Type & name mapping
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum OrderedKind {
    Numeric,
    Decimal,
    Timestamp,
}

/// Which Kotlin length member a `.size` constraint compiles against: `String` has
/// `.length`, while `ByteArray`/`List` have `.size`.
enum SizeAccessor {
    Length,
    Size,
}

fn size_accessor(value_type: &CsilTypeExpression) -> SizeAccessor {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    match base {
        CsilTypeExpression::Builtin(name) if name == "text" || name == "tstr" => {
            SizeAccessor::Length
        }
        _ => SizeAccessor::Size,
    }
}

fn ordered_field_kind(value_type: &CsilTypeExpression) -> OrderedKind {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    if let CsilTypeExpression::Builtin(name) = base {
        match name.as_str() {
            "decimal" => OrderedKind::Decimal,
            "timestamp" => OrderedKind::Timestamp,
            _ => OrderedKind::Numeric,
        }
    } else {
        OrderedKind::Numeric
    }
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` arm of a `Res / ServiceError` union — the error half is surfaced
/// as a thrown `ClientError`, not part of the typed response.
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

fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

fn map_csil_type_to_kotlin(
    type_expr: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
) -> String {
    let base = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "Long".to_string(),
            "uint" => "ULong".to_string(),
            "float" => "Double".to_string(),
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "ByteArray".to_string(),
            "bool" => "Boolean".to_string(),
            // CBOR tag 0 RFC3339 instant; java.time.Instant is exact and stdlib.
            "timestamp" => "java.time.Instant".to_string(),
            // CBOR tag 4 exact decimal; BigDecimal is the JVM-idiomatic exact type.
            "decimal" => "java.math.BigDecimal".to_string(),
            "nil" | "null" => "Unit".to_string(),
            "any" => "Any".to_string(),
            other => pascal_case(other),
        },
        CsilTypeExpression::Reference(name) => pascal_case(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("List<{}>", map_csil_type_to_kotlin(element_type, &None))
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "Map<{}, {}>",
            map_csil_type_to_kotlin(key, &None),
            map_csil_type_to_kotlin(value, &None)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => {
            return map_csil_type_to_kotlin(base_type, occurrence);
        }
        // Kotlin has no anonymous tuple; a fixed-shape array degrades to List<Any?>.
        CsilTypeExpression::Tuple(_) => "List<Any?>".to_string(),
        CsilTypeExpression::Choice(choices) => {
            let reduced = success_type(&CsilTypeExpression::Choice(choices.clone()));
            match reduced {
                CsilTypeExpression::Choice(_) => "Any".to_string(),
                other => map_csil_type_to_kotlin(&other, &None),
            }
        }
        _ => "Any".to_string(),
    };
    match occurrence {
        Some(CsilOccurrence::Optional) => format!("{base}?"),
        _ => base,
    }
}

/// The default expression for a `data class` field: an explicit `@default`/`.default`
/// literal, or `null` for an optional field with no declared default.
fn field_default(entry: &CsilGroupEntry) -> Option<String> {
    if let Some(value) = entry_default_value(entry) {
        return Some(literal_to_kotlin_typed(value, &entry.value_type));
    }
    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
        return Some("null".to_string());
    }
    None
}

fn entry_default_value(entry: &CsilGroupEntry) -> Option<&CsilLiteralValue> {
    for metadata in &entry.metadata {
        if let CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, value }) =
            metadata
            && name == "default"
        {
            return Some(value);
        }
    }
    if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
        for op in constraints {
            if let CsilControlOperator::Default(value) = op {
                return Some(value);
            }
        }
    }
    None
}

fn literal_to_kotlin(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{}\"", kotlin_escape(s)),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        CsilLiteralValue::Bytes(_) => "ByteArray(0)".to_string(),
        CsilLiteralValue::Array(elements) => {
            let inner: Vec<String> = elements.iter().map(literal_to_kotlin).collect();
            format!("listOf({})", inner.join(", "))
        }
    }
}

/// A literal rendered for a specifically-typed field: `decimal`/`timestamp` bounds
/// parse into BigDecimal/Instant; everything else is a plain literal.
fn literal_to_kotlin_typed(value: &CsilLiteralValue, value_type: &CsilTypeExpression) -> String {
    match ordered_field_kind(value_type) {
        OrderedKind::Decimal => {
            if let Some(text) = literal_as_text(value) {
                return format!("java.math.BigDecimal(\"{}\")", kotlin_escape(&text));
            }
        }
        OrderedKind::Timestamp => {
            if let Some(text) = literal_as_text(value) {
                return format!("java.time.Instant.parse(\"{}\")", kotlin_escape(&text));
            }
        }
        OrderedKind::Numeric => {}
    }
    literal_to_kotlin(value)
}

fn literal_as_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn spec_has_channel_ops(input: &WasmGeneratorInput) -> bool {
    input.csil_spec.rules.iter().any(|r| match &r.rule_type {
        CsilRuleType::ServiceDef(def) => service_has_channel_ops(def),
        _ => false,
    })
}

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

/// Strip a trailing `Service` suffix and PascalCase the remainder, matching the
/// wire service base used across the other clients.
fn service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

fn wire_name_from_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

/// The Kotlin property name for a field key: a `@kotlin_name` override, otherwise
/// the wire name lowerCamelCased.
fn kotlin_prop_name(key: &CsilGroupKey, metadata: &[CsilFieldMetadata]) -> String {
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "kotlin_name"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(kt_name) = &param.value
        {
            return kt_name.clone();
        }
    }
    camel_case(&wire_name_from_key(key))
}

fn type_override(metadata: &[CsilFieldMetadata]) -> Option<String> {
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "kotlin_type"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(kt_type) = &param.value
        {
            return Some(kt_type.clone());
        }
    }
    None
}

fn field_description(metadata: &[CsilFieldMetadata]) -> Option<String> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.replace(['\n', '\r'], " "))
        } else {
            None
        }
    })
}

fn kotlin_method_name(name: &str) -> String {
    camel_case(name)
}

/// PascalCase for type names: split on `_`/`-`, capitalize each word.
fn pascal_case(s: &str) -> String {
    s.split(['_', '-'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// lowerCamelCase for properties/functions: PascalCase, then lowercase the leading
/// character.
fn camel_case(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
    }
}

/// SCREAMING_SNAKE_CASE for wire-id `const` names.
fn screaming_snake(s: &str) -> String {
    s.split(['_', '-'])
        .map(|w| w.to_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Escape a string for a Kotlin double-quoted literal.
fn kotlin_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn serialize_and_return_ptr<T: serde::Serialize>(data: &T) -> *mut u8 {
    let serialized = match serde_json::to_string(data) {
        Ok(json) => json,
        Err(_) => return std::ptr::null_mut(),
    };
    let bytes = serialized.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }
    ptr
}

fn create_error_result(error_code: i32) -> *mut u8 {
    let error_output = WasmGeneratorOutput {
        files: vec![],
        warnings: vec![GeneratorWarning {
            level: WarningLevel::Warning,
            message: format!("Generator failed with error code: {error_code}"),
            location: None,
            suggestion: None,
        }],
        stats: GenerationStats::default(),
    };
    serialize_and_return_ptr(&error_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilPosition, CsilRule, CsilServiceOperation, CsilSpecSerialized, GeneratorConfig,
    };

    fn spec(target: &str, rules: Vec<CsilRule>) -> WasmGeneratorInput {
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
            generator_metadata: GeneratorMetadata {
                name: "kotlin".to_string(),
                version: "0.1.0".to_string(),
                description: String::new(),
                target: "kotlin".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
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

    fn entry(
        name: &str,
        value_type: CsilTypeExpression,
        occurrence: Option<CsilOccurrence>,
    ) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type,
            occurrence,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    fn builtin(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Builtin(name.to_string())
    }

    fn reference(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Reference(name.to_string())
    }

    fn op(
        name: &str,
        input: CsilTypeExpression,
        output: CsilTypeExpression,
        direction: CsilServiceDirection,
        wire_id: Option<u64>,
    ) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: input,
            output_type: output,
            direction,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id,
        }
    }

    fn content(out: &WasmGeneratorOutput, suffix: &str) -> String {
        out.files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .map(|f| f.content.clone())
            .unwrap_or_else(|| panic!("no file ending in {suffix}"))
    }

    #[test]
    fn case_helpers() {
        assert_eq!(pascal_case("user_name"), "UserName");
        assert_eq!(pascal_case("deposit-claim"), "DepositClaim");
        assert_eq!(camel_case("user_name"), "userName");
        assert_eq!(camel_case("deposit-claim"), "depositClaim");
        assert_eq!(screaming_snake("deposit-claim"), "DEPOSIT_CLAIM");
    }

    #[test]
    fn data_class_camel_fields_and_nullable_optional() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("user_name", builtin("text"), None),
                entry(
                    "created_at",
                    builtin("timestamp"),
                    Some(CsilOccurrence::Optional),
                ),
                entry("retry_count", builtin("int"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("UserProfile", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("data class UserProfile("));
        assert!(types.contains("val userName: String"));
        assert!(types.contains("// wire key: user_name"));
        assert!(types.contains("val createdAt: java.time.Instant? = null"));
        assert!(types.contains("val retryCount: Long"));
    }

    #[test]
    fn byte_array_gets_content_equality() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("blob", builtin("bytes"), None),
                entry("sig", builtin("bytes"), Some(CsilOccurrence::Optional)),
                entry("name", builtin("text"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Payload", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("val blob: ByteArray"));
        assert!(types.contains("override fun equals(other: Any?): Boolean"));
        assert!(types.contains("override fun hashCode(): Int"));
        // Non-null ByteArray: plain contentEquals (a safe call would warn "unnecessary").
        assert!(types.contains("if (!blob.contentEquals(other.blob)) return false"));
        assert!(types.contains("var result = blob.contentHashCode()"));
        // Nullable ByteArray: null-safe content compare.
        assert!(types.contains("sig?.contentEquals(other.sig) ?: (other.sig == null)"));
        assert!(types.contains("(sig?.contentHashCode() ?: 0)"));
    }

    #[test]
    fn type_mapping_covers_builtins_and_containers() {
        let group = CsilGroupExpression {
            entries: vec![
                entry("a", builtin("uint"), None),
                entry("b", builtin("float"), None),
                entry("c", builtin("bool"), None),
                entry("d", builtin("decimal"), None),
                entry(
                    "e",
                    CsilTypeExpression::Array {
                        element_type: Box::new(builtin("text")),
                        occurrence: None,
                    },
                    None,
                ),
                entry(
                    "f",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                    None,
                ),
                entry("g", reference("other_type"), None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Mix", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("val a: ULong"));
        assert!(types.contains("val b: Double"));
        assert!(types.contains("val c: Boolean"));
        assert!(types.contains("val d: java.math.BigDecimal"));
        assert!(types.contains("val e: List<String>"));
        assert!(types.contains("val f: Map<String, Long>"));
        assert!(types.contains("val g: OtherType"));
    }

    #[test]
    fn type_choice_is_sealed_interface() {
        let out = process_generation(spec(
            "kotlin",
            vec![rule(
                "StringOrNumber",
                CsilRuleType::TypeChoice(vec![builtin("text"), builtin("int")]),
            )],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("sealed interface StringOrNumber"));
        assert!(
            types.contains("data class StringOrNumberArm1(val value: String) : StringOrNumber")
        );
        assert!(types.contains("data class StringOrNumberArm2(val value: Long) : StringOrNumber"));
    }

    #[test]
    fn group_choice_is_sealed_interface_of_records() {
        let g1 = CsilGroupExpression {
            entries: vec![entry("x", builtin("int"), None)],
        };
        let g2 = CsilGroupExpression {
            entries: vec![entry("y", builtin("text"), None)],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Shape", CsilRuleType::GroupChoice(vec![g1, g2]))],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("sealed interface Shape"));
        assert!(types.contains("data class ShapeChoice1("));
        assert!(types.contains(") : Shape"));
        assert!(types.contains("val x: Long"));
        assert!(types.contains("val y: String"));
    }

    #[test]
    fn server_interface_routers_and_wire_ids() {
        let service = CsilServiceDefinition {
            operations: vec![
                op(
                    "list-events",
                    reference("Query"),
                    reference("Result"),
                    CsilServiceDirection::Unidirectional,
                    Some(1),
                ),
                op(
                    "play",
                    reference("Move"),
                    builtin("null"),
                    CsilServiceDirection::Bidirectional,
                    Some(2),
                ),
            ],
            wire_id: Some(7),
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("MatchService", CsilRuleType::ServiceDef(service))],
        ))
        .unwrap();
        let services = content(&out, "Services.kt");
        assert!(services.contains("interface Codec"));
        assert!(services.contains("interface MatchService {"));
        assert!(services.contains("fun listEvents(request: Query): Result"));
        assert!(services.contains("fun play(message: Move)"));
        assert!(services.contains("object MatchServiceWireIds"));
        assert!(services.contains("const val SERVICE: ULong = 7uL"));
        assert!(services.contains("const val OP_PLAY: ULong = 2uL"));
        assert!(services.contains(
            "fun routeMatchServiceChannel(handlers: MatchService, codec: Codec, op: String, data: ByteArray)"
        ));
        // The verbose router dispatches on the verbatim (kebab-case) wire op name.
        assert!(services.contains("\"play\" -> {"));
        assert!(services.contains("handlers.play(message)"));
        assert!(services.contains("fun routeMatchServiceChannelCompact(handlers: MatchService, codec: Codec, op: ULong, data: ByteArray)"));
        assert!(services.contains("2uL -> {"));
        assert!(services.contains("fun encodeMatchServicePlay(codec: Codec, message:"));
        assert!(services.contains("Pair(\"play\", codec.encode(message))"));
    }

    #[test]
    fn client_target_typed_client_strips_service_error() {
        let service = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                reference("SubmitTaskRequest"),
                CsilTypeExpression::Choice(vec![
                    reference("SubmitTaskResponse"),
                    reference("ServiceError"),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-client",
            vec![rule("CorndogsService", CsilRuleType::ServiceDef(service))],
        ))
        .unwrap();
        let client = content(&out, "Client.kt");
        assert!(client.contains("interface Transport"));
        assert!(client.contains("class ClientError"));
        assert!(client.contains("class CorndogsClient(private val transport: Transport)"));
        assert!(client.contains("fun submitTask(request: SubmitTaskRequest): SubmitTaskResponse"));
        // Wire service base + verbatim op name stay un-cased on the wire.
        assert!(client.contains("transport.call(\"Corndogs\", \"submit-task\", request)"));
        assert!(!out.files.iter().any(|f| f.path.ends_with("Services.kt")));
    }

    #[test]
    fn typesonly_skips_service_surface() {
        let service = CsilServiceDefinition {
            operations: vec![op(
                "ping",
                builtin("null"),
                reference("Pong"),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let out = process_generation(spec(
            "kotlin-typesonly",
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![entry("v", builtin("int"), None)],
                    }),
                ),
                rule("PingService", CsilRuleType::ServiceDef(service)),
            ],
        ))
        .unwrap();
        assert!(out.files.iter().any(|f| f.path.ends_with("Types.kt")));
        assert!(!out.files.iter().any(|f| f.path.ends_with("Services.kt")));
        assert!(!out.files.iter().any(|f| f.path.ends_with("Client.kt")));
    }

    #[test]
    fn unknown_subtarget_errors() {
        let r = process_generation(spec(
            "kotlin-bogus",
            vec![rule(
                "X",
                CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            )],
        ));
        assert!(r.is_err());
    }

    #[test]
    fn validation_guards_optional_and_inlines_required() {
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        };
        let mut name_entry = entry("name", builtin("text"), Some(CsilOccurrence::Optional));
        name_entry.metadata = vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinLength(2),
        )];
        let group = CsilGroupExpression {
            entries: vec![name_entry, entry("tags", constrained, None)],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Form", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let validation = content(&out, "Validation.kt");
        assert!(validation.contains("fun Form.validate()"));
        assert!(validation.contains("name?.let { v ->"));
        assert!(validation.contains("v.length >= 2"));
        // `tags` is a text field, so a `.size` constraint must compile against `.length`
        // (Kotlin's `String` has no `.size`), with the property inlined for a required field.
        assert!(validation.contains("require(tags.length >= 3)"));
    }

    #[test]
    fn empty_record_is_not_an_empty_data_class() {
        // A Kotlin `data class` with no primary-constructor properties does not compile.
        let out = process_generation(spec(
            "kotlin",
            vec![rule(
                "Empty",
                CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            )],
        ))
        .unwrap();
        let types = content(&out, "Types.kt");
        assert!(types.contains("class Empty\n"));
        assert!(!types.contains("data class Empty("));
    }

    #[test]
    fn size_check_accessor_matches_kotlin_type() {
        let text_field = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Max(8))],
        };
        let bytes_field = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("bytes")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Exact(32))],
        };
        let group = CsilGroupExpression {
            entries: vec![
                entry("label", text_field, None),
                entry("digest", bytes_field, None),
            ],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Item", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let validation = content(&out, "Validation.kt");
        // String → `.length` (a `.size` here would not compile); ByteArray → `.size`.
        assert!(validation.contains("require(label.length <= 8)"));
        assert!(validation.contains("8 characters"));
        assert!(validation.contains("require(digest.size == 32)"));
        assert!(validation.contains("32 elements"));
    }

    #[test]
    fn required_regex_keeps_literal_v_in_pattern() {
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            // A pattern containing 'v' must survive placeholder substitution intact.
            constraints: vec![CsilControlOperator::Regex("^v[0-9]+$".to_string())],
        };
        let group = CsilGroupExpression {
            entries: vec![entry("version_tag", constrained, None)],
        };
        let out = process_generation(spec(
            "kotlin",
            vec![rule("Tagged", CsilRuleType::GroupDef(group))],
        ))
        .unwrap();
        let validation = content(&out, "Validation.kt");
        // The `$` is escaped for the Kotlin string literal; the leading `v` survives.
        assert!(validation.contains("Regex(\"^v[0-9]+\\$\").containsMatchIn(versionTag)"));
    }

    #[test]
    fn package_option_controls_path_and_declaration() {
        let group = CsilGroupExpression {
            entries: vec![entry("v", builtin("int"), None)],
        };
        let mut input = spec("kotlin", vec![rule("Thing", CsilRuleType::GroupDef(group))]);
        input.config.options.insert(
            "kotlin_package".to_string(),
            serde_json::Value::String("com.example.api".to_string()),
        );
        let out = process_generation(input).unwrap();
        let f = out
            .files
            .iter()
            .find(|f| f.path.ends_with("Types.kt"))
            .unwrap();
        assert_eq!(f.path, "com/example/api/Types.kt");
        assert!(f.content.contains("package com.example.api"));
    }
}
