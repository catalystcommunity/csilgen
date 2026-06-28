//! Elixir code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target elixir` from `csilgen_elixir_generator.wasm`.
//! Emits idiomatic Elixir source — structs with `@type t`/`@enforce_keys`, tagged
//! tuples for handler outcomes, `@behaviour` server handlers, typed clients, and
//! verbose/compact routers — but never the wire bytes. The hand-rolled canonical
//! CBOR codec and the envelopes live in the `:csilgen_transport` library, not here.

use csilgen_common::{
    CsilControlOperator, CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry,
    CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRuleType,
    CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::{HashMap, HashSet};

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "elixir-generator".to_string(),
        version: "0.1.0".to_string(),
        description: "Elixir code generator".to_string(),
        target: "elixir".to_string(),
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
    match deserialize_input(input_ptr, input_len) {
        Ok(input) => match process_generation(input) {
            Ok(output) => write_json(&output),
            Err(_) => std::ptr::null_mut(),
        },
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

fn deserialize_input(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorInput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

/// Which surface a (sub-)target emits. The base `elixir` (and explicit
/// `elixir-server`) emits server handler behaviours + routers; `elixir-client`
/// emits typed clients; `elixir-typesonly` emits the structs alone.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

/// The stock module root. A custom root drives a derived package name; the stock
/// one falls back to the documented default so the common case is stable.
const DEFAULT_MODULE_ROOT: &str = "Csilgen.Generated";

/// The package name used when nothing else can supply one (no explicit
/// `package_name`, stock module root).
const DEFAULT_PACKAGE_NAME: &str = "csilgen_client";

struct ElixirConfig {
    /// Root module namespace, e.g. `MyApp` → `MyApp.DepositClaimRequest`.
    module_root: String,
    generate_validation: bool,
    generate_constructors: bool,
    /// When set, the output directory is laid out as a publishable Mix project:
    /// a `mix.exs` at the root and the generated modules under `lib/`.
    emit_elixir_package: bool,
    /// The Mix app/package name (a snake_case atom), resolved from `package_name`,
    /// a derivation of the module root, or the documented fallback.
    package_name: String,
    /// The package version string emitted into `mix.exs`.
    package_version: String,
}

impl ElixirConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Result<Self, i32> {
        // Elixir has no exact-decimal type in the stdlib, so both `csil` and
        // `library` map to the same in-memory shape (a host-supplied Decimal
        // struct); an unrecognized value is a hard error, matching the other
        // generators, so a typo never silently degrades.
        if let Some(v) = options.get("decimal_mapping") {
            match v.as_str() {
                Some("csil") | Some("library") => {}
                _ => return Err(error_codes::GENERATION_ERROR),
            }
        }
        let module_root = options
            .get("elixir_module")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_MODULE_ROOT)
            .to_string();

        // `emit_packages` is a free-form JSON array authored by the caller, so every
        // step is fallible-but-tolerant: a missing key, a non-array, or non-string
        // members all degrade to "no package", never an error.
        let emit_elixir_package = options
            .get("emit_packages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|e| e.as_str() == Some("elixir")))
            .unwrap_or(false);

        let package_name = options
            .get("package_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(snake_case)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| derive_package_name(&module_root));

        let package_version = options
            .get("package_version")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("0.1.0")
            .to_string();

        Ok(Self {
            module_root,
            generate_validation: options
                .get("generate_validation")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            generate_constructors: options
                .get("generate_constructors")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            emit_elixir_package,
            package_name,
            package_version,
        })
    }

    /// The fully-qualified module name for a CSIL type/service reference.
    fn module(&self, name: &str) -> String {
        format!("{}.{}", self.module_root, pascal_case(name))
    }
}

/// Derive a Mix app name from the module root by snake_casing each dotted segment.
/// The stock root carries no caller intent, so it maps to the documented fallback
/// rather than the literal `csilgen_generated`.
fn derive_package_name(module_root: &str) -> String {
    if module_root == DEFAULT_MODULE_ROOT {
        return DEFAULT_PACKAGE_NAME.to_string();
    }
    let derived = module_root
        .split('.')
        .map(snake_case)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if derived.is_empty() {
        DEFAULT_PACKAGE_NAME.to_string()
    } else {
        derived
    }
}

/// The `mix.exs` for the publishable package: a minimal `def project` with the app
/// atom, version, an Elixir requirement, and empty deps (the generated tree pulls in
/// nothing third-party — its CBOR codec is self-contained). The MixProject module is
/// PascalCased from the app name so two packages never collide on `MixProject`.
fn mix_exs(config: &ElixirConfig) -> String {
    let app = &config.package_name;
    let version = &config.package_version;
    let project_module = pascal_case(app);
    format!(
        "defmodule {project_module}.MixProject do\n  \
         use Mix.Project\n\n  \
         def project do\n    [\n      \
         app: :{app},\n      \
         version: \"{version}\",\n      \
         elixir: \"~> 1.14\",\n      \
         deps: deps()\n    ]\n  \
         end\n\n  \
         defp deps do\n    []\n  end\nend\n"
    )
}

/// Rewrite the emitted file set into a Mix project layout: generated modules move
/// under `lib/`, a `mix.exs` lands at the root, and the `.formatter.exs` already
/// at the root is left in place (config, not a module). Only `.ex` modules move —
/// `.exs` scripts and `mix.exs` stay at the root where Mix expects them.
fn apply_elixir_package(files: &mut Vec<GeneratedFile>, config: &ElixirConfig) {
    for file in files.iter_mut() {
        if file.path.ends_with(".ex") {
            file.path = format!("lib/{}", file.path);
        }
    }
    files.push(GeneratedFile {
        path: "mix.exs".to_string(),
        content: mix_exs(config),
    });
}

// ---------------------------------------------------------------------------
// Package README with a copy-paste CSIL-RPC Quickstart
// ---------------------------------------------------------------------------

/// The package README, with a copy-paste **Quickstart**. For a client package the
/// Quickstart is a complete, dependency-free CSIL-RPC carrier — it reuses this
/// package's own generated `<root>.Cbor` codec to build/parse the envelope, so it
/// adds no third-party dependency (hybrid posture path 1) — plus the typed client and
/// one example call. A spec with no client-callable unary op falls back to a
/// types-only consume section.
fn readme(input: &WasmGeneratorInput, config: &ElixirConfig) -> String {
    let pkg = &config.package_name;
    let mut out = format!(
        "# {pkg}\n\n\
         Generated by csilgen. A typed, transport-agnostic CSIL-RPC client: the\n\
         generated codec owns CBOR (de)serialization; you supply a *carrier* that only\n\
         moves bytes.\n\n\
         ## Install\n\n\
         ```elixir\n\
         # TODO: publish {pkg} to Hex, then add it to your mix.exs deps:\n\
         def deps do\n  \
         [{{:{pkg}, \"~> {ver}\"}}]\n\
         end\n\
         # or, from a local checkout: {{:{pkg}, path: \"..\"}}\n\
         ```\n\n",
        ver = config.package_version
    );

    match first_unary_example(input, config) {
        Some(example) => out.push_str(&readme_quickstart(config, &example)),
        None => {
            let root = &config.module_root;
            out.push_str(&format!(
                "## Quickstart\n\n\
                 This package has no client-callable service operations — use its\n\
                 generated structs and codec directly:\n\n\
                 ```elixir\n\
                 # bytes = {root}.MyType.to_cbor(%{root}.MyType{{...}})\n\
                 # value = {root}.MyType.from_cbor(bytes)\n\
                 ```\n"
            ));
        }
    }
    out
}

/// The client Quickstart: a dependency-free CSIL-RPC carrier (it reuses this
/// package's generated `Cbor` codec for the envelope), the typed client built over
/// it, and the first unary call with a generated sample request literal.
fn readme_quickstart(config: &ElixirConfig, ex: &UnaryExample) -> String {
    let mut out = String::from("## Quickstart\n\n");
    out.push_str(
        "A complete CSIL-RPC carrier (no third-party deps — it reuses this package's\n\
         generated `Cbor` codec for the envelope) plus the typed client. Change the one\n\
         base-URL string.\n\n",
    );
    out.push_str("```elixir\n");
    out.push_str(&carrier_elixir(config));
    out.push('\n');
    // The example: construct the typed client over the carrier and call the first op.
    out.push_str("transport = CsilRpcTransport.new(\"http://localhost:5080\")\n");
    out.push_str(&format!("client = {}.new(transport)\n", ex.client_module));
    if ex.has_request {
        out.push_str(&format!(
            "resp = {}.{}(client, {})\n",
            ex.client_module, ex.method, ex.sample
        ));
    } else {
        out.push_str(&format!(
            "resp = {}.{}(client)\n",
            ex.client_module, ex.method
        ));
    }
    out.push_str("IO.inspect(resp)\n");
    out.push_str("```\n");
    out
}

/// The carrier: a struct implementing the generated `<root>.Transport` behaviour. It
/// wraps the already-encoded request in a `CsilRpcRequest` envelope (tag-24 payload)
/// built dep-free with the generated `Cbor` codec, POSTs it to `{base}/csil/v1/rpc`
/// over raw `:gen_tcp` (kernel app — no `:inets`/`:ssl` dependency), and returns the
/// response payload bytes for the generated client to decode. A non-2xx HTTP status, a
/// non-zero transport `status`, or a typed `ServiceError` arm is raised as an error.
fn carrier_elixir(config: &ElixirConfig) -> String {
    let root = &config.module_root;
    // The behaviour and codec module names are spec-derived (module root), so the
    // carrier is a format! rather than a constant.
    format!(
        r#"defmodule CsilRpcTransport do
  # Dependency path 1: reuse the generated `{root}.Cbor` codec (encode/decode + the
  # tagged value tree) to build and parse the CSIL-RPC envelope, so this carrier adds
  # no third-party dep. It owns only the CSIL-RPC envelope + HTTP; never your types.
  @behaviour {root}.Transport

  alias {root}.Cbor

  defstruct [:rpc_url]

  def new(base_url),
    do: %__MODULE__{{rpc_url: String.trim_trailing(base_url, "/") <> "/csil/v1/rpc"}}

  # The generated client calls this seam with the already-encoded request bytes.
  @impl true
  def call(%__MODULE__{{rpc_url: url}}, service, op, req) when is_binary(req) do
    # CsilRpcRequest = {{ v, service, op, payload: #6.24(bstr) }}: the payload is the
    # encoded request wrapped in CBOR tag 24 (embedded CBOR).
    envelope =
      {{:map,
       [
         {{{{:text, "v"}}, {{:int, 1}}}},
         {{{{:text, "service"}}, {{:text, service}}}},
         {{{{:text, "op"}}, {{:text, op}}}},
         {{{{:text, "payload"}}, {{:tag, 24, {{:bytes, req}}}}}}
       ]}}

    body =
      case post(url, Cbor.encode(envelope)) do
        {{:ok, b}} -> b
        {{:error, reason}} -> raise "csil-rpc #{{service}}/#{{op}}: #{{inspect(reason)}}"
      end

    {{:map, kvs}} = Cbor.decode(body)
    status = Cbor.to_int(fetch!(kvs, "status"))
    if status != 0, do: raise("csil-rpc #{{service}}/#{{op}}: transport status #{{status}}")

    {{:tag, 24, {{:bytes, inner}}}} = fetch!(kvs, "payload")

    # A typed `ServiceError` arm (variant "ServiceError") is an application error,
    # distinct from a transport failure; decode it dep-free with the same codec.
    case fetch(kvs, "variant") do
      {{:text, "ServiceError"}} ->
        {{:map, ekvs}} = Cbor.decode(inner)
        code = Cbor.to_int(fetch!(ekvs, "code"))
        message = Cbor.to_text(fetch!(ekvs, "message"))
        raise "service error #{{code}}: #{{message}}"

      _ ->
        inner
    end
  end

  defp fetch(kvs, key) do
    Enum.find_value(kvs, fn
      {{{{:text, ^key}}, v}} -> v
      _ -> false
    end) || nil
  end

  defp fetch!(kvs, key),
    do: fetch(kvs, key) || raise("csil-rpc: response envelope missing #{{key}}")

  # --- HTTP over :gen_tcp (kernel app; no :inets/:ssl dependency) ---
  defp post(url, body) do
    uri = URI.parse(url)
    host = String.to_charlist(uri.host)
    port = uri.port || 80
    path = uri.path || "/"

    request = [
      "POST ", path, " HTTP/1.1\r\n",
      "Host: ", uri.host, "\r\n",
      "Content-Type: application/cbor\r\n",
      "Content-Length: ", Integer.to_string(byte_size(body)), "\r\n",
      "Connection: close\r\n\r\n",
      body
    ]

    with {{:ok, sock}} <- :gen_tcp.connect(host, port, [:binary, active: false, packet: :raw]),
         :ok <- :gen_tcp.send(sock, IO.iodata_to_binary(request)),
         {{:ok, raw}} <- recv_all(sock, <<>>) do
      :gen_tcp.close(sock)
      parse_http(raw)
    end
  end

  defp recv_all(sock, acc) do
    case :gen_tcp.recv(sock, 0, 10_000) do
      {{:ok, data}} -> recv_all(sock, acc <> data)
      {{:error, :closed}} -> {{:ok, acc}}
      {{:error, reason}} -> {{:error, reason}}
    end
  end

  defp parse_http(raw) do
    case :binary.split(raw, "\r\n\r\n") do
      [head, body] ->
        code =
          head |> :binary.split("\r\n") |> hd() |> String.split(" ") |> Enum.at(1) |> String.to_integer()

        if code in 200..299, do: {{:ok, body}}, else: {{:error, {{:http, code}}}}

      _ ->
        {{:error, :bad_http_response}}
    end
  end
end
"#
    )
}

/// The pieces the Quickstart's example call needs: which client module + function to
/// call and a constructible sample request literal (empty when the op takes no input).
struct UnaryExample {
    client_module: String,
    method: String,
    has_request: bool,
    sample: String,
}

/// The first service (in rule order, matching the emitted client order) with a unary
/// `->` op the generated client actually exposes (record success, and a null or record
/// request — the only shapes `emit_client_module` turns into a callable method),
/// reduced to an example call. `None` falls back to the types-only README.
fn first_unary_example(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<UnaryExample> {
    let records = record_csil_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
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
            let base = service_base(&rule.name);
            return Some(UnaryExample {
                client_module: format!("{}.{base}Client", config.module_root),
                method: snake_case(&op.name),
                has_request: !null_input,
                sample: if null_input {
                    String::new()
                } else {
                    struct_literal(input, config, &ref_name(&op.input_type))
                },
            });
        }
    }
    None
}

/// `%Root.Type{field: <sample>, ...}` over a record's required fields, keyed by the
/// struct field atoms the generated struct uses.
fn struct_literal(input: &WasmGeneratorInput, config: &ElixirConfig, name: &str) -> String {
    let module = config.module(name);
    let Some(group) = find_record(input, name) else {
        return format!("%{module}{{}}");
    };
    let fields: Vec<String> = group
        .entries
        .iter()
        .filter(|e| !is_optional(e))
        .filter_map(|e| {
            field_atom(e)
                .map(|atom| format!("{atom}: {}", elixir_sample(input, config, &e.value_type)))
        })
        .collect();
    if fields.is_empty() {
        format!("%{module}{{}}")
    } else {
        format!("%{module}{{{}}}", fields.join(", "))
    }
}

/// A constructible Elixir literal for `ty`: real values for scalars/collections and a
/// struct literal for a record reference (required fields only). Shapes a generic
/// sample can't fabricate fall back to `nil`, which the user fills in.
fn elixir_sample(
    input: &WasmGeneratorInput,
    config: &ElixirConfig,
    ty: &CsilTypeExpression,
) -> String {
    let base = match ty {
        CsilTypeExpression::Constrained { base_type, .. } => base_type.as_ref(),
        other => other,
    };
    match base {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "<<>>".to_string(),
            "int" | "uint" => "0".to_string(),
            "float" => "0.0".to_string(),
            _ => "nil".to_string(),
        },
        CsilTypeExpression::Array { .. } => "[]".to_string(),
        CsilTypeExpression::Map { .. } => "%{}".to_string(),
        CsilTypeExpression::Reference(name) => match find_record(input, name) {
            Some(_) => struct_literal(input, config, name),
            None => "nil".to_string(),
        },
        _ => "nil".to_string(),
    }
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

fn process_generation(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = ElixirConfig::from_options(&input.config.options)?;
    let surface = match input.config.target.as_str() {
        "elixir" | "elixir-server" => Surface::Server,
        "elixir-client" => Surface::Client,
        "elixir-typesonly" => Surface::TypesOnly,
        _ => return Err(error_codes::GENERATION_ERROR),
    };

    let mut files = Vec::new();
    let mut warnings = Vec::new();

    if let Some(types) = generate_types(&input, &config, &mut warnings) {
        files.push(GeneratedFile {
            path: "types.gen.ex".to_string(),
            content: types,
        });
    }

    // The self-contained canonical-CBOR value codec rides alongside the types (like
    // the OCaml/Go targets): emitted whenever the spec declares a record, since every
    // payload now (de)serializes through it rather than a host-supplied reflection
    // codec. It carries text-vs-bytes explicitly because both are Elixir binaries.
    if let Some(codec) = generate_codec(&input, &config) {
        files.push(GeneratedFile {
            path: "codec.gen.ex".to_string(),
            content: codec,
        });
    }

    if input.csil_spec.service_count > 0 {
        match surface {
            Surface::Client => {
                if let Some(client) = generate_clients(&input, &config) {
                    files.push(GeneratedFile {
                        path: "client.gen.ex".to_string(),
                        content: client,
                    });
                }
            }
            Surface::Server => {
                if let Some(server) = generate_servers(&input, &config) {
                    files.push(GeneratedFile {
                        path: "server.gen.ex".to_string(),
                        content: server,
                    });
                }
            }
            Surface::TypesOnly => {}
        }
    }

    if config.generate_validation
        && let Some(validation) = generate_validation(&input, &config)
    {
        files.push(GeneratedFile {
            path: "validation.gen.ex".to_string(),
            content: validation,
        });
    }

    if config.generate_constructors
        && let Some(constructors) = generate_constructors(&input, &config)
    {
        files.push(GeneratedFile {
            path: "constructors.gen.ex".to_string(),
            content: constructors,
        });
    }

    // A `.formatter.exs` ships alongside so the generated tree runs cleanly
    // through `mix format`, matching the gofmt-stability of the Go target. In
    // package mode the modules live under `lib/`, so the inputs glob must reach
    // there rather than the flat-layout `*.ex`.
    let formatter_inputs = if config.emit_elixir_package {
        "[inputs: [\"mix.exs\", \"lib/**/*.ex\"]]\n"
    } else {
        "[inputs: [\"*.ex\", \"*.exs\"]]\n"
    };
    files.push(GeneratedFile {
        path: ".formatter.exs".to_string(),
        content: formatter_inputs.to_string(),
    });

    // In package mode the output directory must be a valid, publishable Mix project:
    // modules under `lib/`, a `mix.exs` at the root. The default flat layout is left
    // untouched otherwise.
    if config.emit_elixir_package {
        apply_elixir_package(&mut files, &config);
        // The README rides with the publishable package only; its Quickstart names the
        // generated modules, so it stays at the root (Mix expects README.md there).
        files.push(GeneratedFile {
            path: "README.md".to_string(),
            content: readme(&input, &config),
        });
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

const GEN_HEADER: &str = "# Code generated by csilgen; DO NOT EDIT.\n\n";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn generate_types(
    input: &WasmGeneratorInput,
    config: &ElixirConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Option<String> {
    let mut content = String::new();
    content.push_str(GEN_HEADER);
    let mut emitted = false;
    let records = record_csil_names(input);
    let aliases = codec_aliases(input);

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            emit_struct_module(
                &mut content,
                &rule.name,
                group,
                config,
                &records,
                &aliases,
                warnings,
            );
            emitted = true;
        } else if let CsilRuleType::TypeDef(type_expr) = &rule.rule_type {
            // A bare type alias has no struct, but a `@type` keeps the name visible
            // and usable in specs that reference it.
            let module = config.module(&rule.name);
            let mapped = map_type(type_expr, config);
            content.push_str(&format!("defmodule {module} do\n"));
            content.push_str(&format!("  @moduledoc \"Type alias for {}.\"\n", rule.name));
            content.push_str(&format!("  @type t :: {mapped}\n"));
            content.push_str("end\n\n");
            emitted = true;
        }
    }

    if emitted { Some(content) } else { None }
}

fn emit_struct_module(
    content: &mut String,
    name: &str,
    group: &CsilGroupExpression,
    config: &ElixirConfig,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let module = config.module(name);
    let keyed: Vec<(&CsilGroupEntry, String)> = group
        .entries
        .iter()
        .filter_map(|e| field_atom(e).map(|atom| (e, atom)))
        .collect();

    // @enforce_keys are the required fields with no default: a defaulted field
    // can't be enforced (it always has a value), and an optional field is nilable.
    let enforced: Vec<&str> = keyed
        .iter()
        .filter(|(e, _)| !is_optional(e) && entry_default_value(e).is_none())
        .map(|(_, a)| a.as_str())
        .collect();

    content.push_str(&format!("defmodule {module} do\n"));
    content.push_str(&format!(
        "  @moduledoc \"Generated struct for the {name} type.\"\n\n"
    ));

    if !enforced.is_empty() {
        content.push_str(&format!(
            "  @enforce_keys [{}]\n",
            enforced
                .iter()
                .map(|a| format!(":{a}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // defstruct: a field with a default carries it; everything else defaults to nil.
    let struct_fields: Vec<String> = keyed
        .iter()
        .map(|(e, atom)| match entry_default_value(e) {
            Some(value) => format!("{atom}: {}", literal_to_elixir(value)),
            None => format!(":{atom}"),
        })
        .collect();
    content.push_str(&format!("  defstruct [{}]\n\n", struct_fields.join(", ")));

    // @type t with one line per field.
    content.push_str("  @type t :: %__MODULE__{\n");
    for (i, (entry, atom)) in keyed.iter().enumerate() {
        let mut ty = map_type(&entry.value_type, config);
        if is_optional(entry) {
            ty = format!("{ty} | nil");
        }
        let comma = if i + 1 < keyed.len() { "," } else { "" };
        content.push_str(&format!("          {atom}: {ty}{comma}\n"));

        if matches!(visibility(&entry.metadata), CsilFieldVisibility::SendOnly) {
            warnings.push(GeneratorWarning {
                level: WarningLevel::Info,
                message: format!(
                    "Field '{atom}' on '{name}' is @send-only; consider separate request/response types"
                ),
                location: None,
                suggestion: None,
            });
        }
    }
    content.push_str("        }\n");

    // The verbatim CBOR wire keys (snake_case, never atomized on the wire) live in
    // a module attribute so a hand-written encoder/decoder can map struct keys to
    // the exact map keys the conformance contract requires.
    let wire_pairs: Vec<String> = keyed
        .iter()
        .map(|(e, atom)| format!("{atom}: \"{}\"", wire_key(e)))
        .collect();
    content.push_str(&format!("\n  @wire_keys [{}]\n", wire_pairs.join(", ")));
    content.push_str("  @doc \"Maps struct field atoms to their verbatim CBOR wire keys.\"\n");
    content.push_str("  @spec wire_keys() :: keyword()\n");
    content.push_str("  def wire_keys, do: @wire_keys\n");

    emit_struct_codec(content, group, config, records, aliases);

    content.push_str("end\n\n");
}

// ---------------------------------------------------------------------------
// Per-type CBOR codec (codec.gen.ex + per-struct to_cbor/from_cbor)
// ---------------------------------------------------------------------------

/// The set of CSIL record (group) names — references to these get a generated
/// codec call, anything else falls back to a runtime `raise` in the codec body.
fn record_csil_names(input: &WasmGeneratorInput) -> HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::GroupDef(_) => Some(r.name.clone()),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => Some(r.name.clone()),
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through: a `TypeDef` whose target
/// is a map / array / scalar / reference / tuple / constrained (NOT a record group or
/// a choice, which have their own handling). A field referencing one has no codec of
/// its own, so it must encode/decode as the underlying type rather than the runtime
/// `raise` a bare non-record reference would otherwise yield.
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

/// The CBOR encoding of a text key (major type 3 head + bytes); comparing these
/// byte vectors lexicographically is RFC 8949 §4.2.1 canonical key ordering,
/// computed once at generation time so a record's map is canonical without a
/// runtime sort.
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

/// One codec field: its struct atom, the verbatim CBOR wire key, its value type,
/// and whether it is optional. `key_bytes` drives the canonical sort.
struct CodecField<'a> {
    atom: String,
    wire: String,
    key_bytes: Vec<u8>,
    value_type: &'a CsilTypeExpression,
    optional: bool,
}

/// The codec fields of a record, sorted into canonical CBOR map-key order so the
/// emitted map matches the wire contract byte-for-byte without a runtime sort.
fn codec_fields(group: &CsilGroupExpression) -> Vec<CodecField<'_>> {
    let mut fields: Vec<CodecField> = group
        .entries
        .iter()
        .filter_map(|entry| {
            let atom = field_atom(entry)?;
            let wire = match entry.key.as_ref()? {
                CsilGroupKey::Bare(name) => name.clone(),
                CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
                _ => return None,
            };
            Some(CodecField {
                key_bytes: cbor_text_key_bytes(&wire),
                atom,
                wire,
                value_type: &entry.value_type,
                optional: is_optional(entry),
            })
        })
        .collect();
    fields.sort_by(|a, b| a.key_bytes.cmp(&b.key_bytes));
    fields
}

/// An Elixir expression building the CBOR value tree for `expr` (a value of the
/// field's in-memory type). A shape the codec cannot model (a non-record reference,
/// a decimal whose host type is unknown) raises at runtime so the module still
/// compiles and the supported paths stay total.
fn enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    config: &ElixirConfig,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            enc_value(base_type, expr, config, records, aliases)
        }
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "nint" => format!("{{:int, {expr}}}"),
            "float" | "double" | "float16" | "float32" | "float64" => {
                format!("{{:float, {expr}}}")
            }
            "text" | "tstr" => format!("{{:text, {expr}}}"),
            "bytes" | "bstr" => format!("{{:bytes, {expr}}}"),
            "bool" | "true" | "false" => format!("{{:bool, {expr}}}"),
            // CBOR tag 0 RFC3339 UTC instant; normalize through ISO 8601 text.
            "timestamp" => format!("{{:tag, 0, {{:text, DateTime.to_iso8601({expr})}}}}"),
            // The decimal in-memory type is host-supplied, so tag 4 encoding needs
            // host wiring the generator cannot assume; raise rather than guess.
            "decimal" => "raise(\"csilgen: decimal codec needs host wiring\")".to_string(),
            "null" | "nil" | "undefined" => format!("(_ = {expr}; :null)"),
            other => enc_named(other, expr, config, records, aliases),
        },
        CsilTypeExpression::Reference(name) => enc_named(name, expr, config, records, aliases),
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = enc_value(element_type, "csil_e", config, records, aliases);
            format!("{{:array, Enum.map({expr}, fn csil_e -> {inner} end)}}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let ek = enc_value(key, "csil_k", config, records, aliases);
            let ev = enc_value(value, "csil_v", config, records, aliases);
            format!("{{:map, Enum.map({expr}, fn {{csil_k, csil_v}} -> {{{ek}, {ev}}} end)}}")
        }
        _ => "raise(\"csilgen: no codec for this field shape\")".to_string(),
    }
}

/// Encode a reference to a named type: a record delegates to its generated
/// `to_cbor_value/1`. A transparent alias (`StringInt64Map = {* text => int}`,
/// `Tags = [* text]`, `Uuid = text`) has no codec of its own, so its underlying
/// type drives the encoding — the in-memory value (a map/list/scalar) flows through
/// unchanged. A named map alias `{* text => Rec}` thus routes each entry value to
/// the referenced record module's `to_cbor_value/1`. Anything else raises.
fn enc_named(
    name: &str,
    expr: &str,
    config: &ElixirConfig,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
) -> String {
    if records.contains(name) {
        format!("{}.to_cbor_value({expr})", config.module(name))
    } else if let Some(underlying) = aliases.get(name) {
        enc_value(underlying, expr, config, records, aliases)
    } else {
        format!("raise(\"csilgen: no codec for type {name}\")")
    }
}

/// An Elixir expression decoding `expr` (a CBOR value tree) into the field's
/// in-memory value.
fn dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    config: &ElixirConfig,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
) -> String {
    let root = &config.module_root;
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            dec_value(base_type, expr, config, records, aliases)
        }
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "nint" => format!("{root}.Cbor.to_int({expr})"),
            "float" | "double" | "float16" | "float32" | "float64" => {
                format!("{root}.Cbor.to_float({expr})")
            }
            "text" | "tstr" => format!("{root}.Cbor.to_text({expr})"),
            "bytes" | "bstr" => format!("{root}.Cbor.to_bytes({expr})"),
            "bool" | "true" | "false" => format!("{root}.Cbor.to_bool({expr})"),
            "timestamp" => format!(
                "(case {expr} do {{:tag, 0, {{:text, csil_s}}}} -> elem(DateTime.from_iso8601(csil_s), 1) end)"
            ),
            "decimal" => "raise(\"csilgen: decimal codec needs host wiring\")".to_string(),
            "null" | "nil" | "undefined" => format!("(_ = {expr}; nil)"),
            other => dec_named(other, expr, config, records, aliases),
        },
        CsilTypeExpression::Reference(name) => dec_named(name, expr, config, records, aliases),
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = dec_value(element_type, "csil_e", config, records, aliases);
            format!(
                "(case {expr} do {{:array, csil_xs}} -> Enum.map(csil_xs, fn csil_e -> {inner} end) end)"
            )
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let dk = dec_value(key, "csil_k", config, records, aliases);
            let dv = dec_value(value, "csil_v", config, records, aliases);
            format!(
                "(case {expr} do {{:map, csil_kvs}} -> Map.new(csil_kvs, fn {{csil_k, csil_v}} -> {{{dk}, {dv}}} end) end)"
            )
        }
        _ => "raise(\"csilgen: no codec for this field shape\")".to_string(),
    }
}

/// Decode a reference to a named type: a record delegates to its generated
/// `from_cbor_value/1`. A transparent alias decodes as its underlying type — the
/// reconstructed map/list/scalar is the alias-typed field value. A named map alias
/// `{* text => Rec}` thus routes each entry value through the referenced record
/// module's `from_cbor_value/1`. Anything else raises.
fn dec_named(
    name: &str,
    expr: &str,
    config: &ElixirConfig,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
) -> String {
    if records.contains(name) {
        format!("{}.from_cbor_value({expr})", config.module(name))
    } else if let Some(underlying) = aliases.get(name) {
        dec_value(underlying, expr, config, records, aliases)
    } else {
        format!("raise(\"csilgen: no codec for type {name}\")")
    }
}

/// Emit the per-struct codec surface inside a record's module: `to_cbor_value/1`
/// and `from_cbor_value/1` over the shared CBOR value tree, plus the
/// `to_cbor/1`/`from_cbor/1` byte wrappers the typed client calls.
fn emit_struct_codec(
    content: &mut String,
    group: &CsilGroupExpression,
    config: &ElixirConfig,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
) {
    let root = &config.module_root;
    let fields = codec_fields(group);

    content.push_str("\n  @doc \"Builds the canonical CBOR value tree for this struct.\"\n");
    content.push_str(&format!(
        "  @spec to_cbor_value(t()) :: {root}.Cbor.value()\n"
    ));
    content.push_str("  def to_cbor_value(%__MODULE__{} = v) do\n");
    content.push_str("    {:map,\n     Enum.reject(\n       [\n");
    for f in &fields {
        let access = format!("v.{}", f.atom);
        let enc = enc_value(f.value_type, &access, config, records, aliases);
        if f.optional {
            // An absent optional is omitted from the map, never present-with-null.
            content.push_str(&format!(
                "         (if is_nil({access}), do: nil, else: {{{{:text, \"{wire}\"}}, {enc}}}),\n",
                wire = f.wire
            ));
        } else {
            content.push_str(&format!(
                "         {{{{:text, \"{wire}\"}}, {enc}}},\n",
                wire = f.wire
            ));
        }
    }
    content.push_str("       ],\n       &is_nil/1\n     )}\n  end\n");

    content.push_str("\n  @doc \"Reconstructs this struct from a decoded CBOR value tree.\"\n");
    content.push_str("  @spec from_cbor_value(term()) :: t()\n");
    content.push_str("  def from_cbor_value({:map, csil_kvs}) do\n");
    content.push_str("    csil_fields = Map.new(csil_kvs)\n");
    content.push_str("    %__MODULE__{\n");
    for f in &fields {
        if f.optional {
            let dec = dec_value(f.value_type, "csil_v", config, records, aliases);
            content.push_str(&format!(
                "      {atom}: (case Map.get(csil_fields, {{:text, \"{wire}\"}}) do nil -> nil; csil_v -> {dec} end),\n",
                atom = f.atom,
                wire = f.wire
            ));
        } else {
            let fetch = format!("Map.fetch!(csil_fields, {{:text, \"{}\"}})", f.wire);
            let dec = dec_value(f.value_type, &fetch, config, records, aliases);
            content.push_str(&format!("      {atom}: {dec},\n", atom = f.atom));
        }
    }
    content.push_str("    }\n  end\n");

    content.push_str("\n  @doc \"Encodes this struct to canonical CBOR bytes.\"\n");
    content.push_str("  @spec to_cbor(t()) :: binary()\n");
    content.push_str(&format!(
        "  def to_cbor(v), do: {root}.Cbor.encode(to_cbor_value(v))\n"
    ));

    content.push_str("\n  @doc \"Decodes canonical CBOR bytes into this struct.\"\n");
    content.push_str("  @spec from_cbor(binary()) :: t()\n");
    content.push_str(&format!(
        "  def from_cbor(bytes), do: from_cbor_value({root}.Cbor.decode(bytes))\n"
    ));
}

/// Build `codec.gen.ex`: the shared self-contained canonical-CBOR value codec the
/// per-struct `to_cbor`/`from_cbor` build on. `None` when the spec declares no
/// record types (nothing references the codec).
fn generate_codec(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<String> {
    if record_csil_names(input).is_empty() {
        return None;
    }
    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&format!("defmodule {}.Cbor do\n", config.module_root));
    content.push_str(CODEC_RUNTIME_ELIXIR);
    content.push_str("end\n");
    Some(content)
}

// ---------------------------------------------------------------------------
// Clients
// ---------------------------------------------------------------------------

/// The transport seam every generated client delegates to. Defined once at the top
/// of the client file: a `@behaviour` plus a dispatch helper. The transport is an
/// opaque host-owned struct whose module implements `call/4` — the generator never
/// owns the wire.
fn client_prelude(config: &ElixirConfig) -> String {
    let root = &config.module_root;
    let mut out = String::new();
    out.push_str(&format!("defmodule {root}.Transport do\n"));
    out.push_str(
        "  @moduledoc \"\"\"\n  Caller-supplied byte carrier. The host implements this behaviour for its\n  carrier (CBOR over HTTP/WebSocket/etc.): `call/4` takes the already-encoded\n  request bytes and returns the response bytes. The generated client owns\n  (de)serialization via the codec; the carrier only moves bytes.\n  \"\"\"\n\n",
    );
    out.push_str("  @type t :: struct()\n\n");
    out.push_str(
        "  @callback call(t(), service :: String.t(), method :: String.t(), req :: binary()) ::\n              binary()\n\n",
    );
    out.push_str("  @doc \"Dispatches to the transport struct's implementing module.\"\n");
    out.push_str("  @spec call(t(), String.t(), String.t(), binary()) :: binary()\n");
    out.push_str("  def call(%mod{} = transport, service, method, req) do\n");
    out.push_str("    mod.call(transport, service, method, req)\n");
    out.push_str("  end\nend\n\n");
    out
}

fn generate_clients(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<String> {
    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&client_prelude(config));

    let records = record_csil_names(input);
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_module(&mut content, &rule.name, service, config, &records);
            emitted = true;
        }
    }
    if emitted { Some(content) } else { None }
}

/// Whether a type is a reference to a record the codec can (de)serialize, so a
/// typed client method can call the generated `to_cbor`/`from_cbor` directly.
fn is_record_ref(ty: &CsilTypeExpression, records: &HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(name))
}

/// The bare CSIL name of a record reference. Only called after `is_record_ref`
/// confirmed the type is one, so the fallback is never reached.
fn ref_name(ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Reference(name) => name.clone(),
        _ => String::new(),
    }
}

fn emit_client_module(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &ElixirConfig,
    records: &HashSet<String>,
) {
    let base = service_base(name);
    let module = format!("{}.{base}Client", config.module_root);
    let root = &config.module_root;
    // Canonical wire strings (the wire contract): service lowercased, op PascalCased.
    let wire_service = base.to_lowercase();

    content.push_str(&format!("defmodule {module} do\n"));
    content.push_str(&format!(
        "  @moduledoc \"Typed client for the {name} service. The client owns (de)serialization via the codec; the transport only moves bytes.\"\n\n"
    ));
    content.push_str("  @enforce_keys [:transport]\n");
    content.push_str("  defstruct [:transport]\n");
    content.push_str(&format!(
        "  @type t :: %__MODULE__{{transport: {root}.Transport.t()}}\n\n"
    ));
    content.push_str(&format!("  @spec new({root}.Transport.t()) :: t()\n"));
    content.push_str("  def new(transport), do: %__MODULE__{transport: transport}\n");

    for op in &service.operations {
        // Only unary request/response ops belong on the RPC client; channel ops
        // ride the router/encoder surface emitted by the server target.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            content.push_str(&format!(
                "\n  # channel operation {} is not part of the RPC client\n",
                op.name
            ));
            continue;
        }
        let success = success_type(&op.output_type);
        let null_input = is_null_input(&op.input_type);
        // The typed seam needs a record success type (and a record or null request)
        // so the method can call the generated codec. Anything else is skipped with a
        // note rather than emitting an uncompilable call.
        if !is_record_ref(&success, records)
            || !(null_input || is_record_ref(&op.input_type, records))
        {
            content.push_str(&format!(
                "\n  # operation {} has a non-record payload; (de)serialize it manually\n",
                op.name
            ));
            continue;
        }
        let func = snake_case(&op.name);
        let wire_method = wire_method_name(&op.name);
        let resp_mod = config.module(&ref_name(&success));
        let out_ty = format!("{resp_mod}.t()");
        content.push('\n');
        if null_input {
            content.push_str(&format!("  @spec {func}(t()) :: {out_ty}\n"));
            content.push_str(&format!(
                "  def {func}(%__MODULE__{{transport: transport}}) do\n"
            ));
            content.push_str(&format!(
                "    resp = {root}.Transport.call(transport, \"{wire_service}\", \"{wire_method}\", <<>>)\n"
            ));
        } else {
            let req_mod = config.module(&ref_name(&op.input_type));
            let in_ty = format!("{req_mod}.t()");
            content.push_str(&format!("  @spec {func}(t(), {in_ty}) :: {out_ty}\n"));
            content.push_str(&format!(
                "  def {func}(%__MODULE__{{transport: transport}}, req) do\n"
            ));
            content.push_str(&format!(
                "    resp = {root}.Transport.call(transport, \"{wire_service}\", \"{wire_method}\", {req_mod}.to_cbor(req))\n"
            ));
        }
        content.push_str(&format!("    {resp_mod}.from_cbor(resp)\n"));
        content.push_str("  end\n");
    }
    content.push_str("end\n\n");
}

// ---------------------------------------------------------------------------
// Servers (handler behaviours + routers + encoders)
// ---------------------------------------------------------------------------

/// The codec seam routers/encoders use; emitted once when any channel op exists.
fn server_prelude(config: &ElixirConfig, has_channel: bool) -> String {
    let root = &config.module_root;
    let mut out = String::new();
    out.push_str(&format!("defmodule {root}.ServiceError do\n"));
    out.push_str("  @moduledoc \"Transport-level error raised by routers and handlers.\"\n");
    out.push_str("  @enforce_keys [:code, :message]\n");
    out.push_str("  defstruct [:code, :message]\n");
    out.push_str("  @type t :: %__MODULE__{code: integer(), message: String.t()}\nend\n\n");

    if has_channel {
        out.push_str(&format!("defmodule {root}.Codec do\n"));
        out.push_str(
            "  @moduledoc \"\"\"\n  Caller-supplied (de)serialization for channel messages. The generator is\n  codec-agnostic; the host wires this to CBOR, JSON, or anything else.\n  \"\"\"\n\n",
        );
        out.push_str("  @type t :: module()\n\n");
        out.push_str("  @callback encode(value :: term()) :: binary()\n");
        out.push_str("  @callback decode(data :: binary(), type :: module()) :: term()\nend\n\n");
    }
    out
}

fn generate_servers(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<String> {
    let has_channel = input.csil_spec.rules.iter().any(|r| match &r.rule_type {
        CsilRuleType::ServiceDef(def) => service_has_channel_ops(def),
        _ => false,
    });

    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&server_prelude(config, has_channel));

    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_server_module(&mut content, &rule.name, service, config);
            emitted = true;
        }
    }
    if emitted { Some(content) } else { None }
}

fn emit_server_module(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &ElixirConfig,
) {
    let base = service_base(name);
    let module = format!("{}.{base}Server", config.module_root);
    let root = &config.module_root;

    content.push_str(&format!("defmodule {module} do\n"));
    content.push_str(&format!(
        "  @moduledoc \"Server handler behaviour + routers for the {name} service.\"\n\n"
    ));

    // The `@wire-id` ordinals, exposed so a host can reference them rather than
    // hardcoding. Purely additive: emitted only when present.
    if let Some(service_id) = service.wire_id {
        content.push_str(&format!(
            "  @doc \"The @wire-id service ordinal (transport compact profiles).\"\n  def wire_id, do: {service_id}\n\n"
        ));
        for op in &service.operations {
            if let Some(op_id) = op.wire_id {
                content.push_str(&format!(
                    "  @doc \"The @wire-id ordinal for the {} operation.\"\n  def wire_id(:{}), do: {op_id}\n",
                    op.name,
                    snake_case(&op.name)
                ));
            }
        }
        content.push('\n');
    }

    // Handler behaviour: unidirectional ops return Output; bidirectional inbound is
    // fire-and-forget (`:ok`). Reverse ops have no server inbound here.
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

    for op in &inbound {
        let func = snake_case(&op.name);
        let ctx = "ctx :: map()";
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let out_ty = map_type(&success_type(&op.output_type), config);
                if is_null_input(&op.input_type) {
                    content.push_str(&format!(
                        "  @callback {func}({ctx}) :: {{:ok, {out_ty}}} | {{:error, {root}.ServiceError.t()}}\n"
                    ));
                } else {
                    let in_ty = map_type(&op.input_type, config);
                    content.push_str(&format!(
                        "  @callback {func}(req :: {in_ty}, {ctx}) :: {{:ok, {out_ty}}} | {{:error, {root}.ServiceError.t()}}\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                if is_null_input(&op.input_type) {
                    content.push_str(&format!("  @callback {func}({ctx}) :: :ok\n"));
                } else {
                    let in_ty = map_type(&op.input_type, config);
                    content.push_str(&format!(
                        "  @callback {func}(msg :: {in_ty}, {ctx}) :: :ok\n"
                    ));
                }
            }
            CsilServiceDirection::Reverse => {}
        }
    }
    if !inbound.is_empty() {
        content.push('\n');
    }

    if service_has_channel_ops(service) {
        emit_channel_router(content, name, service, config, false);
        if service.wire_id.is_some() {
            emit_channel_router(content, name, service, config, true);
        }
        emit_channel_encoders(content, service, config);
    }

    content.push_str("end\n\n");
}

/// Emit the verbose (`route/4`) or compact (`route_compact/4`) channel dispatcher.
/// Verbose matches on the wire method name; compact on the `@wire-id` ordinal.
fn emit_channel_router(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &ElixirConfig,
    compact: bool,
) {
    let root = &config.module_root;
    let bidi: Vec<&CsilServiceOperation> = service
        .operations
        .iter()
        .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
        .collect();
    let fn_name = if compact { "route_compact" } else { "route" };
    let key_param = if compact { "op" } else { "method" };
    let key_ty = if compact {
        "non_neg_integer()"
    } else {
        "String.t()"
    };

    if compact {
        content.push_str(&format!(
            "  @doc \"Compact-profile router for {name}: dispatch one inbound frame by @wire-id ordinal.\"\n"
        ));
    } else {
        content.push_str(&format!(
            "  @doc \"Verbose-profile router for {name}: dispatch one inbound frame by wire method name.\"\n"
        ));
    }
    content.push_str(&format!(
        "  @spec {fn_name}(module(), {root}.Codec.t(), {key_ty}, binary(), map()) :: :ok | {{:error, {root}.ServiceError.t()}}\n"
    ));

    // Each bidirectional op gets its own function head; the catch-all reports an
    // unknown channel as a transport error.
    for op in &bidi {
        let func = snake_case(&op.name);
        let head = if compact {
            // The all-or-nothing wire-id rule means a bidi op here always has one.
            match op.wire_id {
                Some(id) => id.to_string(),
                None => continue,
            }
        } else {
            format!("\"{}\"", wire_method_name(&op.name))
        };
        if is_null_input(&op.input_type) {
            content.push_str(&format!(
                "  def {fn_name}(handler, _codec, {head} = _{key_param}, _data, ctx), do: handler.{func}(ctx)\n"
            ));
        } else {
            let in_mod = type_module(&op.input_type, config);
            content.push_str(&format!(
                "  def {fn_name}(handler, codec, {head} = _{key_param}, data, ctx) do\n"
            ));
            // The codec's decode/2 takes the target *module* (a runtime value), not the
            // `.t()` typespec — passing `Mod.t()` would call an undefined `t/0`.
            content.push_str(&format!("    msg = codec.decode(data, {in_mod})\n"));
            content.push_str(&format!("    handler.{func}(msg, ctx)\n"));
            content.push_str("  end\n");
        }
    }
    content.push_str(&format!(
        "  def {fn_name}(_handler, _codec, {key_param}, _data, _ctx),\n    do: {{:error, %{root}.ServiceError{{code: 2, message: \"unknown channel #{{inspect({key_param})}}\"}}}}\n\n"
    ));
}

/// Outbound encoders for `<->` and `<-` ops: the server pushes Output to a peer.
fn emit_channel_encoders(
    content: &mut String,
    service: &CsilServiceDefinition,
    config: &ElixirConfig,
) {
    let root = &config.module_root;
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let func = format!("encode_{}", snake_case(&op.name));
        let out_ty = map_type(&op.output_type, config);
        let wire = wire_method_name(&op.name);
        content.push_str(&format!(
            "  @doc \"Encode a `{wire}` message the server pushes to a peer; returns {{method, bytes}}.\"\n"
        ));
        content.push_str(&format!(
            "  @spec {func}({root}.Codec.t(), {out_ty}) :: {{String.t(), binary()}}\n"
        ));
        content.push_str(&format!(
            "  def {func}(codec, msg), do: {{\"{wire}\", codec.encode(msg)}}\n\n"
        ));
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Emit a `<Root>.Validation` module with one `validate_<type>/1` per struct that
/// carries at least one runtime check. Each returns `:ok | {:error, message}`,
/// short-circuiting on the first failure with `with`.
fn generate_validation(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<String> {
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

        let module = config.module(&rule.name);
        let func = format!("validate_{}", snake_case(&rule.name));
        body.push_str(&format!(
            "  @spec {func}({module}.t()) :: :ok | {{:error, String.t()}}\n"
        ));
        body.push_str(&format!("  def {func}(%{module}{{}} = v) do\n"));

        let mut checks: Vec<String> = Vec::new();
        for entry in &group.entries {
            let Some(atom) = field_atom(entry) else {
                continue;
            };
            let optional = is_optional(entry);
            // Length/size checks must call the right BIF for the field's runtime shape:
            // `length/1` raises on a binary or map, so text uses `String.length`, bytes
            // `byte_size`, and maps `map_size` — only lists use `length`.
            let size_fn = collection_size_fn(&entry.value_type);
            for meta in &entry.metadata {
                if let CsilFieldMetadata::Constraint(c) = meta {
                    emit_metadata_check(&mut checks, &atom, optional, size_fn, c);
                }
            }
            if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                for op in constraints {
                    emit_control_check(&mut checks, &atom, optional, size_fn, op);
                }
            }
        }

        if checks.is_empty() {
            body.push_str("    :ok\n");
        } else {
            body.push_str("    with ");
            body.push_str(&checks.join(",\n         "));
            body.push_str(" do\n      :ok\n    end\n");
        }
        body.push_str("  end\n\n");
    }

    if body.is_empty() {
        return None;
    }

    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&format!("defmodule {}.Validation do\n", config.module_root));
    content.push_str("  @moduledoc \"Generated validators from CSIL constraints.\"\n\n");
    content.push_str(&body);
    content.push_str("end\n");
    Some(content)
}

/// A `with`-clause that holds when the field passes; the `else` of the surrounding
/// `with` is implicit (the failing `{:error, _}` short-circuits out). Optional
/// fields skip the check when nil.
fn guard_clause(atom: &str, optional: bool, cond: &str, message: &str) -> String {
    let access = format!("v.{atom}");
    if optional {
        format!(
            ":ok <- (if is_nil({access}) or ({cond}), do: :ok, else: {{:error, \"{message}\"}})"
        )
    } else {
        format!(":ok <- (if {cond}, do: :ok, else: {{:error, \"{message}\"}})")
    }
}

fn emit_metadata_check(
    checks: &mut Vec<String>,
    atom: &str,
    optional: bool,
    size_fn: &str,
    c: &CsilValidationConstraint,
) {
    let access = format!("v.{atom}");
    match c {
        CsilValidationConstraint::MinLength(n) => checks.push(guard_clause(
            atom,
            optional,
            &format!("{size_fn}({access}) >= {n}"),
            &format!("field '{atom}' must have at least {n} characters"),
        )),
        CsilValidationConstraint::MaxLength(n) => checks.push(guard_clause(
            atom,
            optional,
            &format!("{size_fn}({access}) <= {n}"),
            &format!("field '{atom}' must have at most {n} characters"),
        )),
        CsilValidationConstraint::MinItems(n) => checks.push(guard_clause(
            atom,
            optional,
            &format!("{size_fn}({access}) >= {n}"),
            &format!("field '{atom}' must have at least {n} items"),
        )),
        CsilValidationConstraint::MaxItems(n) => checks.push(guard_clause(
            atom,
            optional,
            &format!("{size_fn}({access}) <= {n}"),
            &format!("field '{atom}' must have at most {n} items"),
        )),
        CsilValidationConstraint::MinValue(v) => {
            let bound = literal_to_elixir(v);
            checks.push(guard_clause(
                atom,
                optional,
                &format!("{access} >= {bound}"),
                &format!("field '{atom}' must be at least {bound}"),
            ));
        }
        CsilValidationConstraint::MaxValue(v) => {
            let bound = literal_to_elixir(v);
            checks.push(guard_clause(
                atom,
                optional,
                &format!("{access} <= {bound}"),
                &format!("field '{atom}' must be at most {bound}"),
            ));
        }
        CsilValidationConstraint::Custom { name, value } => {
            if name == "regex"
                && let CsilLiteralValue::Text(pattern) = value
            {
                checks.push(guard_clause(
                    atom,
                    optional,
                    &format!("Regex.match?(~r/{pattern}/, {access})"),
                    &format!(
                        "field '{atom}' must match pattern '{}'",
                        escape_msg(pattern)
                    ),
                ));
            }
        }
    }
}

fn emit_control_check(
    checks: &mut Vec<String>,
    atom: &str,
    optional: bool,
    size_fn: &str,
    op: &CsilControlOperator,
) {
    let access = format!("v.{atom}");
    let ordered = |checks: &mut Vec<String>, ex_op: &str, desc: &str, v: &CsilLiteralValue| {
        let bound = literal_to_elixir(v);
        checks.push(guard_clause(
            atom,
            optional,
            &format!("{access} {ex_op} {bound}"),
            &format!("field '{atom}' must be {desc} {bound}"),
        ));
    };
    match op {
        CsilControlOperator::GreaterEqual(v) => ordered(checks, ">=", "at least", v),
        CsilControlOperator::LessEqual(v) => ordered(checks, "<=", "at most", v),
        CsilControlOperator::GreaterThan(v) => ordered(checks, ">", "greater than", v),
        CsilControlOperator::LessThan(v) => ordered(checks, "<", "less than", v),
        CsilControlOperator::Equal(v) => ordered(checks, "==", "equal to", v),
        CsilControlOperator::NotEqual(v) => ordered(checks, "!=", "not equal to", v),
        CsilControlOperator::Size(size) => emit_size_check(checks, atom, optional, size_fn, size),
        CsilControlOperator::Regex(pattern) => checks.push(guard_clause(
            atom,
            optional,
            &format!("Regex.match?(~r/{pattern}/, {access})"),
            &format!(
                "field '{atom}' must match pattern '{}'",
                escape_msg(pattern)
            ),
        )),
        // .default is applied by the constructor; the encoding-only operators carry
        // no runtime check.
        _ => {}
    }
}

fn emit_size_check(
    checks: &mut Vec<String>,
    atom: &str,
    optional: bool,
    size_fn: &str,
    size: &CsilSizeConstraint,
) {
    let access = format!("v.{atom}");
    let mut one = |ex_op: &str, n: u64, word: &str| {
        checks.push(guard_clause(
            atom,
            optional,
            &format!("{size_fn}({access}) {ex_op} {n}"),
            &format!("field '{atom}' must have {word} {n} elements"),
        ));
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

/// The Elixir size BIF appropriate to a field's runtime shape. `length/1` only
/// accepts lists (it raises on a binary or map), so a `.size`/`@max-length` check
/// must dispatch on the type: text → `String.length`, bytes → `byte_size`, map →
/// `map_size`, list → `length`. Defaults to `String.length` since a bare scalar
/// constraint is overwhelmingly a text length.
fn collection_size_fn(type_expr: &CsilTypeExpression) -> &'static str {
    match type_expr {
        CsilTypeExpression::Constrained { base_type, .. } => collection_size_fn(base_type),
        CsilTypeExpression::Array { .. } => "length",
        CsilTypeExpression::Map { .. } => "map_size",
        CsilTypeExpression::Tuple(_) => "tuple_size",
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "bytes" | "bstr" => "byte_size",
            _ => "String.length",
        },
        _ => "String.length",
    }
}

fn entry_has_check(entry: &CsilGroupEntry) -> bool {
    let meta = entry.metadata.iter().any(|m| match m {
        CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, .. }) => {
            name == "regex"
        }
        CsilFieldMetadata::Constraint(_) => true,
        _ => false,
    });
    let op = match &entry.value_type {
        CsilTypeExpression::Constrained { constraints, .. } => constraints.iter().any(|op| {
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
        }),
        _ => false,
    };
    meta || op
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// Emit a `<Root>.Constructors` module with a `new_<type>/0` per struct that has at
/// least one defaulted field, building the struct with those defaults applied.
fn generate_constructors(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<String> {
    let mut body = String::new();

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        let Some(group) = group else { continue };

        let defaults: Vec<(String, &CsilLiteralValue)> = group
            .entries
            .iter()
            .filter_map(|e| {
                let atom = field_atom(e)?;
                let value = entry_default_value(e)?;
                Some((atom, value))
            })
            .collect();
        if defaults.is_empty() {
            continue;
        }

        let module = config.module(&rule.name);
        let func = format!("new_{}", snake_case(&rule.name));
        body.push_str(&format!("  @spec {func}() :: {module}.t()\n"));
        body.push_str(&format!("  def {func}() do\n"));
        body.push_str(&format!("    %{module}{{\n"));
        for (atom, value) in &defaults {
            body.push_str(&format!("      {atom}: {},\n", literal_to_elixir(value)));
        }
        body.push_str("    }\n  end\n\n");
    }

    if body.is_empty() {
        return None;
    }

    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&format!(
        "defmodule {}.Constructors do\n",
        config.module_root
    ));
    content.push_str("  @moduledoc \"Constructors applying CSIL default values.\"\n\n");
    content.push_str(&body);
    content.push_str("end\n");
    Some(content)
}

// ---------------------------------------------------------------------------
// Type mapping + helpers
// ---------------------------------------------------------------------------

fn map_type(type_expr: &CsilTypeExpression, config: &ElixirConfig) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => map_builtin(name, config),
        CsilTypeExpression::Reference(name) => format!("{}.t()", config.module(name)),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("[{}]", map_type(element_type, config))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            format!(
                "%{{optional({}) => {}}}",
                map_type(key, config),
                map_type(value, config)
            )
        }
        CsilTypeExpression::Tuple(group) => {
            if group.entries.is_empty() {
                return "tuple()".to_string();
            }
            let parts: Vec<String> = group
                .entries
                .iter()
                .map(|e| {
                    let t = map_type(&e.value_type, config);
                    if is_optional(e) {
                        format!("{t} | nil")
                    } else {
                        t
                    }
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        CsilTypeExpression::Choice(choices) => {
            // A choice over text/int literals (e.g. `text / "a" / "b"`) collapses to a
            // repeated `String.t() | String.t()`; dedup so the spec reads as one type.
            let mut seen = Vec::new();
            for c in choices {
                let mapped = map_type(c, config);
                if !seen.contains(&mapped) {
                    seen.push(mapped);
                }
            }
            seen.join(" | ")
        }
        CsilTypeExpression::Constrained { base_type, .. } => map_type(base_type, config),
        CsilTypeExpression::Group(_) => "map()".to_string(),
        CsilTypeExpression::Literal(lit) => match lit {
            CsilLiteralValue::Integer(_) => "integer()".to_string(),
            CsilLiteralValue::Float(_) => "float()".to_string(),
            CsilLiteralValue::Text(_) => "String.t()".to_string(),
            CsilLiteralValue::Bytes(_) => "binary()".to_string(),
            CsilLiteralValue::Bool(_) => "boolean()".to_string(),
            CsilLiteralValue::Null => "nil".to_string(),
            CsilLiteralValue::Array(_) => "list()".to_string(),
        },
        _ => "term()".to_string(),
    }
}

/// The runtime module reference for a message type (no `.t()` suffix) — what a
/// codec's `decode/2` is handed. Channel inputs are always named message types, so
/// the common case is a reference; anything else falls back to stripping `.t()`.
fn type_module(type_expr: &CsilTypeExpression, config: &ElixirConfig) -> String {
    match type_expr {
        CsilTypeExpression::Reference(name) => config.module(name),
        other => {
            let mapped = map_type(other, config);
            mapped
                .strip_suffix(".t()")
                .map(str::to_string)
                .unwrap_or(mapped)
        }
    }
}

fn map_builtin(name: &str, config: &ElixirConfig) -> String {
    match name {
        "int" | "uint" | "nint" => "integer()".to_string(),
        "float" | "double" | "float16" | "float32" | "float64" => "float()".to_string(),
        // `tstr`/`bstr` are the CDDL spellings of `text`/`bytes`.
        "text" | "tstr" => "String.t()".to_string(),
        "bytes" | "bstr" => "binary()".to_string(),
        "bool" | "true" | "false" => "boolean()".to_string(),
        // CBOR tag 0 RFC3339 UTC instant.
        "timestamp" => "DateTime.t()".to_string(),
        // CBOR tag 4 exact decimal. Elixir has no stdlib decimal; the value rides as
        // a host-supplied struct under the transport's Decimal namespace.
        "decimal" => format!("{}.Decimal.t()", config.module_root),
        "null" | "nil" | "undefined" => "nil".to_string(),
        "any" => "term()".to_string(),
        // An unknown name is treated as a reference to a generated type.
        other => format!("{}.t()", config.module(other)),
    }
}

/// The snake_case struct-field atom for an entry, or None for non-named keys.
fn field_atom(entry: &CsilGroupEntry) -> Option<String> {
    match entry.key.as_ref()? {
        CsilGroupKey::Bare(name) => Some(snake_case(name)),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => Some(snake_case(name)),
        _ => None,
    }
}

/// The verbatim CBOR wire key for an entry (never case-transformed — the wire
/// contract keys by the CSIL field name as written).
fn wire_key(entry: &CsilGroupEntry) -> String {
    match entry.key.as_ref() {
        Some(CsilGroupKey::Bare(name)) => name.clone(),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => name.clone(),
        _ => "field".to_string(),
    }
}

fn is_optional(entry: &CsilGroupEntry) -> bool {
    matches!(entry.occurrence, Some(CsilOccurrence::Optional))
}

fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

fn visibility(metadata: &[CsilFieldMetadata]) -> CsilFieldVisibility {
    for m in metadata {
        if let CsilFieldMetadata::Visibility(v) = m {
            return v.clone();
        }
    }
    CsilFieldVisibility::Bidirectional
}

/// The default literal for a field, honoring both the `@default(...)` annotation
/// and the `.default(...)` control operator (annotation wins if both present).
fn entry_default_value(entry: &CsilGroupEntry) -> Option<&CsilLiteralValue> {
    for m in &entry.metadata {
        if let CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, value }) = m
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

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — that half is the error
/// channel of the `{:ok, _} | {:error, _}` return, not part of the typed response.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !matches!(c, CsilTypeExpression::Reference(n) if n == "ServiceError"))
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

/// Strip a trailing `Service` suffix and PascalCase, matching the wire service base
/// used across the other generators' clients.
fn service_base(name: &str) -> String {
    let pascal = pascal_case(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// PascalCase wire method name — same simple rule the other generators use so a
/// frame keyed by method routes identically across targets.
fn wire_method_name(name: &str) -> String {
    pascal_case(name)
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

fn snake_case(s: &str) -> String {
    // CSIL identifiers are already snake_case fields / kebab-case operations; map
    // both to snake_case Elixir atoms/functions. Insert `_` only between a
    // lower/digit→upper boundary so an already-snake name is unchanged.
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for ch in s.chars() {
        if ch == '-' || ch == ' ' {
            out.push('_');
            prev_lower_or_digit = false;
        } else if ch.is_uppercase() {
            if prev_lower_or_digit {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
            prev_lower_or_digit = false;
        } else {
            out.push(ch);
            prev_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

fn literal_to_elixir(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{}\"", escape_msg(s)),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "<<>>".to_string(),
        CsilLiteralValue::Array(elements) => {
            let parts: Vec<String> = elements.iter().map(literal_to_elixir).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

/// Escape a string for safe inclusion in an Elixir double-quoted literal.
fn escape_msg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '#' => out.push_str("\\#"), // neutralize `#{}` interpolation
            _ => out.push(c),
        }
    }
    out
}

/// The self-contained canonical-CBOR value codec injected as the body of the
/// generated `<Root>.Cbor` module. Its value tree carries the bool/float/null items
/// a payload may hold and keeps text distinct from bytes (both are Elixir binaries),
/// so the generated output round-trips without any third-party CBOR dependency.
const CODEC_RUNTIME_ELIXIR: &str = r#"  @moduledoc """
  Self-contained canonical-CBOR value codec. The value tree carries text vs bytes
  explicitly (both are Elixir binaries), so a record's fields encode to the exact
  wire bytes the CSIL contract requires with no third-party dependency.
  """

  import Bitwise

  @type value ::
          {:int, integer()}
          | {:float, float()}
          | {:bool, boolean()}
          | :null
          | {:text, binary()}
          | {:bytes, binary()}
          | {:array, [value()]}
          | {:map, [{value(), value()}]}
          | {:tag, non_neg_integer(), value()}

  @doc "Encodes a CBOR value tree to canonical bytes."
  @spec encode(value()) :: binary()
  def encode(value), do: IO.iodata_to_binary(enc(value))

  defp enc({:int, n}) when n >= 0, do: head(0, n)
  defp enc({:int, n}), do: head(1, -n - 1)
  defp enc({:bool, false}), do: <<0xF4>>
  defp enc({:bool, true}), do: <<0xF5>>
  defp enc(:null), do: <<0xF6>>
  defp enc({:float, f}), do: <<0xFB, f::float-size(64)>>
  defp enc({:text, s}), do: [head(3, byte_size(s)), s]
  defp enc({:bytes, b}), do: [head(2, byte_size(b)), b]
  defp enc({:array, xs}), do: [head(4, length(xs)) | Enum.map(xs, &enc/1)]

  defp enc({:map, kvs}),
    do: [head(5, length(kvs)) | Enum.map(kvs, fn {k, v} -> [enc(k), enc(v)] end)]

  defp enc({:tag, t, v}), do: [head(6, t), enc(v)]

  # Shortest-length argument head per RFC 8949 §3 (the canonical encoding).
  defp head(major, n) do
    mt = bsl(major, 5)

    cond do
      n < 24 -> <<bor(mt, n)>>
      n < 0x100 -> <<bor(mt, 24), n::size(8)>>
      n < 0x10000 -> <<bor(mt, 25), n::size(16)>>
      n < 0x100000000 -> <<bor(mt, 26), n::size(32)>>
      true -> <<bor(mt, 27), n::size(64)>>
    end
  end

  @doc "Decodes canonical CBOR bytes into a value tree."
  @spec decode(binary()) :: value()
  def decode(bin) do
    {value, rest} = dec(bin)
    if rest != <<>>, do: raise("csilgen: trailing bytes after CBOR value")
    value
  end

  defp dec(<<7::size(3), 20::size(5), rest::binary>>), do: {{:bool, false}, rest}
  defp dec(<<7::size(3), 21::size(5), rest::binary>>), do: {{:bool, true}, rest}
  defp dec(<<7::size(3), low::size(5), rest::binary>>) when low in [22, 23], do: {:null, rest}
  defp dec(<<7::size(3), 26::size(5), f::float-size(32), rest::binary>>), do: {{:float, f}, rest}
  defp dec(<<7::size(3), 27::size(5), f::float-size(64), rest::binary>>), do: {{:float, f}, rest}

  defp dec(<<major::size(3), low::size(5), rest::binary>>) do
    {arg, rest} = read_arg(low, rest)

    case major do
      0 ->
        {{:int, arg}, rest}

      1 ->
        {{:int, -arg - 1}, rest}

      2 ->
        <<b::binary-size(arg), r::binary>> = rest
        {{:bytes, b}, r}

      3 ->
        <<s::binary-size(arg), r::binary>> = rest
        {{:text, s}, r}

      4 ->
        dec_array(arg, rest, [])

      5 ->
        dec_map(arg, rest, [])

      6 ->
        {inner, r} = dec(rest)
        {{:tag, arg, inner}, r}
    end
  end

  defp read_arg(low, rest) when low < 24, do: {low, rest}
  defp read_arg(24, <<a::size(8), rest::binary>>), do: {a, rest}
  defp read_arg(25, <<a::size(16), rest::binary>>), do: {a, rest}
  defp read_arg(26, <<a::size(32), rest::binary>>), do: {a, rest}
  defp read_arg(27, <<a::size(64), rest::binary>>), do: {a, rest}

  defp dec_array(0, rest, acc), do: {{:array, Enum.reverse(acc)}, rest}

  defp dec_array(n, rest, acc) do
    {v, r} = dec(rest)
    dec_array(n - 1, r, [v | acc])
  end

  defp dec_map(0, rest, acc), do: {{:map, Enum.reverse(acc)}, rest}

  defp dec_map(n, rest, acc) do
    {k, r1} = dec(rest)
    {v, r2} = dec(r1)
    dec_map(n - 1, r2, [{k, v} | acc])
  end

  @doc "Unwraps a CBOR integer item."
  @spec to_int(value()) :: integer()
  def to_int({:int, n}), do: n

  @doc "Unwraps a CBOR text item."
  @spec to_text(value()) :: binary()
  def to_text({:text, s}), do: s

  @doc "Unwraps a CBOR byte-string item."
  @spec to_bytes(value()) :: binary()
  def to_bytes({:bytes, b}), do: b

  @doc "Unwraps a CBOR boolean item."
  @spec to_bool(value()) :: boolean()
  def to_bool({:bool, b}), do: b

  @doc "Unwraps a CBOR float item, widening an integer item when present."
  @spec to_float(value()) :: float()
  def to_float({:float, f}), do: f
  def to_float({:int, n}), do: n * 1.0
"#;

#[cfg(test)]
mod tests;
