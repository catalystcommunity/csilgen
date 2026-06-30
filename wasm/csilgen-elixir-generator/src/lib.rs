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
            // A path-style `package_name` is the cross-ecosystem source of truth; Hex
            // wants only its tail. See `package_name_last_segment`.
            .map(csilgen_common::package_name_last_segment)
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
// Package genquickstart: a 3-transport, library-based Quickstart
// ---------------------------------------------------------------------------

/// Which transport sections the consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or
/// empty value means "all three". Sections always render in a fixed order so the
/// document reads the same regardless of how the subset was written.
fn wanted_transports(input: &WasmGeneratorInput) -> (bool, bool, bool) {
    let listed = match input.config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
            let has = |t: &str| names.contains(&t);
            // An array that names none of the known transports (all unknown, or empty)
            // falls back to all three rather than an empty doc.
            if has("rpc") || has("events") || has("datagrams") {
                Some((has("rpc"), has("events"), has("datagrams")))
            } else {
                None
            }
        }
        _ => None,
    };
    listed.unwrap_or((true, true, true))
}

/// The package genquickstart: a transport-by-transport Quickstart over the official
/// `csilgen_transport` library. The generated codec owns CBOR (de)serialization and
/// the library owns the envelope/framing/lifecycle; you supply only a *carrier* that
/// moves bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events over TLS,
/// CSIL-Datagrams over UDP) is a complete, copy-paste example built on the library.
fn readme(input: &WasmGeneratorInput, config: &ElixirConfig) -> String {
    let pkg = &config.package_name;
    let mut out = format!(
        "# {pkg}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the official `csilgen_transport` library owns the\n\
         envelope, framing, and connection lifecycle. You supply only a *carrier* that\n\
         moves bytes, so the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or\n\
         raw UDP unchanged.\n\n\
         ## Install\n\n\
         ```elixir\n\
         # TODO: publish {pkg} to Hex, then add it (and the transport lib) to deps:\n\
         def deps do\n  \
         [\n    \
         {{:{pkg}, \"~> {ver}\"}},\n    \
         # not yet published — vendor or git for now:\n    \
         {{:csilgen_transport, github: \"catalystcommunity/csilgen\", sparse: \"transports/elixir\"}}\n  \
         ]\n\
         end\n\
         ```\n\n",
        ver = config.package_version
    );

    let (rpc, events, datagrams) = wanted_transports(input);
    let unary = first_unary_example(input, config);
    let channel = first_channel_example(input, config);
    if rpc {
        out.push_str(&rpc_section(config, unary.as_ref()));
    }
    if events {
        out.push_str(&events_section(config, channel.as_ref()));
    }
    if datagrams {
        out.push_str(&datagrams_section(unary.as_ref()));
    }
    out
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `<root>.Transport` byte
/// seam that builds/parses the envelope with the library's `Csilgen.Transport.RPC`
/// request/response (never hand-rolled) and POSTs it to `{base}/csil/v1/rpc` over
/// `:gen_tcp`. The typed client decodes the success payload; a non-zero transport status
/// and the `ServiceError` application arm are surfaced distinctly.
fn rpc_section(config: &ElixirConfig, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`Csilgen.Transport.RPC`\n\
         request/response); you bring a carrier that moves bytes. The `:gen_tcp` carrier\n\
         below is just one example — swap it for any HTTP client (it implements the\n\
         generated byte seam). It uses only `:gen_tcp` (in `:kernel`), so it runs as-is\n\
         under `mix`/releases with no `extra_applications`.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```elixir\n");
    out.push_str(&rpc_carrier_elixir(config));
    out.push('\n');
    // The example: construct the typed client over the carrier and call the first op.
    out.push_str("transport = CsilRpcHttpCarrier.new(\"http://localhost:5080\")\n");
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
    out.push_str("```\n\n");
    out
}

/// The HTTP carrier: a struct implementing the generated `<root>.Transport` behaviour.
/// It encodes the request with the library's `RPC.encode_request`, POSTs it to
/// `{base}/csil/v1/rpc` over `:gen_tcp`, and decodes the reply with `RPC.decode_response`
/// — the library owns the envelope, the carrier owns only the transport. A non-zero
/// transport status (`RPC.as_transport_error`) and the typed `ServiceError` arm are
/// raised so the generated client decodes success only.
fn rpc_carrier_elixir(config: &ElixirConfig) -> String {
    let root = &config.module_root;
    // The behaviour module name is spec-derived (module root), so the carrier is a
    // format! rather than a constant.
    format!(
        r#"defmodule CsilRpcHttpCarrier do
  # The library owns the CSIL-RPC envelope (`Csilgen.Transport.RPC`); this carrier owns
  # only the HTTP transport. It implements the generated `{root}.Transport` behaviour so
  # the typed client can drive it.
  @behaviour {root}.Transport

  alias Csilgen.Transport.RPC

  defstruct [:rpc_url]

  def new(base_url),
    do: %__MODULE__{{rpc_url: String.trim_trailing(base_url, "/") <> "/csil/v1/rpc"}}

  # The generated client calls this seam with the already-encoded request bytes.
  @impl true
  def call(%__MODULE__{{rpc_url: url}}, service, op, req) when is_binary(req) do
    # Encode the request with the library's RPC envelope (never hand-rolled).
    envelope = RPC.encode_request(RPC.new_request(service, op, req))

    body =
      case post(url, envelope) do
        {{:ok, b}} -> b
        {{:error, reason}} -> raise "csil-rpc #{{service}}/#{{op}}: #{{inspect(reason)}}"
      end

    # Decode the library's RPC response and surface a non-zero transport status.
    {{:ok, resp}} = RPC.decode_response(body)

    case RPC.as_transport_error(resp) do
      :ok -> :ok
      {{:error, reason}} -> raise "csil-rpc #{{service}}/#{{op}}: #{{inspect(reason)}}"
    end

    # A typed `ServiceError` arm rides as a status-0 `variant` — an application error
    # distinct from a transport failure. Surface it so the client decodes success only.
    if resp.variant == "ServiceError",
      do: raise("csil-rpc #{{service}}/#{{op}}: ServiceError")

    resp.payload
  end

  # --- Plain-HTTP POST over :gen_tcp ---
  # `:gen_tcp` lives in `:kernel`, which is always loaded, so this carrier
  # copy-paste-runs inside a `mix` project (or a release) with no
  # `extra_applications` to discover. (TLS — the CSIL-Events example — does need
  # `:ssl`; see that section.)
  defp post(url, body) do
    uri = URI.parse(url)
    host = String.to_charlist(uri.host)
    port = uri.port || 80
    path = uri.path || "/"

    head =
      "POST #{{path}} HTTP/1.1\r\n" <>
        "Host: #{{uri.host}}:#{{port}}\r\n" <>
        "Content-Type: application/cbor\r\n" <>
        "Content-Length: #{{byte_size(body)}}\r\n" <>
        "Connection: close\r\n\r\n"

    with {{:ok, sock}} <-
           :gen_tcp.connect(host, port, [:binary, active: false, packet: :raw], 10_000),
         :ok <- :gen_tcp.send(sock, [head, body]) do
      result = recv_response(sock, <<>>)
      :gen_tcp.close(sock)
      result
    end
  end

  # The server sends `Connection: close`, so read until the socket closes, then
  # split the status line + headers from the body on the blank line.
  defp recv_response(sock, acc) do
    case :gen_tcp.recv(sock, 0, 30_000) do
      {{:ok, data}} -> recv_response(sock, acc <> data)
      {{:error, :closed}} -> parse_response(acc)
      {{:error, reason}} -> {{:error, reason}}
    end
  end

  defp parse_response(raw) do
    case :binary.split(raw, "\r\n\r\n") do
      [head, body] ->
        [status_line | _] = String.split(head, "\r\n")
        case String.split(status_line, " ", parts: 3) do
          [_http, code, _reason] ->
            case Integer.parse(code) do
              {{n, _}} when n in 200..299 -> {{:ok, body}}
              {{n, _}} -> {{:error, {{:http, n}}}}
              :error -> {{:error, :bad_status}}
            end

          _ ->
            {{:error, :bad_status_line}}
        end

      _ ->
        {{:error, :incomplete_response}}
    end
  end
end
"#
    )
}

/// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as a
/// `Csilgen.Transport.Carrier` (CSIL length-prefix framing), performs the
/// `$hello`/`$hello-ack` handshake, sends one outbound event via the generated
/// `encode_<op>`, and runs a recv loop that decodes each frame to an `Event`, answers
/// `$ping` with `$pong`, and dispatches typed events to the generated server `route/5`.
/// When the spec has no record channel op the dispatch wiring is replaced with a note
/// (the handshake + heartbeat still apply to any connection).
fn events_section(config: &ElixirConfig, ch: Option<&ChannelExample>) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library\n\
         owns the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and\n\
         length-prefix framing; the generated router dispatches typed events. The TLS\n\
         carrier below is just one example — a WebSocket/QUIC carrier drops in unchanged.\n\n\
         > This example uses `:ssl` for TLS. Under `mix`/releases, add it to your app's\n\
         > `extra_applications` so it is loaded: `def application, do: [extra_applications: [:ssl]]`.\n\n",
    );
    out.push_str("```elixir\n");
    out.push_str(EVENTS_CARRIER_ELIXIR);
    out.push('\n');
    match ch {
        Some(ch) => out.push_str(&events_session(config, ch)),
        None => out.push_str(EVENTS_NO_CHANNEL_SESSION_ELIXIR),
    }
    out.push_str("```\n\n");
    out
}

/// The TLS `Csilgen.Transport.Carrier` adapter — spec-independent, so a constant. It
/// length-prefixes each outbound frame with the library's `frame_length_prefixed` and
/// deframes inbound socket bytes with `read_length_prefixed`, so the session logic is
/// transport-agnostic.
const EVENTS_CARRIER_ELIXIR: &str = r#"defmodule TlsFrameCarrier do
  # One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix. It
  # implements `Csilgen.Transport.Carrier` so the session logic stays transport-agnostic;
  # the library owns framing, the carrier owns only the socket.
  @behaviour Csilgen.Transport.Carrier

  alias Csilgen.Transport.Carrier

  defstruct [:socket, buffer: <<>>]

  def connect(host, port) do
    :ssl.start()
    opts = [:binary, active: false, packet: :raw]

    case :ssl.connect(String.to_charlist(host), port, opts, 10_000) do
      {:ok, socket} -> {:ok, %__MODULE__{socket: socket}}
      {:error, reason} -> {:error, reason}
    end
  end

  @impl true
  def send_frame(%__MODULE__{socket: socket} = c, frame) do
    with {:ok, wire} <- Carrier.frame_length_prefixed(frame),
         :ok <- :ssl.send(socket, wire) do
      {:ok, c}
    end
  end

  @impl true
  def recv_frame(%__MODULE__{} = c) do
    case Carrier.read_length_prefixed(c.buffer) do
      {:ok, frame, rest} ->
        {:ok, frame, %{c | buffer: rest}}

      :incomplete ->
        case :ssl.recv(c.socket, 0, 30_000) do
          {:ok, data} -> recv_frame(%{c | buffer: c.buffer <> data})
          {:error, :closed} -> :closed
          {:error, reason} -> {:error, reason}
        end

      {:error, reason} ->
        {:error, reason}
    end
  end
end
"#;

/// The channel session body for an Events connection that has a record `<->` op: a
/// `Codec` backed by the op's generated per-type helpers, a handler implementing the
/// generated server behaviour, the handshake, one outbound event via the generated
/// encoder, and the recv loop that heartbeats and dispatches into the generated router.
fn events_session(config: &ElixirConfig, ch: &ChannelExample) -> String {
    format!(
        r#"defmodule ExampleCodec do
  # Back the library's channel Codec seam with this package's generated per-type
  # `to_cbor`/`from_cbor`. encode/1 dispatches on the struct's module; the router hands
  # decode/2 the target module to rebuild.
  @behaviour {root}.Codec

  @impl true
  def encode(value), do: value.__struct__.to_cbor(value)

  @impl true
  def decode(data, type), do: type.from_cbor(data)
end

defmodule ExampleHandler do
  # The generated router dispatches inbound `{wire_method}` events here.
  @behaviour {server}

  @impl true
  def {handler_fn}(msg, _ctx) do
    IO.inspect(msg, label: "event {wire_method}")
    :ok
  end
end

defmodule EventsSession do
  alias Csilgen.Transport.{{Conventions, Events}}
  alias Csilgen.Transport.Events.{{Hello, Heartbeat}}

  def run(host, port) do
    {{:ok, carrier}} = TlsFrameCarrier.connect(host, port)

    # $hello / $hello-ack handshake. The peer's $hello-ack pins the wire profile.
    hello = Events.encode_hello(%Hello{{versions: [1], profiles: ["verbose"], service: "{wire_service}"}})
    {{:ok, carrier}} = TlsFrameCarrier.send_frame(carrier, hello)
    {{:ok, ack, carrier}} = TlsFrameCarrier.recv_frame(carrier)
    {{:ok, ack_map}} = Conventions.decode_envelope(ack)
    {{:ok, profile_name}} = Conventions.get_text(ack_map, "profile")
    {{:ok, profile}} = Events.parse_profile(profile_name)

    # Send one outbound event via the generated encoder, framed as a typed Event.
    {{method, bytes}} = {server}.{encode_fn}(ExampleCodec, %{out_module}{{{sample_field}}})
    out = Events.encode_event(Events.new_verbose("{wire_service}", method, bytes), profile)
    {{:ok, carrier}} = TlsFrameCarrier.send_frame(carrier, out)

    recv_loop(carrier, profile)
  end

  # Recv loop: decode each frame to an Event, answer $ping with $pong (heartbeat), and
  # dispatch the rest into the generated router.
  defp recv_loop(carrier, profile) do
    case TlsFrameCarrier.recv_frame(carrier) do
      {{:ok, frame, carrier}} ->
        {{:ok, ev}} = Events.decode_event(frame, profile)
        carrier = handle_event(carrier, profile, ev)
        recv_loop(carrier, profile)

      :closed ->
        :ok
    end
  end

  defp handle_event(carrier, profile, %Events.Event{{event: "$ping", payload: payload}}) do
    {{:ok, hb}} = Conventions.decode_envelope(payload)
    {{:ok, nonce}} = Conventions.get_int(hb, "nonce")
    pong = Events.new_verbose("{wire_service}", "$pong", Events.encode_heartbeat(%Heartbeat{{nonce: nonce}}))
    {{:ok, carrier}} = TlsFrameCarrier.send_frame(carrier, Events.encode_event(pong, profile))
    carrier
  end

  defp handle_event(carrier, _profile, ev) do
    {server}.route(ExampleHandler, ExampleCodec, ev.event, ev.payload, %{{}})
    carrier
  end
end

EventsSession.run("localhost", 7443)
"#,
        root = config.module_root,
        server = ch.server_module,
        handler_fn = ch.handler_fn,
        wire_method = ch.wire_method,
        wire_service = ch.wire_service,
        encode_fn = ch.encode_fn,
        out_module = ch.out_module,
        sample_field = ch.sample_field,
    )
}

/// The Events session body when the spec declares no record channel op: the handshake
/// and heartbeat still apply, so they are shown, with a note where the dispatch would go.
const EVENTS_NO_CHANNEL_SESSION_ELIXIR: &str = r#"defmodule EventsSession do
  alias Csilgen.Transport.{Conventions, Events}
  alias Csilgen.Transport.Events.{Hello, Heartbeat}

  def run(host, port) do
    {:ok, carrier} = TlsFrameCarrier.connect(host, port)

    # $hello / $hello-ack handshake. The peer's $hello-ack pins the wire profile.
    hello = Events.encode_hello(%Hello{versions: [1], profiles: ["verbose"]})
    {:ok, carrier} = TlsFrameCarrier.send_frame(carrier, hello)
    {:ok, ack, carrier} = TlsFrameCarrier.recv_frame(carrier)
    {:ok, ack_map} = Conventions.decode_envelope(ack)
    {:ok, profile_name} = Conventions.get_text(ack_map, "profile")
    {:ok, profile} = Events.parse_profile(profile_name)

    recv_loop(carrier, profile)
  end

  # Recv loop: answer $ping with $pong. This build exposes no generated channel router
  # (it is emitted by the `elixir`/`elixir-server` target for record `<->`/`<-` ops), so
  # there is no typed dispatch to wire here.
  defp recv_loop(carrier, profile) do
    case TlsFrameCarrier.recv_frame(carrier) do
      {:ok, frame, carrier} ->
        {:ok, ev} = Events.decode_event(frame, profile)

        carrier =
          if ev.event == "$ping" do
            {:ok, hb} = Conventions.decode_envelope(ev.payload)
            {:ok, nonce} = Conventions.get_int(hb, "nonce")
            pong = Events.new_verbose(nil, "$pong", Events.encode_heartbeat(%Heartbeat{nonce: nonce}))
            {:ok, carrier} = TlsFrameCarrier.send_frame(carrier, Events.encode_event(pong, profile))
            carrier
          else
            carrier
          end

        recv_loop(carrier, profile)

      :closed ->
        :ok
    end
  end
end

EventsSession.run("localhost", 7443)
"#;

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in
/// the library's `Datagram`, and send it fire-and-forget over `:gen_udp`. The recv path
/// `Datagrams.decode_datagram`s an inbound datagram and decodes its payload with the
/// generated codec into the RESPONSE type — there is NO synchronous response.
fn datagrams_section(ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram`\n\
         envelope; you bring a datagram carrier. The `:gen_udp` carrier below is one\n\
         example — a WebRTC unreliable channel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req_mod), Some(res_mod)) = (&ex.req_module, &ex.res_module) else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```elixir\n");
    out.push_str(DATAGRAMS_CARRIER_ELIXIR);
    out.push('\n');
    out.push_str(&format!(
        r#"alias Csilgen.Transport.Datagrams

# The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
op_ord = {op_ord}

{{:ok, carrier}} = UdpDatagramCarrier.open("localhost", 9000)

# Fire-and-forget: encode the `->` request via the generated codec, wrap it in the
# library's Datagram, and send it. seq 0 marks an unsequenced datagram.
req = {sample}
payload = {req_mod}.to_cbor(req)

datagram = Datagrams.encode_datagram(Datagrams.new_datagram(op_ord, 0, payload))
{{:ok, carrier}} = UdpDatagramCarrier.send_datagram(carrier, datagram)

# Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is NO
# synchronous response; the caller must tolerate loss and reordering and handle a reply
# whenever (if ever) it shows up.
case UdpDatagramCarrier.recv_datagram(carrier) do
  {{:ok, bytes, _carrier}} ->
    {{:ok, dg}} = Datagrams.decode_datagram(bytes)
    resp = {res_mod}.from_cbor(dg.payload)
    IO.inspect(resp, label: "late response")

  :empty ->
    :ok
end
```

"#,
        op_ord = ex.op_ord,
        sample = ex.sample,
        req_mod = req_mod,
        res_mod = res_mod,
    ));
    out
}

/// The UDP datagram carrier adapter — spec-independent, so a constant. `send_datagram`
/// writes one UDP packet; `recv_datagram` returns the next inbound packet (or `:empty`)
/// and never waits for or correlates a reply.
const DATAGRAMS_CARRIER_ELIXIR: &str = r#"defmodule UdpDatagramCarrier do
  # One example carrier: UDP via :gen_udp. Datagrams are unreliable and unordered, so the
  # carrier never waits for or correlates a reply. The library owns the Datagram envelope;
  # the carrier owns only the socket.
  defstruct [:socket, :host, :port]

  def open(host, port) do
    case :gen_udp.open(0, [:binary, active: false]) do
      {:ok, socket} ->
        {:ok, %__MODULE__{socket: socket, host: String.to_charlist(host), port: port}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  def send_datagram(%__MODULE__{socket: socket, host: host, port: port} = c, bytes) do
    case :gen_udp.send(socket, host, port, bytes) do
      :ok -> {:ok, c}
      {:error, reason} -> {:error, reason}
    end
  end

  def recv_datagram(%__MODULE__{socket: socket} = c) do
    case :gen_udp.recv(socket, 0, 1_000) do
      {:ok, {_addr, _port, bytes}} -> {:ok, bytes, c}
      {:error, _reason} -> :empty
    end
  end
end
"#;

/// The pieces a unary (`->`) example call needs: the client module + method to call, a
/// constructible sample request literal, the request/response record module names (so
/// the datagram section can name `to_cbor`/`from_cbor`), and the op's datagram ordinal.
struct UnaryExample {
    client_module: String,
    method: String,
    has_request: bool,
    sample: String,
    req_module: Option<String>,
    res_module: Option<String>,
    op_ord: u32,
}

/// The first service (in rule order, matching the emitted client order) with a unary
/// `->` op the generated client actually exposes (record success, and a null or record
/// request — the only shapes `emit_client_module` turns into a callable method),
/// reduced to an example call. `None` when no service has a usable unary op.
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
                req_module: (!null_input).then(|| config.module(&ref_name(&op.input_type))),
                res_module: Some(config.module(&ref_name(&success))),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.map(|id| id as u32).unwrap_or(1),
            });
        }
    }
    None
}

/// The pieces the Events session needs: the generated server module + router/encoder
/// names, the inbound handler function, the outbound record module, a sample field for
/// the outbound literal, and the wire method/service strings.
struct ChannelExample {
    server_module: String,
    handler_fn: String,
    wire_method: String,
    wire_service: String,
    encode_fn: String,
    out_module: String,
    sample_field: String,
}

/// The first service (in rule order) with a `<->` op whose input and success output are
/// both records (so the generated router + encoder + per-type codec helpers exist).
/// `None` when no service has a usable channel op — the Events section then shows the
/// handshake/heartbeat without dispatch wiring.
fn first_channel_example(
    input: &WasmGeneratorInput,
    config: &ElixirConfig,
) -> Option<ChannelExample> {
    let records = record_csil_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            // The router decodes the input; the encoder encodes the success output.
            // Require both to be records for a compiling sample.
            let success = success_type(&op.output_type);
            if !is_record_ref(&op.input_type, &records) || !is_record_ref(&success, &records) {
                continue;
            }
            let base = service_base(&rule.name);
            return Some(ChannelExample {
                server_module: format!("{}.{base}Server", config.module_root),
                handler_fn: snake_case(&op.name),
                wire_method: wire_method_name(&op.name),
                wire_service: base.to_lowercase(),
                encode_fn: format!("encode_{}", snake_case(&op.name)),
                out_module: config.module(&ref_name(&success)),
                sample_field: channel_sample_field(input, config, &ref_name(&success)),
            });
        }
    }
    None
}

/// The `field: <sample>, ...` body for the outbound record literal in the Events
/// session — every required field of the outbound record, or empty for a record with
/// none. All `@enforce_keys` must be supplied or the `%Mod{...}` literal won't even
/// compile, so this mirrors `struct_literal` rather than naming just the first field.
fn channel_sample_field(input: &WasmGeneratorInput, config: &ElixirConfig, name: &str) -> String {
    let Some(group) = find_record(input, name) else {
        return String::new();
    };
    group
        .entries
        .iter()
        .filter(|e| !is_optional(e))
        .filter_map(|e| {
            field_atom(e)
                .map(|atom| format!("{atom}: {}", elixir_sample(input, config, &e.value_type)))
        })
        .collect::<Vec<_>>()
        .join(", ")
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

    // A package's `genquickstart.md` demonstrates both the calling side (the RPC and
    // Datagrams sections, over the generated client) and the handling side (the Events
    // section, over the generated server router), so a package must carry both surfaces
    // for its own quickstart to compile — regardless of which surface the target named.
    // A flat (non-package) build stays byte-identical: it emits only the requested
    // surface. Mirrors the OCaml generator's package-mode all-surfaces rule.
    let pkg_mode = config.emit_elixir_package;
    let want_client =
        matches!(surface, Surface::Client) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    let want_server =
        matches!(surface, Surface::Server) || (pkg_mode && !matches!(surface, Surface::TypesOnly));
    if input.csil_spec.service_count > 0 {
        if want_client && let Some(client) = generate_clients(&input, &config) {
            files.push(GeneratedFile {
                path: "client.gen.ex".to_string(),
                content: client,
            });
        }
        if want_server && let Some(server) = generate_servers(&input, &config) {
            files.push(GeneratedFile {
                path: "server.gen.ex".to_string(),
                content: server,
            });
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
        // The Quickstart carrier rides with the publishable package only; it names the
        // generated modules, so it stays at the root beside `mix.exs`. It is named
        // `genquickstart.md` rather than `README.md` so it never collides with a
        // consumer's own hand-written `README.md` (the consumer supplies that).
        // Only an explicit `emit_readme: false` suppresses it; absent or non-bool keeps it.
        if input
            .config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            files.push(GeneratedFile {
                path: "genquickstart.md".to_string(),
                content: readme(&input, &config),
            });
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
    let choices = choice_csil_types(input);
    let cx = Codec {
        config,
        records: &records,
        aliases: &aliases,
        choices: &choices,
    };

    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(group) => Some(group),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => Some(group),
            _ => None,
        };
        if let Some(group) = group {
            emit_struct_module(&mut content, &rule.name, group, &cx, warnings);
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
    cx: &Codec,
    warnings: &mut Vec<GeneratorWarning>,
) {
    let config = cx.config;
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
    // Elixir requires keyword entries (`key: default`) to come last in the list, so the
    // defaulted fields are partitioned after the bare atoms. The split is stable, so
    // within each group fields keep CSIL declaration order, and the canonical wire layout
    // (computed independently in `codec_fields`) is unaffected.
    let bare_fields: Vec<String> = keyed
        .iter()
        .filter(|(e, _)| entry_default_value(e).is_none())
        .map(|(_, atom)| format!(":{atom}"))
        .collect();
    let defaulted_fields: Vec<String> = keyed
        .iter()
        .filter_map(|(e, atom)| {
            entry_default_value(e)
                .map(|value| format!("{atom}: {}", default_literal(&e.value_type, value, config)))
        })
        .collect();
    let struct_fields: Vec<String> = bare_fields.into_iter().chain(defaulted_fields).collect();
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

    emit_struct_codec(content, group, cx);

    content.push_str("end\n\n");
}

/// True when a field's (possibly constrained) type is the exact-decimal core type.
fn is_decimal_type(ty: &CsilTypeExpression) -> bool {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => is_decimal_type(base_type),
        CsilTypeExpression::Builtin(name) => name == "decimal",
        _ => false,
    }
}

/// Render a field's `@default(...)` literal as an Elixir defstruct default. A decimal
/// default is built into the generated `Decimal` struct at generation time (parsing
/// the literal the way `Decimal.from_string/1` would) so the default needs no
/// compile-time call into another module.
fn default_literal(
    ty: &CsilTypeExpression,
    value: &CsilLiteralValue,
    config: &ElixirConfig,
) -> String {
    if is_decimal_type(ty)
        && let CsilLiteralValue::Text(s) = value
    {
        let (exp, mant) = parse_decimal(s);
        return format!(
            "%{}.Decimal{{exponent: {exp}, mantissa: {mant}}}",
            config.module_root
        );
    }
    literal_to_elixir(value)
}

/// Parse a decimal string into the exact `(exponent, mantissa)` pair the wire's CBOR
/// tag-4 fraction carries, identical to the reference `CsilDecimal::from_str`: mantissa
/// is every digit run together, exponent is the negated fractional-digit count.
fn parse_decimal(s: &str) -> (i64, i128) {
    let t = s.trim();
    let (negative, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int_part, frac_part) = rest.split_once('.').unwrap_or((rest, ""));
    let digits: String = format!("{int_part}{frac_part}");
    let magnitude: i128 = digits.parse().unwrap_or(0);
    let mantissa = if negative { -magnitude } else { magnitude };
    (-(frac_part.len() as i64), mantissa)
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

/// The codec context threaded through the per-field encoder/decoder: the module
/// config, the record name set, the transparent aliases, and the named choice types
/// (enums and unions) whose codecs are emitted inline. Bundled so adding a construct
/// touches one place rather than every signature.
struct Codec<'a> {
    config: &'a ElixirConfig,
    records: &'a HashSet<String>,
    aliases: &'a HashMap<String, CsilTypeExpression>,
    choices: &'a HashMap<String, Vec<CsilTypeExpression>>,
}

/// The named choice types (`Color = "a" / "b"`, `IdOrName = uint / text`) keyed by
/// name. A field referencing one gets an inline enum (bare-literal) or union
/// (tagged-sum) codec rather than the runtime `raise` a bare reference yields.
fn choice_csil_types(input: &WasmGeneratorInput) -> HashMap<String, Vec<CsilTypeExpression>> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(variants)) => {
                Some((rule.name.clone(), variants.clone()))
            }
            _ => None,
        })
        .collect()
}

/// When every variant of a choice is a literal it is an enum encoded as the BARE
/// literal (the literal is its own discriminant); the first variant's literal kind
/// fixes the scalar codec. A mixed choice is a union (tagged sum). Returns the
/// representative literal for the enum case, or `None` for a union.
fn enum_literal_kind(variants: &[CsilTypeExpression]) -> Option<&CsilLiteralValue> {
    let mut first = None;
    for v in variants {
        match v {
            CsilTypeExpression::Literal(l) => {
                if first.is_none() {
                    first = Some(l);
                }
            }
            _ => return None,
        }
    }
    first
}

/// The bare-literal encode for an enum value of the given literal kind.
fn enum_enc(kind: &CsilLiteralValue, expr: &str) -> String {
    match kind {
        CsilLiteralValue::Integer(_) => format!("{{:int, {expr}}}"),
        CsilLiteralValue::Float(_) => format!("{{:float, {expr}}}"),
        CsilLiteralValue::Bool(_) => format!("{{:bool, {expr}}}"),
        CsilLiteralValue::Bytes(_) => format!("{{:bytes, {expr}}}"),
        _ => format!("{{:text, {expr}}}"),
    }
}

/// The bare-literal decode for an enum value of the given literal kind.
fn enum_dec(kind: &CsilLiteralValue, expr: &str, root: &str) -> String {
    match kind {
        CsilLiteralValue::Integer(_) => format!("{root}.Cbor.to_int({expr})"),
        CsilLiteralValue::Float(_) => format!("{root}.Cbor.to_float({expr})"),
        CsilLiteralValue::Bool(_) => format!("{root}.Cbor.to_bool({expr})"),
        CsilLiteralValue::Bytes(_) => format!("{root}.Cbor.to_bytes({expr})"),
        _ => format!("{root}.Cbor.to_text({expr})"),
    }
}

/// A runtime guard selecting a union variant from a bare in-memory value (Elixir
/// models a union as the underlying value, not a tagged wrapper), so the encoder
/// picks the first declared variant whose guard matches the value's shape.
fn union_guard(ty: &CsilTypeExpression, expr: &str) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => union_guard(base_type, expr),
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "uint" | "nint" => format!("is_integer({expr})"),
            "float" | "double" | "float16" | "float32" | "float64" => format!("is_float({expr})"),
            "text" | "tstr" | "bytes" | "bstr" => format!("is_binary({expr})"),
            "bool" | "true" | "false" => format!("is_boolean({expr})"),
            _ => "true".to_string(),
        },
        CsilTypeExpression::Array { .. } => format!("is_list({expr})"),
        CsilTypeExpression::Map { .. } => format!("is_map({expr})"),
        CsilTypeExpression::Tuple(_) => format!("is_tuple({expr})"),
        _ => "true".to_string(),
    }
}

/// Encode a named choice: an enum as the bare literal, a union as the locked
/// `[variant_index, value]` tagged-sum CBOR array (index = 0-based declaration order).
fn enc_choice(name: &str, variants: &[CsilTypeExpression], expr: &str, cx: &Codec) -> String {
    if let Some(kind) = enum_literal_kind(variants) {
        return enum_enc(kind, expr);
    }
    let arms: Vec<String> = variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let guard = union_guard(v, expr);
            let inner = enc_value(v, expr, cx);
            format!("{guard} -> {{:array, [{{:int, {i}}}, {inner}]}}")
        })
        .collect();
    format!(
        "(cond do {}; true -> raise(\"csilgen: value does not match any {name} variant\") end)",
        arms.join("; ")
    )
}

/// An Elixir expression building the CBOR value tree for `expr` (a value of the
/// field's in-memory type). A shape the codec cannot model (a bare non-record,
/// non-choice reference) raises at runtime so the module still compiles and the
/// supported paths stay total.
fn enc_value(ty: &CsilTypeExpression, expr: &str, cx: &Codec) -> String {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => enc_value(base_type, expr, cx),
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
            // CBOR tag 4 exact decimal fraction `[exponent, mantissa]` against the
            // generated self-contained Decimal struct.
            "decimal" => format!(
                "{{:tag, 4, {{:array, [{{:int, {expr}.exponent}}, {{:int, {expr}.mantissa}}]}}}}"
            ),
            // `any` rides as the codec's own value tree, so it passes through unchanged.
            "any" => expr.to_string(),
            "null" | "nil" | "undefined" => format!("(_ = {expr}; :null)"),
            other => enc_named(other, expr, cx),
        },
        CsilTypeExpression::Reference(name) => enc_named(name, expr, cx),
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = enc_value(element_type, "csil_e", cx);
            format!("{{:array, Enum.map({expr}, fn csil_e -> {inner} end)}}")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let ek = enc_value(key, "csil_k", cx);
            let ev = enc_value(value, "csil_v", cx);
            format!("{{:map, Enum.map({expr}, fn {{csil_k, csil_v}} -> {{{ek}, {ev}}} end)}}")
        }
        // A tuple is a positional CBOR array; an absent optional element is `null`
        // in place (fixed length), matching the locked wire.
        CsilTypeExpression::Tuple(group) => {
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let elem = format!("elem({expr}, {i})");
                    let enc = enc_value(&e.value_type, &elem, cx);
                    if is_optional(e) {
                        format!("(if is_nil({elem}), do: :null, else: {enc})")
                    } else {
                        enc
                    }
                })
                .collect();
            format!("{{:array, [{}]}}", parts.join(", "))
        }
        CsilTypeExpression::Choice(variants) => enc_choice("inline", variants, expr, cx),
        _ => "raise(\"csilgen: no codec for this field shape\")".to_string(),
    }
}

/// Encode a reference to a named type: a record delegates to its generated
/// `to_cbor_value/1`; a named choice gets its enum/union codec; a transparent alias
/// (`StringInt64Map = {* text => int}`, `Tags = [* text]`, `Uuid = text`) encodes as
/// its underlying type. Anything else raises.
fn enc_named(name: &str, expr: &str, cx: &Codec) -> String {
    if cx.records.contains(name) {
        format!("{}.to_cbor_value({expr})", cx.config.module(name))
    } else if let Some(variants) = cx.choices.get(name) {
        enc_choice(name, variants, expr, cx)
    } else if let Some(underlying) = cx.aliases.get(name) {
        enc_value(underlying, expr, cx)
    } else {
        format!("raise(\"csilgen: no codec for type {name}\")")
    }
}

/// Decode a named choice: an enum from its bare literal, a union from the locked
/// `[variant_index, value]` tagged-sum CBOR array (dispatch on the 0-based index).
fn dec_choice(name: &str, variants: &[CsilTypeExpression], expr: &str, cx: &Codec) -> String {
    let root = &cx.config.module_root;
    if let Some(kind) = enum_literal_kind(variants) {
        return enum_dec(kind, expr, root);
    }
    let arms: Vec<String> = variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let inner = dec_value(v, "csil_inner", cx);
            format!("{i} -> {inner}")
        })
        .collect();
    format!(
        "(case {expr} do {{:array, [{{:int, csil_idx}}, csil_inner]}} -> (case csil_idx do {}; csil_other -> raise(\"csilgen: unknown {name} variant index #{{csil_other}}\") end) end)",
        arms.join("; ")
    )
}

/// An Elixir expression decoding `expr` (a CBOR value tree) into the field's
/// in-memory value.
fn dec_value(ty: &CsilTypeExpression, expr: &str, cx: &Codec) -> String {
    let root = &cx.config.module_root;
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => dec_value(base_type, expr, cx),
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
            "decimal" => format!(
                "(case {expr} do {{:tag, 4, {{:array, [{{:int, csil_exp}}, {{:int, csil_mant}}]}}}} -> %{root}.Decimal{{exponent: csil_exp, mantissa: csil_mant}} end)"
            ),
            "any" => expr.to_string(),
            "null" | "nil" | "undefined" => format!("(_ = {expr}; nil)"),
            other => dec_named(other, expr, cx),
        },
        CsilTypeExpression::Reference(name) => dec_named(name, expr, cx),
        CsilTypeExpression::Array { element_type, .. } => {
            let inner = dec_value(element_type, "csil_e", cx);
            format!(
                "(case {expr} do {{:array, csil_xs}} -> Enum.map(csil_xs, fn csil_e -> {inner} end) end)"
            )
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let dk = dec_value(key, "csil_k", cx);
            let dv = dec_value(value, "csil_v", cx);
            format!(
                "(case {expr} do {{:map, csil_kvs}} -> Map.new(csil_kvs, fn {{csil_k, csil_v}} -> {{{dk}, {dv}}} end) end)"
            )
        }
        // Positional tuple decode: bind each array element, rebuild the tuple; a `null`
        // in an optional slot becomes `nil`.
        CsilTypeExpression::Tuple(group) => {
            let binds: Vec<String> = (0..group.entries.len())
                .map(|i| format!("csil_t{i}"))
                .collect();
            let parts: Vec<String> = group
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let b = format!("csil_t{i}");
                    let dec = dec_value(&e.value_type, &b, cx);
                    if is_optional(e) {
                        format!("(case {b} do :null -> nil; _ -> {dec} end)")
                    } else {
                        dec
                    }
                })
                .collect();
            format!(
                "(case {expr} do {{:array, [{}]}} -> {{{}}} end)",
                binds.join(", "),
                parts.join(", ")
            )
        }
        CsilTypeExpression::Choice(variants) => dec_choice("inline", variants, expr, cx),
        _ => "raise(\"csilgen: no codec for this field shape\")".to_string(),
    }
}

/// Decode a reference to a named type: a record delegates to its generated
/// `from_cbor_value/1`; a named choice gets its enum/union codec; a transparent alias
/// decodes as its underlying type. Anything else raises.
fn dec_named(name: &str, expr: &str, cx: &Codec) -> String {
    if cx.records.contains(name) {
        format!("{}.from_cbor_value({expr})", cx.config.module(name))
    } else if let Some(variants) = cx.choices.get(name) {
        dec_choice(name, variants, expr, cx)
    } else if let Some(underlying) = cx.aliases.get(name) {
        dec_value(underlying, expr, cx)
    } else {
        format!("raise(\"csilgen: no codec for type {name}\")")
    }
}

/// Emit the per-struct codec surface inside a record's module: `to_cbor_value/1`
/// and `from_cbor_value/1` over the shared CBOR value tree, plus the
/// `to_cbor/1`/`from_cbor/1` byte wrappers the typed client calls.
fn emit_struct_codec(content: &mut String, group: &CsilGroupExpression, cx: &Codec) {
    let root = &cx.config.module_root;
    let fields = codec_fields(group);

    content.push_str("\n  @doc \"Builds the canonical CBOR value tree for this struct.\"\n");
    content.push_str(&format!(
        "  @spec to_cbor_value(t()) :: {root}.Cbor.value()\n"
    ));
    // A fieldless record never reads the struct, so bind it underscored to stay clean.
    let value_binding = if fields.is_empty() { "_v" } else { "v" };
    content.push_str(&format!(
        "  def to_cbor_value(%__MODULE__{{}} = {value_binding}) do\n"
    ));
    content.push_str("    {:map,\n     Enum.reject(\n       [\n");
    for f in &fields {
        let access = format!("v.{}", f.atom);
        let enc = enc_value(f.value_type, &access, cx);
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
    // A fieldless record never reads the decoded pairs, so bind them underscored.
    if fields.is_empty() {
        content.push_str("  def from_cbor_value({:map, _csil_kvs}) do\n");
        content.push_str("    %__MODULE__{}\n  end\n");
    } else {
        content.push_str("  def from_cbor_value({:map, csil_kvs}) do\n");
        content.push_str("    csil_fields = Map.new(csil_kvs)\n");
        content.push_str("    %__MODULE__{\n");
        emit_from_cbor_fields(content, &fields, cx);
        content.push_str("    }\n  end\n");
    }

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

/// Emit the `field: <decoder>` body of a record's `from_cbor_value/1`. An optional
/// field decodes inside a `case` whose present-arm binds the decoded value; when that
/// field's type has no real decoder (a non-record reference falls back to a `raise`),
/// the bound value is never read, so the binding is underscored to stay warning-clean.
fn emit_from_cbor_fields(content: &mut String, fields: &[CodecField<'_>], cx: &Codec) {
    for f in fields {
        if f.optional {
            let dec = dec_value(f.value_type, "csil_v", cx);
            let bind = if dec.contains("csil_v") {
                "csil_v"
            } else {
                "_csil_v"
            };
            content.push_str(&format!(
                "      {atom}: (case Map.get(csil_fields, {{:text, \"{wire}\"}}) do nil -> nil; {bind} -> {dec} end),\n",
                atom = f.atom,
                wire = f.wire
            ));
        } else {
            let fetch = format!("Map.fetch!(csil_fields, {{:text, \"{}\"}})", f.wire);
            let dec = dec_value(f.value_type, &fetch, cx);
            content.push_str(&format!("      {atom}: {dec},\n", atom = f.atom));
        }
    }
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
    // The exact-decimal core type rides on a self-contained struct (Elixir has no
    // stdlib decimal); emit it only when the spec actually uses `decimal`.
    if spec_uses_decimal(input) {
        content.push('\n');
        content.push_str(&format!("defmodule {}.Decimal do\n", config.module_root));
        content.push_str(DECIMAL_MODULE_ELIXIR);
        content.push_str("end\n");
    }
    Some(content)
}

/// Whether any record field is the exact-decimal core type (directly, or nested in an
/// array/map/tuple/constrained), so the generated `Decimal` struct is emitted only
/// when something needs it.
fn spec_uses_decimal(input: &WasmGeneratorInput) -> bool {
    fn ty_uses_decimal(ty: &CsilTypeExpression) -> bool {
        match ty {
            CsilTypeExpression::Builtin(name) => name == "decimal",
            CsilTypeExpression::Constrained { base_type, .. } => ty_uses_decimal(base_type),
            CsilTypeExpression::Array { element_type, .. } => ty_uses_decimal(element_type),
            CsilTypeExpression::Map { key, value, .. } => {
                ty_uses_decimal(key) || ty_uses_decimal(value)
            }
            CsilTypeExpression::Tuple(group) | CsilTypeExpression::Group(group) => {
                group.entries.iter().any(|e| ty_uses_decimal(&e.value_type))
            }
            CsilTypeExpression::Choice(variants) => variants.iter().any(ty_uses_decimal),
            _ => false,
        }
    }
    input
        .csil_spec
        .rules
        .iter()
        .any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                group.entries.iter().any(|e| ty_uses_decimal(&e.value_type))
            }
            CsilRuleType::TypeDef(ty) => ty_uses_decimal(ty),
            _ => false,
        })
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
    let aliases = codec_aliases(input);
    let choices = choice_csil_types(input);
    let cx = Codec {
        config,
        records: &records,
        aliases: &aliases,
        choices: &choices,
    };
    let mut emitted = false;
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_client_module(&mut content, &rule.name, service, &cx);
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

/// Whether `enc_value`/`dec_value` model an op-boundary type faithfully, so a per-op
/// codec helper is correct rather than silently lossy. Records, scalars, transparent
/// aliases, named choices (enums and unions), inline enums, arrays, maps, and tuples
/// all resolve to real codec expressions. An inline multi-variant non-literal choice
/// carries no wire discriminator the bare in-memory value can recover, and an unmodeled
/// reference has no codec, so those two keep the skip-with-note path the client falls
/// back to.
fn op_boundary_expressible(
    ty: &CsilTypeExpression,
    records: &HashSet<String>,
    aliases: &HashMap<String, CsilTypeExpression>,
    choices: &HashMap<String, Vec<CsilTypeExpression>>,
) -> bool {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => {
            op_boundary_expressible(base_type, records, aliases, choices)
        }
        CsilTypeExpression::Builtin(_) => true,
        CsilTypeExpression::Reference(name) => {
            records.contains(name) || aliases.contains_key(name) || choices.contains_key(name)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            op_boundary_expressible(element_type, records, aliases, choices)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            op_boundary_expressible(key, records, aliases, choices)
                && op_boundary_expressible(value, records, aliases, choices)
        }
        CsilTypeExpression::Tuple(_) => true,
        CsilTypeExpression::Choice(variants) => enum_literal_kind(variants).is_some(),
        _ => false,
    }
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
    cx: &Codec,
) {
    let config = cx.config;
    let records = cx.records;
    let base = service_base(name);
    let module = format!("{}.{base}Client", config.module_root);
    let root = &config.module_root;
    // Canonical wire strings (the wire contract): service lowercased, op PascalCased.
    let wire_service = base.to_lowercase();
    // The shared `<root>.Cbor` module the per-op helpers call is only emitted when the
    // spec declares records; without it a non-record boundary has no (de)serializer.
    let has_codec = !records.is_empty();

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

    // Per-op private (de)serializers for non-record boundaries, accumulated and emitted
    // after the public methods so they live in the same module the methods call.
    let mut helpers = String::new();
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
        let req_ok = null_input
            || op_boundary_expressible(&op.input_type, records, cx.aliases, cx.choices);
        let resp_ok = op_boundary_expressible(&success, records, cx.aliases, cx.choices);
        // A non-record boundary rides the `<root>.Cbor` module via a per-op helper, which
        // only exists when the spec declares records. Only a genuinely inexpressible
        // boundary (an inline multi-variant choice with no wire discriminator, or an
        // unmodeled reference) — or any non-record boundary in a spec with no codec — is
        // skipped now; every other op gets a method.
        let needs_codec = (!null_input && !is_record_ref(&op.input_type, records))
            || !is_record_ref(&success, records);
        if !req_ok || !resp_ok || (needs_codec && !has_codec) {
            content.push_str(&format!(
                "\n  # operation {} has a payload csilgen can't (de)serialize; handle it manually\n",
                op.name
            ));
            continue;
        }
        let func = snake_case(&op.name);
        let wire_method = wire_method_name(&op.name);
        // A record success reuses its module's `from_cbor`; any other shape gets a per-op
        // decoder built from the same value builders the record codec uses.
        let (out_ty, decode_resp) = if is_record_ref(&success, records) {
            let resp_mod = config.module(&ref_name(&success));
            (format!("{resp_mod}.t()"), format!("{resp_mod}.from_cbor(resp)"))
        } else {
            helpers.push_str(&format!(
                "\n  defp decode_{func}_response(csil_bytes) do\n    csil_root = {root}.Cbor.decode(csil_bytes)\n    {}\n  end\n",
                dec_value(&success, "csil_root", cx)
            ));
            (map_type(&success, config), format!("decode_{func}_response(resp)"))
        };
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
            // A record request reuses its module's `to_cbor`; any other shape gets a
            // per-op encoder over the shared value builders.
            let (in_ty, encode_req) = if is_record_ref(&op.input_type, records) {
                let req_mod = config.module(&ref_name(&op.input_type));
                (format!("{req_mod}.t()"), format!("{req_mod}.to_cbor(req)"))
            } else {
                helpers.push_str(&format!(
                    "\n  defp encode_{func}_request(req), do: {root}.Cbor.encode({})\n",
                    enc_value(&op.input_type, "req", cx)
                ));
                (
                    map_type(&op.input_type, config),
                    format!("encode_{func}_request(req)"),
                )
            };
            content.push_str(&format!("  @spec {func}(t(), {in_ty}) :: {out_ty}\n"));
            content.push_str(&format!(
                "  def {func}(%__MODULE__{{transport: transport}}, req) do\n"
            ));
            content.push_str(&format!(
                "    resp = {root}.Transport.call(transport, \"{wire_service}\", \"{wire_method}\", {encode_req})\n"
            ));
        }
        content.push_str(&format!("    {decode_resp}\n"));
        content.push_str("  end\n");
    }
    content.push_str(&helpers);
    content.push_str("end\n\n");
}

// ---------------------------------------------------------------------------
// Servers (handler behaviours + routers + encoders)
// ---------------------------------------------------------------------------

/// The codec seam routers/encoders use; emitted once when any channel op exists.
fn server_prelude(
    config: &ElixirConfig,
    has_channel: bool,
    spec_defines_service_error: bool,
) -> String {
    let root = &config.module_root;
    let mut out = String::new();
    if !spec_defines_service_error {
        out.push_str(&format!("defmodule {root}.ServiceError do\n"));
        out.push_str("  @moduledoc \"Transport-level error raised by routers and handlers.\"\n");
        out.push_str("  @enforce_keys [:code, :message]\n");
        out.push_str("  defstruct [:code, :message]\n");
        out.push_str("  @type t :: %__MODULE__{code: integer(), message: String.t()}\nend\n\n");
    }

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

    // A spec that declares its own `ServiceError` record already emits that module
    // (with a codec) in types.gen.ex; emitting the framework's plain twin here would
    // redefine the module and drop the codec. Defer to the spec's when present.
    let spec_defines_service_error =
        record_csil_names(input).contains("ServiceError")
            || input.csil_spec.rules.iter().any(|r| {
                r.name == "ServiceError" && matches!(r.rule_type, CsilRuleType::TypeDef(_))
            });

    let mut content = String::new();
    content.push_str(GEN_HEADER);
    content.push_str(&server_prelude(
        config,
        has_channel,
        spec_defines_service_error,
    ));

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
        // A single `@doc` for the whole `wire_id/1` function — Elixir attaches it to
        // the first clause, and a per-clause `@doc` warns about redefining it.
        if service.operations.iter().any(|op| op.wire_id.is_some()) {
            content
                .push_str("  @doc \"The @wire-id ordinal for an operation, by its name atom.\"\n");
        }
        for op in &service.operations {
            if let Some(op_id) = op.wire_id {
                content.push_str(&format!(
                    "  def wire_id(:{}), do: {op_id}\n",
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
            // A decimal field's ordered bounds compare exact decimal values, never the
            // Elixir term order of the wrapping struct, so the check must route through
            // the generated `Decimal` comparison.
            let decimal = is_decimal_type(&entry.value_type);
            for meta in &entry.metadata {
                if let CsilFieldMetadata::Constraint(c) = meta {
                    emit_metadata_check(&mut checks, &atom, optional, size_fn, decimal, config, c);
                }
            }
            if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
                for op in constraints {
                    emit_control_check(&mut checks, &atom, optional, size_fn, decimal, config, op);
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

/// Display a comparison bound without surrounding quotes so a text bound (a decimal
/// literal like `"0.0"`) cannot break the emitted Elixir message string literal.
fn bound_display(v: &CsilLiteralValue) -> String {
    match v {
        CsilLiteralValue::Text(s) => escape_msg(s),
        other => literal_to_elixir(other),
    }
}

/// A `must be {desc} {bound}` ordered comparison check. A decimal field compares exact
/// decimal values via the generated `Decimal` codec; everything else uses the native
/// Elixir comparison.
#[allow(clippy::too_many_arguments)]
fn ordered_check(
    checks: &mut Vec<String>,
    atom: &str,
    optional: bool,
    decimal: bool,
    config: &ElixirConfig,
    ex_op: &str,
    desc: &str,
    v: &CsilLiteralValue,
) {
    let access = format!("v.{atom}");
    let cond = if decimal {
        let root = &config.module_root;
        let bound = format!("{root}.Decimal.from_string({})", literal_to_elixir(v));
        let cmp = format!("{root}.Decimal.compare({access}, {bound})");
        match ex_op {
            ">=" => format!("{cmp} != :lt"),
            "<=" => format!("{cmp} != :gt"),
            ">" => format!("{cmp} == :gt"),
            "<" => format!("{cmp} == :lt"),
            "==" => format!("{cmp} == :eq"),
            _ => format!("{cmp} != :eq"),
        }
    } else {
        format!("{access} {ex_op} {}", literal_to_elixir(v))
    };
    checks.push(guard_clause(
        atom,
        optional,
        &cond,
        &format!("field '{atom}' must be {desc} {}", bound_display(v)),
    ));
}

#[allow(clippy::too_many_arguments)]
fn emit_metadata_check(
    checks: &mut Vec<String>,
    atom: &str,
    optional: bool,
    size_fn: &str,
    decimal: bool,
    config: &ElixirConfig,
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
            ordered_check(checks, atom, optional, decimal, config, ">=", "at least", v)
        }
        CsilValidationConstraint::MaxValue(v) => {
            ordered_check(checks, atom, optional, decimal, config, "<=", "at most", v)
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

#[allow(clippy::too_many_arguments)]
fn emit_control_check(
    checks: &mut Vec<String>,
    atom: &str,
    optional: bool,
    size_fn: &str,
    decimal: bool,
    config: &ElixirConfig,
    op: &CsilControlOperator,
) {
    let access = format!("v.{atom}");
    let ordered = |checks: &mut Vec<String>, ex_op: &str, desc: &str, v: &CsilLiteralValue| {
        ordered_check(checks, atom, optional, decimal, config, ex_op, desc, v);
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
        <<b::binary-size(^arg), r::binary>> = rest
        {{:bytes, b}, r}

      3 ->
        <<s::binary-size(^arg), r::binary>> = rest
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

/// The self-contained exact-decimal struct injected as the body of the generated
/// `<Root>.Decimal` module. It carries the exact `(exponent, mantissa)` the CBOR tag-4
/// fraction wire form needs (value = mantissa * 10^exponent), with parsing identical to
/// the reference generators so the bytes interoperate, plus an exact value comparison
/// the generated validators use for `.ge`/`.le` decimal bounds.
const DECIMAL_MODULE_ELIXIR: &str = r#"  @moduledoc """
  Exact base-10 decimal carried on the wire as a CBOR tag-4 decimal fraction — the
  two-element array `[exponent, mantissa]`, value = mantissa * 10^exponent — so it
  interoperates byte-for-byte with the other CSIL generators.
  """

  @enforce_keys [:exponent, :mantissa]
  defstruct [:exponent, :mantissa]

  @type t :: %__MODULE__{exponent: integer(), mantissa: integer()}

  @doc "Parse a decimal string into the exact (exponent, mantissa) pair."
  @spec from_string(String.t()) :: t()
  def from_string(s) do
    t = String.trim(s)

    {negative, rest} =
      case t do
        "-" <> r -> {true, r}
        "+" <> r -> {false, r}
        _ -> {false, t}
      end

    {int_part, frac_part} =
      case String.split(rest, ".", parts: 2) do
        [i, f] -> {i, f}
        [i] -> {i, ""}
      end

    digits = int_part <> frac_part
    magnitude = if digits == "", do: 0, else: String.to_integer(digits)
    mantissa = if negative, do: -magnitude, else: magnitude
    %__MODULE__{exponent: -String.length(frac_part), mantissa: mantissa}
  end

  @doc "Compare two decimals by exact value: :lt | :eq | :gt."
  @spec compare(t(), t()) :: :lt | :eq | :gt
  def compare(%__MODULE__{} = a, %__MODULE__{} = b) do
    e = min(a.exponent, b.exponent)
    av = a.mantissa * pow10(a.exponent - e)
    bv = b.mantissa * pow10(b.exponent - e)

    cond do
      av < bv -> :lt
      av > bv -> :gt
      true -> :eq
    end
  end

  @doc "True when a >= b by exact value."
  @spec gte?(t(), t()) :: boolean()
  def gte?(a, b), do: compare(a, b) != :lt

  @doc "True when a <= b by exact value."
  @spec lte?(t(), t()) :: boolean()
  def lte?(a, b), do: compare(a, b) != :gt

  # Scaling a mantissa to a common (smaller) exponent only ever raises 10 to a
  # non-negative power, so a simple repeated-multiply stays exact with no float.
  defp pow10(0), do: 1
  defp pow10(n) when n > 0, do: 10 * pow10(n - 1)
"#;

#[cfg(test)]
mod tests;
