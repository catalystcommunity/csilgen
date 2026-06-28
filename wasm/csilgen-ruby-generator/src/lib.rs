//! Ruby code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target ruby` from `csilgen_ruby_generator.wasm`.
//! Emits idiomatic Ruby 3.2+ source: `Data.define` value objects, transport-agnostic
//! clients, server handler classes with verbose/compact router twins, and `validate`
//! methods. The generator emits *shapes and routing only* — never wire bytes; the host
//! supplies a duck-typed transport/codec seam.
//!
//! Sub-targets dispatch on `config.target`:
//! - `ruby` / `ruby-server` → value types + server handlers + routers + encoders
//! - `ruby-client`          → value types + transport-agnostic client classes
//! - `ruby-typesonly`       → value types alone
//!
//! The WASM-boundary exports (`get_metadata`/`allocate`/`deallocate`/`generate`) and the
//! `write_json` helper are the stable ABI; the codegen lives in
//! `generate_ruby_code_from_serialized`, which is also the entry the integration tests call.

use convert_case::{Case, Casing};
use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation,
    CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression, CsilValidationConstraint,
    GeneratedFile, GenerationStats, GeneratorCapability, GeneratorConfig, GeneratorMetadata,
    WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "ruby-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Ruby code generator".to_string(),
        target: "ruby".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
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

    let files = generate_ruby_code_from_serialized(&input.csil_spec, &input.config)?;
    let total_size: usize = files.iter().map(|f| f.content.len()).sum();
    let files_generated = files.len();

    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::new(),
        stats: GenerationStats {
            files_generated,
            total_size_bytes: total_size,
            services_count: input.csil_spec.service_count,
            fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
            generation_time_ms: 0,
            peak_memory_bytes: None,
        },
    })
}

/// Which generated surface a target requests. Mirrors Go/Python's `Surface` so the
/// three sub-targets (server, client, types-only) stay aligned across generators.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

/// Public entry the integration tests call directly (the cdylib's `generate` export
/// goes through `process_generation` instead). Returns the emitted `.rb` files, or a
/// WASM error code on an unknown sub-target so a typo fails loudly rather than
/// silently degrading to the default surface.
pub fn generate_ruby_code_from_serialized(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<Vec<GeneratedFile>, i32> {
    let surface = match config.target.as_str() {
        "ruby" | "ruby-server" => Surface::Server,
        "ruby-client" => Surface::Client,
        "ruby-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let mut files = Vec::new();

    if let Some(types) = generate_types_file(spec) {
        files.push(GeneratedFile {
            path: "types.rb".to_string(),
            content: types,
        });
    }

    // The per-type CBOR codec rides alongside the types (every surface that has the
    // value classes can serialize them), so a typesonly consumer still gets usable,
    // wire-ready types. Emitted only when the spec declares record types.
    if let Some(codec) = generate_codec_file(spec) {
        files.push(GeneratedFile {
            path: "codec.rb".to_string(),
            content: codec,
        });
    }

    if spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_client_file(spec) {
                    files.push(GeneratedFile {
                        path: "client.rb".to_string(),
                        content: client,
                    });
                }
            }
            Surface::Server => {
                if let Some(server) = generate_server_file(spec) {
                    files.push(GeneratedFile {
                        path: "server.rb".to_string(),
                        content: server,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
    }

    // Package mode is opt-in and additive: only when the host asks for a Ruby package
    // does the flat set of `.rb` files become a self-contained, publishable RubyGem.
    if emit_packages_includes(config, "ruby") {
        return Ok(wrap_as_ruby_gem(files, config, spec));
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// Self-contained publishable RubyGem packaging (opt-in via `emit_packages`)
// ---------------------------------------------------------------------------

/// True when `config.options["emit_packages"]` is a JSON array that contains `lang`.
/// Parsed defensively: a missing key, a non-array value, or non-string elements all
/// read as "not requested" rather than erroring, so a malformed option degrades to the
/// unchanged (non-package) output.
fn emit_packages_includes(config: &GeneratorConfig, lang: &str) -> bool {
    config
        .options
        .get("emit_packages")
        .and_then(|value| value.as_array())
        .map(|array| array.iter().any(|element| element.as_str() == Some(lang)))
        .unwrap_or(false)
}

/// A non-empty string option, treating absent / wrong-typed / empty as "not set".
fn option_str<'a>(config: &'a GeneratorConfig, key: &str) -> Option<&'a str> {
    config
        .options
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
}

/// The gem name: `package_name` if given, else derived from the output directory's
/// basename, else `csilgen_client`. Always sanitized to a valid lib-file stem so
/// `require "<gem>"` resolves `lib/<gem>.rb`.
fn package_gem_name(config: &GeneratorConfig) -> String {
    if let Some(name) = option_str(config, "package_name") {
        let sanitized = sanitize_gem_name(name);
        if !sanitized.is_empty() {
            return sanitized;
        }
    }
    // The output directory is usually named for the package, so its basename is the
    // most meaningful fallback before the generic default.
    let derived = config
        .output_dir
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .map(sanitize_gem_name)
        .unwrap_or_default();
    if derived.is_empty() {
        "csilgen_client".to_string()
    } else {
        derived
    }
}

/// Reduce an arbitrary name to a gem-safe `snake_case` stem: lowercase alphanumerics,
/// every other run collapsed to a single `_`, no leading/trailing separator. Hyphens
/// are folded to `_` rather than kept so the gem name and its single `lib/<gem>.rb`
/// entry agree (a hyphenated gem name conventionally maps to a nested `lib/<a>/<b>.rb`).
fn sanitize_gem_name(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_underscore = false;
    for ch in raw.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            if pending_underscore && !out.is_empty() {
                out.push('_');
            }
            out.push(lowered);
            pending_underscore = false;
        } else {
            pending_underscore = true;
        }
    }
    out
}

/// The gem version: `package_version` if it is a plausible `Gem::Version` string, else
/// `0.1.0`. The shape check keeps a malformed option from producing a gemspec that
/// `gem build` would reject.
fn package_version(config: &GeneratorConfig) -> String {
    match option_str(config, "package_version") {
        Some(version) if is_gem_version(version) => version.to_string(),
        _ => "0.1.0".to_string(),
    }
}

fn is_gem_version(version: &str) -> bool {
    version.chars().next().is_some_and(|c| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Repackage the flat `.rb` output as a publishable RubyGem: relocate every generated
/// source under `lib/`, add a `lib/<gem>.rb` entry point that `require_relative`s them,
/// and a root `<gem>.gemspec`. The relative requires inside the generated files stay
/// valid because the whole set moves together into `lib/`.
fn wrap_as_ruby_gem(
    files: Vec<GeneratedFile>,
    config: &GeneratorConfig,
    spec: &CsilSpecSerialized,
) -> Vec<GeneratedFile> {
    let gem_name = package_gem_name(config);
    let version = package_version(config);

    // The entry point loads each generated module by basename; capture them before the
    // files are relocated under `lib/`.
    let requires: Vec<String> = files
        .iter()
        .map(|file| file.path.trim_end_matches(".rb").to_string())
        .collect();

    let mut out: Vec<GeneratedFile> = files
        .into_iter()
        .map(|file| GeneratedFile {
            path: format!("lib/{}", file.path),
            content: file.content,
        })
        .collect();

    out.push(GeneratedFile {
        path: format!("lib/{gem_name}.rb"),
        content: emit_gem_entry(&requires),
    });
    out.push(GeneratedFile {
        path: format!("{gem_name}.gemspec"),
        content: emit_gemspec(&gem_name, &version),
    });
    out.push(GeneratedFile {
        path: "README.md".to_string(),
        content: emit_readme(&gem_name, spec),
    });
    out
}

// ---------------------------------------------------------------------------
// Package README with a copy-paste CSIL-RPC Quickstart
// ---------------------------------------------------------------------------

/// The package README, with a copy-paste **Quickstart**. For a client package the
/// Quickstart is a complete, dependency-free CSIL-RPC carrier — it reuses this gem's
/// own generated `CsilCbor` codec to build/parse the envelope, so it adds no
/// third-party gem (hybrid posture path 1) — plus the typed client and one example
/// call. A spec with no unary operations falls back to a types-only consume section.
fn emit_readme(gem_name: &str, spec: &CsilSpecSerialized) -> String {
    let mut out = format!(
        "# {gem_name}\n\n\
         Generated by csilgen. A typed, transport-agnostic CSIL-RPC client: the\n\
         generated codec owns CBOR (de)serialization; you supply a *carrier* that only\n\
         moves bytes.\n\n\
         ## Install\n\n\
         ```sh\n\
         # TODO: publish {gem_name} to rubygems.org, then:\n\
         gem install {gem_name}\n\
         # or, from a local checkout, in your Gemfile:\n\
         #   gem \"{gem_name}\", path: \".\"\n\
         ```\n\n"
    );

    match first_unary_example(spec) {
        Some(example) => out.push_str(&readme_quickstart(gem_name, &example)),
        None => {
            out.push_str(&format!(
                "## Quickstart\n\n\
                 This package has no service operations — require its generated value\n\
                 types and codec directly:\n\n\
                 ```ruby\n\
                 require \"{gem_name}\"\n\
                 # build a value object and serialize it with the generated codec:\n\
                 #   bytes = MyType.new(...).to_cbor\n\
                 #   value = MyType.from_cbor(bytes)\n\
                 ```\n"
            ));
        }
    }
    finalize(out)
}

/// The client Quickstart: a dependency-free CSIL-RPC carrier (it reuses this gem's
/// generated `CsilCbor` codec for the envelope), the typed client built over it, and
/// the first unary call with a generated sample request literal.
fn readme_quickstart(gem_name: &str, ex: &UnaryExample) -> String {
    let mut out = String::from("## Quickstart\n\n");
    out.push_str(
        "A complete CSIL-RPC carrier (no third-party gems — it reuses this package's\n\
         generated `CsilCbor` codec for the envelope) plus the typed client. Change the\n\
         one base-URL string.\n\n",
    );
    out.push_str("```ruby\n");
    out.push_str("require \"net/http\"\n");
    out.push_str("require \"uri\"\n");
    out.push_str(&format!("require {}\n\n", ruby_string_literal(gem_name)));
    out.push_str(CARRIER_RUBY);
    out.push('\n');
    // The example: construct the typed client over the carrier and call the first op.
    out.push_str(&format!(
        "client = {}.new(CsilRpcTransport.new(\"http://localhost:5080\"))\n",
        ex.client_class
    ));
    if ex.has_request {
        out.push_str(&format!("resp = client.{}({})\n", ex.method, ex.sample));
    } else {
        out.push_str(&format!("resp = client.{}\n", ex.method));
    }
    out.push_str("puts resp.inspect\n");
    out.push_str("```\n");
    out
}

/// The carrier body — identical for every spec, so it is a constant. It implements the
/// generated client's transport seam (`call(service, op, req_bytes) -> resp_bytes`),
/// wraps the already-encoded request in a `CsilRpcRequest` envelope (tag-24 payload),
/// POSTs it to `{base_url}/csil/v1/rpc` with stdlib `net/http`, and returns the
/// response payload bytes for the generated client to decode. A non-2xx HTTP status, a
/// non-zero transport `status`, or a typed `ServiceError` arm is raised as an error.
const CARRIER_RUBY: &str = r##"# Dependency path 1: reuse the generated `CsilCbor` codec (encode/decode/Tag) to
# build and parse the CSIL-RPC envelope, so this carrier adds no third-party gem.
# It owns only the CSIL-RPC envelope + HTTP; it never touches your typed values.
class CsilRpcTransport
  def initialize(base_url)
    @uri = URI("#{base_url.chomp("/")}/csil/v1/rpc")
  end

  # The generated client calls this seam with the already-encoded request bytes.
  def call(service, op, req_bytes)
    # CsilRpcRequest = { v, service, op, payload: #6.24(bstr) }: the payload is the
    # encoded request wrapped in CBOR tag 24 (embedded CBOR).
    envelope = {
      "v" => 1,
      "service" => service,
      "op" => op,
      "payload" => CsilCbor::Tag.new(24, req_bytes.b)
    }
    http = Net::HTTP.new(@uri.host, @uri.port)
    http.use_ssl = (@uri.scheme == "https")
    post = Net::HTTP::Post.new(@uri.request_uri)
    post["content-type"] = "application/cbor"
    post["accept"] = "application/cbor"
    post.body = CsilCbor.encode(envelope)
    res = http.request(post)
    raise "csil-rpc #{service}/#{op}: http #{res.code}" unless res.code.to_i.between?(200, 299)

    reply = CsilCbor.decode((res.body || "").b)
    status = reply["status"]
    raise "csil-rpc #{service}/#{op}: transport status #{status}" if status != 0

    payload = reply["payload"]
    unless payload.is_a?(CsilCbor::Tag) && payload.tag == 24
      raise "csil-rpc: response payload is not a tag-24 byte string"
    end
    inner = payload.value

    # A typed `ServiceError` arm (variant "ServiceError") is an application error,
    # distinct from a transport failure.
    if reply["variant"] == "ServiceError"
      err = CsilCbor.decode(inner)
      raise "service error #{err["code"]}: #{err["message"]}"
    end
    inner
  end
end
"##;

/// The pieces the Quickstart's example call needs: which client class + method to
/// call and a constructible sample request literal (empty when the op takes no input).
struct UnaryExample {
    client_class: String,
    method: String,
    has_request: bool,
    sample: String,
}

/// The first service (in rule order, matching the emitted client order) that has a
/// unary `->` operation, reduced to an example call. `None` for a serviceless /
/// no-unary spec, which falls back to the types-only README.
fn first_unary_example(spec: &CsilSpecSerialized) -> Option<UnaryExample> {
    for rule in &spec.rules {
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
        let has_request = !op_input_is_null(&op.input_type);
        return Some(UnaryExample {
            client_class: format!("{}Client", wire_service_base(&rule.name)),
            method: ruby_method_name(&op.name),
            has_request,
            sample: if has_request {
                ruby_sample(spec, &op.input_type)
            } else {
                String::new()
            },
        });
    }
    None
}

/// A constructible Ruby literal for `ty`: real values for scalars/collections and a
/// `Type.new(field: ...)` for a record reference (required fields only). Shapes a
/// generic sample can't fabricate (choices, tuples, unknown refs) fall back to `nil`,
/// which the user fills in.
fn ruby_sample(spec: &CsilSpecSerialized, ty: &CsilTypeExpression) -> String {
    match codec_unwrap(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "\"\".b".to_string(),
            "timestamp" => "Time.now".to_string(),
            "decimal" => "BigDecimal(\"0\")".to_string(),
            "int" | "uint" => "0".to_string(),
            "float" => "0.0".to_string(),
            _ => "nil".to_string(),
        },
        CsilTypeExpression::Array { .. } => "[]".to_string(),
        CsilTypeExpression::Map { .. } => "{}".to_string(),
        CsilTypeExpression::Reference(name) => match find_record(spec, name) {
            Some(group) => record_literal(spec, name, group),
            None => "nil".to_string(),
        },
        _ => "nil".to_string(),
    }
}

/// `Type.new(field: <sample>, ...)` over a record's required fields, keyed by the
/// verbatim CSIL field names the generated `Data.define` value object uses.
fn record_literal(spec: &CsilSpecSerialized, name: &str, group: &CsilGroupExpression) -> String {
    let class = ruby_class_name(name);
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| e.key.is_some())
        .filter(|e| !matches!(e.occurrence, Some(CsilOccurrence::Optional)))
        .map(|e| {
            let field = field_name(e.key.as_ref().unwrap());
            format!("{field}: {}", ruby_sample(spec, &e.value_type))
        })
        .collect();
    if fields.is_empty() {
        format!("{class}.new")
    } else {
        format!("{class}.new({})", fields.join(", "))
    }
}

/// The record a type reference names, if any. A `Name = { ... }` rule parses as
/// `TypeDef(Group(..))`, while a bare group rule is `GroupDef(..)`; both are records.
fn find_record<'a>(spec: &'a CsilSpecSerialized, name: &str) -> Option<&'a CsilGroupExpression> {
    spec.rules
        .iter()
        .filter(|r| r.name == name)
        .find_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        })
}

/// The `lib/<gem>.rb` entry point: a require_relative for each relocated source so a
/// single `require "<gem>"` loads the whole generated surface.
fn emit_gem_entry(requires: &[String]) -> String {
    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    for module in requires {
        content.push_str(&format!(
            "require_relative {}\n",
            ruby_string_literal(module)
        ));
    }
    finalize(content)
}

/// The `<gem>.gemspec`. `files` is globbed from `lib/` so it tracks whatever surface
/// was generated, and `required_ruby_version` is `>= 3.2` because the value objects are
/// `Data.define`. No runtime dependencies: the generated codec is self-contained.
fn emit_gemspec(gem_name: &str, version: &str) -> String {
    let name_lit = ruby_string_literal(gem_name);
    let version_lit = ruby_string_literal(version);
    let summary_lit = ruby_string_literal(&format!("CSIL-generated Ruby package {gem_name}."));

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    content.push_str("Gem::Specification.new do |spec|\n");
    content.push_str(&format!("  spec.name = {name_lit}\n"));
    content.push_str(&format!("  spec.version = {version_lit}\n"));
    content.push_str(&format!("  spec.summary = {summary_lit}\n"));
    content.push_str("  spec.authors = [\"csilgen\"]\n");
    content.push_str("  spec.license = \"Apache-2.0\"\n");
    content.push_str("  spec.required_ruby_version = \">= 3.2\"\n");
    content.push_str("  spec.files = Dir[\"lib/**/*.rb\"]\n");
    content.push_str("  spec.require_paths = [\"lib\"]\n");
    content.push_str("end\n");
    finalize(content)
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// A CSIL type name (snake_case) → a Ruby class/constant name (PascalCase).
fn ruby_class_name(name: &str) -> String {
    name.to_case(Case::Pascal)
}

/// A CSIL operation name (kebab-case) → a Ruby method name (snake_case). A kebab is
/// illegal in a Ruby method name, so `deposit-claim` → `deposit_claim`.
fn ruby_method_name(name: &str) -> String {
    name.to_case(Case::Snake)
}

/// PascalCase a name with the same simple rule the Go/Python/TS clients use for the
/// wire. convert_case diverges on acronyms, and the wire string must agree
/// byte-for-byte across every language, so this is hand-rolled rather than
/// `to_case(Case::Pascal)` — a case transform must never leak onto the wire.
fn wire_method_name(name: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            cap = true;
        } else if cap {
            out.extend(ch.to_uppercase());
            cap = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The wire service base: strip a trailing `Service`, matching the Go/Python clients
/// so all languages address the same service string. Built from `wire_method_name`
/// (not convert_case) so the lowercased result agrees on the wire.
fn wire_service_base(name: &str) -> String {
    let pascal = wire_method_name(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

// ---------------------------------------------------------------------------
// Ruby string literals
// ---------------------------------------------------------------------------

/// A complete, always-valid Ruby double-quoted string literal for `s`, so an embedded
/// quote, backslash, or newline can never break the surrounding source. `#` is escaped
/// because Ruby performs `#{...}`/`#@`/`#$` interpolation inside double quotes.
fn ruby_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '#' => out.push_str("\\#"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

const FROZEN_HEADER: &str = "# frozen_string_literal: true\n";

/// Normalize a generated file to exactly one trailing newline. The per-type/-class
/// emitters each append a separating blank line, which would otherwise leave the file
/// ending in two newlines (a `Layout/TrailingEmptyLines` offense under standardrb).
fn finalize(mut content: String) -> String {
    while content.ends_with('\n') {
        content.pop();
    }
    content.push('\n');
    content
}

/// True when the spec uses the named builtin anywhere, so a `require` is emitted only
/// when the feature is actually present.
fn spec_uses_builtin(spec: &CsilSpecSerialized, builtin: &str) -> bool {
    spec.rules.iter().any(|rule| match &rule.rule_type {
        CsilRuleType::GroupDef(group) => group
            .entries
            .iter()
            .any(|e| type_uses_builtin(&e.value_type, builtin)),
        CsilRuleType::TypeDef(type_expr) => type_uses_builtin(type_expr, builtin),
        CsilRuleType::ServiceDef(service) => service.operations.iter().any(|op| {
            type_uses_builtin(&op.input_type, builtin)
                || type_uses_builtin(&op.output_type, builtin)
        }),
        _ => false,
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

fn generate_types_file(spec: &CsilSpecSerialized) -> Option<String> {
    let mut body = String::new();
    let mut has_types = false;

    for rule in &spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            has_types = true;
            body.push_str(&emit_value_type(&rule.name, &rule.doc_comments, group));
            continue;
        }
        if let CsilRuleType::TypeDef(type_expr) = &rule.rule_type {
            has_types = true;
            // Ruby is dynamically typed and has no type alias; surface the aliased
            // shape as a comment so the intent stays visible to a reader.
            for line in &rule.doc_comments {
                body.push_str(&format!("# {line}\n"));
            }
            body.push_str(&format!(
                "# {} is an alias for {}.\n\n",
                ruby_class_name(&rule.name),
                map_csil_type_to_ruby(type_expr)
            ));
        }
    }

    if !has_types {
        return None;
    }

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    // Only `require` what mapped/validated types actually use, so a spec of plain
    // scalars never pulls in bigdecimal/time.
    let mut needs_blank = false;
    if spec_uses_builtin(spec, "decimal") {
        content.push_str("require \"bigdecimal\"\n");
        needs_blank = true;
    }
    if spec_uses_builtin(spec, "timestamp") {
        content.push_str("require \"time\"\n");
        needs_blank = true;
    }
    if needs_blank {
        content.push('\n');
    }
    content.push_str(&body);
    Some(finalize(content))
}

/// Emit one `Data.define` value object. Optional fields and `.default`/`@default`
/// operators force an `initialize` override (Data has no default-arg constructor), and
/// any field carrying a runtime constraint adds a `validate` method.
fn emit_value_type(name: &str, doc_comments: &[String], group: &CsilGroupExpression) -> String {
    let class_name = ruby_class_name(name);
    let mut out = String::new();

    for line in doc_comments {
        out.push_str(&format!("# {line}\n"));
    }

    let fields: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();

    // A per-field summary comment: Ruby can't attach a doc to a `Data.define` member,
    // so the field/type lines sit above the class as a readable header.
    for entry in &fields {
        let field = field_name(entry.key.as_ref().unwrap());
        let ty = map_csil_type_to_ruby(&entry.value_type);
        if let Some(desc) = field_description(&entry.metadata) {
            out.push_str(&format!("# {field} [{ty}] {desc}\n"));
        } else {
            out.push_str(&format!("# {field} [{ty}]\n"));
        }
        if let Some(depends) = depends_comment(&entry.metadata) {
            out.push_str(&format!("#   depends-on: {depends}\n"));
        }
    }

    let members: Vec<String> = fields
        .iter()
        .map(|e| format!(":{}", field_name(e.key.as_ref().unwrap())))
        .collect();

    let needs_initialize = fields.iter().any(|e| {
        matches!(e.occurrence, Some(CsilOccurrence::Optional)) || entry_default_value(e).is_some()
    });
    let needs_validate = fields.iter().any(|e| entry_has_check(e));

    if members.is_empty() {
        out.push_str(&format!("{class_name} = Data.define\n\n"));
        return out;
    }

    if !needs_initialize && !needs_validate {
        out.push_str(&format!(
            "{class_name} = Data.define({})\n\n",
            members.join(", ")
        ));
        return out;
    }

    out.push_str(&format!(
        "{class_name} = Data.define({}) do\n",
        members.join(", ")
    ));

    if needs_initialize {
        out.push_str(&emit_initialize(&fields));
    }

    if needs_validate {
        if needs_initialize {
            out.push('\n');
        }
        out.push_str(&emit_validate(&fields));
    }

    out.push_str("end\n\n");
    out
}

/// `Data.define` generates a keyword constructor with no defaults; reopening it lets
/// optional fields default to `nil` and `.default(...)` fields to their literal. A bare
/// `super` forwards the same-named keyword args to the generated constructor.
fn emit_initialize(fields: &[&CsilGroupEntry]) -> String {
    // Ruby requires every optional keyword parameter to follow the required ones, so
    // the two are collected separately and concatenated. `super` forwards arguments by
    // name, so this reordering relative to the field/member order is purely cosmetic.
    let mut required = Vec::new();
    let mut optional = Vec::new();
    for entry in fields {
        let field = field_name(entry.key.as_ref().unwrap());
        if let Some(default) = entry_default_value(entry) {
            let value = literal_value_to_ruby_value(default, &entry.value_type);
            optional.push(format!("{field}: {value}"));
        } else if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            optional.push(format!("{field}: nil"));
        } else {
            required.push(format!("{field}:"));
        }
    }
    let mut params = required;
    params.extend(optional);
    let mut out = String::new();
    out.push_str(&format!("  def initialize({})\n", params.join(", ")));
    out.push_str("    super\n");
    out.push_str("  end\n");
    out
}

/// A field's Ruby reader name plus whether it is optional (may be `nil`). Threaded
/// through the check emitters so each guards a `nil` optional before reading it.
#[derive(Clone, Copy)]
struct FieldRef<'a> {
    name: &'a str,
    optional: bool,
}

/// Emit the `validate` method: idiomatic Ruby raises `ArgumentError` on the first
/// violation. Optional fields are guarded so a `nil` value is skipped rather than
/// raising a `NoMethodError`.
fn emit_validate(fields: &[&CsilGroupEntry]) -> String {
    let mut out = String::new();
    out.push_str("  # Raises ArgumentError on the first constraint violation.\n");
    out.push_str("  def validate\n");
    for entry in fields {
        let field = field_name(entry.key.as_ref().unwrap());
        let fref = FieldRef {
            name: &field,
            optional: matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
        };
        for metadata in &entry.metadata {
            if let CsilFieldMetadata::Constraint(constraint) = metadata {
                emit_metadata_constraint(&mut out, fref, &entry.value_type, constraint);
            }
        }
        if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
            for op in constraints {
                emit_control_op_check(&mut out, fref, &entry.value_type, op);
            }
        }
    }
    out.push_str("    nil\n");
    out.push_str("  end\n");
    out
}

/// Emit a single guarded check: `raise ArgumentError, <msg> if <guard><condition>`. The
/// guard skips the check when an optional field is `nil`.
fn push_guarded(out: &mut String, field: FieldRef, condition: &str, message: &str) {
    let lit = ruby_string_literal(message);
    let guard = if field.optional {
        format!("!{}.nil? && ", field.name)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "    raise ArgumentError, {lit} if {guard}{condition}\n"
    ));
}

// ---------------------------------------------------------------------------
// Validation: which constraints actually yield a runtime check
// ---------------------------------------------------------------------------

fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let meta_check = entry.metadata.iter().any(|m| match m {
        CsilFieldMetadata::Constraint(c) => constraint_is_check(c),
        _ => false,
    });
    let op_check = match &entry.value_type {
        CsilTypeExpression::Constrained { constraints, .. } => {
            constraints.iter().any(control_op_is_check)
        }
        _ => false,
    };
    meta_check || op_check
}

fn constraint_is_check(constraint: &CsilValidationConstraint) -> bool {
    // `@default` is a constructor concern; every other annotation (including a `regex`
    // Custom) produces a runtime check.
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

/// Whether a field's (possibly constrained) base is an ordered core type needing a
/// typed bound rather than a bare scalar compare: `decimal` parses through
/// `BigDecimal`, `timestamp` through `Time.iso8601`.
enum OrderedKind {
    Numeric,
    Decimal,
    Timestamp,
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

/// Emit one ordered comparison honoring the field's type. `ruby_op` is the operator
/// whose truth means the constraint is violated (`.ge` is violated when the value is
/// `<` the bound). Numeric fields compare directly; `decimal`/`timestamp` parse the
/// bound into the matching Ruby value so the comparison always type-checks at runtime.
fn emit_ordered_check(
    out: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: (&str, &str),
    value: &CsilLiteralValue,
) {
    let (ruby_op, desc) = op;
    let access = field.name;
    let name = field.name;
    match ordered_field_kind(value_type) {
        OrderedKind::Numeric => {
            let bound = literal_value_to_ruby(value);
            let condition = format!("{access} {ruby_op} {bound}");
            let message = format!("field '{name}' must be {desc} {bound}");
            push_guarded(out, field, &condition, &message);
        }
        OrderedKind::Decimal => {
            let Some(text) = literal_as_decimal_text(value) else {
                return;
            };
            let bound = format!("BigDecimal({})", ruby_string_literal(&text));
            let condition = format!("{access} {ruby_op} {bound}");
            let message = format!("field '{name}' must be {desc} {text}");
            push_guarded(out, field, &condition, &message);
        }
        OrderedKind::Timestamp => {
            let Some(text) = literal_as_timestamp_text(value) else {
                return;
            };
            let bound = format!("Time.iso8601({})", ruby_string_literal(&text));
            let condition = format!("{access} {ruby_op} {bound}");
            let message = format!("field '{name}' must be {desc} {text}");
            push_guarded(out, field, &condition, &message);
        }
    }
}

/// Emit a single `@`-annotation ValidationConstraint as a Ruby check.
fn emit_metadata_constraint(
    out: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    constraint: &CsilValidationConstraint,
) {
    match constraint {
        CsilValidationConstraint::MinLength(n) => {
            let unit = if *n == 1 { "character" } else { "characters" };
            emit_len_check(out, field, "<", *n, &format!("at least {n} {unit}"));
        }
        CsilValidationConstraint::MaxLength(n) => {
            let unit = if *n == 1 { "character" } else { "characters" };
            emit_len_check(out, field, ">", *n, &format!("at most {n} {unit}"));
        }
        CsilValidationConstraint::MinItems(n) => {
            let unit = if *n == 1 { "item" } else { "items" };
            emit_len_check(out, field, "<", *n, &format!("at least {n} {unit}"));
        }
        CsilValidationConstraint::MaxItems(n) => {
            let unit = if *n == 1 { "item" } else { "items" };
            emit_len_check(out, field, ">", *n, &format!("at most {n} {unit}"));
        }
        CsilValidationConstraint::MinValue(v) => {
            emit_ordered_check(out, field, value_type, ("<", "at least"), v);
        }
        CsilValidationConstraint::MaxValue(v) => {
            emit_ordered_check(out, field, value_type, (">", "at most"), v);
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                emit_regex_check(out, field, pattern);
            }
        }
    }
}

/// Emit a single `.`-control-operator. Comparisons and size/regex become runtime
/// checks; `.default` is applied by `initialize`; the encoding-only operators
/// (.bits/.and/.within/.json/.cbor/.cborseq) leave a comment so their presence is
/// visible but they never fail validation.
fn emit_control_op_check(
    out: &mut String,
    field: FieldRef,
    value_type: &CsilTypeExpression,
    op: &CsilControlOperator,
) {
    let name = field.name;
    match op {
        CsilControlOperator::GreaterEqual(v) => {
            emit_ordered_check(out, field, value_type, ("<", "at least"), v)
        }
        CsilControlOperator::LessEqual(v) => {
            emit_ordered_check(out, field, value_type, (">", "at most"), v)
        }
        CsilControlOperator::GreaterThan(v) => {
            emit_ordered_check(out, field, value_type, ("<=", "greater than"), v)
        }
        CsilControlOperator::LessThan(v) => {
            emit_ordered_check(out, field, value_type, (">=", "less than"), v)
        }
        CsilControlOperator::Equal(v) => {
            emit_ordered_check(out, field, value_type, ("!=", "equal to"), v)
        }
        CsilControlOperator::NotEqual(v) => {
            emit_ordered_check(out, field, value_type, ("==", "not equal to"), v)
        }
        CsilControlOperator::Size(size) => emit_size_check(out, field, size),
        CsilControlOperator::Regex(pattern) => emit_regex_check(out, field, pattern),
        CsilControlOperator::Default(_) => {}
        CsilControlOperator::Bits(bits) => {
            out.push_str(&format!(
                "    # field '{name}' carries .bits({bits}); a bit-set encoding hint, not a runtime check\n"
            ));
        }
        CsilControlOperator::And(_) => {
            out.push_str(&format!(
                "    # field '{name}' carries .and; intersection constraint left to the consumer\n"
            ));
        }
        CsilControlOperator::Within(_) => {
            out.push_str(&format!(
                "    # field '{name}' carries .within; range membership left to the consumer\n"
            ));
        }
        CsilControlOperator::Json | CsilControlOperator::Cbor | CsilControlOperator::Cborseq => {
            out.push_str(&format!(
                "    # field '{name}' carries an embedded-encoding operator; handled at (de)serialization, not validated\n"
            ));
        }
    }
}

/// A `.length`-based check shared by `@min-length`/`.size`/etc.; Ruby strings, arrays,
/// and hashes all respond to `.length`.
fn emit_len_check(out: &mut String, field: FieldRef, op: &str, n: u64, tail: &str) {
    let access = field.name;
    let name = field.name;
    let condition = format!("{access}.length {op} {n}");
    let message = format!("field '{name}' must have {tail}");
    push_guarded(out, field, &condition, &message);
}

fn emit_size_check(out: &mut String, field: FieldRef, size: &CsilSizeConstraint) {
    let mut one = |op: &str, n: u64, word: &str| {
        emit_len_check(out, field, op, n, &format!("{word} {n} elements"));
    };
    match size {
        CsilSizeConstraint::Exact(n) => one("!=", *n, "exactly"),
        CsilSizeConstraint::Min(n) => one("<", *n, "at least"),
        CsilSizeConstraint::Max(n) => one(">", *n, "at most"),
        CsilSizeConstraint::Range { min, max } => {
            one("<", *min, "at least");
            one(">", *max, "at most");
        }
    }
}

/// `Regexp.new(<literal>)` is used rather than a `/.../ ` literal so an arbitrary
/// pattern string can't break out of the regex delimiter; `match?` avoids setting the
/// `$~` global. The check fails when the value does NOT match.
fn emit_regex_check(out: &mut String, field: FieldRef, pattern: &str) {
    let access = field.name;
    let name = field.name;
    let condition = format!(
        "!{access}.match?(Regexp.new({}))",
        ruby_string_literal(pattern)
    );
    let message = format!("field '{name}' must match pattern '{pattern}'");
    push_guarded(out, field, &condition, &message);
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

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

/// A literal as a Ruby value for an `initialize` default, honoring `decimal`/`timestamp`
/// fields (a bare string literal would be the wrong runtime type for those).
fn literal_value_to_ruby_value(
    value: &CsilLiteralValue,
    value_type: &CsilTypeExpression,
) -> String {
    match ordered_field_kind(value_type) {
        OrderedKind::Decimal => {
            if let Some(text) = literal_as_decimal_text(value) {
                return format!("BigDecimal({})", ruby_string_literal(&text));
            }
        }
        OrderedKind::Timestamp => {
            if let Some(text) = literal_as_timestamp_text(value) {
                return format!("Time.iso8601({})", ruby_string_literal(&text));
            }
        }
        OrderedKind::Numeric => {}
    }
    literal_value_to_ruby(value)
}

fn literal_value_to_ruby(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => ruby_string_literal(s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "\"\".b".to_string(),
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(literal_value_to_ruby).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Per-type CBOR codec (codec.rb)
// ---------------------------------------------------------------------------
//
// Ruby has no derive/serde CBOR ecosystem and a record's wire form must match the
// other languages byte-for-byte, so — like the C/Zig/OCaml/Dart/Swift/Go/Python
// generators — the Ruby generator emits a self-contained per-type codec. It is the
// one place this generator emits payload-wire bytes rather than shapes only, because
// nothing else can. Each record's map keys are laid down in canonical RFC 8949
// §4.2.1 order, fixed at generation time, so the bytes are stable without a runtime
// sort.

/// The CBOR encoding of a text key (major type 3 head + bytes); comparing these byte
/// vectors lexicographically is RFC 8949 §4.2.1 key ordering, computed once at
/// generation time so the emitted map is canonical without a runtime sort.
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

/// The CSIL rule names that get a generated codec. Only records (which encode as a
/// CBOR map) are covered; a `Reference` to one of these recurses into its codec.
fn codec_record_names(spec: &CsilSpecSerialized) -> std::collections::HashSet<String> {
    spec.rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(r.name.clone()),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(r.name.clone()),
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference / tuple / constrained (NOT a record group or a
/// choice, which have their own handling). A field referencing one must encode/decode
/// as the underlying type rather than passing the bare value through the stub.
fn codec_aliases(
    spec: &CsilSpecSerialized,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    spec.rules
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

/// A constraint wraps its base for codec purposes; the wire form is the base's.
fn codec_unwrap(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => codec_unwrap(base_type),
        other => other,
    }
}

/// A Ruby expression building the value-tree node for `expr` (a value of the Ruby
/// type for `ty`). The tree is plain Ruby (Integer/Float/bool/nil/String/Array/Hash)
/// plus `CsilCbor::Tag` for the tagged core types; `CsilCbor.encode` walks it. A
/// `bytes` field is forced to a binary String so the encoder keys it to CBOR major
/// type 2 — the text/bytes choice is driven by the CSIL field type here, not by the
/// runtime Ruby class (both are `String`).
fn enc_tree(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match codec_unwrap(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "float" | "bool" => expr.to_string(),
            "text" | "tstr" => expr.to_string(),
            "bytes" | "bstr" => format!("({expr}).b"),
            "timestamp" => format!("CsilCbor::Tag.new(0, ({expr}).getutc.iso8601)"),
            "decimal" => format!("CsilCbor::Tag.new(4, CsilCbor.decimal_to_tag({expr}))"),
            "nil" | "null" => "nil".to_string(),
            _ => format!("({expr})"),
        },
        CsilTypeExpression::Reference(name) if records.contains(name) => {
            format!("({expr}).csil_to_tree")
        }
        // A reference to a transparent alias (`StringInt64Map = {* text => int}`,
        // `Tags = [* text]`, `Uuid = text`) has no codec of its own; its Ruby value is
        // just the underlying Hash/Array/scalar, so encode it as the underlying type and
        // let the same `expr` flow through. A map-of-record alias recurses into the
        // record codec via the underlying Map's value type.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            enc_tree(&aliases[name], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = enc_tree(element_type, "csil_e", records, aliases);
            format!("({expr}).map {{ |csil_e| {inner} }}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let ek = enc_tree(key, "csil_k", records, aliases);
            let ev = enc_tree(value, "csil_v", records, aliases);
            format!(
                "({expr}).each_with_object({{}}) {{ |(csil_k, csil_v), csil_h| csil_h[{ek}] = {ev} }}"
            )
        }
        _ => format!("({expr})"),
    }
}

/// A Ruby expression decoding the value-tree node `expr` back into the Ruby value
/// for `ty`. `decode` already returns a binary String for major type 2 and a UTF-8
/// String for major type 3, so `text`/`bytes` need no further coercion here.
fn dec_tree(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    match codec_unwrap(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "float" | "bool" | "text" | "tstr" | "bytes" | "bstr" => {
                expr.to_string()
            }
            "timestamp" => format!("Time.iso8601(({expr}).value)"),
            "decimal" => format!("CsilCbor.tag_to_decimal(({expr}).value)"),
            "nil" | "null" => "nil".to_string(),
            _ => expr.to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(name) => {
            format!("{}.csil_from_tree({expr})", ruby_class_name(name))
        }
        // A reference to a transparent alias decodes as its underlying type; the decoded
        // Hash/Array/scalar is the alias's Ruby value verbatim. A map-of-record alias
        // recurses into the record codec via the underlying Map's value type.
        CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
            dec_tree(&aliases[name], expr, records, aliases)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = dec_tree(element_type, "csil_e", records, aliases);
            format!("({expr}).map {{ |csil_e| {inner} }}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let dk = dec_tree(key, "csil_k", records, aliases);
            let dv = dec_tree(value, "csil_v", records, aliases);
            format!(
                "({expr}).each_with_object({{}}) {{ |(csil_k, csil_v), csil_h| csil_h[{dk}] = {dv} }}"
            )
        }
        _ => expr.to_string(),
    }
}

/// Reopen one generated value class with `to_cbor` / `self.from_cbor` and the
/// internal tree builders the nested-record path calls. The map is built in
/// canonical key order; an absent optional is omitted on encode and read back as
/// `nil` (a missing key) on decode.
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
) -> String {
    let class = ruby_class_name(name);
    let in_order: Vec<&CsilGroupEntry> = group.entries.iter().filter(|e| e.key.is_some()).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "# CBOR codec for {class}: a map keyed by the verbatim CSIL field names in\n"
    ));
    out.push_str("# canonical RFC 8949 order.\n");
    out.push_str(&format!("class {class}\n"));

    out.push_str("  def to_cbor\n");
    out.push_str("    CsilCbor.encode(csil_to_tree)\n");
    out.push_str("  end\n\n");

    out.push_str("  def csil_to_tree\n");
    out.push_str("    csil_map = {}\n");
    // Canonical RFC 8949 key order, fixed here so the emitted map needs no runtime sort.
    let mut canonical = in_order.clone();
    canonical.sort_by(|a, b| {
        let ka = cbor_text_key_bytes(&field_name(a.key.as_ref().unwrap()));
        let kb = cbor_text_key_bytes(&field_name(b.key.as_ref().unwrap()));
        ka.cmp(&kb)
    });
    for entry in &canonical {
        let field = field_name(entry.key.as_ref().unwrap());
        let node = enc_tree(&entry.value_type, &field, records, aliases);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            out.push_str(&format!(
                "    csil_map[\"{field}\"] = {node} unless {field}.nil?\n"
            ));
        } else {
            out.push_str(&format!("    csil_map[\"{field}\"] = {node}\n"));
        }
    }
    out.push_str("    csil_map\n");
    out.push_str("  end\n\n");

    out.push_str("  def self.from_cbor(bytes)\n");
    out.push_str("    csil_from_tree(CsilCbor.decode(bytes))\n");
    out.push_str("  end\n\n");

    out.push_str("  def self.csil_from_tree(node)\n");
    if in_order.is_empty() {
        out.push_str("    new\n");
    } else {
        out.push_str("    new(\n");
        let parts: Vec<String> = in_order
            .iter()
            .map(|entry| {
                let field = field_name(entry.key.as_ref().unwrap());
                let access = format!("node[\"{field}\"]");
                let dec = dec_tree(&entry.value_type, &access, records, aliases);
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    format!("      {field}: (node.key?(\"{field}\") ? {dec} : nil)")
                } else {
                    format!("      {field}: {dec}")
                }
            })
            .collect();
        out.push_str(&parts.join(",\n"));
        out.push('\n');
        out.push_str("    )\n");
    }
    out.push_str("  end\n");
    out.push_str("end\n\n");
    out
}

/// Build `codec.rb`: the self-contained canonical-CBOR module plus a reopening of
/// every generated value class with `to_cbor` / `self.from_cbor`. `None` when the
/// spec declares no record types. Named `codec.rb` (not `codec.gen.rb`) so a
/// consumer can `require_relative "codec"`.
fn generate_codec_file(spec: &CsilSpecSerialized) -> Option<String> {
    let records = codec_record_names(spec);
    if records.is_empty() {
        return None;
    }
    let aliases = codec_aliases(spec);

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    // The codec reopens the generated classes, so it needs them loaded first.
    content.push_str("require_relative \"types\"\n");
    if spec_uses_builtin(spec, "timestamp") {
        content.push_str("require \"time\"\n");
    }
    if spec_uses_builtin(spec, "decimal") {
        content.push_str("require \"bigdecimal\"\n");
    }
    content.push('\n');
    content.push_str(CODEC_RUNTIME_RUBY);
    if spec_uses_builtin(spec, "decimal") {
        content.push_str(CODEC_DECIMAL_RUBY);
    }
    content.push('\n');

    for rule in &spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            content.push_str(&emit_record_codec(&rule.name, group, &records, &aliases));
        }
    }

    Some(finalize(content))
}

/// The self-contained canonical-CBOR runtime every generated codec builds on. It
/// walks a plain Ruby value tree (Integer/Float/true/false/nil/String/Array/Hash)
/// plus `CsilCbor::Tag` for the tagged core types, so the generated output needs no
/// third-party CBOR gem. The text/bytes split is by String encoding: a binary
/// (ASCII-8BIT) String is a `bytes` field (major type 2), any other encoding is
/// `text` (major type 3); the per-record codec sets the encoding from the CSIL type.
const CODEC_RUNTIME_RUBY: &str = r#"# A tagged CBOR value (major type 6): a semantic tag wrapping an inner value,
# carrying the timestamp (tag 0) and decimal (tag 4) core types through the
# otherwise-plain Ruby value tree.
module CsilCbor
  Tag = Struct.new(:tag, :value)

  module_function

  # Canonical-CBOR encode of a Ruby value tree to a binary String.
  def encode(value)
    buf = "".b
    put(buf, value)
    buf
  end

  def put_head(buf, major, arg)
    mt = major << 5
    if arg < 24
      buf << (mt | arg).chr
    elsif arg < 0x100
      buf << (mt | 24).chr << arg.chr
    elsif arg < 0x10000
      buf << (mt | 25).chr << [arg].pack("n")
    elsif arg < 0x100000000
      buf << (mt | 26).chr << [arg].pack("N")
    else
      buf << (mt | 27).chr << [arg >> 32, arg & 0xffffffff].pack("NN")
    end
  end

  def put(buf, value)
    case value
    when Integer
      if value >= 0
        put_head(buf, 0, value)
      else
        put_head(buf, 1, -1 - value)
      end
    when Float
      buf << "\xfb".b << [value].pack("G")
    when true
      buf << "\xf5".b
    when false
      buf << "\xf4".b
    when nil
      buf << "\xf6".b
    when String
      if value.encoding == Encoding::BINARY
        put_head(buf, 2, value.bytesize)
        buf << value
      else
        bin = value.b
        put_head(buf, 3, bin.bytesize)
        buf << bin
      end
    when Array
      put_head(buf, 4, value.length)
      value.each { |item| put(buf, item) }
    when Hash
      put_head(buf, 5, value.length)
      value.each do |k, v|
        put(buf, k)
        put(buf, v)
      end
    when Tag
      put_head(buf, 6, value.tag)
      put(buf, value.value)
    else
      raise ArgumentError, "csilgen: cannot encode #{value.class}"
    end
  end

  # Decode a binary String to a Ruby value tree.
  def decode(bytes)
    bin = bytes.b
    value, pos = take(bin, 0)
    raise ArgumentError, "csilgen: trailing bytes" unless pos == bin.bytesize

    value
  end

  def read_arg(bin, pos, low)
    if low < 24
      [low, pos + 1]
    elsif low == 24
      [bin.getbyte(pos + 1), pos + 2]
    elsif low == 25
      [bin[pos + 1, 2].unpack1("n"), pos + 3]
    elsif low == 26
      [bin[pos + 1, 4].unpack1("N"), pos + 5]
    elsif low == 27
      hi, lo = bin[pos + 1, 8].unpack("NN")
      [(hi << 32) | lo, pos + 9]
    else
      raise ArgumentError, "csilgen: bad head"
    end
  end

  def take(bin, pos)
    ib = bin.getbyte(pos)
    major = ib >> 5
    low = ib & 0x1f
    if major == 7
      case low
      when 20 then [false, pos + 1]
      when 21 then [true, pos + 1]
      when 22, 23 then [nil, pos + 1]
      when 26 then [[bin[pos + 1, 4].unpack1("N")].pack("N").unpack1("g"), pos + 5]
      when 27 then [bin[pos + 1, 8].unpack1("G"), pos + 9]
      else raise ArgumentError, "csilgen: unsupported simple value"
      end
    else
      arg, p = read_arg(bin, pos, low)
      case major
      when 0
        [arg, p]
      when 1
        [-1 - arg, p]
      when 2
        [bin[p, arg].b, p + arg]
      when 3
        [bin[p, arg].dup.force_encoding(Encoding::UTF_8), p + arg]
      when 4
        items = []
        arg.times do
          item, p = take(bin, p)
          items << item
        end
        [items, p]
      when 5
        hash = {}
        arg.times do
          k, p = take(bin, p)
          v, p = take(bin, p)
          hash[k] = v
        end
        [hash, p]
      when 6
        inner, p = take(bin, p)
        [Tag.new(arg, inner), p]
      else
        raise ArgumentError, "csilgen: bad major"
      end
    end
  end
end
"#;

/// The `decimal` core type's tag-4 (de)serializers, appended to `CsilCbor` only when
/// the spec uses `decimal` so a `bigdecimal` dependency is never pulled in otherwise.
const CODEC_DECIMAL_RUBY: &str = r#"
module CsilCbor
  module_function

  # The CBOR tag-4 decimal-fraction payload [exponent, mantissa] for a BigDecimal,
  # exact: value = mantissa * 10**exponent.
  def decimal_to_tag(value)
    sign, digits, _base, exp = value.split
    [exp - digits.length, sign * digits.to_i]
  end

  def tag_to_decimal(pair)
    exp, mant = pair
    BigDecimal(mant) * (BigDecimal(10)**exp)
  end
end
"#;

// ---------------------------------------------------------------------------
// Type mapping (doc-comment only — Ruby is dynamically typed)
// ---------------------------------------------------------------------------

fn map_csil_type_to_ruby(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" => "Integer".to_string(),
            "float" => "Float".to_string(),
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "String".to_string(),
            "bool" => "Boolean".to_string(),
            "timestamp" => "Time".to_string(),
            "decimal" => "BigDecimal".to_string(),
            "nil" | "null" => "nil".to_string(),
            other => ruby_class_name(other),
        },
        CsilTypeExpression::Reference(name) => ruby_class_name(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("Array<{}>", map_csil_type_to_ruby(element_type))
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "Hash<{}, {}>",
            map_csil_type_to_ruby(key),
            map_csil_type_to_ruby(value)
        ),
        CsilTypeExpression::Constrained { base_type, .. } => map_csil_type_to_ruby(base_type),
        CsilTypeExpression::Choice(choices) => {
            // Literal arms (a string/enum choice like `"a" / "b"`) all map to the same
            // underlying Ruby class, so collapse duplicates: `String | "a" | "b"`
            // becomes a clean `String` rather than `String | Object | Object`.
            let mut parts: Vec<String> = Vec::new();
            for choice in choices {
                let mapped = map_csil_type_to_ruby(choice);
                if !parts.contains(&mapped) {
                    parts.push(mapped);
                }
            }
            parts.join(" | ")
        }
        CsilTypeExpression::Literal(value) => literal_type_name(value).to_string(),
        CsilTypeExpression::Tuple(_) => "Array".to_string(),
        _ => "Object".to_string(),
    }
}

/// The Ruby class a literal value documents as. A literal type arm (e.g. an enum's
/// `"pending"`) is shown by its class so a choice of string literals reads as `String`.
fn literal_type_name(value: &CsilLiteralValue) -> &'static str {
    match value {
        CsilLiteralValue::Integer(_) => "Integer",
        CsilLiteralValue::Float(_) => "Float",
        CsilLiteralValue::Text(_) | CsilLiteralValue::Bytes(_) => "String",
        CsilLiteralValue::Bool(_) => "Boolean",
        CsilLiteralValue::Null => "nil",
        CsilLiteralValue::Array(_) => "Array",
    }
}

// ---------------------------------------------------------------------------
// Field metadata helpers
// ---------------------------------------------------------------------------

fn field_name(key: &CsilGroupKey) -> String {
    // CSIL fields are snake_case and double as the verbatim CBOR map key, so they are
    // kept as-is — no case transform that could leak onto the wire.
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

fn field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.as_str())
        } else {
            None
        }
    })
}

fn depends_comment(metadata: &[CsilFieldMetadata]) -> Option<String> {
    metadata
        .iter()
        .find_map(|meta| match meta {
            CsilFieldMetadata::DependsOnExpr(condition) => {
                Some(render_depends_condition(condition))
            }
            CsilFieldMetadata::DependsOn { field, value } => Some(match value {
                Some(value) => format!("{field} == {}", literal_value_to_ruby(value)),
                None => field.clone(),
            }),
            _ => None,
        })
        // The condition lands in a `#` line comment; an embedded break would push the
        // remainder onto an uncommented line, so collapse it to one line.
        .map(|rendered| rendered.replace(['\n', '\r'], " "))
}

fn render_depends_condition(condition: &CsilDependsCondition) -> String {
    match condition {
        CsilDependsCondition::Compare { field, op, value } => match (op, value) {
            (Some(op), Some(value)) => format!(
                "{field} {} {}",
                depends_compare_op_str(op),
                literal_value_to_ruby(value)
            ),
            _ => field.clone(),
        },
        CsilDependsCondition::All(conditions) => join_depends_conditions(conditions, "&&"),
        CsilDependsCondition::Any(conditions) => join_depends_conditions(conditions, "||"),
    }
}

fn join_depends_conditions(conditions: &[CsilDependsCondition], separator: &str) -> String {
    conditions
        .iter()
        .map(render_depends_condition)
        .collect::<Vec<_>>()
        .join(&format!(" {separator} "))
}

fn depends_compare_op_str(op: &CsilDependsCompareOp) -> &'static str {
    match op {
        CsilDependsCompareOp::Eq => "==",
        CsilDependsCompareOp::Ne => "!=",
        CsilDependsCompareOp::Lt => "<",
        CsilDependsCompareOp::Le => "<=",
        CsilDependsCompareOp::Gt => ">",
        CsilDependsCompareOp::Ge => ">=",
    }
}

// ---------------------------------------------------------------------------
// Operation helpers
// ---------------------------------------------------------------------------

/// A push op (`<- Event`) carries a `null` input type: there is no request body to
/// send, so the client/handler method drops the request parameter.
fn op_input_is_null(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — that error half rides the
/// transport, not the returned value.
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

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

const CLIENT_PRELUDE: &str = "\
# Each generated client owns (de)serialization via the generated CBOR codec and
# delegates only byte movement to a host-supplied transport seam: a duck-typed
# object responding to `call(service, op, req_bytes) -> resp_bytes`. The carrier
# moves bytes (HTTP, a queue, an in-process loop); it never sees the typed values.
";

fn generate_client_file(spec: &CsilSpecSerialized) -> Option<String> {
    let records = codec_record_names(spec);
    let mut body = String::new();
    let mut emitted = false;
    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_client_class(&rule.name, service));
            if let Some(wire_ids) = emit_wire_ids(&rule.name, service) {
                body.push_str(&wire_ids);
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    // The typed methods call `to_cbor`/`from_cbor` on the generated value classes,
    // which the codec defines; pull it in (it requires the types) when records exist.
    if !records.is_empty() {
        content.push_str("require_relative \"codec\"\n\n");
    }
    content.push_str(CLIENT_PRELUDE);
    content.push('\n');
    content.push_str(&body);
    Some(finalize(content))
}

fn emit_client_class(name: &str, service: &CsilServiceDefinition) -> String {
    let base = wire_service_base(name);
    let client = format!("{base}Client");
    let wire_service = base.to_lowercase();

    let mut out = String::new();
    out.push_str(&format!("# Typed client for the {name} service.\n"));
    out.push_str(&format!("class {client}\n"));
    out.push_str("  def initialize(transport)\n");
    out.push_str("    @transport = transport\n");
    out.push_str("  end\n");

    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops ride
        // the router/encoder surface emitted by the server target.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            out.push_str(&format!(
                "\n  # channel operation {} is not part of the RPC client\n",
                op.name
            ));
            continue;
        }
        let method = ruby_method_name(&op.name);
        let wire_method = wire_method_name(&op.name);
        let has_input = !op_input_is_null(&op.input_type);
        let success = success_type(&op.output_type);
        let out_ty = map_csil_type_to_ruby(&success);

        out.push('\n');
        if op.doc_comments.is_empty() {
            out.push_str(&format!("  # {}: -> {out_ty}\n", op.name));
        } else {
            for line in &op.doc_comments {
                out.push_str(&format!("  # {line}\n"));
            }
        }
        // The request encodes to its canonical CBOR bytes; a null-input op sends an
        // empty byte payload since it carries no request body.
        let payload = match (has_input, &op.input_type) {
            (false, _) => "\"\".b".to_string(),
            (true, CsilTypeExpression::Reference(_)) => "req.to_cbor".to_string(),
            // A non-record request can't self-serialize; the caller passes ready bytes.
            (true, _) => "req".to_string(),
        };
        let call = format!("@transport.call(\"{wire_service}\", \"{wire_method}\", {payload})");
        // A reference success type is a generated value class, so decode the reply
        // bytes through its codec; anything else rides back as raw bytes.
        let body = match &success {
            CsilTypeExpression::Reference(resp) => {
                format!("{}.from_cbor({call})", ruby_class_name(resp))
            }
            _ => call,
        };
        if has_input {
            out.push_str(&format!("  def {method}(req)\n"));
        } else {
            out.push_str(&format!("  def {method}\n"));
        }
        out.push_str(&format!("    {body}\n"));
        out.push_str("  end\n");
    }

    out.push_str("end\n\n");
    out
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

const SERVER_CODEC_NOTE: &str = "\
# The router/encoder functions take a host-supplied codec: a duck-typed object
# responding to `encode(value) -> bytes` and `decode(bytes, type) -> value`. The
# generator is codec-agnostic; the implementer wires it to CBOR, JSON, or anything else.
";

fn generate_server_file(spec: &CsilSpecSerialized) -> Option<String> {
    let mut body = String::new();
    let mut emitted = false;
    let has_channel_ops = spec.rules.iter().any(
        |r| matches!(&r.rule_type, CsilRuleType::ServiceDef(def) if service_has_channel_ops(def)),
    );

    for rule in &spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            body.push_str(&emit_handlers_class(&rule.name, service));
            if service_has_channel_ops(service) {
                body.push_str(&emit_router_module(&rule.name, service));
            }
            emitted = true;
        }
    }
    if !emitted {
        return None;
    }

    let mut content = String::new();
    content.push_str(FROZEN_HEADER);
    content.push('\n');
    content.push_str("# Code generated by csilgen; DO NOT EDIT.\n\n");
    if has_channel_ops {
        content.push_str(SERVER_CODEC_NOTE);
        content.push('\n');
    }
    content.push_str(&body);
    Some(finalize(content))
}

fn emit_handlers_class(name: &str, service: &CsilServiceDefinition) -> String {
    let base = wire_service_base(name);
    let handler_class = format!("{base}Handlers");
    let mut out = String::new();

    out.push_str(&format!(
        "# Server-side handlers for the {name} service. Subclass and override each\n"
    ));
    out.push_str("# operation; the unimplemented base raises NotImplementedError.\n");
    out.push_str(&format!("class {handler_class}\n"));

    // Unidirectional ops are request/response; bidirectional are fire-and-forget
    // inbound. Reverse ops are server-push only and have no inbound handler.
    let inbound: Vec<&CsilServiceOperation> = service
        .operations
        .iter()
        .filter(|op| {
            matches!(
                op.direction,
                CsilServiceDirection::Unidirectional | CsilServiceDirection::Bidirectional
            )
        })
        .collect();

    if inbound.is_empty() {
        out.push_str("  # Reverse-only service: the server only pushes, never receives.\n");
    } else {
        for (i, op) in inbound.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let method = ruby_method_name(&op.name);
            let has_input = !op_input_is_null(&op.input_type);
            let param = match (has_input, &op.direction) {
                (false, _) => String::new(),
                (true, CsilServiceDirection::Bidirectional) => "(msg)".to_string(),
                (true, _) => "(req)".to_string(),
            };
            if op.doc_comments.is_empty() {
                out.push_str(&format!("  # {}\n", op.name));
            } else {
                for line in &op.doc_comments {
                    out.push_str(&format!("  # {line}\n"));
                }
            }
            out.push_str(&format!("  def {method}{param}\n"));
            out.push_str(&format!(
                "    raise NotImplementedError, \"{handler_class}#{method}\"\n"
            ));
            out.push_str("  end\n");
        }
    }
    out.push_str("end\n\n");
    out
}

/// Emit the router module for a channel-bearing service: wire-id constants, the verbose
/// router (dispatch on the wire method string), the compact router twin (dispatch on
/// the `@wire-id` ordinal, only when wire-ids are present so wire-id-free output stays
/// byte-identical), and the per-op outbound encoders.
fn emit_router_module(name: &str, service: &CsilServiceDefinition) -> String {
    let base = wire_service_base(name);
    let router = format!("{base}Router");
    let mut out = String::new();

    out.push_str(&format!(
        "# Channel router and encoders for the {name} service.\n"
    ));
    out.push_str(&format!("module {router}\n"));
    out.push_str("  module_function\n\n");

    // Wire-id constants, additive: nothing emitted unless the service carries them.
    if let Some(consts) = emit_wire_id_consts(service) {
        out.push_str(&consts);
    }

    let bidi: Vec<&CsilServiceOperation> = service
        .operations
        .iter()
        .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
        .collect();

    // Verbose router: dispatch one inbound frame by its wire method name.
    out.push_str(&format!(
        "  # Decode one inbound channel frame for {name} and dispatch to a handler\n"
    ));
    out.push_str("  # method, keyed by the verbose wire method name.\n");
    out.push_str("  def route_channel(handlers, codec, method, data)\n");
    out.push_str("    case method\n");
    for op in &bidi {
        let wire = wire_method_name(&op.name);
        let method = ruby_method_name(&op.name);
        out.push_str(&format!("    when \"{wire}\"\n"));
        if op_input_is_null(&op.input_type) {
            out.push_str(&format!("      handlers.{method}\n"));
        } else {
            let ty = map_csil_type_to_ruby(&op.input_type);
            out.push_str(&format!("      msg = codec.decode(data, {ty})\n"));
            out.push_str(&format!("      handlers.{method}(msg)\n"));
        }
    }
    out.push_str("    else\n");
    out.push_str("      raise ArgumentError, \"unknown channel method #{method}\"\n");
    out.push_str("    end\n");
    out.push_str("  end\n\n");

    // Compact router twin: dispatch by @wire-id ordinal, only for wire-id services.
    if service.wire_id.is_some() {
        out.push_str("  # Compact transport profile: dispatch one inbound frame by its @wire-id\n");
        out.push_str("  # ordinal. The verbose twin is route_channel; the host calls whichever\n");
        out.push_str("  # matches the profile negotiated on the wire.\n");
        out.push_str("  def route_channel_compact(handlers, codec, op, data)\n");
        out.push_str("    case op\n");
        for op in &bidi {
            let Some(op_id) = op.wire_id else { continue };
            let method = ruby_method_name(&op.name);
            out.push_str(&format!("    when {op_id}\n"));
            if op_input_is_null(&op.input_type) {
                out.push_str(&format!("      handlers.{method}\n"));
            } else {
                let ty = map_csil_type_to_ruby(&op.input_type);
                out.push_str(&format!("      msg = codec.decode(data, {ty})\n"));
                out.push_str(&format!("      handlers.{method}(msg)\n"));
            }
        }
        out.push_str("    else\n");
        out.push_str("      raise ArgumentError, \"unknown channel ordinal #{op}\"\n");
        out.push_str("    end\n");
        out.push_str("  end\n\n");
    }

    // Outbound encoders for <-> and <- ops (server pushes Output to a peer).
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let wire = wire_method_name(&op.name);
        let method = ruby_method_name(&op.name);
        out.push_str(&format!(
            "  # Encode a `{wire}` message the server pushes to a peer; returns\n"
        ));
        out.push_str("  # [method, bytes] for the implementer to frame on its connection.\n");
        out.push_str(&format!("  def encode_{method}(codec, msg)\n"));
        out.push_str(&format!("    [\"{wire}\", codec.encode(msg)]\n"));
        out.push_str("  end\n\n");
    }

    // Trim the trailing blank line before the module's `end`.
    if out.ends_with("\n\n") {
        out.pop();
    }
    out.push_str("end\n\n");
    out
}

/// The wire-id constants for a router module, indented two spaces. Returns None for a
/// wire-id-free service so its output stays byte-identical.
fn emit_wire_id_consts(service: &CsilServiceDefinition) -> Option<String> {
    let service_id = service.wire_id?;
    let mut out = String::new();
    out.push_str("  # Wire-id ordinals (transport compact profiles).\n");
    out.push_str(&format!("  SERVICE_WIRE_ID = {service_id}\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            let const_name = format!("OP_{}_WIRE_ID", op.name.to_case(Case::ScreamingSnake));
            out.push_str(&format!("  {const_name} = {op_id}\n"));
        }
    }
    out.push('\n');
    Some(out)
}

/// Module-level wire-id constants emitted alongside a client class (so a host can
/// reference ordinals without a router). Additive: None when the service is wire-id-free.
fn emit_wire_ids(name: &str, service: &CsilServiceDefinition) -> Option<String> {
    let service_id = service.wire_id?;
    let base = wire_service_base(name);
    let module = format!("{base}WireIds");
    let mut out = String::new();
    out.push_str(&format!(
        "# Wire-id ordinals for the {name} service (transport compact profiles).\n"
    ));
    out.push_str(&format!("module {module}\n"));
    out.push_str(&format!("  SERVICE = {service_id}\n"));
    for op in &service.operations {
        if let Some(op_id) = op.wire_id {
            let const_name = format!("OP_{}", op.name.to_case(Case::ScreamingSnake));
            out.push_str(&format!("  {const_name} = {op_id}\n"));
        }
    }
    out.push_str("end\n\n");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{CsilPosition, CsilRule};

    #[test]
    fn class_and_method_naming() {
        assert_eq!(ruby_class_name("user_profile"), "UserProfile");
        assert_eq!(ruby_method_name("deposit-claim"), "deposit_claim");
        assert_eq!(wire_method_name("deposit-claim"), "DepositClaim");
        assert_eq!(wire_service_base("CorndogsService"), "Corndogs");
    }

    #[test]
    fn string_literal_escapes_interpolation() {
        assert_eq!(ruby_string_literal("a#{b}"), "\"a\\#{b}\"");
        assert_eq!(ruby_string_literal("x\"y"), "\"x\\\"y\"");
    }

    fn bare(name: &str, ty: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: None,
            metadata: vec![],
            doc_comments: vec![],
        }
    }

    fn opt(name: &str, ty: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            occurrence: Some(CsilOccurrence::Optional),
            ..bare(name, ty)
        }
    }

    fn builtin(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Builtin(name.to_string())
    }

    fn task_group() -> CsilGroupExpression {
        CsilGroupExpression {
            entries: vec![
                bare("uuid", builtin("text")),
                bare("current_state", builtin("text")),
                bare("payload", builtin("bytes")),
                opt("priority", builtin("int")),
                bare(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                ),
                bare(
                    "tags",
                    CsilTypeExpression::Array {
                        element_type: Box::new(builtin("text")),
                        occurrence: None,
                    },
                ),
            ],
        }
    }

    #[test]
    fn codec_keys_in_canonical_order() {
        let mut records = std::collections::HashSet::new();
        records.insert("Task".to_string());
        let aliases = std::collections::HashMap::new();
        let out = emit_record_codec("Task", &task_group(), &records, &aliases);
        assert!(out.contains("class Task"));
        assert!(out.contains("def to_cbor"));
        assert!(out.contains("def self.from_cbor(bytes)"));
        // text -> the value as-is; bytes -> forced binary (CBOR major type 2).
        assert!(out.contains("csil_map[\"uuid\"] = uuid"));
        assert!(out.contains("csil_map[\"payload\"] = (payload).b"));
        // An absent optional is omitted on encode.
        assert!(out.contains("csil_map[\"priority\"] = priority unless priority.nil?"));
        // Canonical RFC 8949 key order: tags(4) < uuid(4) < labels(6) < payload(7) <
        // priority(8) < current_state(13).
        let body = &out[out.find("csil_to_tree").unwrap()..];
        let p_tags = body.find("\"tags\"").unwrap();
        let p_uuid = body.find("\"uuid\"").unwrap();
        let p_state = body.find("\"current_state\"").unwrap();
        assert!(p_tags < p_uuid && p_uuid < p_state);
        // A missing optional decodes to nil.
        assert!(out.contains("priority: (node.key?(\"priority\") ? node[\"priority\"] : nil)"));
    }

    #[test]
    fn codec_file_carries_runtime_and_classes() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "Task".to_string(),
                rule_type: CsilRuleType::GroupDef(task_group()),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };
        let codec = generate_codec_file(&spec).expect("records present -> codec emitted");
        assert!(codec.starts_with("# frozen_string_literal: true\n"));
        assert!(codec.contains("require_relative \"types\""));
        assert!(codec.contains("module CsilCbor"));
        assert!(codec.contains("def encode(value)"));
        assert!(codec.contains("def decode(bytes)"));
        // No timestamp/decimal in this spec, so no extra requires or helpers.
        assert!(!codec.contains("require \"bigdecimal\""));
        assert!(!codec.contains("decimal_to_tag"));
    }

    #[test]
    fn no_codec_without_records() {
        let spec = CsilSpecSerialized {
            rules: vec![],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };
        assert!(generate_codec_file(&spec).is_none());
    }

    #[test]
    fn client_uses_byte_seam_and_codec() {
        let service = CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "submit-task".to_string(),
                input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                output_type: CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Task".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                direction: CsilServiceDirection::Unidirectional,
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        };
        let out = emit_client_class("CorndogsService", &service);
        // The request self-encodes, the reply self-decodes; the transport only moves
        // bytes. The ServiceError arm is dropped from the success type.
        assert!(out.contains(
            "Task.from_cbor(@transport.call(\"corndogs\", \"SubmitTask\", req.to_cbor))"
        ));
    }

    /// A spec with a record and a unary service, generated in package mode, so the
    /// README's full carrier Quickstart is exercised end to end.
    fn pingpong_spec() -> CsilSpecSerialized {
        let user = CsilRule {
            name: "user".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare("name", builtin("text")), bare("id", builtin("int"))],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: vec![],
        };
        let service = CsilRule {
            name: "user_service".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "get-user".to_string(),
                    input_type: CsilTypeExpression::Reference("user".to_string()),
                    output_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Reference("user".to_string()),
                        CsilTypeExpression::Reference("ServiceError".to_string()),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: vec![],
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: vec![],
        };
        CsilSpecSerialized {
            rules: vec![user, service],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        }
    }

    fn package_config() -> GeneratorConfig {
        let mut options = std::collections::HashMap::new();
        options.insert("emit_packages".to_string(), serde_json::json!(["ruby"]));
        options.insert("package_name".to_string(), serde_json::json!("user_client"));
        GeneratorConfig {
            target: "ruby-client".to_string(),
            output_dir: "/tmp/csilgen-ruby-readme".to_string(),
            options,
        }
    }

    #[test]
    fn package_readme_has_quickstart_carrier_and_example() {
        let files = generate_ruby_code_from_serialized(&pingpong_spec(), &package_config())
            .expect("package generation succeeded");
        let readme = files
            .iter()
            .find(|f| f.path == "README.md")
            .expect("README.md emitted in package mode");
        let body = &readme.content;

        // Title + the require for this gem.
        assert!(body.starts_with("# user_client\n"));
        assert!(body.contains("require \"user_client\""));

        // The carrier implements the generated transport seam.
        assert!(body.contains("class CsilRpcTransport"));
        assert!(body.contains("def call(service, op, req_bytes)"));

        // It POSTs to the CSIL-RPC endpoint, wraps the payload in CBOR tag 24, and
        // reuses the generated codec (no third-party gem).
        assert!(body.contains("/csil/v1/rpc"));
        assert!(body.contains("Net::HTTP::Post"));
        assert!(body.contains("CsilCbor::Tag.new(24, req_bytes.b)"));
        assert!(body.contains("CsilCbor.encode(envelope)"));
        assert!(body.contains("CsilCbor.decode"));

        // The status / typed ServiceError arms are handled.
        assert!(body.contains("transport status"));
        assert!(body.contains("if reply[\"variant\"] == \"ServiceError\""));
        assert!(body.contains("service error #{err[\"code\"]}"));

        // Client construction over the carrier + the first unary call with a generated
        // sample literal (required fields only, verbatim CSIL field names).
        assert!(body.contains("UserClient.new(CsilRpcTransport.new(\"http://localhost:5080\"))"));
        assert!(body.contains("resp = client.get_user(User.new(name: \"example\", id: 0))"));
    }

    #[test]
    fn serviceless_package_readme_falls_back_to_types_only() {
        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "user".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![bare("name", builtin("text"))],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        };
        let mut config = package_config();
        config.target = "ruby-typesonly".to_string();
        let files = generate_ruby_code_from_serialized(&spec, &config)
            .expect("package generation succeeded");
        let readme = files
            .iter()
            .find(|f| f.path == "README.md")
            .expect("README.md emitted");
        // No carrier for a types-only package; the consume section appears instead.
        assert!(!readme.content.contains("class CsilRpcTransport"));
        assert!(readme.content.contains("require its generated value"));
    }
}
