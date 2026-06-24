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
use std::collections::HashMap;

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

struct ElixirConfig {
    /// Root module namespace, e.g. `MyApp` → `MyApp.DepositClaimRequest`.
    module_root: String,
    generate_validation: bool,
    generate_constructors: bool,
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
            .unwrap_or("Csilgen.Generated")
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
        })
    }

    /// The fully-qualified module name for a CSIL type/service reference.
    fn module(&self, name: &str) -> String {
        format!("{}.{}", self.module_root, pascal_case(name))
    }
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
    // through `mix format`, matching the gofmt-stability of the Go target.
    files.push(GeneratedFile {
        path: ".formatter.exs".to_string(),
        content: "[inputs: [\"*.ex\", \"*.exs\"]]\n".to_string(),
    });

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

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            emit_struct_module(&mut content, &rule.name, group, config, warnings);
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

    content.push_str("end\n\n");
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
        "  @moduledoc \"\"\"\n  Caller-supplied transport seam. The host implements this behaviour for its\n  carrier (CBOR over HTTP/WebSocket/etc.); the generated clients only call it.\n  \"\"\"\n\n",
    );
    out.push_str("  @type t :: struct()\n\n");
    out.push_str(
        "  @callback call(t(), service :: String.t(), method :: String.t(), req :: term()) ::\n              {:ok, term()} | {:error, term()}\n\n",
    );
    out.push_str("  @doc \"Dispatches to the transport struct's implementing module.\"\n");
    out.push_str(
        "  @spec call(t(), String.t(), String.t(), term()) :: {:ok, term()} | {:error, term()}\n",
    );
    out.push_str("  def call(%mod{} = transport, service, method, req) do\n");
    out.push_str("    mod.call(transport, service, method, req)\n");
    out.push_str("  end\nend\n\n");
    out
}

fn generate_clients(input: &WasmGeneratorInput, config: &ElixirConfig) -> Option<String> {
    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&client_prelude(config));

    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_module(&mut content, &rule.name, service, config);
            emitted = true;
        }
    }
    if emitted { Some(content) } else { None }
}

fn emit_client_module(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &ElixirConfig,
) {
    let base = service_base(name);
    let module = format!("{}.{base}Client", config.module_root);
    let wire_service = base.to_lowercase();

    content.push_str(&format!("defmodule {module} do\n"));
    content.push_str(&format!(
        "  @moduledoc \"Typed client for the {name} service.\"\n\n"
    ));
    content.push_str("  @enforce_keys [:transport]\n");
    content.push_str("  defstruct [:transport]\n");
    content.push_str(&format!(
        "  @type t :: %__MODULE__{{transport: {}.Transport.t()}}\n\n",
        config.module_root
    ));
    content.push_str(&format!(
        "  @spec new({}.Transport.t()) :: t()\n",
        config.module_root
    ));
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
        let func = snake_case(&op.name);
        let wire_method = wire_method_name(&op.name);
        let out_ty = map_type(&success_type(&op.output_type), config);
        let has_input = !is_null_input(&op.input_type);
        content.push('\n');
        if has_input {
            let in_ty = map_type(&op.input_type, config);
            content.push_str(&format!(
                "  @spec {func}(t(), {in_ty}) :: {{:ok, {out_ty}}} | {{:error, term()}}\n"
            ));
            content.push_str(&format!(
                "  def {func}(%__MODULE__{{transport: transport}}, req) do\n"
            ));
            content.push_str(&format!(
                "    {}.Transport.call(transport, \"{wire_service}\", \"{wire_method}\", req)\n",
                config.module_root
            ));
        } else {
            content.push_str(&format!(
                "  @spec {func}(t()) :: {{:ok, {out_ty}}} | {{:error, term()}}\n"
            ));
            content.push_str(&format!(
                "  def {func}(%__MODULE__{{transport: transport}}) do\n"
            ));
            content.push_str(&format!(
                "    {}.Transport.call(transport, \"{wire_service}\", \"{wire_method}\", nil)\n",
                config.module_root
            ));
        }
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

#[cfg(test)]
mod tests;
