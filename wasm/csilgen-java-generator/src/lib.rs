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
    /// When set, the output directory is laid out as a publishable Maven project:
    /// a `pom.xml` at the root and sources under `src/main/java/<package path>/`.
    /// Triggered by `emit_packages` containing `"java"`; otherwise the flat default
    /// layout is unchanged.
    package_mode: bool,
    group_id: String,
    artifact_id: String,
    version: String,
}

impl JavaConfig {
    fn from_input(input: &WasmGeneratorInput) -> Result<Self, i32> {
        let opt = |key: &str| input.config.options.get(key).and_then(|v| v.as_str());

        let package = opt("java_package")
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

        // Maven coordinates: groupId is the reversed-domain package itself; artifactId is
        // the explicit `package_name` or a kebab of the package's last segment; version
        // defaults to the conventional first release.
        let group_id = package.clone();
        let artifact_id = opt("package_name")
            .map(str::to_string)
            .unwrap_or_else(|| derive_artifact_id(&package));
        let version = opt("package_version").unwrap_or("0.1.0").to_string();

        Ok(Self {
            package,
            surface,
            package_mode: wants_java_package(input),
            group_id,
            artifact_id,
            version,
        })
    }

    /// The relative file path for a top-level public class. In package mode the file
    /// lands under Maven's standard `src/main/java/<package path>/` source root; the
    /// default flat layout keeps the class directly under the package dir.
    fn path_for(&self, class: &str) -> String {
        let pkg = self.package.replace('.', "/");
        if self.package_mode {
            format!("src/main/java/{pkg}/{class}.java")
        } else {
            format!("{pkg}/{class}.java")
        }
    }

    /// The file preamble: the generated-code marker plus the package statement.
    fn header(&self) -> String {
        let pkg = &self.package;
        format!("// Code generated by csilgen; DO NOT EDIT.\n\npackage {pkg};\n\n")
    }
}

/// Whether the caller asked for a self-contained publishable Java package. The trigger
/// is the `emit_packages` option containing `"java"`. Parsed defensively because the
/// option can reach us in several shapes (see `emit_targets`).
fn wants_java_package(input: &WasmGeneratorInput) -> bool {
    input
        .config
        .options
        .get("emit_packages")
        .map(emit_targets)
        .is_some_and(|targets| targets.iter().any(|t| t == "java"))
}

/// Reduce an `emit_packages` option value to the set of target names it names. The value
/// is meant to be a JSON array of strings, but a host may instead hand us the array as a
/// JSON-encoded string, or a bare/comma-separated string; each shape collapses to the
/// same name list rather than being rejected.
fn emit_targets(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(s) => {
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                // Not a JSON array: treat it as a plain (possibly comma-separated) list.
                _ => s
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect(),
            }
        }
        _ => Vec::new(),
    }
}

/// Derive a Maven artifactId from a reversed-domain package when none is given: the
/// package's last segment, kebab-cased to the conventional artifactId style.
fn derive_artifact_id(package: &str) -> String {
    let last = package.rsplit('.').next().unwrap_or(package);
    let kebab = last.to_case(Case::Kebab);
    if kebab.is_empty() {
        "generated".to_string()
    } else {
        kebab
    }
}

/// Escape the five XML metacharacters so a coordinate carrying one stays well-formed in
/// the emitted `pom.xml`.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Build the `pom.xml` for package mode: a minimal, dependency-free Maven project pinned
/// to Java 17 via `maven.compiler.release`, with the resolved coordinates. The sources
/// already sit under Maven's standard `src/main/java` layout, so no build-helper plugin
/// is needed for Maven to find them.
fn generate_pom(config: &JavaConfig) -> GeneratedFile {
    let group = xml_escape(&config.group_id);
    let artifact = xml_escape(&config.artifact_id);
    let version = xml_escape(&config.version);
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!-- Generated by csilgen; DO NOT EDIT. -->\n\
         <project xmlns=\"http://maven.apache.org/POM/4.0.0\"\n\
         \x20        xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n\
         \x20        xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 http://maven.apache.org/xsd/maven-4.0.0.xsd\">\n\
         \x20   <modelVersion>4.0.0</modelVersion>\n\
         \n\
         \x20   <groupId>{group}</groupId>\n\
         \x20   <artifactId>{artifact}</artifactId>\n\
         \x20   <version>{version}</version>\n\
         \x20   <packaging>jar</packaging>\n\
         \n\
         \x20   <properties>\n\
         \x20       <maven.compiler.release>17</maven.compiler.release>\n\
         \x20       <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n\
         \x20   </properties>\n\
         </project>\n"
    );
    GeneratedFile {
        path: "pom.xml".to_string(),
        content,
    }
}

/// The package `README.md` with a copy-paste **Quickstart**. For a client package the
/// Quickstart is a complete, dependency-free CSIL-RPC carrier — it reuses this package's
/// own generated `CsilCbor` codec to build/parse the envelope, so it adds no third-party
/// dependency (the hybrid posture's path 1) — the typed client constructed over it, and
/// one example call. A serviceless / types-only package gets a shorter consume-the-types
/// section without a carrier.
fn generate_readme(input: &WasmGeneratorInput, config: &JavaConfig) -> GeneratedFile {
    let artifact = &config.artifact_id;
    let mut out = format!(
        "# {artifact}\n\n\
         Generated by csilgen. A typed, transport-agnostic CSIL-RPC client: the generated\n\
         codec owns CBOR (de)serialization; you supply a *carrier* that only moves bytes.\n\n\
         ## Install\n\n\
         This package builds to a standard Maven artifact. Install it to your local\n\
         repository with `mvn install` — TODO: publish it to a shared repository — then\n\
         depend on it:\n\n\
         ```xml\n\
         <dependency>\n\
         \x20   <groupId>{}</groupId>\n\
         \x20   <artifactId>{artifact}</artifactId>\n\
         \x20   <version>{}</version>\n\
         </dependency>\n\
         ```\n\n",
        xml_escape(&config.group_id),
        xml_escape(&config.version),
    );

    // The carrier+example only makes sense for the client surface, whose `Transport`
    // seam and per-service client the snippet wires together; a server / types-only
    // package has no such classes, so it gets the consume-the-types section.
    let example = match config.surface {
        Surface::Client => first_unary_example(input),
        _ => None,
    };
    match example {
        Some(example) => out.push_str(&readme_quickstart(config, &example)),
        None => out.push_str(&format!(
            "## Quickstart\n\n\
             This package has no service operations — import its generated record types and\n\
             the `CsilCbor` codec directly:\n\n\
             ```java\n\
             import {0}.*;\n\n\
             // byte[] bytes = CsilCbor.encodeYourRecord(value);\n\
             // YourRecord back = CsilCbor.decodeYourRecord(bytes);\n\
             ```\n",
            config.package
        )),
    }

    GeneratedFile {
        path: "README.md".to_string(),
        content: out,
    }
}

/// The client Quickstart: the dependency-free blocking CSIL-RPC carrier over
/// `java.net.http.HttpClient`, the typed client constructed on it, and the first
/// service's first unary call with a generated sample request literal.
fn readme_quickstart(config: &JavaConfig, ex: &UnaryExample) -> String {
    let mut out = String::from("## Quickstart\n\n");
    out.push_str(
        "A complete CSIL-RPC carrier (no third-party deps — it reuses this package's\n\
         generated `CsilCbor` codec for the envelope) plus the typed client. Change the one\n\
         base-URL string.\n\n",
    );
    out.push_str("```java\n");
    out.push_str(&format!("package {};\n\n", config.package));
    out.push_str(
        "import java.net.URI;\n\
         import java.net.http.HttpClient;\n\
         import java.net.http.HttpRequest;\n\
         import java.net.http.HttpResponse;\n\
         import java.util.List;\n\n",
    );
    out.push_str(CARRIER_JAVA);
    out.push('\n');
    out.push_str("public final class Example {\n");
    out.push_str("    public static void main(String[] args) {\n");
    out.push_str(&format!(
        "        {0} client = new {0}(new CsilRpcTransport(\"http://localhost:5080\"));\n",
        ex.client_class
    ));
    if ex.has_request {
        out.push_str(&format!(
            "        {} resp = client.{}({});\n",
            ex.response_class, ex.method, ex.sample
        ));
    } else {
        out.push_str(&format!(
            "        {} resp = client.{}();\n",
            ex.response_class, ex.method
        ));
    }
    out.push_str("        System.out.println(resp);\n");
    out.push_str("    }\n}\n");
    out.push_str("```\n");
    out
}

/// The CSIL-RPC carrier body, identical for every spec, so it is a constant. It builds a
/// `CsilRpcRequest` envelope (tag-24 payload) from the generated `CsilCbor` value model,
/// POSTs it to `{baseUrl}/csil/v1/rpc` with the JDK's blocking `HttpClient`, and returns
/// the unwrapped response payload bytes for the generated client to decode. A non-zero
/// transport `status` or a typed `ServiceError` arm becomes a `ClientException`.
const CARRIER_JAVA: &str = r#"// The carrier owns only the CSIL-RPC envelope + HTTP; it never touches your types.
// Hybrid posture path 1: it reuses the generated CsilCbor value model to build/parse
// the envelope, so it adds no third-party dependency.
final class CsilRpcTransport implements Transport {
    private final HttpClient http = HttpClient.newHttpClient();
    private final String baseUrl;

    CsilRpcTransport(String baseUrl) {
        // Trim any trailing slash so the joined path is exactly one "/csil/v1/rpc".
        this.baseUrl = baseUrl.replaceAll("/+$", "");
    }

    @Override
    public byte[] call(String service, String op, byte[] req) throws ClientException {
        // CsilRpcRequest = { v, service, op, payload: #6.24(bstr) }. The payload is the
        // already-encoded request wrapped in CBOR tag 24 (embedded CBOR); keys are laid
        // down in canonical (length-then-bytewise) order.
        CsilCbor.CborValue envelope = new CsilCbor.CborMap(List.of(
            new CsilCbor.CborEntry(new CsilCbor.CborText("v"), new CsilCbor.CborUint(1)),
            new CsilCbor.CborEntry(new CsilCbor.CborText("op"), new CsilCbor.CborText(op)),
            new CsilCbor.CborEntry(new CsilCbor.CborText("service"), new CsilCbor.CborText(service)),
            new CsilCbor.CborEntry(new CsilCbor.CborText("payload"),
                new CsilCbor.CborTag(24, new CsilCbor.CborBytes(req == null ? new byte[0] : req)))));

        HttpResponse<byte[]> http_resp;
        try {
            HttpRequest http_req = HttpRequest.newBuilder()
                .uri(URI.create(baseUrl + "/csil/v1/rpc"))
                .header("Content-Type", "application/cbor")
                .header("Accept", "application/cbor")
                .POST(HttpRequest.BodyPublishers.ofByteArray(CsilCbor.encode(envelope)))
                .build();
            http_resp = http.send(http_req, HttpResponse.BodyHandlers.ofByteArray());
        } catch (java.io.IOException e) {
            throw new ClientException("csil-rpc " + service + "/" + op + ": " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new ClientException("csil-rpc " + service + "/" + op + ": interrupted", e);
        }
        if (http_resp.statusCode() != 200) {
            throw new ClientException(
                "csil-rpc " + service + "/" + op + ": http " + http_resp.statusCode());
        }

        CsilCbor.CborValue env = CsilCbor.decode(http_resp.body());
        long status = CsilCbor.asI64(CsilCbor.require(env, "status"));
        if (status != 0) {
            throw new ClientException(
                "csil-rpc " + service + "/" + op + ": transport status " + status);
        }

        CsilCbor.CborValue payload = CsilCbor.require(env, "payload");
        if (!(payload instanceof CsilCbor.CborTag tag) || tag.num() != 24
            || !(tag.inner() instanceof CsilCbor.CborBytes inner)) {
            throw new ClientException("csil-rpc: response payload is not a tag-24 byte string");
        }

        // A typed ServiceError arm (variant "ServiceError") is an application error,
        // distinct from a transport failure: decode { code, message } and surface it.
        CsilCbor.CborValue variant = CsilCbor.mapGet(env, "variant");
        if (variant instanceof CsilCbor.CborText v && v.value().equals("ServiceError")) {
            CsilCbor.CborValue e = CsilCbor.decode(inner.value());
            throw new ClientException("service error "
                + CsilCbor.asI64(CsilCbor.require(e, "code")) + ": "
                + CsilCbor.asText(CsilCbor.require(e, "message")));
        }
        return inner.value();
    }
}
"#;

/// The pieces the README's example call needs: which client class + method to call, the
/// typed response class to print, and a compiling sample request literal (empty when the
/// op takes no request).
struct UnaryExample {
    client_class: String,
    method: String,
    response_class: String,
    has_request: bool,
    sample: String,
}

/// The first service (in rule order, matching the emitted client) that has a unary `->`
/// operation the typed client actually exposes — success and request both records (or a
/// null request) — reduced to an example call. `None` for a serviceless package.
fn first_unary_example(input: &WasmGeneratorInput) -> Option<UnaryExample> {
    let records = record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(def) = &rule.rule_type else {
            continue;
        };
        for op in &def.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let null_input = is_null_input(&op.input_type);
            if !is_record_ref(&success, &records)
                || !(null_input || is_record_ref(&op.input_type, &records))
            {
                continue;
            }
            return Some(UnaryExample {
                client_class: format!("{}Client", service_base(&rule.name)),
                method: op.name.to_case(Case::Camel),
                response_class: record_ref_class(&success),
                has_request: !null_input,
                sample: if null_input {
                    String::new()
                } else {
                    java_sample(input, &op.input_type)
                },
            });
        }
    }
    None
}

/// A compiling Java expression producing a sample value of `ty` for the README example.
/// Records recurse into their canonical constructor; scalars get a representative
/// literal; maps/lists use the empty target-typed factories; shapes a generic sample
/// can't fabricate fall back to `null`, which a reference-typed component accepts.
fn java_sample(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "new byte[0]".to_string(),
            "int" | "uint" => "0L".to_string(),
            "float" => "0.0".to_string(),
            "timestamp" => "java.time.Instant.now()".to_string(),
            "decimal" => "java.math.BigDecimal.ZERO".to_string(),
            _ => "null".to_string(),
        },
        CsilTypeExpression::Array { .. } => "java.util.List.of()".to_string(),
        CsilTypeExpression::Map { .. } => "java.util.Map.of()".to_string(),
        CsilTypeExpression::Constrained { base_type, .. } => java_sample(input, base_type),
        CsilTypeExpression::Reference(name) => match find_record(input, name) {
            Some(group) => record_literal(input, name, group),
            None => match find_alias(input, name) {
                // A transparent alias is a one-component wrapper record over its target.
                Some(underlying) => format!(
                    "new {}({})",
                    name.to_case(Case::Pascal),
                    java_sample(input, &underlying)
                ),
                None => "null".to_string(),
            },
        },
        _ => "null".to_string(),
    }
}

/// `new Class(arg, ...)` over a record's canonical constructor: every named component in
/// declared order, optional components passed as `null`, required ones a typed sample.
fn record_literal(input: &WasmGeneratorInput, name: &str, group: &CsilGroupExpression) -> String {
    let args: Vec<String> = group
        .entries
        .iter()
        .filter(|e| entry_field_name(e).is_some())
        .map(|e| {
            if matches!(e.occurrence, Some(CsilOccurrence::Optional)) {
                "null".to_string()
            } else {
                java_sample(input, &e.value_type)
            }
        })
        .collect();
    format!("new {}({})", name.to_case(Case::Pascal), args.join(", "))
}

/// The record group a reference names, if any: a `Name = { ... }` rule (`TypeDef(Group)`)
/// or a bare group rule (`GroupDef`).
fn find_record<'a>(input: &'a WasmGeneratorInput, name: &str) -> Option<&'a CsilGroupExpression> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        }
    })
}

/// The underlying type of a transparent alias a reference names (its wrapper record's one
/// component), or `None` when the name is not such an alias.
fn find_alias(input: &WasmGeneratorInput, name: &str) -> Option<CsilTypeExpression> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some(other.clone()),
            },
            _ => None,
        }
    })
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

    // The self-contained per-record CBOR codec is emitted on every surface whenever the
    // spec has record types: that codec is what every payload (de)serializes through now
    // (the typed client owns the wire; no reflection path remains).
    if let Some(codec) = generate_codec(input, &config) {
        files.push(codec);
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
                let records = record_names(input);
                files.push(generate_transport_iface(&config));
                files.push(generate_client_error(&config));
                for (name, def, doc) in &services {
                    files.push(generate_client(&config, name, def, doc, &records));
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
    let mut files: Vec<GeneratedFile> = files
        .into_iter()
        .map(|mut f| {
            f.content = finalize_file(&f.content);
            f
        })
        .collect();

    // In package mode the output directory is a publishable Maven project: the sources are
    // already laid under `src/main/java/...` by `path_for`, so the only addition is the
    // build descriptor. Emitted after import-hoisting since the pom is not Java source.
    if config.package_mode {
        files.push(generate_pom(&config));
        // Only an explicit `emit_readme: false` suppresses the README; absent or non-bool
        // leaves the publishable package's Quickstart in place.
        if input
            .config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            files.push(generate_readme(input, &config));
        }
    }

    Ok(files)
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
    "java.io.ByteArrayOutputStream",
    "java.math.BigDecimal",
    "java.math.BigInteger",
    "java.nio.charset.StandardCharsets",
    "java.time.Instant",
    "java.util.ArrayList",
    "java.util.Arrays",
    "java.util.LinkedHashMap",
    "java.util.List",
    "java.util.Map",
    "java.util.Objects",
    "java.util.function.Function",
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
// Per-type CBOR codec (CsilCbor.java)
//
// CSIL is the CBOR Service Interface Language; the canonical wire is a CBOR map
// keyed by the CSIL field name verbatim. Java has no derive/reflection CBOR codec
// in its stdlib, and the transport lib's value model is package-private, so the
// generator emits a self-contained per-record codec (the same shape the C/Zig/
// OCaml/Dart/Swift/Go targets emit) so the bytes are owned by generated code and
// agree byte-for-byte across every language.
// ---------------------------------------------------------------------------

/// The PascalCase names of every record type in the spec (a `GroupDef`, or a
/// `TypeDef` wrapping a `Group`). Only records get a codec, so a `Reference` to one
/// of these is what a field/operation payload (de)serializes through.
fn record_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(_) => Some(rule.name.to_case(Case::Pascal)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => {
                Some(rule.name.to_case(Case::Pascal))
            }
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through, keyed by PascalCase name:
/// a `TypeDef` whose target is a map / array / scalar / reference / tuple (NOT a record
/// group or a choice, which generate their own classes and have their own handling).
///
/// Java represents such an alias as a wrapper record (`record StringInt64Map(Map<...>
/// value) {}`), so a field typed as the alias holds the wrapper, not the underlying
/// value. The codec therefore unwraps `.value()` on encode and re-wraps on decode
/// rather than emitting the `CborNull`/`null` stub a bare non-record reference yields.
fn codec_aliases(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((rule.name.to_case(Case::Pascal), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Whether a type is a reference to a record the codec can (de)serialize, so a typed
/// client method can call the generated `encode<T>`/`decode<T>` directly.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(&name.to_case(Case::Pascal)))
}

/// The PascalCase class name of a record `Reference`. Only called after
/// `is_record_ref` has confirmed the type is a record reference.
fn record_ref_class(ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Reference(name) => name.to_case(Case::Pascal),
        _ => String::new(),
    }
}

/// The CBOR encoding of a text key. Comparing these byte slices lexicographically is
/// exactly RFC 8949 §4.2.1 canonical key ordering, computed at generation time so the
/// emitted encoder lays a record's map keys down in canonical order.
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
    } else {
        head.push(mt | 25);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    }
    head.extend_from_slice(bytes);
    head
}

fn codec_unwrap_constrained(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => base_type,
        other => other,
    }
}

/// Collapse a choice the way `map_type` does: a single non-literal arm narrows to that
/// arm's type (so the codec agrees with the field's declared Java type); anything else
/// has no precise model and is carried as `null`/`CborNull`.
fn codec_collapse_choice(choices: &[CsilTypeExpression]) -> Option<&CsilTypeExpression> {
    let non_literal: Vec<&CsilTypeExpression> = choices
        .iter()
        .filter(|c| !matches!(c, CsilTypeExpression::Literal(_)))
        .collect();
    match non_literal.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// A Java expression building a `CborValue` from `expr` (a typed value of the field's
/// mapped Java type). `depth` keeps nested-lambda parameter names distinct, since Java
/// forbids a lambda parameter shadowing one already in scope.
fn java_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    depth: usize,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("new CborInt({expr})"),
            "uint" => format!("new CborUint({expr})"),
            "float" | "float64" | "double" => format!("new CborFloat({expr})"),
            "text" | "tstr" => format!("new CborText({expr})"),
            "bytes" | "bstr" => format!("new CborBytes({expr})"),
            "bool" => format!("new CborBool({expr})"),
            "timestamp" => format!("encTimestamp({expr})"),
            "decimal" => format!("encDecimal({expr})"),
            _ => "new CborNull()".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&name.to_case(Case::Pascal)) => {
            format!("enc{}({expr})", name.to_case(Case::Pascal))
        }
        // A reference to a transparent alias has no codec of its own; encode its
        // underlying value. The field holds the wrapper record, so reach through
        // `.value()` to the underlying map/array/scalar the real encoder expects.
        CsilTypeExpression::Reference(name)
            if aliases.contains_key(&name.to_case(Case::Pascal)) =>
        {
            let pascal = name.to_case(Case::Pascal);
            java_enc_value(
                &aliases[&pascal],
                &format!("({expr}).value()"),
                records,
                aliases,
                depth,
            )
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let p = format!("csilElem{depth}");
            let inner = java_enc_value(element_type, &p, records, aliases, depth + 1);
            format!("encArray({expr}, {p} -> {inner})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kp = format!("csilK{depth}");
            let vp = format!("csilV{depth}");
            let kenc = java_enc_value(key, &kp, records, aliases, depth + 1);
            let venc = java_enc_value(value, &vp, records, aliases, depth + 1);
            format!("encMap({expr}, {kp} -> {kenc}, {vp} -> {venc})")
        }
        CsilTypeExpression::Choice(choices) => match codec_collapse_choice(choices) {
            Some(only) => java_enc_value(only, expr, records, aliases, depth),
            None => "new CborNull()".to_string(),
        },
        // A type the codec cannot model precisely (a non-record reference, a tuple,
        // `any`) is carried as null rather than emitting uncompilable code.
        _ => "new CborNull()".to_string(),
    }
}

/// A Java expression decoding a typed value from `expr` (a `CborValue`). Unmodeled
/// shapes (a non-record reference, a tuple, `any`) map to a reference Java type, so
/// `null` is a type-compatible placeholder there.
fn java_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    depth: usize,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("asI64({expr})"),
            "uint" => format!("asU64({expr})"),
            "float" | "float64" | "double" => format!("asF64({expr})"),
            "text" | "tstr" => format!("asText({expr})"),
            "bytes" | "bstr" => format!("asBytes({expr})"),
            "bool" => format!("asBool({expr})"),
            "timestamp" => format!("asTimestamp({expr})"),
            "decimal" => format!("asDecimal({expr})"),
            _ => "null".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&name.to_case(Case::Pascal)) => {
            format!("dec{}({expr})", name.to_case(Case::Pascal))
        }
        // The underlying decoder yields the unwrapped map/array/scalar value; rewrap it
        // in the alias's generated wrapper record so the field's declared Java type holds.
        CsilTypeExpression::Reference(name)
            if aliases.contains_key(&name.to_case(Case::Pascal)) =>
        {
            let pascal = name.to_case(Case::Pascal);
            let inner = java_dec_value(&aliases[&pascal], expr, records, aliases, depth);
            format!("new {pascal}({inner})")
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let p = format!("csilE{depth}");
            let inner = java_dec_value(element_type, &p, records, aliases, depth + 1);
            format!("decArray({expr}, {p} -> {inner})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kp = format!("csilK{depth}");
            let vp = format!("csilV{depth}");
            let kdec = java_dec_value(key, &kp, records, aliases, depth + 1);
            let vdec = java_dec_value(value, &vp, records, aliases, depth + 1);
            format!("decMap({expr}, {kp} -> {kdec}, {vp} -> {vdec})")
        }
        CsilTypeExpression::Choice(choices) => match codec_collapse_choice(choices) {
            Some(only) => java_dec_value(only, expr, records, aliases, depth),
            None => "null".to_string(),
        },
        _ => "null".to_string(),
    }
}

/// Emit the `enc<T>`/`dec<T>` pair plus the public `encode<T>`/`decode<T>` byte
/// wrappers for one record. The encoder lays keys in canonical order; the decoder
/// reads by key in declaration order (order is irrelevant on decode).
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    let class = name.to_case(Case::Pascal);
    // (member, wire, entry) in declaration order, plus a canonical-key-order copy for
    // the encoder so the emitted map is deterministic across languages.
    let named: Vec<(String, String, &CsilGroupEntry)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let member = entry_field_name(e)?;
            let wire = entry_wire_name(e)?;
            Some((member, wire, e))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    let mut out = String::new();
    out.push_str(&format!("    static CborValue enc{class}({class} v) {{\n"));
    out.push_str(&format!(
        "        java.util.List<CborEntry> csilEntries = new java.util.ArrayList<>({});\n",
        named.len()
    ));
    for (member, wire, entry) in &canonical {
        let wire_lit = java_string(wire);
        let enc = java_enc_value(
            &entry.value_type,
            &format!("v.{member}()"),
            records,
            aliases,
            0,
        );
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // An absent optional is omitted from the map entirely (wire contract).
            out.push_str(&format!("        if (v.{member}() != null) {{\n"));
            out.push_str(&format!(
                "            csilEntries.add(new CborEntry(new CborText({wire_lit}), {enc}));\n"
            ));
            out.push_str("        }\n");
        } else {
            out.push_str(&format!(
                "        csilEntries.add(new CborEntry(new CborText({wire_lit}), {enc}));\n"
            ));
        }
    }
    out.push_str("        return new CborMap(csilEntries);\n    }\n\n");

    out.push_str(&format!(
        "    static {class} dec{class}(CborValue csilRoot) {{\n"
    ));
    for (member, wire, entry) in &named {
        let wire_lit = java_string(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // A missing optional key leaves the field null; a present one decodes into
            // the boxed Java type so absent and present stay distinguishable.
            let bty = map_type_boxed(&entry.value_type);
            let dec = java_dec_value(&entry.value_type, "csilField", records, aliases, 0);
            out.push_str(&format!("        {bty} {member};\n"));
            out.push_str("        {\n");
            out.push_str(&format!(
                "            CborValue csilField = mapGet(csilRoot, {wire_lit});\n"
            ));
            out.push_str(&format!(
                "            {member} = csilField != null ? {dec} : null;\n"
            ));
            out.push_str("        }\n");
        } else {
            let ty = map_type(&entry.value_type);
            let dec = java_dec_value(
                &entry.value_type,
                &format!("require(csilRoot, {wire_lit})"),
                records,
                aliases,
                0,
            );
            out.push_str(&format!("        {ty} {member} = {dec};\n"));
        }
    }
    let args: Vec<&str> = named.iter().map(|(m, _, _)| m.as_str()).collect();
    out.push_str(&format!(
        "        return new {class}({});\n    }}\n\n",
        args.join(", ")
    ));

    out.push_str(&format!(
        "    public static byte[] encode{class}({class} v) {{\n        return encode(enc{class}(v));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static {class} decode{class}(byte[] data) {{\n        return dec{class}(decode(data));\n    }}\n\n"
    ));
    out
}

/// Build `CsilCbor.java`: the self-contained canonical-CBOR runtime plus an
/// `encode`/`decode` pair per record. `None` when the spec declares no records.
fn generate_codec(input: &WasmGeneratorInput, config: &JavaConfig) -> Option<GeneratedFile> {
    let records = record_names(input);
    if records.is_empty() {
        return None;
    }
    let aliases = codec_aliases(input);
    let mut body = String::new();
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            body.push_str(&emit_record_codec(&rule.name, group, &records, &aliases));
        }
    }

    let mut code = config.header();
    code.push_str("/**\n");
    code.push_str(
        " * Self-contained canonical-CBOR codec for the generated record types. The wire\n",
    );
    code.push_str(
        " * is owned here, never by reflection: a record is a CBOR map keyed by the CSIL\n",
    );
    code.push_str(" * field name verbatim, with map keys laid down in RFC 8949 canonical order.\n");
    code.push_str(" */\n");
    code.push_str("public final class CsilCbor {\n");
    code.push_str("    private CsilCbor() {}\n\n");
    code.push_str(CODEC_RUNTIME_JAVA);
    // The tagged-core (de)serializers are only worth their JDK imports when the spec
    // actually uses the type; the body references the helper iff a field needs it.
    if body.contains("Timestamp(") {
        code.push('\n');
        code.push_str(CODEC_TIMESTAMP_JAVA);
    }
    if body.contains("Decimal(") {
        code.push('\n');
        code.push_str(CODEC_DECIMAL_JAVA);
    }
    code.push('\n');
    code.push_str(&body);
    code.push_str("}\n");
    Some(GeneratedFile {
        path: config.path_for("CsilCbor"),
        content: code,
    })
}

// ---------------------------------------------------------------------------
// Client surface
// ---------------------------------------------------------------------------

fn generate_transport_iface(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* The caller-supplied byte carrier: it performs the call named by ({@code service},\n\
         \x20* {@code op}) with the already-encoded request bytes and returns the response bytes,\n\
         \x20* or throws. The generated client owns (de)serialization via the codec; the carrier\n\
         \x20* only moves bytes, so it can be HTTP, a queue, or an in-process loop. Synchronous\n\
         \x20* and blocking — no CompletableFuture.\n\
         \x20*/\n\
         public interface Transport {\n\
         \x20   byte[] call(String service, String op, byte[] req) throws ClientException;\n\
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
    records: &std::collections::HashSet<String>,
) -> GeneratedFile {
    let base = service_base(name);
    let class = format!("{base}Client");
    let wire_service = base.to_lowercase();

    let mut code = config.header();
    let mut prose = clean_doc(doc);
    prose.push(format!("A typed, blocking client for the {name} service."));
    prose.push(
        "The client owns (de)serialization via the codec; the transport only moves bytes."
            .to_string(),
    );
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
        let success = success_type(&op.output_type);
        let null_input = is_null_input(&op.input_type);
        // The typed-codec path needs a record success type (and a record or null
        // request) so the method can call the generated encode/decode. Anything else is
        // skipped with a note rather than emitting an uncompilable call.
        if !is_record_ref(&success, records)
            || !(null_input || is_record_ref(&op.input_type, records))
        {
            code.push('\n');
            code.push_str(&format!(
                "    // operation '{}' has a non-record payload; (de)serialize it manually\n",
                op.name
            ));
            continue;
        }
        let method = wire_method_name(&op.name);
        let camel = op.name.to_case(Case::Camel);
        let resp_class = record_ref_class(&success);
        let (params, req_bytes) = if null_input {
            (String::new(), "null".to_string())
        } else {
            let input = map_type(&op.input_type);
            let req_class = record_ref_class(&op.input_type);
            (
                format!("{input} req"),
                format!("CsilCbor.encode{req_class}(req)"),
            )
        };
        code.push('\n');
        code.push_str(&javadoc("    ", &clean_doc(&op.doc_comments), &[]));
        code.push_str(&format!(
            "    public {resp_class} {camel}({params}) throws ClientException {{\n"
        ));
        code.push_str(&format!(
            "        return CsilCbor.decode{resp_class}(transport.call(\"{wire_service}\", \"{method}\", {req_bytes}));\n"
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

/// The self-contained canonical-CBOR (RFC 8949 subset) value model, encoder, decoder,
/// generic composite helpers, and accessors every generated codec builds on. `bytes`
/// is a Java `byte[]` carried as a CBOR byte string (major type 2) by construction,
/// never an array of integers. Emitted as the body of `final class CsilCbor`, so JDK
/// types are written by FQN and hoisted to imports like the rest of this generator.
const CODEC_RUNTIME_JAVA: &str = r#"    /** A minimal canonical-CBOR value tree: a closed set of variants the codec builds and walks. */
    public sealed interface CborValue
        permits CborUint, CborInt, CborBool, CborFloat, CborNull,
                CborText, CborBytes, CborArray, CborMap, CborTag {}

    public record CborUint(long value) implements CborValue {}
    public record CborInt(long value) implements CborValue {}
    public record CborBool(boolean value) implements CborValue {}
    public record CborFloat(double value) implements CborValue {}
    public record CborNull() implements CborValue {}
    public record CborText(String value) implements CborValue {}
    public record CborBytes(byte[] value) implements CborValue {}
    public record CborArray(java.util.List<CborValue> items) implements CborValue {}
    public record CborEntry(CborValue key, CborValue val) {}
    public record CborMap(java.util.List<CborEntry> entries) implements CborValue {}
    public record CborTag(long num, CborValue inner) implements CborValue {}

    /**
     * Thrown when a CBOR payload is malformed or a required field is missing. Unchecked
     * so the generated codec methods read cleanly; a decoding fault is a protocol error,
     * not a recoverable application error (those ride inside the decoded payload).
     */
    public static final class CsilCborException extends RuntimeException {
        public CsilCborException(String message) {
            super(message);
        }
    }

    public static byte[] encode(CborValue v) {
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        enc(v, out);
        return out.toByteArray();
    }

    private static void head(int major, long n, java.io.ByteArrayOutputStream out) {
        int mt = major << 5;
        if (Long.compareUnsigned(n, 24L) < 0) {
            out.write(mt | (int) n);
        } else if (Long.compareUnsigned(n, 0x100L) < 0) {
            out.write(mt | 24);
            out.write((int) (n & 0xff));
        } else if (Long.compareUnsigned(n, 0x10000L) < 0) {
            out.write(mt | 25);
            out.write((int) ((n >> 8) & 0xff));
            out.write((int) (n & 0xff));
        } else if (Long.compareUnsigned(n, 0x100000000L) < 0) {
            out.write(mt | 26);
            for (int i = 24; i >= 0; i -= 8) {
                out.write((int) ((n >> i) & 0xff));
            }
        } else {
            out.write(mt | 27);
            for (int i = 56; i >= 0; i -= 8) {
                out.write((int) ((n >> i) & 0xff));
            }
        }
    }

    private static void enc(CborValue v, java.io.ByteArrayOutputStream out) {
        if (v instanceof CborUint x) {
            head(0, x.value(), out);
        } else if (v instanceof CborInt x) {
            if (x.value() >= 0) {
                head(0, x.value(), out);
            } else {
                head(1, -1 - x.value(), out);
            }
        } else if (v instanceof CborBool x) {
            out.write(x.value() ? 0xf5 : 0xf4);
        } else if (v instanceof CborNull) {
            out.write(0xf6);
        } else if (v instanceof CborFloat x) {
            long bits = Double.doubleToRawLongBits(x.value());
            out.write(0xfb);
            for (int i = 56; i >= 0; i -= 8) {
                out.write((int) ((bits >> i) & 0xff));
            }
        } else if (v instanceof CborText x) {
            byte[] u = x.value().getBytes(java.nio.charset.StandardCharsets.UTF_8);
            head(3, u.length, out);
            out.write(u, 0, u.length);
        } else if (v instanceof CborBytes x) {
            head(2, x.value().length, out);
            out.write(x.value(), 0, x.value().length);
        } else if (v instanceof CborArray x) {
            head(4, x.items().size(), out);
            for (CborValue e : x.items()) {
                enc(e, out);
            }
        } else if (v instanceof CborMap x) {
            head(5, x.entries().size(), out);
            for (CborEntry e : x.entries()) {
                enc(e.key(), out);
                enc(e.val(), out);
            }
        } else if (v instanceof CborTag x) {
            head(6, x.num(), out);
            enc(x.inner(), out);
        }
    }

    public static CborValue decode(byte[] b) {
        int[] pos = {0};
        CborValue v = dec(b, pos);
        if (pos[0] != b.length) {
            throw new CsilCborException("csil cbor: trailing bytes");
        }
        return v;
    }

    private static void requireLen(byte[] b, int need) {
        if (need > b.length) {
            throw new CsilCborException("csil cbor: truncated input");
        }
    }

    private static long readArg(byte[] b, int[] pos, int low) {
        if (low < 24) {
            pos[0] += 1;
            return low;
        }
        switch (low) {
            case 24:
                requireLen(b, pos[0] + 2);
                long v24 = b[pos[0] + 1] & 0xffL;
                pos[0] += 2;
                return v24;
            case 25:
                requireLen(b, pos[0] + 3);
                long v25 = ((b[pos[0] + 1] & 0xffL) << 8) | (b[pos[0] + 2] & 0xffL);
                pos[0] += 3;
                return v25;
            case 26: {
                requireLen(b, pos[0] + 5);
                long v = 0;
                for (int i = 1; i <= 4; i++) {
                    v = (v << 8) | (b[pos[0] + i] & 0xffL);
                }
                pos[0] += 5;
                return v;
            }
            case 27: {
                requireLen(b, pos[0] + 9);
                long v = 0;
                for (int i = 1; i <= 8; i++) {
                    v = (v << 8) | (b[pos[0] + i] & 0xffL);
                }
                pos[0] += 9;
                return v;
            }
            default:
                throw new CsilCborException("csil cbor: reserved additional info");
        }
    }

    private static CborValue dec(byte[] b, int[] pos) {
        if (pos[0] >= b.length) {
            throw new CsilCborException("csil cbor: unexpected end of input");
        }
        int ib = b[pos[0]] & 0xff;
        int major = ib >> 5;
        int low = ib & 0x1f;
        if (major == 7) {
            switch (low) {
                case 20:
                    pos[0] += 1;
                    return new CborBool(false);
                case 21:
                    pos[0] += 1;
                    return new CborBool(true);
                case 22:
                case 23:
                    pos[0] += 1;
                    return new CborNull();
                case 26: {
                    long bits = readArg(b, pos, low);
                    return new CborFloat(Float.intBitsToFloat((int) bits));
                }
                case 27: {
                    long bits = readArg(b, pos, low);
                    return new CborFloat(Double.longBitsToDouble(bits));
                }
                default:
                    throw new CsilCborException("csil cbor: unsupported simple value");
            }
        }
        long arg = readArg(b, pos, low);
        switch (major) {
            case 0:
                return new CborUint(arg);
            case 1:
                if (arg < 0) {
                    throw new CsilCborException("csil cbor: negative integer out of range");
                }
                return new CborInt(-1 - arg);
            case 2: {
                int n = (int) arg;
                requireLen(b, pos[0] + n);
                byte[] slice = java.util.Arrays.copyOfRange(b, pos[0], pos[0] + n);
                pos[0] += n;
                return new CborBytes(slice);
            }
            case 3: {
                int n = (int) arg;
                requireLen(b, pos[0] + n);
                String s = new String(b, pos[0], n, java.nio.charset.StandardCharsets.UTF_8);
                pos[0] += n;
                return new CborText(s);
            }
            case 4: {
                int n = (int) arg;
                java.util.List<CborValue> items = new java.util.ArrayList<>(n);
                for (int i = 0; i < n; i++) {
                    items.add(dec(b, pos));
                }
                return new CborArray(items);
            }
            case 5: {
                int n = (int) arg;
                java.util.List<CborEntry> entries = new java.util.ArrayList<>(n);
                for (int i = 0; i < n; i++) {
                    CborValue k = dec(b, pos);
                    CborValue val = dec(b, pos);
                    entries.add(new CborEntry(k, val));
                }
                return new CborMap(entries);
            }
            case 6:
                return new CborTag(arg, dec(b, pos));
            default:
                throw new CsilCborException("csil cbor: unexpected major type");
        }
    }

    public static long asI64(CborValue v) {
        if (v instanceof CborUint x) {
            if (x.value() < 0) {
                throw new CsilCborException("csil cbor: integer overflows int64");
            }
            return x.value();
        }
        if (v instanceof CborInt x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected integer");
    }

    public static long asU64(CborValue v) {
        if (v instanceof CborUint x) {
            return x.value();
        }
        if (v instanceof CborInt x) {
            if (x.value() < 0) {
                throw new CsilCborException("csil cbor: negative integer where unsigned expected");
            }
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected unsigned integer");
    }

    public static double asF64(CborValue v) {
        if (v instanceof CborFloat x) {
            return x.value();
        }
        if (v instanceof CborUint x) {
            return (double) x.value();
        }
        if (v instanceof CborInt x) {
            return (double) x.value();
        }
        throw new CsilCborException("csil cbor: expected float");
    }

    public static boolean asBool(CborValue v) {
        if (v instanceof CborBool x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected bool");
    }

    public static String asText(CborValue v) {
        if (v instanceof CborText x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected text");
    }

    public static byte[] asBytes(CborValue v) {
        if (v instanceof CborBytes x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected bytes");
    }

    public static java.util.List<CborValue> asArray(CborValue v) {
        if (v instanceof CborArray x) {
            return x.items();
        }
        throw new CsilCborException("csil cbor: expected array");
    }

    public static java.util.List<CborEntry> asMap(CborValue v) {
        if (v instanceof CborMap x) {
            return x.entries();
        }
        throw new CsilCborException("csil cbor: expected map");
    }

    public static CborValue mapGet(CborValue v, String key) {
        if (v instanceof CborMap x) {
            for (CborEntry e : x.entries()) {
                if (e.key() instanceof CborText k && k.value().equals(key)) {
                    return e.val();
                }
            }
        }
        return null;
    }

    public static CborValue require(CborValue v, String key) {
        CborValue x = mapGet(v, key);
        if (x == null) {
            throw new CsilCborException("csil cbor: missing field " + key);
        }
        return x;
    }

    public static <E> CborValue encArray(java.util.List<E> xs, java.util.function.Function<E, CborValue> f) {
        java.util.List<CborValue> items = new java.util.ArrayList<>(xs.size());
        for (E x : xs) {
            items.add(f.apply(x));
        }
        return new CborArray(items);
    }

    public static <K, V> CborValue encMap(java.util.Map<K, V> m, java.util.function.Function<K, CborValue> kf, java.util.function.Function<V, CborValue> vf) {
        java.util.List<CborEntry> entries = new java.util.ArrayList<>(m.size());
        for (java.util.Map.Entry<K, V> e : m.entrySet()) {
            entries.add(new CborEntry(kf.apply(e.getKey()), vf.apply(e.getValue())));
        }
        return new CborMap(entries);
    }

    public static <E> java.util.List<E> decArray(CborValue v, java.util.function.Function<CborValue, E> f) {
        java.util.List<CborValue> xs = asArray(v);
        java.util.List<E> out = new java.util.ArrayList<>(xs.size());
        for (CborValue x : xs) {
            out.add(f.apply(x));
        }
        return out;
    }

    public static <K, V> java.util.Map<K, V> decMap(CborValue v, java.util.function.Function<CborValue, K> kf, java.util.function.Function<CborValue, V> vf) {
        java.util.Map<K, V> out = new java.util.LinkedHashMap<>();
        for (CborEntry e : asMap(v)) {
            out.put(kf.apply(e.key()), vf.apply(e.val()));
        }
        return out;
    }
"#;

/// Timestamp (CBOR tag 0, RFC3339, always UTC) codec, appended only when the spec uses
/// `timestamp` so `java.time.Instant` is never an unused import. `Instant` is the UTC
/// instant type and its `toString` is RFC3339 with a `Z` offset, sub-second preserved.
const CODEC_TIMESTAMP_JAVA: &str = r#"    public static CborValue encTimestamp(java.time.Instant t) {
        return new CborTag(0, new CborText(t.toString()));
    }

    public static java.time.Instant asTimestamp(CborValue v) {
        if (v instanceof CborTag t && t.num() == 0 && t.inner() instanceof CborText s) {
            return java.time.Instant.parse(s.value());
        }
        throw new CsilCborException("csil cbor: expected CBOR tag 0 timestamp");
    }
"#;

/// Decimal (CBOR tag 4 `[exponent, mantissa]`, exact) codec, appended only when the
/// spec uses `decimal`. `BigDecimal` is Java's exact decimal; its unscaled value and
/// scale map straight onto the tag-4 wire form, with a bignum fallback (tag 2/3) when
/// the mantissa exceeds 64 bits so no precision is lost.
const CODEC_DECIMAL_JAVA: &str = r#"    public static CborValue encDecimal(java.math.BigDecimal d) {
        long exp = -(long) d.scale();
        return new CborTag(4, new CborArray(java.util.List.of(new CborInt(exp), encBigInt(d.unscaledValue()))));
    }

    public static java.math.BigDecimal asDecimal(CborValue v) {
        if (v instanceof CborTag t && t.num() == 4 && t.inner() instanceof CborArray a && a.items().size() == 2) {
            long exp = asI64(a.items().get(0));
            java.math.BigInteger mant = decBigInt(a.items().get(1));
            return new java.math.BigDecimal(mant, (int) -exp);
        }
        throw new CsilCborException("csil cbor: expected CBOR tag 4 decimal");
    }

    private static CborValue encBigInt(java.math.BigInteger m) {
        if (m.bitLength() <= 63) {
            return new CborInt(m.longValue());
        }
        if (m.signum() >= 0 && m.bitLength() <= 64) {
            return new CborUint(m.longValue());
        }
        if (m.signum() >= 0) {
            return new CborTag(2, new CborBytes(magnitudeBytes(m)));
        }
        java.math.BigInteger n = m.negate().subtract(java.math.BigInteger.ONE);
        return new CborTag(3, new CborBytes(magnitudeBytes(n)));
    }

    private static java.math.BigInteger decBigInt(CborValue v) {
        if (v instanceof CborUint x) {
            return unsignedToBigInteger(x.value());
        }
        if (v instanceof CborInt x) {
            return java.math.BigInteger.valueOf(x.value());
        }
        if (v instanceof CborTag t && t.inner() instanceof CborBytes bs) {
            java.math.BigInteger mag = new java.math.BigInteger(1, bs.value());
            if (t.num() == 2) {
                return mag;
            }
            if (t.num() == 3) {
                return mag.negate().subtract(java.math.BigInteger.ONE);
            }
        }
        throw new CsilCborException("csil cbor: expected integer mantissa");
    }

    private static java.math.BigInteger unsignedToBigInteger(long v) {
        if (v >= 0) {
            return java.math.BigInteger.valueOf(v);
        }
        return java.math.BigInteger.valueOf(v).add(java.math.BigInteger.ONE.shiftLeft(64));
    }

    private static byte[] magnitudeBytes(java.math.BigInteger m) {
        byte[] full = m.toByteArray();
        int start = 0;
        while (start < full.length - 1 && full[start] == 0) {
            start++;
        }
        return java.util.Arrays.copyOfRange(full, start, full.length);
    }
"#;

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
        // The request/response must be records for the typed-codec path to engage.
        let request = CsilGroupExpression {
            entries: vec![bare("queue", builtin("text"), None)],
        };
        let response = CsilGroupExpression {
            entries: vec![bare("uuid", builtin("text"), None)],
        };
        let files = generate_java(&input_for(
            vec![
                rule("SubmitTaskRequest", CsilRuleType::GroupDef(request)),
                rule("SubmitTaskResponse", CsilRuleType::GroupDef(response)),
                rule("CorndogsService", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();
        assert!(files.iter().any(|f| f.path.ends_with("Transport.java")));
        assert!(
            files
                .iter()
                .any(|f| f.path.ends_with("ClientException.java"))
        );
        // The self-contained per-record codec rides along with the client.
        assert!(files.iter().any(|f| f.path.ends_with("CsilCbor.java")));
        let f = file(&files, "CorndogsClient.java");
        assert!(f.content.contains("public final class CorndogsClient"));
        // ServiceError stripped from the typed return; method is camelCase.
        assert!(f.content.contains(
            "public SubmitTaskResponse submitTask(SubmitTaskRequest req) throws ClientException"
        ));
        // The client encodes the request and decodes the response through the codec over
        // a dumb byte seam: service lowercased, op PascalCased, matching peers.
        assert!(f.content.contains(
            "return CsilCbor.decodeSubmitTaskResponse(transport.call(\"corndogs\", \"SubmitTask\", CsilCbor.encodeSubmitTaskRequest(req)));"
        ));
        // no server interface for the client target.
        assert!(!files.iter().any(|f| f.path.ends_with("Corndogs.java")));
    }

    #[test]
    fn transport_seam_is_a_dumb_byte_carrier() {
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
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![bare("ok", builtin("bool"), None)],
                    }),
                ),
                rule("HealthService", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();
        let t = file(&files, "Transport.java");
        // The seam moves bytes only — no reflection Class<Resp>, no Object payload.
        assert!(t.content.contains(
            "byte[] call(String service, String op, byte[] req) throws ClientException;"
        ));
        assert!(!t.content.contains("Class<Resp>"));
        assert!(!t.content.contains("Object req"));
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
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![bare("ok", builtin("bool"), None)],
                    }),
                ),
                rule("Health", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();
        let f = file(&files, "HealthClient.java");
        assert!(
            f.content
                .contains("public Pong ping() throws ClientException")
        );
        // A null-input op sends a null payload; the response decodes through the codec.
        assert!(
            f.content.contains(
                "return CsilCbor.decodePong(transport.call(\"health\", \"Ping\", null));"
            )
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

    /// The corndogs `java-client` spec used by the codec/round-trip tests: a `Task`
    /// record exercising every scalar/optional/map/list shape, a wrapping request, and
    /// the `submit-task` operation.
    fn corndogs_rules() -> Vec<CsilRule> {
        let task = CsilGroupExpression {
            entries: vec![
                bare("uuid", builtin("text"), None),
                bare("current_state", builtin("text"), None),
                bare("payload", builtin("bytes"), None),
                bare("priority", builtin("int"), Some(CsilOccurrence::Optional)),
                bare(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                    None,
                ),
                bare(
                    "tags",
                    CsilTypeExpression::Array {
                        element_type: Box::new(builtin("text")),
                        occurrence: None,
                    },
                    None,
                ),
            ],
        };
        let request = CsilGroupExpression {
            entries: vec![
                bare(
                    "task",
                    CsilTypeExpression::Reference("Task".to_string()),
                    None,
                ),
                bare("queue", builtin("text"), None),
            ],
        };
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Task".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        // A record reachable only through a map-of-record alias, to prove the alias arm
        // recurses into the per-record codec (`M = {* text => SomeRecord}`).
        let item = CsilGroupExpression {
            entries: vec![bare("name", builtin("text"), None)],
        };
        // A record whose fields are transparent map aliases — the regression case: a named
        // map alias must round-trip, not stub to an empty map.
        let bag = CsilGroupExpression {
            entries: vec![
                bare(
                    "counts",
                    CsilTypeExpression::Reference("StringInt64Map".to_string()),
                    None,
                ),
                bare(
                    "items",
                    CsilTypeExpression::Reference("ItemMap".to_string()),
                    None,
                ),
            ],
        };
        vec![
            rule("Task", CsilRuleType::GroupDef(task)),
            rule("SubmitTaskRequest", CsilRuleType::GroupDef(request)),
            rule("Item", CsilRuleType::GroupDef(item)),
            rule(
                "StringInt64Map",
                CsilRuleType::TypeDef(CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(builtin("int")),
                    occurrence: None,
                }),
            ),
            rule(
                "ItemMap",
                CsilRuleType::TypeDef(CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(CsilTypeExpression::Reference("Item".to_string())),
                    occurrence: None,
                }),
            ),
            rule("Bag", CsilRuleType::GroupDef(bag)),
            rule("CorndogsService", CsilRuleType::ServiceDef(svc)),
        ]
    }

    #[test]
    fn codec_emits_self_contained_value_model_and_per_record_pairs() {
        let files = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        let f = file(&files, "CsilCbor.java");
        // A self-contained canonical-CBOR value model with every variant.
        assert!(f.content.contains("public sealed interface CborValue"));
        for variant in [
            "CborUint",
            "CborInt",
            "CborBool",
            "CborFloat",
            "CborNull",
            "CborText",
            "CborBytes",
            "CborArray",
            "CborMap",
            "CborTag",
        ] {
            assert!(
                f.content.contains(&format!("record {variant}(")),
                "missing variant {variant}"
            );
        }
        // Public per-record byte wrappers.
        assert!(
            f.content
                .contains("public static byte[] encodeTask(Task v)")
        );
        assert!(
            f.content
                .contains("public static Task decodeTask(byte[] data)")
        );
        assert!(
            f.content
                .contains("public static byte[] encodeSubmitTaskRequest(SubmitTaskRequest v)")
        );
        // text -> CborText (major 3); bytes -> CborBytes (major 2).
        assert!(
            f.content
                .contains("new CborEntry(new CborText(\"uuid\"), new CborText(v.uuid()))")
        );
        assert!(
            f.content
                .contains("new CborEntry(new CborText(\"payload\"), new CborBytes(v.payload()))")
        );
        // Optional absent -> omitted on encode, null on decode.
        assert!(f.content.contains("if (v.priority() != null) {"));
        assert!(
            f.content
                .contains("priority = csilField != null ? asI64(csilField) : null;")
        );
        // Composite map/list go through the generic helpers.
        assert!(f.content.contains(
            "encMap(v.labels(), csilK0 -> new CborText(csilK0), csilV0 -> new CborInt(csilV0))"
        ));
        assert!(
            f.content
                .contains("encArray(v.tags(), csilElem0 -> new CborText(csilElem0))")
        );
        // The nested record reference recurses into its own codec.
        assert!(
            f.content
                .contains("new CborEntry(new CborText(\"task\"), encTask(v.task()))")
        );
        // A named map alias field reaches through its wrapper record's `.value()` into the
        // underlying map codec instead of stubbing to `CborNull`/`null` (the regression).
        assert!(f.content.contains(
            "encMap((v.counts()).value(), csilK0 -> new CborText(csilK0), csilV0 -> new CborInt(csilV0))"
        ));
        assert!(
            f.content
                .contains("new StringInt64Map(decMap(require(csilRoot, \"counts\")")
        );
        // A map-of-record alias recurses to the referenced record's own codec.
        assert!(f.content.contains(
            "encMap((v.items()).value(), csilK0 -> new CborText(csilK0), csilV0 -> encItem(csilV0))"
        ));
        // Canonical RFC 8949 key order: among the length-4 keys, "tags" precedes "uuid".
        let tags_at = f.content.find("new CborText(\"tags\")").unwrap();
        let uuid_at = f.content.find("new CborText(\"uuid\")").unwrap();
        assert!(tags_at < uuid_at, "map keys not in canonical order");
        // The JDK types the codec writes are hoisted to imports.
        assert!(f.content.contains("import java.util.ArrayList;"));
        assert!(f.content.contains("import java.util.function.Function;"));
        // No tabs anywhere in the generated codec.
        assert!(!f.content.contains('\t'));
    }

    /// The generic decoder must be symmetric with the encoder for major type 6: the
    /// encoder writes `CborTag` (e.g. the CSIL-RPC envelope's `#6.24(bstr)`), so the
    /// decoder needs a `case 6` that reconstructs it, or any tagged payload throws
    /// "unexpected major type" on decode.
    #[test]
    fn codec_decoder_handles_tag_major_type() {
        let files = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        let f = file(&files, "CsilCbor.java");
        assert!(
            f.content
                .contains("case 6:\n                return new CborTag(arg, dec(b, pos));"),
            "decoder missing `case 6` that reconstructs a CborTag"
        );
    }

    /// Generate the corndogs `java-client` spec, write a Driver with a loopback byte
    /// transport, compile every generated + driver source with `javac`, and run it,
    /// asserting a full typed round-trip. Skips cleanly when `javac` is absent.
    #[test]
    fn codec_round_trips_through_javac() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-java-codec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            sources.push(path);
        }
        let driver = dir.join("csilgen/generated/Driver.java");
        std::fs::write(&driver, JAVA_CODEC_DRIVER).unwrap();
        sources.push(driver);

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let run = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("csilgen.generated.Driver")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "java run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\n{stdout}\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_CODEC_DRIVER: &str = r#"package csilgen.generated;

import java.util.List;
import java.util.Map;

public final class Driver {
    // A loopback "server" on the far side of the dumb byte seam: it decodes the typed
    // request, then encodes its task as the typed response, exercising both directions.
    static final class Loopback implements Transport {
        public byte[] call(String service, String op, byte[] req) throws ClientException {
            if (!service.equals("corndogs") || !op.equals("SubmitTask")) {
                throw new ClientException("unexpected route " + service + "/" + op);
            }
            SubmitTaskRequest in = CsilCbor.decodeSubmitTaskRequest(req);
            return CsilCbor.encodeTask(in.task());
        }
    }

    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    public static void main(String[] args) throws Exception {
        byte[] payload = new byte[] {(byte) 0xde, (byte) 0xad, (byte) 0xbe};
        Task task = new Task("u-123", "PENDING", payload, 7L, Map.of("a", 1L, "b", 2L), List.of("x", "y"));
        SubmitTaskRequest req = new SubmitTaskRequest(task, "default");

        // Direct codec round-trip through the nested record.
        SubmitTaskRequest back = CsilCbor.decodeSubmitTaskRequest(CsilCbor.encodeSubmitTaskRequest(req));
        check(back.task().uuid().equals("u-123"), "uuid");
        check(back.task().currentState().equals("PENDING"), "current_state");
        check(java.util.Arrays.equals(back.task().payload(), payload), "payload");
        check(back.task().priority() != null && back.task().priority() == 7L, "priority");
        check(back.task().labels().size() == 2 && back.task().labels().get("a") == 1L && back.task().labels().get("b") == 2L, "labels");
        check(back.task().tags().size() == 2 && back.task().tags().get(0).equals("x") && back.task().tags().get(1).equals("y"), "tags");
        check(back.queue().equals("default"), "queue");

        // An absent optional must round-trip to null, not a zero value.
        Task noPrio = new Task("u-2", "S", new byte[] {1}, null, Map.of(), List.of());
        SubmitTaskRequest back2 = CsilCbor.decodeSubmitTaskRequest(CsilCbor.encodeSubmitTaskRequest(new SubmitTaskRequest(noPrio, "q")));
        check(back2.task().priority() == null, "absent optional null");

        // Typed client round-trip over the loopback carrier.
        CorndogsClient client = new CorndogsClient(new Loopback());
        Task resp = client.submitTask(req);
        check(resp.uuid().equals("u-123"), "client uuid");
        check(java.util.Arrays.equals(resp.payload(), payload), "client payload");
        check(resp.priority() != null && resp.priority() == 7L, "client priority");
        check(resp.tags().size() == 2 && resp.tags().get(1).equals("y"), "client tags");

        // Named map aliases must round-trip, not stub to empty: a scalar-valued map alias
        // and a map-of-record alias both reach through the generated wrapper records.
        Bag bag = new Bag(
            new StringInt64Map(Map.of("a", 1L, "b", 2L)),
            new ItemMap(Map.of("k", new Item("hello"))));
        Bag bagBack = CsilCbor.decodeBag(CsilCbor.encodeBag(bag));
        check(bagBack.counts().value().size() == 2
            && bagBack.counts().value().get("a") == 1L
            && bagBack.counts().value().get("b") == 2L, "named map alias entries");
        check(bagBack.items().value().size() == 1
            && bagBack.items().value().get("k").name().equals("hello"), "map-of-record alias entries");

        // A tagged value (e.g. the CSIL-RPC envelope's tag 24 #6.24(bstr)) must survive a
        // generic round-trip: the decoder grew a `case 6` mirroring the encoder's tag branch,
        // so encode->decode reconstructs the tag number and its inner payload.
        byte[] tagPayload = new byte[] {(byte) 0xca, (byte) 0xfe, (byte) 0x01};
        CsilCbor.CborValue tagBack = CsilCbor.decode(
            CsilCbor.encode(new CsilCbor.CborTag(24, new CsilCbor.CborBytes(tagPayload))));
        check(tagBack instanceof CsilCbor.CborTag, "tag major decoded");
        CsilCbor.CborTag rt = (CsilCbor.CborTag) tagBack;
        check(rt.num() == 24, "tag number");
        check(rt.inner() instanceof CsilCbor.CborBytes
            && java.util.Arrays.equals(((CsilCbor.CborBytes) rt.inner()).value(), tagPayload),
            "tag inner bytes");

        System.out.println("ok");
    }
}
"#;

    /// Build a package-mode-capable input from the corndogs spec, overlaying the given
    /// option key/values onto the config so each test sets only the coordinates it cares
    /// about.
    fn package_input(target: &str, opts: &[(&str, serde_json::Value)]) -> WasmGeneratorInput {
        let mut input = input_for(corndogs_rules(), target);
        for (k, v) in opts {
            input.config.options.insert((*k).to_string(), v.clone());
        }
        input
    }

    #[test]
    fn pom_emitted_only_when_emit_packages_includes_java() {
        // No trigger: the default flat layout, no pom, sources directly under the package.
        let plain = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        assert!(!plain.iter().any(|f| f.path == "pom.xml"));
        assert!(
            plain
                .iter()
                .any(|f| f.path == "csilgen/generated/Task.java")
        );

        // emit_packages that does not name java is inert (another language was requested).
        let other = generate_java(&package_input(
            "java-client",
            &[("emit_packages", serde_json::json!(["go", "rust"]))],
        ))
        .unwrap();
        assert!(!other.iter().any(|f| f.path == "pom.xml"));

        // With "java": a pom with the resolved coordinates, and sources relaid under
        // Maven's standard src/main/java/<package path> root with no flat-layout twin.
        let pkg = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("corndogs-client")),
                ("package_version", serde_json::json!("1.2.3")),
            ],
        ))
        .unwrap();
        let pom = pkg
            .iter()
            .find(|f| f.path == "pom.xml")
            .expect("pom.xml emitted");
        assert!(
            pom.content
                .contains("<groupId>community.catalyst.demo</groupId>")
        );
        assert!(
            pom.content
                .contains("<artifactId>corndogs-client</artifactId>")
        );
        assert!(pom.content.contains("<version>1.2.3</version>"));
        assert!(
            pom.content
                .contains("<maven.compiler.release>17</maven.compiler.release>")
        );
        assert!(
            pkg.iter()
                .any(|f| f.path == "src/main/java/community/catalyst/demo/Task.java")
        );
        assert!(
            !pkg.iter()
                .any(|f| f.path == "community/catalyst/demo/Task.java")
        );
    }

    #[test]
    fn package_coordinates_default_and_parse_defensively() {
        // Absent package_name/version: artifactId is the kebab of the package's last
        // segment and version falls back to the conventional first release.
        let pkg = generate_java(&package_input(
            "java-typesonly",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("com.example.widgetApi")),
            ],
        ))
        .unwrap();
        let pom = pkg.iter().find(|f| f.path == "pom.xml").unwrap();
        assert!(
            pom.content
                .contains("<groupId>com.example.widgetApi</groupId>")
        );
        assert!(pom.content.contains("<artifactId>widget-api</artifactId>"));
        assert!(pom.content.contains("<version>0.1.0</version>"));

        // emit_packages handed in as a JSON-encoded string still triggers.
        let as_string = generate_java(&package_input(
            "java-typesonly",
            &[("emit_packages", serde_json::json!("[\"java\"]"))],
        ))
        .unwrap();
        assert!(as_string.iter().any(|f| f.path == "pom.xml"));

        // A bare comma-separated string is tolerated as well.
        let csv = generate_java(&package_input(
            "java-typesonly",
            &[("emit_packages", serde_json::json!("go, java"))],
        ))
        .unwrap();
        assert!(csv.iter().any(|f| f.path == "pom.xml"));
    }

    /// Generate a `java-client` package into a temp dir, assert the pom.xml is well-formed
    /// XML (parsed by the JDK's own parser, no third-party dep), and compile ALL laid-out
    /// `src/main/java/**.java` with `javac` to prove the sources are a coherent, compilable
    /// package. Maven itself is never invoked (offline it would need a populated local
    /// repo); javac + XML-validity is the proof. Skips cleanly when `javac` is absent.
    #[test]
    fn package_pom_is_well_formed_xml_and_sources_compile() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("corndogs-client")),
                ("package_version", serde_json::json!("0.1.0")),
            ],
        ))
        .unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-java-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        let mut pom_path = None;
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            if f.path == "pom.xml" {
                pom_path = Some(path);
            } else if f.path.starts_with("src/main/java/") && f.path.ends_with(".java") {
                sources.push(path);
            }
        }
        let pom_path = pom_path.expect("pom.xml present in package mode");

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();

        // Prove the pom is well-formed XML through the JDK's own DOM parser: a malformed
        // document throws on parse and the validator exits non-zero.
        let validator = dir.join("PomCheck.java");
        std::fs::write(&validator, JAVA_POM_VALIDATOR).unwrap();
        let vcompile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .arg(&validator)
            .output()
            .unwrap();
        assert!(
            vcompile.status.success(),
            "javac PomCheck failed:\n{}",
            String::from_utf8_lossy(&vcompile.stderr)
        );
        let vrun = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("PomCheck")
            .arg(&pom_path)
            .output()
            .unwrap();
        assert!(
            vrun.status.success(),
            "pom.xml is not well-formed XML:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&vrun.stdout),
            String::from_utf8_lossy(&vrun.stderr)
        );

        // Compile every laid-out source: a clean compile proves the package's sources form
        // a coherent, publishable unit under the standard Maven layout.
        assert!(
            !sources.is_empty(),
            "no src/main/java sources were laid out"
        );
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed on laid-out package sources:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_POM_VALIDATOR: &str = r#"import javax.xml.parsers.DocumentBuilderFactory;
import java.io.File;

public final class PomCheck {
    public static void main(String[] args) throws Exception {
        DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(new File(args[0]));
        System.out.println("ok");
    }
}
"#;

    /// The package README only rides along in package mode (`emit_packages` names java).
    #[test]
    fn readme_emitted_only_in_package_mode() {
        let plain = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        assert!(!plain.iter().any(|f| f.path == "README.md"));

        let pkg = generate_java(&package_input(
            "java-client",
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        assert!(pkg.iter().any(|f| f.path == "README.md"));
    }

    /// `emit_readme: false` suppresses only the README; the rest of the package (notably the
    /// pom and the laid-out sources) is unchanged.
    #[test]
    fn emit_readme_false_suppresses_only_readme() {
        let on = generate_java(&package_input(
            "java-client",
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        assert!(on.iter().any(|f| f.path == "README.md"));

        let off = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("emit_readme", serde_json::json!(false)),
            ],
        ))
        .unwrap();
        assert!(!off.iter().any(|f| f.path == "README.md"));
        // Everything other than the README is still emitted.
        assert!(off.iter().any(|f| f.path == "pom.xml"));
        let on_without_readme: Vec<_> = on
            .iter()
            .filter(|f| f.path != "README.md")
            .map(|f| &f.path)
            .collect();
        let off_paths: Vec<_> = off.iter().map(|f| &f.path).collect();
        assert_eq!(on_without_readme, off_paths);
    }

    /// The client README's Quickstart must be a complete, self-describing CSIL-RPC
    /// carrier: the seam implementation, the canonical endpoint POST, the tag-24 payload
    /// wrap, the status + ServiceError handling, client construction over the carrier, and
    /// an example call with a generated sample literal. (No jvm toolchain is available
    /// here, so the carrier is asserted by content, not compiled — runtime verify skipped.)
    #[test]
    fn readme_quickstart_has_carrier_and_example() {
        let files = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("corndogs-client")),
                ("package_version", serde_json::json!("1.2.3")),
            ],
        ))
        .unwrap();
        let r = file(&files, "README.md");
        let c = &r.content;

        // Title + Maven consume coordinates (the resolved coordinates, escaped).
        assert!(c.starts_with("# corndogs-client\n"));
        assert!(c.contains("<groupId>community.catalyst.demo</groupId>"));
        assert!(c.contains("<artifactId>corndogs-client</artifactId>"));
        assert!(c.contains("<version>1.2.3</version>"));

        // The carrier is in this package and implements the generated transport seam.
        assert!(c.contains("package community.catalyst.demo;"));
        assert!(c.contains("class CsilRpcTransport implements Transport"));
        assert!(c.contains(
            "public byte[] call(String service, String op, byte[] req) throws ClientException"
        ));
        // Hybrid path 1: it reuses the generated CsilCbor model — no third-party dep.
        assert!(c.contains("no third-party dep"));
        assert!(c.contains("new CsilCbor.CborMap("));

        // Stdlib blocking HTTP POST to the canonical endpoint.
        assert!(c.contains("import java.net.http.HttpClient;"));
        assert!(c.contains("/csil/v1/rpc"));
        assert!(c.contains(".POST(HttpRequest.BodyPublishers.ofByteArray("));

        // The tag-24 embedded-CBOR payload wrap.
        assert!(c.contains("new CsilCbor.CborTag(24, new CsilCbor.CborBytes("));

        // Transport status gate + typed ServiceError arm handling.
        assert!(c.contains("long status = CsilCbor.asI64(CsilCbor.require(env, \"status\"));"));
        assert!(c.contains("if (status != 0)"));
        assert!(c.contains("v.value().equals(\"ServiceError\")"));
        assert!(c.contains("throw new ClientException(\"service error \""));

        // Client construction over the carrier + the example call with a sample literal.
        assert!(c.contains("CorndogsClient client = new CorndogsClient(new CsilRpcTransport("));
        // submit-task takes SubmitTaskRequest { task: Task, queue: text }; the sample
        // fabricates the nested record (optional priority -> null) and returns a Task.
        assert!(c.contains("Task resp = client.submitTask(new SubmitTaskRequest(new Task("));
        assert!(c.contains("System.out.println(resp);"));
        // The carrier snippet is space-indented like the rest of the surface.
        let snippet = c.split("```java").nth(1).unwrap();
        assert!(!snippet.contains('\t'));
    }

    /// A null-input op yields a no-argument example call, and a serviceless package falls
    /// back to the types-only consume section rather than a carrier.
    #[test]
    fn readme_handles_null_input_and_serviceless() {
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
        let files = generate_java(&package_input_with(
            "java-client",
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![bare("ok", builtin("bool"), None)],
                    }),
                ),
                rule("HealthService", CsilRuleType::ServiceDef(svc)),
            ],
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        let r = file(&files, "README.md");
        assert!(
            r.content.contains("Pong resp = client.ping();"),
            "null-input op should call with no args"
        );

        // A types-only spec (no services) gets the consume-the-types section, no carrier.
        let typed = generate_java(&package_input_with(
            "java-typesonly",
            vec![rule(
                "Money",
                CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![bare("amount", builtin("int"), None)],
                }),
            )],
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        let tr = file(&typed, "README.md");
        assert!(tr.content.contains("has no service operations"));
        assert!(!tr.content.contains("implements Transport"));
    }

    /// Like `package_input`, but over a caller-supplied rule set rather than the corndogs
    /// fixture, so a test can shape a minimal spec and still set package options.
    fn package_input_with(
        target: &str,
        rules: Vec<CsilRule>,
        opts: &[(&str, serde_json::Value)],
    ) -> WasmGeneratorInput {
        let mut input = input_for(rules, target);
        for (k, v) in opts {
            input.config.options.insert((*k).to_string(), v.clone());
        }
        input
    }

    /// The fenced `java` block out of the README — exactly what a user copy-pastes.
    fn readme_java_block(readme: &str) -> &str {
        let after = readme
            .split_once("```java\n")
            .expect("README has a ```java block")
            .1;
        after.split_once("\n```").expect("java block is closed").0
    }

    /// Path-1 verification: write the generated `java-client` package, drop the README's
    /// carrier+example block in as a source file, and compile the whole package with
    /// `javac`. A clean compile proves the carrier is valid against the real generated
    /// `Transport` seam, `CsilCbor` codec, and typed client. Skips when `javac` is absent
    /// (the build sandbox has no jvm toolchain unless one is installed).
    #[test]
    fn readme_carrier_compiles_against_generated_package() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("corndogs-client")),
                ("package_version", serde_json::json!("0.1.0")),
            ],
        ))
        .unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-java-readme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        let mut readme = None;
        for f in &files {
            if f.path == "README.md" {
                readme = Some(f.content.clone());
                continue;
            }
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            if f.path.starts_with("src/main/java/") && f.path.ends_with(".java") {
                sources.push(path);
            }
        }

        // The README block declares `public final class Example` in `community.catalyst.demo`;
        // javac needs that public class in `Example.java` under the matching source root.
        let example = dir.join("src/main/java/community/catalyst/demo/Example.java");
        std::fs::write(&example, readme_java_block(&readme.unwrap())).unwrap();
        sources.push(example);

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed on README carrier + generated package:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
