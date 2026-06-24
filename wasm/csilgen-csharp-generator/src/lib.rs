//! C# code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target csharp` from `csilgen_csharp_generator.wasm`.
//! Emits idiomatic modern C# (net8.0 / C# 12): file-scoped `namespace Csilgen.Transport;`,
//! `sealed record` types with `required`/nullable `init` properties, closed
//! discriminated-union emulation for variants, a primary-constructor client, and a
//! server interface + verbose/compact channel routers — never the wire bytes.

use csilgen_common::{
    CsilControlOperator, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
};
use csilgen_common::{CsilFieldMetadata, GeneratorWarning};
use std::collections::HashMap;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "csharp-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "C# (.NET 8 / C# 12) code generator".to_string(),
        target: "csharp".to_string(),
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
    render(input)
}

/// In-memory C# type chosen for the CSIL `decimal` core type. The wire form is CBOR
/// tag 4 either way; only the emitted property type differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` record (no NuGet dependency).
    Csil,
    /// Use the BCL `decimal` (System.Decimal).
    Library,
}

struct CsharpConfig {
    namespace: String,
    decimal_mapping: DecimalMapping,
}

impl CsharpConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
        let namespace = options
            .get("csharp_namespace")
            .or_else(|| options.get("namespace"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Csilgen.Transport")
            .to_string();

        // A typo in decimal_mapping is a hard error so misconfiguration surfaces at
        // generation time rather than silently degrading to the default.
        let decimal_mapping = match options.get("decimal_mapping") {
            None => DecimalMapping::Csil,
            Some(v) => match v.as_str() {
                Some("csil") => DecimalMapping::Csil,
                Some("library") => DecimalMapping::Library,
                _ => return Err(error_codes::GENERATION_ERROR),
            },
        };

        Ok(Self {
            namespace,
            decimal_mapping,
        })
    }
}

/// The single typed entry point used by both the WASM `generate` export and the
/// integration tests. Kept `pub` so the `rlib` crate-type lets tests drive real
/// generation (a `cdylib`-only crate cannot be linked by integration tests).
pub fn render(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = CsharpConfig::from_options(&input.config.options)?;
    let warnings: Vec<GeneratorWarning> = Vec::new();
    let mut files = Vec::new();

    // The base `csharp` (and explicit `csharp-server`) target emits the server
    // surface; `csharp-client` emits the typed client. An unrecognized sub-target is
    // an error, not a silent fall-through.
    enum Surface {
        Server,
        Client,
    }
    let surface = match input.config.target.as_str() {
        "csharp" | "csharp-server" => Surface::Server,
        "csharp-client" => Surface::Client,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    if let Some(types) = generate_types(&input, &config) {
        files.push(GeneratedFile {
            path: "Types.gen.cs".to_string(),
            content: types,
        });
    }

    // The self-contained CsilDecimal record is only worth emitting under the default
    // mapping and only when the spec actually uses `decimal`; the library mapping
    // pulls the BCL `decimal` instead, so no helper is generated.
    if config.decimal_mapping == DecimalMapping::Csil && spec_uses_builtin(&input, "decimal") {
        files.push(GeneratedFile {
            path: "CsilDecimal.gen.cs".to_string(),
            content: csil_decimal_file(&config),
        });
    }

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_client(&input, &config) {
                    files.push(GeneratedFile {
                        path: "Client.gen.cs".to_string(),
                        content: client,
                    });
                }
            }
            Surface::Server => {
                if let Some(services) = generate_services(&input, &config) {
                    files.push(GeneratedFile {
                        path: "Services.gen.cs".to_string(),
                        content: services,
                    });
                }
            }
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

const FILE_HEADER: &str = "// <auto-generated>\n// Code generated by csilgen; DO NOT EDIT.\n// </auto-generated>\n#nullable enable\n";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn generate_types(input: &WasmGeneratorInput, config: &CsharpConfig) -> Option<String> {
    let mut body = String::new();
    let mut aliases: Vec<String> = Vec::new();
    let mut has_types = false;

    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                has_types = true;
                emit_record(&mut body, &rule.name, group, config);
            }
            CsilRuleType::TypeDef(type_expr) => {
                has_types = true;
                if let CsilTypeExpression::Group(group) = type_expr {
                    emit_record(&mut body, &rule.name, group, config);
                } else {
                    // A scalar/reference/collection alias. `global using` (not a plain
                    // file-scoped `using`) so the named type is visible from the service
                    // and client files too; the target is namespace-qualified because a
                    // global-using alias resolves its right side in the global namespace.
                    let target = map_csil_type_qualified(type_expr, config);
                    aliases.push(format!(
                        "global using {} = {target};",
                        pascal_ident(&rule.name)
                    ));
                }
            }
            CsilRuleType::TypeChoice(choices) => {
                has_types = true;
                emit_type_choice(&mut body, &rule.name, choices, config);
            }
            CsilRuleType::GroupChoice(choices) => {
                has_types = true;
                emit_group_choice(&mut body, &rule.name, choices, config);
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    if !has_types {
        return None;
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    // Using-alias directives must precede the file-scoped namespace.
    if !aliases.is_empty() {
        for alias in &aliases {
            content.push_str(alias);
            content.push('\n');
        }
        content.push('\n');
    }
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(&body);
    Some(content)
}

/// Emit a CSIL struct as a `public sealed record` with `required`/nullable `init`
/// properties. The CSIL field name is preserved verbatim as the CBOR wire key in a
/// comment above each property; the PascalCase property name is generator-side only.
fn emit_record(body: &mut String, name: &str, group: &CsilGroupExpression, config: &CsharpConfig) {
    let record = pascal_ident(name);
    body.push_str(&format!("public sealed record {record}\n{{\n"));

    for entry in &group.entries {
        if let Some(key) = &entry.key {
            let wire = wire_key(key);
            let prop = pascal_ident(&wire);
            let base = map_csil_type(&entry.value_type, config);
            let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));

            body.push_str(&format!("    // CBOR key: {wire}\n"));
            if optional {
                let nullable = csharp_nullable(&base);
                body.push_str(&format!("    public {nullable} {prop} {{ get; init; }}\n"));
            } else {
                body.push_str(&format!(
                    "    public required {base} {prop} {{ get; init; }}\n"
                ));
            }
        }
    }

    if group.entries.iter().any(entry_has_check) {
        body.push('\n');
        body.push_str(
            "    /// <summary>Throws System.ArgumentException when a field violates a CSIL constraint.</summary>\n",
        );
        body.push_str("    public void Validate()\n    {\n");
        for entry in &group.entries {
            if let Some(key) = &entry.key {
                let field = FieldRef {
                    prop: pascal_ident(&wire_key(key)),
                    optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
                };
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Constraint(constraint) = metadata {
                        emit_metadata_constraint(
                            body,
                            &field,
                            &entry.value_type,
                            constraint,
                            config,
                        );
                    }
                }
                if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                    for op in constraints {
                        emit_control_op_check(body, &field, &entry.value_type, op, config);
                    }
                }
            }
        }
        body.push_str("    }\n");
    }

    body.push_str("}\n\n");
}

/// A type-choice is either a closed enum (every arm a literal) or a tagged union of
/// reference/shape arms emulated as a `sealed abstract record` base plus one
/// `sealed record` per arm.
fn emit_type_choice(
    body: &mut String,
    name: &str,
    choices: &[CsilTypeExpression],
    config: &CsharpConfig,
) {
    if !choices.is_empty() && choices.iter().all(is_literal_choice) {
        emit_enum(body, name, choices);
        return;
    }

    let base = pascal_ident(name);
    body.push_str(
        "// Closed discriminated union; consume with an exhaustive `switch` expression.\n",
    );
    body.push_str(&format!("public abstract record {base};\n"));
    for (index, choice) in choices.iter().enumerate() {
        match choice {
            CsilTypeExpression::Reference(reference) => {
                let arm = pascal_ident(reference);
                let inner = pascal_ident(reference);
                // The arm wraps the referenced type; the CSIL variant wire name is the
                // reference verbatim so a decoder can map the tag back to this arm.
                body.push_str(&format!("// variant '{reference}'\n"));
                body.push_str(&format!(
                    "public sealed record {base}{arm}({inner} Value) : {base};\n"
                ));
            }
            other => {
                let arm = format!("Variant{}", index + 1);
                let inner = map_csil_type(other, config);
                body.push_str(&format!(
                    "public sealed record {base}{arm}({inner} Value) : {base};\n"
                ));
            }
        }
    }
    body.push('\n');
}

/// A group-choice is a tagged union whose arms are anonymous records; each arm
/// becomes a `sealed record` carrying that arm's fields.
fn emit_group_choice(
    body: &mut String,
    name: &str,
    choices: &[CsilGroupExpression],
    config: &CsharpConfig,
) {
    let base = pascal_ident(name);
    body.push_str(
        "// Closed discriminated union; consume with an exhaustive `switch` expression.\n",
    );
    body.push_str(&format!("public abstract record {base};\n\n"));
    for (index, choice) in choices.iter().enumerate() {
        let arm = format!("{base}Variant{}", index + 1);
        body.push_str(&format!("// variant {} of {base}\n", index + 1));
        body.push_str(&format!("public sealed record {arm} : {base}\n{{\n"));
        for entry in &choice.entries {
            if let Some(key) = &entry.key {
                let wire = wire_key(key);
                let prop = pascal_ident(&wire);
                let base_type = map_csil_type(&entry.value_type, config);
                let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
                body.push_str(&format!("    // CBOR key: {wire}\n"));
                if optional {
                    let nullable = csharp_nullable(&base_type);
                    body.push_str(&format!("    public {nullable} {prop} {{ get; init; }}\n"));
                } else {
                    body.push_str(&format!(
                        "    public required {base_type} {prop} {{ get; init; }}\n"
                    ));
                }
            }
        }
        body.push_str("}\n\n");
    }
}

fn emit_enum(body: &mut String, name: &str, choices: &[CsilTypeExpression]) {
    let enum_name = pascal_ident(name);
    body.push_str(&format!("public enum {enum_name}\n{{\n"));
    for choice in choices {
        if let CsilTypeExpression::Literal(literal) = choice {
            match literal {
                CsilLiteralValue::Text(text) => {
                    // The literal text is the wire value verbatim; the member name is a
                    // generator-side PascalCase mapping of it.
                    body.push_str(&format!("    // wire value: {text}\n"));
                    body.push_str(&format!("    {},\n", pascal_ident(text)));
                }
                CsilLiteralValue::Integer(value) => {
                    body.push_str(&format!("    Value{value} = {value},\n"));
                }
                _ => {}
            }
        }
    }
    body.push_str("}\n\n");
}

fn is_literal_choice(choice: &CsilTypeExpression) -> bool {
    matches!(
        choice,
        CsilTypeExpression::Literal(CsilLiteralValue::Text(_))
            | CsilTypeExpression::Literal(CsilLiteralValue::Integer(_))
    )
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

const CLIENT_PRELUDE: &str = "\
/// <summary>The caller-supplied transport seam. The generator never owns the wire:
/// the host encodes <c>request</c>, performs the call named by (service, op), and
/// decodes the typed response. Synchronous and blocking — no Task/async.</summary>
public interface ICsilTransport
{
    TResponse Call<TRequest, TResponse>(string service, string op, TRequest request);
}

/// <summary>Raised by a generated client when the transport reports a failure.</summary>
public sealed class CsilClientException : System.Exception
{
    public long Code { get; }

    public CsilClientException(long code, string message) : base(message)
    {
        Code = code;
    }
}
";

fn generate_client(input: &WasmGeneratorInput, config: &CsharpConfig) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_class(&mut body, &rule.name, service, config);
            emitted = true;
        }
    }

    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(CLIENT_PRELUDE);
    content.push('\n');
    content.push_str(&body);
    Some(content)
}

fn emit_client_class(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
) {
    let base = service_base(name);
    let client = format!("{base}Client");
    let wire_service = base.to_lowercase();

    body.push_str(&format!(
        "/// <summary>Typed RPC client for the {name} service.</summary>\n"
    ));
    body.push_str(&format!(
        "public sealed class {client}(ICsilTransport transport)\n{{\n"
    ));

    for operation in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops ride
        // the router/encoder surface emitted by the server target.
        if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
            body.push_str(&format!(
                "    // channel operation {} is not part of the RPC client\n",
                operation.name
            ));
            continue;
        }
        let method = pascal_ident(&operation.name);
        let output = map_csil_type(&success_type(&operation.output_type), config);
        match op_param(&operation.input_type) {
            None => {
                body.push_str(&format!(
                    "    public {output} {method}() =>\n        transport.Call<object?, {output}>(\"{wire_service}\", \"{method}\", null);\n"
                ));
            }
            Some(param) => {
                let input = map_csil_type(&operation.input_type, config);
                body.push_str(&format!(
                    "    public {output} {method}({input} {param}) =>\n        transport.Call<{input}, {output}>(\"{wire_service}\", \"{method}\", {param});\n"
                ));
            }
        }
    }

    body.push_str("}\n\n");
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

const CODEC_PRELUDE: &str = "\
/// <summary>The consumer-supplied (de)serialization layer for channel messages. The
/// generator is codec-agnostic; the implementer wires this to CBOR, JSON, or anything
/// else its protocol expects.</summary>
public interface ICsilCodec
{
    byte[] Encode(object value);
    object Decode(byte[] data, System.Type targetType);
}
";

fn generate_services(input: &WasmGeneratorInput, config: &CsharpConfig) -> Option<String> {
    let mut body = String::new();
    let needs_codec = spec_has_channel_ops(input);
    let mut emitted = false;

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_service_interface(&mut body, &rule.name, service, config);
            emit_wire_ids(&mut body, &rule.name, service);
            if service_has_channel_ops(service) {
                emit_channel_router(&mut body, &rule.name, service, config);
            }
            emitted = true;
        }
    }

    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    if needs_codec {
        content.push_str(CODEC_PRELUDE);
        content.push('\n');
    }
    content.push_str(&body);
    Some(content)
}

fn emit_service_interface(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
) {
    let iface = service_interface_name(name);
    body.push_str(&format!(
        "/// <summary>Server handler interface for the {name} service.</summary>\n"
    ));
    body.push_str(&format!("public interface {iface}\n{{\n"));

    for operation in &service.operations {
        let method = pascal_ident(&operation.name);
        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_csil_type(&success_type(&operation.output_type), config);
                match op_param(&operation.input_type) {
                    None => body.push_str(&format!("    {output} {method}();\n")),
                    Some(param) => {
                        let input = map_csil_type(&operation.input_type, config);
                        body.push_str(&format!("    {output} {method}({input} {param});\n"));
                    }
                }
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the host's plumbing pulls a frame and hands
                // it to the channel router, which decodes and dispatches here.
                let input = map_csil_type(&operation.input_type, config);
                let param =
                    op_param(&operation.input_type).unwrap_or_else(|| "message".to_string());
                body.push_str(&format!("    void {method}({input} {param});\n"));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound handler method.
            }
        }
    }

    body.push_str("}\n\n");
}

/// Emit wire-id ordinal constants exposing `@wire-id(N)` so a host references them
/// instead of hardcoding. Purely additive: emits nothing unless the service carries a
/// wire-id, keeping wire-id-free output byte-identical.
fn emit_wire_ids(body: &mut String, name: &str, service: &CsilServiceDefinition) {
    let Some(service_id) = service.wire_id else {
        return;
    };
    let base = service_base(name);
    body.push_str(&format!(
        "/// <summary>Wire-id ordinals for the {name} service (transport compact profiles).</summary>\n"
    ));
    body.push_str(&format!("public static class {base}WireIds\n{{\n"));
    body.push_str(&format!("    public const ulong Service = {service_id};\n"));
    for operation in &service.operations {
        if let Some(op_id) = operation.wire_id {
            let method = pascal_ident(&operation.name);
            body.push_str(&format!("    public const ulong {method} = {op_id};\n"));
        }
    }
    body.push_str("}\n\n");
}

/// Emit the channel router(s). The verbose router dispatches on the wire method name;
/// the compact twin dispatches on the `@wire-id` ordinal and is emitted ONLY when the
/// service carries wire-ids, so wire-id-free specs stay byte-identical. Outbound
/// encoders are emitted for every server-pushed (bidirectional/reverse) op.
fn emit_channel_router(
    body: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &CsharpConfig,
) {
    let base = service_base(name);
    let iface = service_interface_name(name);
    body.push_str(&format!(
        "/// <summary>Channel routers + outbound encoders for the {name} service.</summary>\n"
    ));
    body.push_str(&format!("public static class {base}Router\n{{\n"));

    // Verbose router: switch on the wire method name string.
    body.push_str(&format!(
        "    public static void RouteChannel({iface} handlers, ICsilCodec codec, string method, byte[] data)\n    {{\n"
    ));
    body.push_str("        switch (method)\n        {\n");
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method = pascal_ident(&operation.name);
        let input = map_csil_type(&operation.input_type, config);
        body.push_str(&format!("            case \"{method}\":\n            {{\n"));
        body.push_str(&format!(
            "                var message = ({input})codec.Decode(data, typeof({input}));\n"
        ));
        body.push_str(&format!("                handlers.{method}(message);\n"));
        body.push_str("                return;\n            }\n");
    }
    body.push_str(
        "            default:\n                throw new System.ArgumentException($\"unknown channel method '{method}'\");\n",
    );
    body.push_str("        }\n    }\n\n");

    // Compact router: emitted only for wire-id-bearing services.
    if service.wire_id.is_some() {
        body.push_str(&format!(
            "    public static void RouteChannelCompact({iface} handlers, ICsilCodec codec, ulong op, byte[] data)\n    {{\n"
        ));
        body.push_str("        switch (op)\n        {\n");
        for operation in &service.operations {
            if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let Some(op_id) = operation.wire_id else {
                continue;
            };
            let method = pascal_ident(&operation.name);
            let input = map_csil_type(&operation.input_type, config);
            body.push_str(&format!("            case {op_id}:\n            {{\n"));
            body.push_str(&format!(
                "                var message = ({input})codec.Decode(data, typeof({input}));\n"
            ));
            body.push_str(&format!("                handlers.{method}(message);\n"));
            body.push_str("                return;\n            }\n");
        }
        body.push_str(
            "            default:\n                throw new System.ArgumentException($\"unknown channel ordinal {op}\");\n",
        );
        body.push_str("        }\n    }\n\n");
    }

    // Outbound encoders for every server-pushed op (bidirectional + reverse).
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method = pascal_ident(&operation.name);
        let output = map_csil_type(&operation.output_type, config);
        body.push_str(&format!(
            "    public static (string Method, byte[] Data) Encode{method}(ICsilCodec codec, {output} message)\n    {{\n"
        ));
        body.push_str("        var data = codec.Encode(message);\n");
        body.push_str(&format!("        return (\"{method}\", data);\n"));
        body.push_str("    }\n\n");
    }

    // Each member trails a blank line as a separator; drop the last one so the class
    // closes without a stray blank line before its `}` (what dotnet format expects).
    if body.ends_with("\n\n") {
        body.pop();
    }
    body.push_str("}\n\n");
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A property's C# name plus whether it is optional (nullable). Threaded through the
/// check emitters so each can guard a null optional with `if (X is { } value)`.
struct FieldRef {
    prop: String,
    optional: bool,
}

impl FieldRef {
    /// The expression a check reads. An optional field is unwrapped to the bound
    /// non-null `value` inside its guard; a required field reads the property directly.
    fn access(&self) -> &str {
        if self.optional { "value" } else { &self.prop }
    }

    /// Wrap a check, guarding it behind a null test when the field is optional.
    fn wrap(&self, cond: &str, message: &str) -> String {
        if self.optional {
            format!(
                "        if ({prop} is {{ }} value)\n        {{\n            if ({cond})\n            {{\n                throw new System.ArgumentException(\"{message}\");\n            }}\n        }}\n",
                prop = self.prop
            )
        } else {
            format!(
                "        if ({cond})\n        {{\n            throw new System.ArgumentException(\"{message}\");\n        }}\n"
            )
        }
    }
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
    // `@default` is a construction concern, not a Validate() check; `regex` is the
    // only Custom that yields one.
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

fn emit_metadata_constraint(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
    config: &CsharpConfig,
) {
    match constraint {
        CsilValidationConstraint::MinLength(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                "<",
                *n,
                &format!("at least {n} characters"),
            );
        }
        CsilValidationConstraint::MaxLength(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                ">",
                *n,
                &format!("at most {n} characters"),
            );
        }
        CsilValidationConstraint::MinItems(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                "<",
                *n,
                &format!("at least {n} items"),
            );
        }
        CsilValidationConstraint::MaxItems(n) => {
            emit_len_check(
                body,
                field,
                value_type,
                ">",
                *n,
                &format!("at most {n} items"),
            );
        }
        CsilValidationConstraint::MinValue(v) => {
            emit_ordered_check(body, field, value_type, ("<", "at least"), v, config);
        }
        CsilValidationConstraint::MaxValue(v) => {
            emit_ordered_check(body, field, value_type, (">", "at most"), v, config);
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                emit_regex_check(body, field, pattern);
            }
        }
    }
}

fn emit_control_op_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
    config: &CsharpConfig,
) {
    match op {
        CsilControlOperator::GreaterEqual(v) => {
            emit_ordered_check(body, field, value_type, ("<", "at least"), v, config)
        }
        CsilControlOperator::LessEqual(v) => {
            emit_ordered_check(body, field, value_type, (">", "at most"), v, config)
        }
        CsilControlOperator::GreaterThan(v) => {
            emit_ordered_check(body, field, value_type, ("<=", "greater than"), v, config)
        }
        CsilControlOperator::LessThan(v) => {
            emit_ordered_check(body, field, value_type, (">=", "less than"), v, config)
        }
        CsilControlOperator::Equal(v) => {
            emit_ordered_check(body, field, value_type, ("!=", "equal to"), v, config)
        }
        CsilControlOperator::NotEqual(v) => {
            emit_ordered_check(body, field, value_type, ("==", "not equal to"), v, config)
        }
        CsilControlOperator::Size(size) => emit_size_check(body, field, value_type, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(body, field, pattern),
        // Applied at construction / (de)serialization, not validated here.
        CsilControlOperator::Default(_)
        | CsilControlOperator::Bits(_)
        | CsilControlOperator::And(_)
        | CsilControlOperator::Within(_)
        | CsilControlOperator::Json
        | CsilControlOperator::Cbor
        | CsilControlOperator::Cborseq => {}
    }
}

fn emit_len_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    op: &str,
    n: u64,
    tail: &str,
) {
    let accessor = len_accessor(value_type);
    let cond = format!("{}.{accessor} {op} {n}", field.access());
    let message = csharp_escape(&format!("field '{}' must have {tail}", field.prop));
    body.push_str(&field.wrap(&cond, &message));
}

fn emit_size_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    size: &CsilSizeConstraint,
) {
    match size {
        CsilSizeConstraint::Exact(n) => emit_len_check(
            body,
            field,
            value_type,
            "!=",
            *n,
            &format!("exactly {n} elements"),
        ),
        CsilSizeConstraint::Min(n) => emit_len_check(
            body,
            field,
            value_type,
            "<",
            *n,
            &format!("at least {n} elements"),
        ),
        CsilSizeConstraint::Max(n) => emit_len_check(
            body,
            field,
            value_type,
            ">",
            *n,
            &format!("at most {n} elements"),
        ),
        CsilSizeConstraint::Range { min, max } => {
            emit_len_check(
                body,
                field,
                value_type,
                "<",
                *min,
                &format!("at least {min} elements"),
            );
            emit_len_check(
                body,
                field,
                value_type,
                ">",
                *max,
                &format!("at most {max} elements"),
            );
        }
    }
}

fn emit_regex_check(body: &mut String, field: &FieldRef, pattern: &str) {
    let cond = format!(
        "!System.Text.RegularExpressions.Regex.IsMatch({}, \"{}\")",
        field.access(),
        csharp_escape(pattern)
    );
    let message = csharp_escape(&format!(
        "field '{}' must match pattern '{}'",
        field.prop, pattern
    ));
    body.push_str(&field.wrap(&cond, &message));
}

/// One ordered comparison honoring the field's type. `vop` is the C# operator whose
/// truth means the constraint is violated; numeric/timestamp fields compare with
/// operators, a CsilDecimal compares through `CompareTo` so the emitted C# is valid.
fn emit_ordered_check(
    body: &mut String,
    field: &FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
    config: &CsharpConfig,
) {
    let (vop, desc) = op;
    let access = field.access();
    match ordered_kind(value_type, config) {
        OrderedKind::Numeric => {
            let Some(rendered) = literal_as_number(value) else {
                return;
            };
            let cond = format!("{access} {vop} {rendered}");
            let message =
                csharp_escape(&format!("field '{}' must be {desc} {rendered}", field.prop));
            body.push_str(&field.wrap(&cond, &message));
        }
        OrderedKind::LibraryDecimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let bound = format!(
                "decimal.Parse(\"{}\", System.Globalization.CultureInfo.InvariantCulture)",
                csharp_escape(&text)
            );
            let cond = format!("{access} {vop} {bound}");
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.wrap(&cond, &message));
        }
        OrderedKind::CsilDecimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let cond = format!(
                "{access}.CompareTo(CsilDecimal.Parse(\"{}\")) {vop} 0",
                csharp_escape(&text)
            );
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.wrap(&cond, &message));
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            let bound = format!("System.DateTimeOffset.Parse(\"{}\")", csharp_escape(&text));
            let cond = format!("{access} {vop} {bound}");
            let message = csharp_escape(&format!("field '{}' must be {desc} {text}", field.prop));
            body.push_str(&field.wrap(&cond, &message));
        }
    }
}

enum OrderedKind {
    Numeric,
    CsilDecimal,
    LibraryDecimal,
    Timestamp,
}

fn ordered_kind(value_type: &CsilTypeExpression, config: &CsharpConfig) -> OrderedKind {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    if let CsilTypeExpression::Builtin(name) = base {
        match name.as_str() {
            "decimal" => match config.decimal_mapping {
                DecimalMapping::Csil => OrderedKind::CsilDecimal,
                DecimalMapping::Library => OrderedKind::LibraryDecimal,
            },
            "timestamp" => OrderedKind::Timestamp,
            _ => OrderedKind::Numeric,
        }
    } else {
        OrderedKind::Numeric
    }
}

/// `.Length` for strings/byte arrays, `.Count` for collections; defaults to `.Length`.
fn len_accessor(value_type: &CsilTypeExpression) -> &'static str {
    let base = match value_type {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    match base {
        CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. } => "Count",
        CsilTypeExpression::Builtin(name) if name == "bytes" || name == "bstr" => "Length",
        _ => "Length",
    }
}

fn literal_as_number(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn literal_as_decimal_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn literal_as_timestamp_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Type mapping & helpers
// ---------------------------------------------------------------------------

/// Map a CSIL type expression to a non-nullable C# type string. Optionality is
/// applied by the caller (a record property appends `?`), so this never embeds `?`.
fn map_csil_type(type_expr: &CsilTypeExpression, config: &CsharpConfig) -> String {
    map_csil_type_inner(type_expr, config, false)
}

/// Same mapping, but generator-emitted type names (records, the `CsilDecimal` helper)
/// are prefixed with the configured namespace. A `global using` alias resolves its
/// right-hand side in the *global* namespace, so the target must be fully qualified or
/// the alias fails to find a type that lives inside `namespace Csilgen.Transport;`.
fn map_csil_type_qualified(type_expr: &CsilTypeExpression, config: &CsharpConfig) -> String {
    map_csil_type_inner(type_expr, config, true)
}

fn map_csil_type_inner(
    type_expr: &CsilTypeExpression,
    config: &CsharpConfig,
    qualify: bool,
) -> String {
    let prefix = if qualify {
        format!("{}.", config.namespace)
    } else {
        String::new()
    };
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "long".to_string(),
            "uint" => "ulong".to_string(),
            "float" => "double".to_string(),
            // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`.
            "text" | "tstr" => "string".to_string(),
            "bytes" | "bstr" => "byte[]".to_string(),
            "bool" => "bool".to_string(),
            // CBOR tag 0, RFC3339, always UTC per the wire contract.
            "timestamp" => "System.DateTimeOffset".to_string(),
            // CBOR tag 4 exact decimal; concrete C# type depends on decimal_mapping. Only
            // the generated `CsilDecimal` lives in our namespace and needs qualifying.
            "decimal" => match config.decimal_mapping {
                DecimalMapping::Csil => format!("{prefix}CsilDecimal"),
                DecimalMapping::Library => "decimal".to_string(),
            },
            // CDDL's open `any`/`nil`/`null` are the untyped CBOR item — `object?` in C#.
            "any" | "nil" | "null" => "object?".to_string(),
            other => format!("{prefix}{}", pascal_ident(other)),
        },
        CsilTypeExpression::Reference(name) => format!("{prefix}{}", pascal_ident(name)),
        CsilTypeExpression::Array { element_type, .. } => {
            format!(
                "System.Collections.Generic.List<{}>",
                map_csil_type_inner(element_type, config, qualify)
            )
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "System.Collections.Generic.Dictionary<{}, {}>",
            map_csil_type_inner(key, config, qualify),
            map_csil_type_inner(value, config, qualify)
        ),
        // C# value tuple preserves per-position types where Go would use a struct.
        CsilTypeExpression::Tuple(group) => csharp_tuple(&group.entries, config, qualify),
        CsilTypeExpression::Constrained { base_type, .. } => {
            map_csil_type_inner(base_type, config, qualify)
        }
        _ => "object".to_string(),
    }
}

/// Append C#'s nullable marker without doubling it on an already-nullable type.
fn csharp_nullable(base: &str) -> String {
    if base.ends_with('?') {
        base.to_string()
    } else {
        format!("{base}?")
    }
}

fn csharp_tuple(entries: &[CsilGroupEntry], config: &CsharpConfig, qualify: bool) -> String {
    let fields: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let field_type = map_csil_type_inner(&entry.value_type, config, qualify);
            let field_name = match &entry.key {
                Some(key) => pascal_ident(&wire_key(key)),
                None => format!("Field{index}"),
            };
            format!("{field_type} {field_name}")
        })
        .collect();
    format!("({})", fields.join(", "))
}

/// The wire (CBOR map key) string for a group key — the CSIL name verbatim.
fn wire_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` arm of a `Res / ServiceError` union — that error half surfaces as a
/// thrown exception, not part of the typed response.
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

/// A push op (`-> Event`) carries a `null` input type: on a unary RPC there is no
/// request to send, so the request parameter is dropped.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// The camelCase parameter name for an operation's request, or `None` when the op
/// takes no input. A reference input names the parameter after its type (camelCased,
/// keyword-escaped — e.g. input `Event` yields `@event`); anything else is `request`.
fn op_param(type_expr: &CsilTypeExpression) -> Option<String> {
    if op_input_is_null(type_expr) {
        return None;
    }
    match type_expr {
        CsilTypeExpression::Reference(name) => Some(camel_ident(name)),
        _ => Some("request".to_string()),
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

fn spec_uses_builtin(input: &WasmGeneratorInput, builtin: &str) -> bool {
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group) => group
                .entries
                .iter()
                .any(|e| type_uses_builtin(&e.value_type, builtin)),
            CsilRuleType::TypeDef(type_expr) => type_uses_builtin(type_expr, builtin),
            CsilRuleType::TypeChoice(choices) => {
                choices.iter().any(|c| type_uses_builtin(c, builtin))
            }
            CsilRuleType::GroupChoice(choices) => choices.iter().any(|g| {
                g.entries
                    .iter()
                    .any(|e| type_uses_builtin(&e.value_type, builtin))
            }),
            CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
                type_uses_builtin(&op.input_type, builtin)
                    || type_uses_builtin(&op.output_type, builtin)
            }),
        })
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

/// `FooService` keeps its `Service` suffix on the interface (`IFooService`); a bare
/// `Attestation` gains one (`IAttestationService`).
fn service_interface_name(name: &str) -> String {
    format!("I{}Service", service_base(name))
}

/// The service base used for the client class and wire-id prefix: the PascalCased
/// name with any trailing `Service` removed.
fn service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

// ---------------------------------------------------------------------------
// Identifier casing & keyword escaping
// ---------------------------------------------------------------------------

/// PascalCase the identifier, then `@`-escape it if it collides with a C# keyword.
fn pascal_ident(s: &str) -> String {
    escape_keyword(&pascal_case(s))
}

/// camelCase the identifier, then `@`-escape it. This is where CSIL `event` (or a
/// reference type `Event` used as a parameter) becomes the escaped `@event`, since
/// C# keywords are lowercase and only surface a collision in camelCase contexts.
fn camel_ident(s: &str) -> String {
    escape_keyword(&camel_case(s))
}

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

fn camel_case(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
    }
}

/// `@`-escape an identifier that collides with a C# reserved keyword so it stays a
/// legal identifier (e.g. `event` -> `@event`).
fn escape_keyword(ident: &str) -> String {
    if is_csharp_keyword(ident) {
        format!("@{ident}")
    } else {
        ident.to_string()
    }
}

fn is_csharp_keyword(ident: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "abstract",
        "as",
        "base",
        "bool",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "checked",
        "class",
        "const",
        "continue",
        "decimal",
        "default",
        "delegate",
        "do",
        "double",
        "else",
        "enum",
        "event",
        "explicit",
        "extern",
        "false",
        "finally",
        "fixed",
        "float",
        "for",
        "foreach",
        "goto",
        "if",
        "implicit",
        "in",
        "int",
        "interface",
        "internal",
        "is",
        "lock",
        "long",
        "namespace",
        "new",
        "null",
        "object",
        "operator",
        "out",
        "override",
        "params",
        "private",
        "protected",
        "public",
        "readonly",
        "ref",
        "return",
        "sbyte",
        "sealed",
        "short",
        "sizeof",
        "stackalloc",
        "static",
        "string",
        "struct",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "uint",
        "ulong",
        "unchecked",
        "unsafe",
        "ushort",
        "using",
        "virtual",
        "void",
        "volatile",
        "while",
    ];
    KEYWORDS.contains(&ident)
}

/// Escape a string for safe inclusion inside a C# double-quoted (non-verbatim)
/// literal so an embedded quote/backslash/newline can never break the literal.
fn csharp_escape(s: &str) -> String {
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

// ---------------------------------------------------------------------------
// CsilDecimal helper file
// ---------------------------------------------------------------------------

const CSIL_DECIMAL_BODY: &str = r#"/// <summary>The exact, base-10 `decimal` core type. On the wire it is CBOR tag 4
/// (decimal fraction): a two-element array [exponent, mantissa] whose value is
/// Mantissa * 10^Exponent. The value is kept as exact integers, never a float, so no
/// precision is lost. The BCL `System.Formats.Cbor` package is deliberately not taken
/// (it is an out-of-band NuGet dependency); the transport library hand-rolls the codec.</summary>
public sealed record CsilDecimal(long Exponent, System.Numerics.BigInteger Mantissa)
    : System.IComparable<CsilDecimal>
{
    /// <summary>Parse canonical decimal text (what ToString emits) into an exact value.</summary>
    public static CsilDecimal Parse(string text)
    {
        text = text.Trim();
        bool negative = false;
        if (text.StartsWith('-'))
        {
            negative = true;
            text = text[1..];
        }
        else if (text.StartsWith('+'))
        {
            text = text[1..];
        }

        string intPart = text;
        string fracPart = "";
        int dot = text.IndexOf('.');
        if (dot >= 0)
        {
            intPart = text[..dot];
            fracPart = text[(dot + 1)..];
        }

        string digits = intPart + fracPart;
        if (digits.Length == 0)
        {
            digits = "0";
        }

        var mantissa = System.Numerics.BigInteger.Parse(digits);
        if (negative)
        {
            mantissa = -mantissa;
        }
        return new CsilDecimal(-fracPart.Length, mantissa);
    }

    /// <summary>Exact ordering: both values are scaled to a common exponent and their
    /// integer mantissas compared, so no float rounding can flip the result.</summary>
    public int CompareTo(CsilDecimal? other)
    {
        if (other is null)
        {
            return 1;
        }

        System.Numerics.BigInteger left = Mantissa;
        System.Numerics.BigInteger right = other.Mantissa;
        if (Exponent > other.Exponent)
        {
            left *= System.Numerics.BigInteger.Pow(10, (int)(Exponent - other.Exponent));
        }
        else if (other.Exponent > Exponent)
        {
            right *= System.Numerics.BigInteger.Pow(10, (int)(other.Exponent - Exponent));
        }
        return left.CompareTo(right);
    }
}
"#;

fn csil_decimal_file(config: &CsharpConfig) -> String {
    let mut content = String::new();
    content.push_str(FILE_HEADER);
    content.push('\n');
    content.push_str(&format!("namespace {};\n\n", config.namespace));
    content.push_str(CSIL_DECIMAL_BODY);
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CsharpConfig {
        CsharpConfig {
            namespace: "Csilgen.Transport".to_string(),
            decimal_mapping: DecimalMapping::Csil,
        }
    }

    #[test]
    fn pascal_and_camel_casing() {
        assert_eq!(pascal_case("subject_id"), "SubjectId");
        assert_eq!(pascal_case("deposit-claim"), "DepositClaim");
        assert_eq!(camel_case("DepositRequest"), "depositRequest");
        assert_eq!(camel_case("Event"), "event");
    }

    #[test]
    fn keyword_escaping() {
        // camelCase is where lowercase C# keywords surface a collision.
        assert_eq!(camel_ident("Event"), "@event");
        assert_eq!(camel_ident("Int"), "@int");
        assert_eq!(escape_keyword("class"), "@class");
        assert_eq!(escape_keyword("object"), "@object");
        assert_eq!(escape_keyword("params"), "@params");
        // PascalCase rarely collides; `Event` is not itself a keyword.
        assert_eq!(pascal_ident("event"), "Event");
        assert_eq!(pascal_ident("subject_id"), "SubjectId");
    }

    #[test]
    fn type_mapping_core() {
        let c = config();
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("int".to_string()), &c),
            "long"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("text".to_string()), &c),
            "string"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("tstr".to_string()), &c),
            "string"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("bytes".to_string()), &c),
            "byte[]"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("timestamp".to_string()), &c),
            "System.DateTimeOffset"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("decimal".to_string()), &c),
            "CsilDecimal"
        );
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Reference("User".to_string()), &c),
            "User"
        );
    }

    #[test]
    fn any_maps_to_object() {
        // CDDL `any` is the open CBOR item; C# has no `Any` type, so it must be `object?`.
        let c = config();
        assert_eq!(
            map_csil_type(&CsilTypeExpression::Builtin("any".to_string()), &c),
            "object?"
        );
    }

    #[test]
    fn alias_targets_are_namespace_qualified() {
        // A `global using` resolves its right side in the global namespace, so generated
        // types (records, the CsilDecimal helper) must carry the namespace prefix.
        let c = config();
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Builtin("decimal".to_string()), &c),
            "Csilgen.Transport.CsilDecimal"
        );
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Reference("User".to_string()), &c),
            "Csilgen.Transport.User"
        );
        // Predefined/BCL targets stay unqualified even when qualifying.
        assert_eq!(
            map_csil_type_qualified(&CsilTypeExpression::Builtin("int".to_string()), &c),
            "long"
        );
    }

    #[test]
    fn service_naming() {
        assert_eq!(service_interface_name("FooService"), "IFooService");
        assert_eq!(service_interface_name("Attestation"), "IAttestationService");
        assert_eq!(service_base("CorndogsService"), "Corndogs");
        assert_eq!(service_base("Attestation"), "Attestation");
    }
}
