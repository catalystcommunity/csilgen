//! Rust code generator for CSIL specifications (WASM module)
//!
//! This generator produces idiomatic Rust code with serde serialization support,
//! service trait definitions, and proper handling of CSIL metadata.

use csilgen_common::{
    ChoiceClass, CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition,
    CsilFieldMetadata, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilPosition, CsilRule, CsilRuleType, CsilServiceDefinition,
    CsilServiceDirection, CsilServiceOperation, CsilSizeConstraint, CsilSpecSerialized,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, GeneratorWarning, HoistOptions, WarningLevel,
    WasmGeneratorInput, WasmGeneratorOutput, choice_arm_literal, classify_choice,
    hoist_inline_composites, wasm_interface::*,
};
use std::collections::HashMap;
use std::collections::HashSet;

/// Which artifact surface a generator run emits, selected by the sub-target.
enum Surface {
    /// Server-side trait surface (`rust` / `rust-server`).
    Server,
    /// Transport-agnostic client (`rust-client`).
    Client,
    /// Types alone, no service surface (`rust-typesonly`).
    TypesOnly,
}

/// In-memory type chosen for the `decimal` core type, selected by the
/// `decimal_mapping` file option. The wire form (CBOR tag 4) is identical for
/// both; only the generated Rust type differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper (default).
    Csil,
    /// Map to `rust_decimal::Decimal`, taking a dependency on that crate.
    Library,
}

/// Read & validate the `decimal_mapping` option. Any value other than `csil` or
/// `library` is rejected at generation time so misconfiguration surfaces here
/// instead of producing code that references a type the consumer never wired up
/// (the same validate-early idiom the TypeScript generator uses for
/// `ts_bidirectional_transport`).
fn decimal_mapping(input: &WasmGeneratorInput) -> Result<DecimalMapping, String> {
    match input.config.options.get("decimal_mapping") {
        None => Ok(DecimalMapping::Csil),
        Some(v) => match v.as_str() {
            Some("csil") => Ok(DecimalMapping::Csil),
            Some("library") => Ok(DecimalMapping::Library),
            Some(other) => Err(format!(
                "decimal_mapping must be \"csil\" or \"library\", got {other:?}"
            )),
            None => Err(format!("decimal_mapping must be a string, got {v:?}")),
        },
    }
}

/// Whether this run should emit a self-contained publishable crate for Rust.
///
/// Triggered by `config.options["emit_packages"]` containing the token `"rust"`.
/// Parsed defensively: the option is normally a JSON array of language tokens,
/// but a single string or a JSON-encoded array string is tolerated too, and any
/// other shape simply means "off" rather than an error — so a malformed option
/// never blocks the default (non-package) output a consumer already relies on.
fn emit_rust_package(input: &WasmGeneratorInput) -> bool {
    let Some(value) = input.config.options.get("emit_packages") else {
        return false;
    };
    let tokens: Vec<String> = match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        // A consumer that can only pass string options may hand us the array as a
        // JSON-encoded string; fall back to treating a bare string as one token.
        serde_json::Value::String(s) => {
            serde_json::from_str::<Vec<String>>(s).unwrap_or_else(|_| vec![s.clone()])
        }
        _ => return false,
    };
    tokens.iter().any(|t| t == "rust")
}

/// Which transport sections a consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored, and an absent or empty
/// value (or one that names none of the three) means "all three". Mirrors the
/// TypeScript reference so the CLI's `--readme-csil-*` flags drive every generator the
/// same way.
fn wanted_transports(input: &WasmGeneratorInput) -> (bool, bool, bool) {
    let listed = match input.config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
            let any_known = ["rpc", "events", "datagrams"]
                .iter()
                .any(|t| names.contains(t));
            if any_known {
                Some((
                    names.contains("rpc"),
                    names.contains("events"),
                    names.contains("datagrams"),
                ))
            } else {
                None
            }
        }
        _ => None,
    };
    listed.unwrap_or((true, true, true))
}

/// Crate name for package mode: the `package_name` option verbatim, else a name
/// derived from the first service's base (e.g. `CorndogsService` -> `corndogs`),
/// else the neutral `csilgen_client`.
fn package_name(input: &WasmGeneratorInput) -> String {
    if let Some(name) = input
        .config
        .options
        .get("package_name")
        .and_then(|v| v.as_str())
    {
        // A path-style `package_name` is the cross-ecosystem source of truth; the crate
        // name wants only its tail. See `package_name_last_segment`.
        return csilgen_common::package_name_last_segment(name).to_string();
    }
    for rule in &input.csil_spec.rules {
        if matches!(rule.rule_type, CsilRuleType::ServiceDef(_)) {
            return sanitize_crate_name(&RustCodeGenerator::service_base(&rule.name));
        }
    }
    "csilgen_client".to_string()
}

/// Crate version for package mode: the `package_version` option, else `0.1.0`.
fn package_version(input: &WasmGeneratorInput) -> String {
    input
        .config
        .options
        .get("package_version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.1.0")
        .to_string()
}

/// Fold an arbitrary derived label into a valid lowercase Cargo crate name,
/// replacing anything outside `[a-z0-9_-]` so a service named with punctuation
/// can't produce an unbuildable `Cargo.toml`. Empty input keeps the neutral name.
fn sanitize_crate_name(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "csilgen_client".to_string()
    } else {
        out
    }
}

/// Self-contained exact-decimal helper injected into `types.rs` only when the
/// spec uses `decimal` under the default `csil` mapping. It carries the exact
/// CBOR tag-4 payload `[exponent, mantissa]` and converts to/from a decimal
/// library purely through strings, so the generated crate needs no decimal
/// dependency in default mode.
const CSIL_DECIMAL: &str = r#"/// Exact base-10 decimal carried on the wire as a CBOR tag-4 decimal fraction:
/// the two-element array `[exponent, mantissa]`, value = mantissa * 10^exponent.
/// Stored as that exact pair so no precision is lost. Convert to/from a decimal
/// library (e.g. `rust_decimal::Decimal`) through `as_str` / `from_str`, which is
/// why this type needs no such dependency of its own.
///
/// Equality and ordering are by *value*, not by representation: `(-2, 0)` ("0.00")
/// and `(-1, 0)` ("0.0") are equal, and the comparison honours the exact base-10
/// magnitude after normalizing differing exponents. This keeps `.eq`/`.ne` and the
/// `.ge/.le/.gt/.lt` validation bounds correct regardless of how a value was scaled.
///
/// On the wire it encodes as a CBOR **tag 4** decimal fraction — the two-element
/// array `[exponent, mantissa]` — so it interoperates byte-for-byte with the Go,
/// TypeScript, and Python generators. The generated `codec.gen.rs` owns that wire
/// form directly (reading/writing `exponent` and `mantissa` against its own value
/// model), so this type carries no serde impl and the crate needs no CBOR library.
#[derive(Debug, Clone, Copy)]
pub struct CsilDecimal {
    pub exponent: i64,
    pub mantissa: i128,
}

impl PartialEq for CsilDecimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for CsilDecimal {}

impl PartialOrd for CsilDecimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CsilDecimal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let sign_a = self.mantissa.signum();
        let sign_b = other.mantissa.signum();
        if sign_a != sign_b {
            return sign_a.cmp(&sign_b);
        }
        if sign_a == 0 {
            return Ordering::Equal;
        }
        // Same nonzero sign: compare absolute magnitudes, then flip for negatives.
        let magnitude = Self::cmp_magnitude(
            self.mantissa.unsigned_abs(),
            self.exponent,
            other.mantissa.unsigned_abs(),
            other.exponent,
        );
        if sign_a < 0 {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

impl CsilDecimal {
    /// Order two positive magnitudes `ma * 10^ea` and `mb * 10^eb` without ever
    /// scaling a mantissa (so no overflow): after stripping trailing zeros each
    /// value lands in the decade `[10^(w-1), 10^w)` for `w = digits + exponent`, so
    /// differing weights settle the order outright and equal weights reduce to a
    /// trailing-zero-padded digit-string compare.
    fn cmp_magnitude(mut ma: u128, mut ea: i64, mut mb: u128, mut eb: i64) -> std::cmp::Ordering {
        while ma.is_multiple_of(10) {
            ma /= 10;
            ea += 1;
        }
        while mb.is_multiple_of(10) {
            mb /= 10;
            eb += 1;
        }
        let digits_a = ma.to_string();
        let digits_b = mb.to_string();
        let weight_a = digits_a.len() as i64 + ea;
        let weight_b = digits_b.len() as i64 + eb;
        if weight_a != weight_b {
            return weight_a.cmp(&weight_b);
        }
        let width = digits_a.len().max(digits_b.len());
        let padded_a = format!("{digits_a:0<width$}");
        let padded_b = format!("{digits_b:0<width$}");
        padded_a.cmp(&padded_b)
    }
}

impl From<CsilDecimal> for (i64, i128) {
    fn from(d: CsilDecimal) -> Self {
        (d.exponent, d.mantissa)
    }
}

impl From<(i64, i128)> for CsilDecimal {
    fn from((exponent, mantissa): (i64, i128)) -> Self {
        Self { exponent, mantissa }
    }
}

#[allow(clippy::should_implement_trait, clippy::wrong_self_convention)]
impl CsilDecimal {
    pub fn new(exponent: i64, mantissa: i128) -> Self {
        Self { exponent, mantissa }
    }

    /// Canonical decimal string for the exact value: `(-2, 12345)` renders as
    /// `123.45`. Round-trips through `from_str` by value.
    pub fn as_str(&self) -> String {
        let digits = self.mantissa.unsigned_abs().to_string();
        let body = if self.exponent >= 0 {
            let mut out = digits;
            out.push_str(&"0".repeat(self.exponent as usize));
            out
        } else {
            let scale = (-self.exponent) as usize;
            if digits.len() > scale {
                let point = digits.len() - scale;
                format!("{}.{}", &digits[..point], &digits[point..])
            } else {
                let pad = "0".repeat(scale - digits.len());
                format!("0.{pad}{digits}")
            }
        };
        if self.mantissa < 0 {
            format!("-{body}")
        } else {
            body
        }
    }

    /// Parse a decimal string into the exact `(exponent, mantissa)` pair.
    pub fn from_str(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let (negative, rest) = match t.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let (int_part, frac_part) = match rest.split_once('.') {
            Some((i, f)) => (i, f),
            None => (rest, ""),
        };
        if frac_part.contains('.') || (int_part.is_empty() && frac_part.is_empty()) {
            return Err(format!("invalid decimal: {s}"));
        }
        let mut digits = String::with_capacity(int_part.len() + frac_part.len());
        digits.push_str(int_part);
        digits.push_str(frac_part);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("invalid decimal: {s}"));
        }
        let magnitude: i128 = digits
            .parse()
            .map_err(|e| format!("decimal out of range: {e}"))?;
        let mantissa = if negative { -magnitude } else { magnitude };
        Ok(Self {
            exponent: -(frac_part.len() as i64),
            mantissa,
        })
    }
}

impl std::fmt::Display for CsilDecimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}"#;

/// Error type injected into `types.rs` when at least one generated struct gets a
/// `validate` method. Self-contained so the generated crate needs no validation
/// dependency.
const VALIDATION_ERROR: &str = r#"/// Returned by a generated `validate` method when a field violates one of its
/// CSIL constraints. `field` names the offending field; `message` explains.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "validation failed for `{}`: {}",
            self.field, self.message
        )
    }
}

impl std::error::Error for ValidationError {}"#;

/// The shared client error type emitted at the top of the sync/async-drop-in
/// `client.rs`. The async twin reuses this type (imported from the sync module)
/// rather than redefining it, so both clients coexist in one module tree. The
/// transport trait is emitted separately by `client_prelude` because only its
/// `call` method differs between the sync and async shapes.
const CLIENT_ERROR: &str = r#"/// Error from a generated client call: a structured error the service returned,
/// or a transport-level failure. The caller-supplied `Transport` decides how an
/// error response maps onto `Service`.
#[derive(Debug, Clone)]
pub enum ClientError {
    Service { code: i64, message: String },
    Transport(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Service { code, message } => write!(f, "service error {code}: {message}"),
            ClientError::Transport(msg) => write!(f, "transport error: {msg}"),
        }
    }
}

impl std::error::Error for ClientError {}"#;

/// The pieces a unary (`->`) example call needs: the client struct to construct, the
/// method to call, whether that method takes a request, a compiling sample request
/// literal (empty when the op takes none), the request/response record type names (so
/// the datagram section can name `encode_<req>`/`decode_<res>`), and the op's datagram
/// ordinal.
struct RustExample {
    client_struct: String,
    method: String,
    null_input: bool,
    sample: String,
    req_snake: Option<String>,
    res_snake: Option<String>,
    op_ord: u64,
}

/// The pieces the Events session needs to dispatch through the generated channel
/// router (`route_<service>_channel`) and outbound encoder (`encode_<service>_<op>`):
/// the service trait name (which also names the router/encoder and the handler impl),
/// the wire service name, the demonstrated `<->` op's CSIL name (to give that one trait
/// method a real body), and a compiling literal for the op's success output record (the
/// encoder's argument).
struct RustChannelExample {
    service_trait: String,
    wire_service: String,
    op_name: String,
    outbound_sample: String,
}

/// The CSIL-RPC HTTP carrier the Quickstart embeds — spec-independent, so a constant.
/// It builds the request envelope with the library's `RpcRequest`, POSTs it to
/// `{base_url}/csil/v1/rpc` with `ureq`, and parses the reply with `RpcResponse`.
/// `into_transport_error` surfaces a non-zero transport status; the typed `ServiceError`
/// arm (a status-0 variant) is surfaced separately as `ClientError::Service`. It
/// implements the generated sync `Transport`, so the typed client rides it unchanged.
const RPC_CARRIER_RUST: &str = r#"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (RpcRequest/RpcResponse); the carrier owns only the transport. Swap ureq for any
// HTTP client — it implements the generated Transport seam.
pub struct HttpRpcCarrier {
    base_url: String,
}

impl HttpRpcCarrier {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

impl Transport for HttpRpcCarrier {
    fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError> {
        let envelope = RpcRequest::new(service, op, req.to_vec())
            .encode()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let url = format!("{}/csil/v1/rpc", self.base_url);
        let resp = ureq::post(&url)
            .set("Content-Type", "application/cbor")
            .set("Accept", "application/cbor")
            .send_bytes(&envelope)
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let mut body = Vec::new();
        resp.into_reader()
            .read_to_end(&mut body)
            .map_err(|e| ClientError::Transport(e.to_string()))?;

        // into_transport_error surfaces any non-zero transport status distinctly from a
        // typed application error.
        let decoded = RpcResponse::decode(&body)
            .map_err(|e| ClientError::Transport(e.to_string()))?
            .into_transport_error()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        // A typed application error rides as a status-0 "ServiceError" variant — distinct
        // from a transport failure. Surface it so the typed client decodes success only.
        if decoded.variant.as_deref() == Some("ServiceError") {
            return Err(ClientError::Service {
                code: 0,
                message: format!("csil-rpc {service}/{op}: ServiceError"),
            });
        }
        Ok(decoded.payload)
    }
}
"#;

/// The TLS `StreamCarrier` opener — spec-independent. std has no TLS, so the example
/// opens a plain `TcpStream` (which the compile-check uses) and notes that production
/// wraps it in a rustls (or native-tls) TLS stream; the library's 4-byte length-prefix
/// framing is identical over any `Read + Write`.
const EVENTS_CARRIER_RUST: &str = r#"// One example carrier: a TLS byte stream framed by the library's StreamCarrier (CSIL
// 4-byte length prefix). std has no TLS, so wrap a TcpStream in a rustls (or native-tls)
// TlsStream for production — the framing is identical over any Read + Write.

// The max-frame guard is a carrier setting, not a generated constant: raise it when a
// peer accepts payloads larger than the 16 MiB default (the envelope adds framing and
// request metadata around the payload, so the limit must exceed the largest payload),
// or lower it to harden an exposed listener. Valid limits are 1..=MAX_FRAME_LIMIT and
// are checked at construction.
const MAX_FRAME: usize = MAX_FRAME_DEFAULT;

fn open_tls_carrier(addr: &str) -> Result<StreamCarrier<TcpStream>, Box<dyn std::error::Error>> {
    let stream = TcpStream::connect(addr)?;
    Ok(StreamCarrier::with_max_frame(stream, MAX_FRAME)?)
}
"#;

/// The Events session body when the spec declares no usable channel op: the handshake
/// and heartbeat still apply, so they are shown, with a note where typed dispatch would
/// go. Spec-independent, so a constant.
const EVENTS_NO_CHANNEL_SESSION_RUST: &str = r#"fn session(carrier: &mut impl FrameCarrier) -> Result<(), Box<dyn std::error::Error>> {
    // $hello / $hello-ack handshake (control plane).
    let hello = Hello {
        versions: vec![VERSION],
        profiles: vec![Profile::Verbose.as_str().to_string()],
        service: None,
        auth: None,
    };
    carrier.send_frame(&hello.encode()?)?;
    let ack_frame = carrier
        .recv_frame()?
        .ok_or_else(|| "connection closed during handshake".to_string())?;
    let profile =
        Profile::parse(&HelloAck::decode(&ack_frame)?.profile).ok_or_else(|| "unsupported profile".to_string())?;

    // Recv loop: answer $ping with $pong. This package declares no <->/<- operations,
    // so there is no typed channel event to decode with the generated codec.
    while let Some(frame) = carrier.recv_frame()? {
        let ev = Event::decode(&frame, profile)?;
        if ev.event.as_deref() == Some(control::PING_NAME) {
            let ping = Heartbeat::decode(&ev.payload)?;
            let pong = Heartbeat {
                nonce: ping.nonce,
                at: None,
            };
            carrier.send_frame(
                &Event::verbose(None, control::PONG_NAME, pong.encode()?).encode(profile)?,
            )?;
        }
    }
    Ok(())
}

fn main() {
    let mut carrier = open_tls_carrier("localhost:7443").expect("connect");
    if let Err(err) = session(&mut carrier) {
        eprintln!("session failed: {err}");
    }
}
"#;

/// Whether a generated client run emits the blocking client, the async client, or
/// both, selected by the `client_style` option. The default is `Both`: every
/// consumer keeps their existing blocking client and gains an async twin alongside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStyle {
    Sync,
    Async,
    Both,
}

/// Read & validate the `client_style` option. Validated early (before any file is
/// emitted) so a misconfiguration fails the run rather than producing output, the
/// same validate-early idiom this generator uses for `decimal_mapping`. Absent
/// value defaults to `Both`.
fn client_style(input: &WasmGeneratorInput) -> Result<ClientStyle, String> {
    match input.config.options.get("client_style") {
        None => Ok(ClientStyle::Both),
        Some(v) => match v.as_str() {
            Some("sync") => Ok(ClientStyle::Sync),
            Some("async") => Ok(ClientStyle::Async),
            Some("both") => Ok(ClientStyle::Both),
            Some(other) => Err(format!(
                "client_style must be \"sync\", \"async\", or \"both\", got {other:?}"
            )),
            None => Err(format!("client_style must be a string, got {v:?}")),
        },
    }
}

/// Per-file shape of a generated client: whether its methods/transport seam are
/// async, plus the `Async` symbol marker that keeps the async twin distinct from
/// the sync client when both land in one module tree. `marker` is empty for a
/// stand-alone client (sync, or the async drop-in) that owns the canonical names.
#[derive(Clone, Copy)]
struct ClientShape {
    is_async: bool,
    marker: &'static str,
}

impl ClientShape {
    /// `async ` (trailing space) for the async shapes, empty for sync, so it drops
    /// straight into `pub {kw}fn` / `{kw}fn` without disturbing spacing.
    fn async_kw(self) -> &'static str {
        if self.is_async { "async " } else { "" }
    }

    /// `.await` spliced before the transport call's `?`, empty for sync, so the call
    /// site reads `…call(…).await?` for async vs `…call(…)?` for sync.
    fn dot_await(self) -> &'static str {
        if self.is_async { ".await" } else { "" }
    }

    /// The transport trait name: `Transport` for the sync/async drop-in clients,
    /// `AsyncTransport` for the marked twin so it coexists with the sync trait.
    fn transport_trait(self) -> String {
        format!("{}Transport", self.marker)
    }

    /// A per-service client struct name (`FooClient`, or `FooAsyncClient` for the twin).
    fn client_name(self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }
}

/// The client prelude: the shared `ClientError` (only for the canonical-name
/// clients; the marked twin imports it from the sync module) plus the
/// caller-supplied byte-carrier trait, whose `call` is `async fn` for the async
/// shapes. The generator never owns the wire (CBOR-over-HTTP etc.).
fn client_prelude(shape: ClientShape) -> String {
    let mut out = String::new();
    // A marked twin shares the sync module's `ClientError`; redefining it would
    // collide when both clients are re-exported from the module root.
    if shape.marker.is_empty() {
        out.push_str(CLIENT_ERROR);
        out.push_str("\n\n");
    }
    let trait_name = shape.transport_trait();
    let async_kw = shape.async_kw();
    out.push_str(
        "/// The caller-supplied byte carrier: it performs the call named by `(service, op)`\n\
         /// with the already-encoded request bytes and returns the response bytes, or an\n\
         /// error. The generated client owns (de)serialization via the codec; the carrier\n\
         /// only moves bytes, so it can be HTTP, a queue, or an in-process loop.\n",
    );
    out.push_str(&format!("pub trait {trait_name} {{\n"));
    out.push_str(&format!(
        "    {async_kw}fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError>;\n"
    ));
    out.push_str("}\n");
    out
}

/// The self-contained canonical-CBOR (RFC 8949 subset) value model, encoder,
/// decoder, generic composite helpers, and accessors every generated per-type codec
/// builds on. `bytes` is a Rust `Vec<u8>` carried as a CBOR byte string (major type
/// 2) by construction, never an array of integers. CSIL is the CBOR Service
/// Interface Language; the canonical wire is a CBOR map keyed by the CSIL field name
/// verbatim. Rust has serde + ciborium available, but the references for this batch
/// (C/Zig/OCaml/Dart/Swift/Go) instead emit a self-contained per-type codec so the
/// bytes are owned by generated code; Rust follows the same shape so every target
/// agrees byte-for-byte.
const CODEC_RUNTIME_RUST: &str = r#"/// A decode failure: the CBOR was malformed or did not match the expected shape.
#[derive(Debug, Clone, PartialEq)]
pub struct CsilCborError(pub String);

impl std::fmt::Display for CsilCborError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CsilCborError {}

/// A minimal canonical-CBOR value tree: a closed set of variants the generated codec
/// builds and walks. A map is an ordered list of pairs, so the encoder controls the
/// wire order of a record's keys explicitly (laid down in canonical order).
#[derive(Debug, Clone, PartialEq)]
pub enum CsilCborValue {
    Uint(u64),
    Int(i64),
    Bool(bool),
    Float(f64),
    Null,
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<CsilCborValue>),
    Map(Vec<(CsilCborValue, CsilCborValue)>),
    Tag(u64, Box<CsilCborValue>),
}

fn cbor_int(x: i64) -> CsilCborValue {
    CsilCborValue::Int(x)
}
fn cbor_uint(x: u64) -> CsilCborValue {
    CsilCborValue::Uint(x)
}
fn cbor_float(x: f64) -> CsilCborValue {
    CsilCborValue::Float(x)
}
fn cbor_bool(x: bool) -> CsilCborValue {
    CsilCborValue::Bool(x)
}
fn cbor_text(x: &str) -> CsilCborValue {
    CsilCborValue::Text(x.to_string())
}
fn cbor_bytes(x: &[u8]) -> CsilCborValue {
    CsilCborValue::Bytes(x.to_vec())
}

/// Serialize a value tree to canonical CBOR bytes.
fn cbor_encode(v: &CsilCborValue) -> Vec<u8> {
    let mut out = Vec::new();
    cbor_enc(v, &mut out);
    out
}

fn cbor_head(major: u8, n: u64, out: &mut Vec<u8>) {
    let mt = major << 5;
    if n < 24 {
        out.push(mt | n as u8);
    } else if n < 0x100 {
        out.push(mt | 24);
        out.push(n as u8);
    } else if n < 0x10000 {
        out.push(mt | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n < 0x1_0000_0000 {
        out.push(mt | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(mt | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

fn cbor_enc(v: &CsilCborValue, out: &mut Vec<u8>) {
    match v {
        CsilCborValue::Uint(x) => cbor_head(0, *x, out),
        // A non-negative `Int` rides major type 0 so it is byte-identical to a `Uint`
        // of the same magnitude; only a genuinely negative value uses major type 1.
        CsilCborValue::Int(x) => {
            if *x >= 0 {
                cbor_head(0, *x as u64, out);
            } else {
                cbor_head(1, (-(*x + 1)) as u64, out);
            }
        }
        CsilCborValue::Bool(x) => out.push(if *x { 0xf5 } else { 0xf4 }),
        CsilCborValue::Null => out.push(0xf6),
        CsilCborValue::Float(x) => {
            out.push(0xfb);
            out.extend_from_slice(&x.to_bits().to_be_bytes());
        }
        CsilCborValue::Text(s) => {
            let bytes = s.as_bytes();
            cbor_head(3, bytes.len() as u64, out);
            out.extend_from_slice(bytes);
        }
        CsilCborValue::Bytes(b) => {
            cbor_head(2, b.len() as u64, out);
            out.extend_from_slice(b);
        }
        CsilCborValue::Array(items) => {
            cbor_head(4, items.len() as u64, out);
            for item in items {
                cbor_enc(item, out);
            }
        }
        CsilCborValue::Map(entries) => {
            cbor_head(5, entries.len() as u64, out);
            for (k, val) in entries {
                cbor_enc(k, out);
                cbor_enc(val, out);
            }
        }
        CsilCborValue::Tag(num, inner) => {
            cbor_head(6, *num, out);
            cbor_enc(inner, out);
        }
    }
}

/// Parse a full CBOR item and reject trailing bytes, so a payload that is not
/// exactly one value is an error rather than a silently-truncated read.
fn cbor_decode(b: &[u8]) -> Result<CsilCborValue, CsilCborError> {
    let mut pos = 0usize;
    let v = cbor_dec(b, &mut pos)?;
    if pos != b.len() {
        return Err(CsilCborError(format!(
            "csil cbor: {} trailing bytes",
            b.len() - pos
        )));
    }
    Ok(v)
}

fn cbor_read_arg(b: &[u8], pos: &mut usize, low: u8) -> Result<u64, CsilCborError> {
    if low < 24 {
        *pos += 1;
        return Ok(low as u64);
    }
    let width = match low {
        24 => 1usize,
        25 => 2,
        26 => 4,
        27 => 8,
        _ => {
            return Err(CsilCborError(format!(
                "csil cbor: reserved additional info {low}"
            )))
        }
    };
    if *pos + 1 + width > b.len() {
        return Err(CsilCborError("csil cbor: truncated argument".to_string()));
    }
    let mut v = 0u64;
    for &byte in &b[*pos + 1..*pos + 1 + width] {
        v = (v << 8) | byte as u64;
    }
    *pos += 1 + width;
    Ok(v)
}

fn cbor_dec(b: &[u8], pos: &mut usize) -> Result<CsilCborValue, CsilCborError> {
    if *pos >= b.len() {
        return Err(CsilCborError(
            "csil cbor: unexpected end of input".to_string(),
        ));
    }
    let ib = b[*pos];
    let major = ib >> 5;
    let low = ib & 0x1f;
    if major == 7 {
        return match low {
            20 => {
                *pos += 1;
                Ok(CsilCborValue::Bool(false))
            }
            21 => {
                *pos += 1;
                Ok(CsilCborValue::Bool(true))
            }
            22 | 23 => {
                *pos += 1;
                Ok(CsilCborValue::Null)
            }
            26 => {
                let bits = cbor_read_arg(b, pos, low)?;
                Ok(CsilCborValue::Float(f32::from_bits(bits as u32) as f64))
            }
            27 => {
                let bits = cbor_read_arg(b, pos, low)?;
                Ok(CsilCborValue::Float(f64::from_bits(bits)))
            }
            _ => Err(CsilCborError(format!(
                "csil cbor: unsupported simple value {low}"
            ))),
        };
    }
    let arg = cbor_read_arg(b, pos, low)?;
    match major {
        0 => Ok(CsilCborValue::Uint(arg)),
        1 => {
            if arg > i64::MAX as u64 {
                return Err(CsilCborError(
                    "csil cbor: negative integer out of range".to_string(),
                ));
            }
            Ok(CsilCborValue::Int(-1 - arg as i64))
        }
        2 => {
            let n = arg as usize;
            if *pos + n > b.len() {
                return Err(CsilCborError(
                    "csil cbor: truncated byte string".to_string(),
                ));
            }
            let slice = b[*pos..*pos + n].to_vec();
            *pos += n;
            Ok(CsilCborValue::Bytes(slice))
        }
        3 => {
            let n = arg as usize;
            if *pos + n > b.len() {
                return Err(CsilCborError(
                    "csil cbor: truncated text string".to_string(),
                ));
            }
            let s = std::str::from_utf8(&b[*pos..*pos + n])
                .map_err(|e| CsilCborError(format!("csil cbor: invalid utf-8: {e}")))?
                .to_string();
            *pos += n;
            Ok(CsilCborValue::Text(s))
        }
        4 => {
            let n = arg as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(cbor_dec(b, pos)?);
            }
            Ok(CsilCborValue::Array(items))
        }
        5 => {
            let n = arg as usize;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let k = cbor_dec(b, pos)?;
                let val = cbor_dec(b, pos)?;
                entries.push((k, val));
            }
            Ok(CsilCborValue::Map(entries))
        }
        6 => {
            let inner = cbor_dec(b, pos)?;
            Ok(CsilCborValue::Tag(arg, Box::new(inner)))
        }
        _ => Err(CsilCborError(format!(
            "csil cbor: unexpected major type {major}"
        ))),
    }
}

/// Map a typed slice to a CBOR array via the per-element encoder.
fn cbor_enc_array<E>(xs: &[E], f: impl Fn(&E) -> CsilCborValue) -> CsilCborValue {
    CsilCborValue::Array(xs.iter().map(f).collect())
}

/// Map a typed map to a CBOR map. Rust `HashMap` iteration is unordered, so the inner
/// map's entry order is not canonicalized; the record's own keys (laid down at
/// generation time) are what the cross-language wire contract pins.
fn cbor_enc_map<K, V>(
    m: &std::collections::HashMap<K, V>,
    kf: impl Fn(&K) -> CsilCborValue,
    vf: impl Fn(&V) -> CsilCborValue,
) -> CsilCborValue {
    CsilCborValue::Map(m.iter().map(|(k, v)| (kf(k), vf(v))).collect())
}

fn cbor_dec_array<E>(
    v: &CsilCborValue,
    f: impl Fn(&CsilCborValue) -> Result<E, CsilCborError>,
) -> Result<Vec<E>, CsilCborError> {
    cbor_as_array(v)?.iter().map(f).collect()
}

fn cbor_dec_map<K: std::cmp::Eq + std::hash::Hash, V>(
    v: &CsilCborValue,
    kf: impl Fn(&CsilCborValue) -> Result<K, CsilCborError>,
    vf: impl Fn(&CsilCborValue) -> Result<V, CsilCborError>,
) -> Result<std::collections::HashMap<K, V>, CsilCborError> {
    let entries = cbor_as_map(v)?;
    let mut out = std::collections::HashMap::with_capacity(entries.len());
    for (k, val) in entries {
        out.insert(kf(k)?, vf(val)?);
    }
    Ok(out)
}

fn cbor_map_get<'a>(v: &'a CsilCborValue, key: &str) -> Option<&'a CsilCborValue> {
    if let CsilCborValue::Map(entries) = v {
        for (k, val) in entries {
            if matches!(k, CsilCborValue::Text(name) if name == key) {
                return Some(val);
            }
        }
    }
    None
}

fn cbor_expect_value(v: &CsilCborValue, expected: &CsilCborValue) -> Result<(), CsilCborError> {
    if v == expected {
        Ok(())
    } else {
        Err(CsilCborError(format!(
            "csil cbor: expected literal {expected:?}, got {v:?}"
        )))
    }
}

fn cbor_require<'a>(v: &'a CsilCborValue, key: &str) -> Result<&'a CsilCborValue, CsilCborError> {
    cbor_map_get(v, key).ok_or_else(|| CsilCborError(format!("csil cbor: missing field {key:?}")))
}

fn cbor_as_i64(v: &CsilCborValue) -> Result<i64, CsilCborError> {
    match v {
        CsilCborValue::Uint(x) => i64::try_from(*x)
            .map_err(|_| CsilCborError("csil cbor: integer overflows i64".to_string())),
        CsilCborValue::Int(x) => Ok(*x),
        _ => Err(CsilCborError("csil cbor: expected integer".to_string())),
    }
}

fn cbor_as_u64(v: &CsilCborValue) -> Result<u64, CsilCborError> {
    match v {
        CsilCborValue::Uint(x) => Ok(*x),
        CsilCborValue::Int(x) if *x >= 0 => Ok(*x as u64),
        CsilCborValue::Int(_) => Err(CsilCborError(
            "csil cbor: negative integer where unsigned expected".to_string(),
        )),
        _ => Err(CsilCborError(
            "csil cbor: expected unsigned integer".to_string(),
        )),
    }
}

fn cbor_as_f64(v: &CsilCborValue) -> Result<f64, CsilCborError> {
    match v {
        CsilCborValue::Float(x) => Ok(*x),
        CsilCborValue::Uint(x) => Ok(*x as f64),
        CsilCborValue::Int(x) => Ok(*x as f64),
        _ => Err(CsilCborError("csil cbor: expected float".to_string())),
    }
}

fn cbor_as_bool(v: &CsilCborValue) -> Result<bool, CsilCborError> {
    match v {
        CsilCborValue::Bool(b) => Ok(*b),
        _ => Err(CsilCborError("csil cbor: expected bool".to_string())),
    }
}

fn cbor_as_text(v: &CsilCborValue) -> Result<String, CsilCborError> {
    match v {
        CsilCborValue::Text(s) => Ok(s.clone()),
        _ => Err(CsilCborError("csil cbor: expected text".to_string())),
    }
}

fn cbor_as_bytes(v: &CsilCborValue) -> Result<Vec<u8>, CsilCborError> {
    match v {
        CsilCborValue::Bytes(b) => Ok(b.clone()),
        _ => Err(CsilCborError("csil cbor: expected byte string".to_string())),
    }
}

fn cbor_as_array(v: &CsilCborValue) -> Result<&[CsilCborValue], CsilCborError> {
    match v {
        CsilCborValue::Array(a) => Ok(a),
        _ => Err(CsilCborError("csil cbor: expected array".to_string())),
    }
}

fn cbor_as_map(v: &CsilCborValue) -> Result<&[(CsilCborValue, CsilCborValue)], CsilCborError> {
    match v {
        CsilCborValue::Map(m) => Ok(m),
        _ => Err(CsilCborError("csil cbor: expected map".to_string())),
    }
}"#;

/// Timestamp (CBOR tag 0, RFC3339, always UTC) codec, emitted only when the spec
/// uses `timestamp` so `chrono` is never an unused dependency.
const CODEC_TIMESTAMP_RUST: &str = r#"/// Encode a UTC instant as CBOR tag 0 RFC3339 text in UTC, per the wire contract;
/// sub-second precision is preserved when present and the `Z` offset is forced.
fn csil_enc_timestamp(t: &chrono::DateTime<chrono::Utc>) -> CsilCborValue {
    let text = t.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
    CsilCborValue::Tag(0, Box::new(CsilCborValue::Text(text)))
}

/// Decode a CBOR tag 0 RFC3339 timestamp back to a UTC instant.
fn csil_as_timestamp(v: &CsilCborValue) -> Result<chrono::DateTime<chrono::Utc>, CsilCborError> {
    let CsilCborValue::Tag(0, inner) = v else {
        return Err(CsilCborError(
            "csil cbor: expected CBOR tag 0 timestamp".to_string(),
        ));
    };
    let CsilCborValue::Text(s) = inner.as_ref() else {
        return Err(CsilCborError(
            "csil cbor: timestamp content must be text".to_string(),
        ));
    };
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| CsilCborError(format!("csil cbor: invalid timestamp: {e}")))
}"#;

/// Exact-integer (de)serialization for a decimal mantissa: a CBOR integer when it
/// fits in 64 bits, otherwise a bignum (RFC 8949 §3.4.3, tag 2 non-negative / tag 3
/// negative) so the full `i128` value stays exact. Emitted only alongside the decimal
/// codec.
const CODEC_BIGINT_RUST: &str = r#"fn csil_bigint_be_bytes(mut n: u128) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push((n & 0xff) as u8);
        n >>= 8;
    }
    out.reverse();
    out
}

fn csil_be_bytes_to_u128(bytes: &[u8]) -> Result<u128, CsilCborError> {
    if bytes.len() > 16 {
        return Err(CsilCborError(
            "csil cbor: bignum exceeds 128 bits".to_string(),
        ));
    }
    let mut n: u128 = 0;
    for &b in bytes {
        n = (n << 8) | b as u128;
    }
    Ok(n)
}

/// Encode an exact integer mantissa: a CBOR integer when it fits in 64 bits,
/// otherwise a bignum so the value stays exact across the wire.
fn csil_enc_bigint(m: i128) -> CsilCborValue {
    if let Ok(v) = i64::try_from(m) {
        CsilCborValue::Int(v)
    } else if let Ok(v) = u64::try_from(m) {
        CsilCborValue::Uint(v)
    } else if m >= 0 {
        CsilCborValue::Tag(
            2,
            Box::new(CsilCborValue::Bytes(csil_bigint_be_bytes(m as u128))),
        )
    } else {
        // A negative bignum encodes the magnitude of -1 - value.
        let mag = (-(m + 1)) as u128;
        CsilCborValue::Tag(3, Box::new(CsilCborValue::Bytes(csil_bigint_be_bytes(mag))))
    }
}

fn csil_dec_bigint(v: &CsilCborValue) -> Result<i128, CsilCborError> {
    match v {
        CsilCborValue::Uint(x) => Ok(*x as i128),
        CsilCborValue::Int(x) => Ok(*x as i128),
        CsilCborValue::Tag(num, inner) => {
            let CsilCborValue::Bytes(bytes) = inner.as_ref() else {
                return Err(CsilCborError(
                    "csil cbor: bignum content must be a byte string".to_string(),
                ));
            };
            let mag = csil_be_bytes_to_u128(bytes)?;
            match num {
                2 => i128::try_from(mag).map_err(|_| {
                    CsilCborError("csil cbor: decimal mantissa overflows i128".to_string())
                }),
                3 => {
                    let val = i128::try_from(mag).map_err(|_| {
                        CsilCborError("csil cbor: decimal mantissa overflows i128".to_string())
                    })?;
                    Ok(-1 - val)
                }
                _ => Err(CsilCborError(format!(
                    "csil cbor: unexpected bignum tag {num}"
                ))),
            }
        }
        _ => Err(CsilCborError(
            "csil cbor: expected integer mantissa".to_string(),
        )),
    }
}"#;

/// Decimal codec under the default `csil` mapping: the generated `CsilDecimal`
/// (exponent + i128 mantissa) maps straight onto CBOR tag 4 `[exponent, mantissa]`.
const CODEC_DECIMAL_CSIL_RUST: &str = r#"/// Encode a `CsilDecimal` as CBOR tag 4: `[exponent, mantissa]`.
fn csil_enc_decimal(d: &CsilDecimal) -> CsilCborValue {
    CsilCborValue::Tag(
        4,
        Box::new(CsilCborValue::Array(vec![
            CsilCborValue::Int(d.exponent),
            csil_enc_bigint(d.mantissa),
        ])),
    )
}

/// Decode a CBOR tag 4 decimal fraction into an exact `CsilDecimal`.
fn csil_as_decimal(v: &CsilCborValue) -> Result<CsilDecimal, CsilCborError> {
    let CsilCborValue::Tag(4, inner) = v else {
        return Err(CsilCborError(
            "csil cbor: expected CBOR tag 4 decimal".to_string(),
        ));
    };
    let CsilCborValue::Array(arr) = inner.as_ref() else {
        return Err(CsilCborError(
            "csil cbor: tag 4 content must be [exponent, mantissa]".to_string(),
        ));
    };
    if arr.len() != 2 {
        return Err(CsilCborError(
            "csil cbor: tag 4 content must be [exponent, mantissa]".to_string(),
        ));
    }
    let exponent = cbor_as_i64(&arr[0])?;
    let mantissa = csil_dec_bigint(&arr[1])?;
    Ok(CsilDecimal { exponent, mantissa })
}"#;

/// Decimal codec under the `library` mapping: `rust_decimal::Decimal` carries the same
/// exact value (mantissa * 10^-scale), so it maps onto CBOR tag 4 directly.
const CODEC_DECIMAL_LIBRARY_RUST: &str = r#"/// Encode a `rust_decimal::Decimal` as CBOR tag 4: `[exponent, mantissa]`.
fn csil_enc_decimal(d: &rust_decimal::Decimal) -> CsilCborValue {
    let exponent = -(d.scale() as i64);
    CsilCborValue::Tag(
        4,
        Box::new(CsilCborValue::Array(vec![
            CsilCborValue::Int(exponent),
            csil_enc_bigint(d.mantissa()),
        ])),
    )
}

/// Decode a CBOR tag 4 decimal fraction into a `rust_decimal::Decimal`.
fn csil_as_decimal(v: &CsilCborValue) -> Result<rust_decimal::Decimal, CsilCborError> {
    let CsilCborValue::Tag(4, inner) = v else {
        return Err(CsilCborError(
            "csil cbor: expected CBOR tag 4 decimal".to_string(),
        ));
    };
    let CsilCborValue::Array(arr) = inner.as_ref() else {
        return Err(CsilCborError(
            "csil cbor: tag 4 content must be [exponent, mantissa]".to_string(),
        ));
    };
    if arr.len() != 2 {
        return Err(CsilCborError(
            "csil cbor: tag 4 content must be [exponent, mantissa]".to_string(),
        ));
    }
    let exponent = cbor_as_i64(&arr[0])?;
    let mantissa = csil_dec_bigint(&arr[1])?;
    // `rust_decimal`'s scale is non-negative, so a non-negative wire exponent
    // (trailing-zero magnitude) is folded into the mantissa rather than the scale.
    if exponent >= 0 {
        let pow = 10i128
            .checked_pow(exponent as u32)
            .ok_or_else(|| CsilCborError("csil cbor: decimal exponent too large".to_string()))?;
        let scaled = mantissa
            .checked_mul(pow)
            .ok_or_else(|| CsilCborError("csil cbor: decimal mantissa overflow".to_string()))?;
        Ok(rust_decimal::Decimal::from_i128_with_scale(scaled, 0))
    } else {
        Ok(rust_decimal::Decimal::from_i128_with_scale(
            mantissa,
            (-exponent) as u32,
        ))
    }
}"#;

/// A just-enough model of the expressions the codec emitters build, so they can
/// be laid out the way rustfmt would lay them out. rustfmt reformats from the
/// AST, so the only emission that survives `rustfmt --check` unchanged is
/// rustfmt's own canonical form — which depends on widths the emitter can only
/// judge with the whole expression tree in hand.
#[derive(Clone)]
enum RustExpr {
    /// Text that never wraps internally.
    Atom(String),
    /// `head(args...)`. `macro_like` marks `format!`-style invocations, whose
    /// stacked arguments rustfmt leaves without trailing commas.
    Call {
        head: String,
        args: Vec<RustExpr>,
        macro_like: bool,
    },
    /// `|params| body`.
    Closure { params: String, body: Box<RustExpr> },
    /// `CsilCborValue::Array(vec![elems...])`.
    ArrayVec(Vec<RustExpr>),
    /// `match &place { Some(csil_t) => inner, None => CsilCborValue::Null }`.
    MatchOpt { place: String, inner: Box<RustExpr> },
    /// The positional tuple decoder's multi-statement closure body.
    TupleDec {
        arity: usize,
        elems: Vec<(RustExpr, bool)>,
    },
    /// The literal decoder's two-statement closure body.
    LitDec { expected: String, value: String },
}

impl RustExpr {
    fn call(head: &str, args: Vec<RustExpr>) -> RustExpr {
        RustExpr::Call {
            head: head.to_string(),
            args,
            macro_like: false,
        }
    }

    fn closure(params: &str, body: RustExpr) -> RustExpr {
        RustExpr::Closure {
            params: params.to_string(),
            body: Box::new(body),
        }
    }

    /// The one-line rendering, when rustfmt would accept one: every call's
    /// argument list within `fn_call_width` (60) and every array literal within
    /// `array_width` (60). Line fit against `max_width` is the caller's check,
    /// since only the caller knows the column.
    fn flat(&self) -> Option<String> {
        match self {
            RustExpr::Atom(s) => Some(s.clone()),
            RustExpr::Call { head, args, .. } => {
                let parts: Option<Vec<String>> = args.iter().map(RustExpr::flat).collect();
                let joined = parts?.join(", ");
                if joined.len() <= 60 {
                    Some(format!("{head}({joined})"))
                } else {
                    None
                }
            }
            RustExpr::Closure { params, body } => {
                let b = body.flat()?;
                Some(format!("{params} {b}"))
            }
            RustExpr::ArrayVec(elems) => {
                let parts: Option<Vec<String>> = elems.iter().map(RustExpr::flat).collect();
                let joined = parts?.join(", ");
                if joined.len() <= 60 {
                    Some(format!("CsilCborValue::Array(vec![{joined}])"))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "rust-code-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Rust struct/enum/service generator with serde support".to_string(),
        target: "rust".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/rust-generator".to_string()),
    };

    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(_) => return std::ptr::null(),
    };

    let bytes = metadata_json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Memory deallocation (WASM export)
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Main generator function (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => return std::ptr::null_mut(),
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated_ptr.add(4), bytes.len());
            }

            allocated_ptr
        }
        Err(_code) => std::ptr::null_mut(),
    }
}

/// Process the generation request
fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = match std::str::from_utf8(input_slice) {
        Ok(s) => s,
        Err(_e) => return Err(error_codes::INVALID_INPUT),
    };

    let input: WasmGeneratorInput = match serde_json::from_str(input_str) {
        Ok(input) => input,
        Err(_) => return Err(error_codes::SERIALIZATION_ERROR),
    };

    let mut generator = RustCodeGenerator::new(&input);
    let result = generator.generate();

    match result {
        Ok(files) => {
            let total_size = files.iter().map(|f| f.content.len()).sum();

            let stats = GenerationStats {
                files_generated: files.len(),
                total_size_bytes: total_size,
                services_count: input.csil_spec.service_count,
                fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
                generation_time_ms: 200,       // Mock generation time
                peak_memory_bytes: Some(4096), // Mock memory usage
            };

            let output = WasmGeneratorOutput {
                files,
                warnings: generator.warnings,
                stats,
            };

            Ok(output)
        }
        Err(_e) => Err(error_codes::GENERATION_ERROR),
    }
}

/// Rust code generator implementation
struct RustCodeGenerator<'a> {
    input: &'a WasmGeneratorInput,
    /// The spec every generation method actually reads: `input.csil_spec` with
    /// every inline (anonymous) choice reachable through a record field (or an
    /// array element/map key-value/tuple element inside one) hoisted out to its
    /// own synthesized `TypeChoice` rule (see `hoist_inline`). Computed
    /// once here so every `records`/`aliases`/`type_choice` lookup — all keyed by
    /// rule name against this field — sees the hoisted rules exactly like any
    /// other named choice, with no second inline-shape code path required.
    spec: CsilSpecSerialized,
    warnings: Vec<GeneratorWarning>,
    type_definitions: HashSet<String>,
    /// In-memory type chosen for `decimal`; validated once in `generate`.
    decimal_mapping: DecimalMapping,
    /// Set while emitting types when any struct gains a `validate` method, so the
    /// shared `ValidationError` type is injected exactly when it is referenced.
    needs_validation_error: bool,
    /// Set when emitted validation references `regex`, so the dependency note in
    /// the module root lists it only when actually required.
    uses_regex: bool,
}

impl<'a> RustCodeGenerator<'a> {
    fn new(input: &'a WasmGeneratorInput) -> Self {
        Self {
            spec: Self::hoist_inline(&input.csil_spec),
            input,
            warnings: Vec::new(),
            type_definitions: HashSet::new(),
            decimal_mapping: DecimalMapping::Csil,
            needs_validation_error: false,
            uses_regex: false,
        }
    }

    /// Hoist every inline (anonymous) composite in the spec to a synthesized named
    /// rule, so `records`/`aliases`/`type_choice`/`map_type_to_rust` — all of which
    /// dispatch purely off `CsilTypeExpression::Reference` resolved by rule name —
    /// can reach it with no second inline-shape code path.
    ///
    /// A `ServiceDef` rule's inline op `input_type`/`output_type` still needs this
    /// crate's own hoist (`rewrite_hoisted_choices`, unchanged from the
    /// pre-migration local hoist): the shared `csilgen_common::hoist_inline_composites`
    /// deliberately leaves `ServiceDef` completely untouched (see `hoist.rs`'s
    /// `rewrite_rule`) — an op boundary's `Success / ServiceError` shape is a
    /// generator-specific idiom the shared pass has no business special-casing —
    /// so a service op is skipped when it IS that shape (see
    /// `choice_needs_error_split`; hoisting it would corrupt the split the
    /// client/server emitters rely on) and otherwise hoisted locally. Without
    /// this, an op boundary like `create-user: Req -> User / UserError` (a
    /// genuine union, not the reserved `ServiceError`-splitting idiom) fell
    /// through `map_type_to_rust`'s generic-choice fallback to
    /// `serde_json::Value` — a type the generated crate never declares a
    /// dependency on, so it fails to compile. Every other rule kind
    /// (`GroupDef`/`TypeDef`/`TypeChoice`/`GroupChoice`) delegates to the shared
    /// hoist. `hoist_all_literal_choices: true` because Rust has no
    /// anonymous-sum-type field syntax: an inline `"a" / "b"` in a field position
    /// must become a synthesized named `enum` exactly like the pre-migration
    /// local hoist always did (its `Choice(arms)` arm hoisted unconditionally,
    /// with no all-literal check).
    ///
    /// The two kinds of hoisting are interleaved ONE ORIGINAL RULE AT A TIME, in
    /// original declaration order, to reproduce the pre-migration single pass's
    /// declaration order byte-for-byte: that pass visited every rule
    /// (`GroupDef`/`TypeDef`/`TypeChoice`/`GroupChoice`/`ServiceDef` alike) in one
    /// loop and appended every synthesized rule to one combined list in that
    /// visitation order, so a service's op-hoisted rules and a record's
    /// field-hoisted rules landed interleaved by original declaration order —
    /// e.g. `examples/build-integration/npm-project/api.csil` declares a
    /// hoistable record (`Notification`) BEFORE its `service` block and another
    /// (`CreateNotificationRequest`) AFTER it, so neither "service first" nor
    /// "records first" as two whole-spec passes reproduces this (proven by an
    /// earlier attempt at each, both left a declaration-order-only diff against
    /// the pre-migration baseline on a real `examples/` spec). The shared hoist
    /// has no per-rule entry point — `hoist_inline_composites` only takes a whole
    /// spec — so each original rule is run through it ONE AT A TIME: every OTHER
    /// rule (both remaining originals and everything hoisted by an earlier
    /// iteration) is swapped for a `name_reservation_stub`, so the shared hoist's
    /// name-collision avoidance (`canonical_key`-keyed, whole-spec) still sees
    /// the full universe of names in play even though only rule `i` is actually
    /// hoisted per call. `service_count`/`fields_with_metadata_count` don't
    /// affect hoisting and are irrelevant to the scratch spec, so a fixed `0` is
    /// fine there.
    fn hoist_inline(spec: &CsilSpecSerialized) -> CsilSpecSerialized {
        let mut rules: Vec<CsilRule> = spec.rules.clone();
        let original_len = rules.len();
        let mut synthesized: Vec<CsilRule> = Vec::new();

        for i in 0..original_len {
            if let CsilRuleType::ServiceDef(service) = rules[i].rule_type.clone() {
                let owner = rules[i].name.clone();
                let mut service = service;
                for op in &mut service.operations {
                    let op_snake = Self::to_snake_case(&op.name);
                    if !Self::choice_needs_error_split(&op.input_type) {
                        op.input_type = Self::rewrite_hoisted_choices(
                            op.input_type.clone(),
                            &format!("{owner}_{op_snake}_request"),
                            &mut synthesized,
                        );
                    }
                    if !Self::choice_needs_error_split(&op.output_type) {
                        op.output_type = Self::rewrite_hoisted_choices(
                            op.output_type.clone(),
                            &format!("{owner}_{op_snake}_response"),
                            &mut synthesized,
                        );
                    }
                }
                rules[i].rule_type = CsilRuleType::ServiceDef(service);
                continue;
            }

            let mut scratch_rules: Vec<CsilRule> = rules
                .iter()
                .enumerate()
                .map(|(j, r)| {
                    if j == i {
                        r.clone()
                    } else {
                        Self::name_reservation_stub(r)
                    }
                })
                .collect();
            scratch_rules.extend(synthesized.iter().map(Self::name_reservation_stub));
            let scratch_len = scratch_rules.len();
            let scratch = CsilSpecSerialized {
                rules: scratch_rules,
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            };
            let hoisted = hoist_inline_composites(
                &scratch,
                HoistOptions {
                    hoist_all_literal_choices: true,
                },
            );
            // The shared hoist rewrites every input rule in place (same index,
            // same length) and appends any newly synthesized rule after that —
            // `rules[i]` and the new tail are exactly where they land here.
            rules[i] = hoisted.rules[i].clone();
            synthesized.extend(hoisted.rules[scratch_len..].iter().cloned());
        }

        rules.extend(synthesized);
        CsilSpecSerialized {
            rules,
            source_content: spec.source_content.clone(),
            service_count: spec.service_count,
            fields_with_metadata_count: spec.fields_with_metadata_count,
        }
    }

    /// A name-only stand-in for `rule` fed to a per-rule `hoist_inline_composites`
    /// call (see `hoist_inline`): reserves `rule.name`'s canonical key in the
    /// shared hoist's collision-avoidance set without contributing any hoisting
    /// of its own. `ServiceDef` is the shared hoist's own designated no-op rule
    /// kind (`hoist.rs`'s `rewrite_rule` clones it untouched, see the module
    /// docs), so an empty one is a deliberate, guaranteed-inert stub rather than
    /// a repurposed "normal" rule kind that would need to stay empty by
    /// convention only.
    fn name_reservation_stub(rule: &CsilRule) -> CsilRule {
        CsilRule {
            name: rule.name.clone(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: Vec::new(),
                wire_id: None,
            }),
            position: rule.position.clone(),
            doc_comments: Vec::new(),
        }
    }

    /// Whether `ty` is the `Success / ServiceError` shape that `success_type`
    /// (and every op codepath that calls it) already splits into
    /// `Result<Success, ServiceError>`. That convention owns this exact shape —
    /// hoisting must leave it alone, or the split silently stops firing and a
    /// perfectly good `Result` return type turns into an opaque union.
    fn choice_needs_error_split(ty: &CsilTypeExpression) -> bool {
        match ty {
            CsilTypeExpression::Constrained { base_type, .. } => {
                Self::choice_needs_error_split(base_type)
            }
            CsilTypeExpression::Choice(arms) => arms.iter().any(is_service_error),
            _ => false,
        }
    }

    /// Hoist every field of one inline group body in place; `owner` names the
    /// synthesized type `<owner>_<field>`, matching a hand-written named-choice
    /// rule for that field. Only reached now via `rewrite_hoisted_choices`'s own
    /// `Group` arm (see `hoist_inline` — the general `GroupDef`/`TypeDef(Group)`
    /// case is the shared hoist's job; this is the service-op-signature-scoped
    /// leftover), so this stays as the mutually-recursive pair it always was with
    /// `rewrite_hoisted_choices` rather than being folded into it.
    fn hoist_group_fields(
        owner: &str,
        group: &mut CsilGroupExpression,
        synthesized: &mut Vec<CsilRule>,
    ) {
        for entry in &mut group.entries {
            let Some(wire) = Self::entry_wire_name(entry) else {
                continue;
            };
            // Deliberately NOT `escape_rust_ident`: that produces `r#type` for a
            // keyword field, which is only valid syntax as a whole identifier. Here
            // the field name is just a component glued onto `owner` (`Notification`
            // + `type` -> `Notification_type`), and the combined name is never
            // itself a keyword, so no raw-identifier escaping is needed — applying
            // it anyway produced the unparseable `Notification_r#type`.
            let field = Self::to_snake_case(&wire);
            let synth_stem = format!("{owner}_{field}");
            entry.value_type =
                Self::rewrite_hoisted_choices(entry.value_type.clone(), &synth_stem, synthesized);
        }
    }

    /// Recursively hoist every inline choice/group reachable through `ty` — a
    /// service operation's `input_type`/`output_type` (see `hoist_service_ops`),
    /// directly, or nested through an array element / map key / map value / tuple
    /// element / inline group field — into a synthesized rule named `synth_stem`
    /// (suffixed `_item`/`_key`/`_value`/`_<index>`/`_<field>` per nesting step so
    /// nested hoists stay unique), replacing it with a `Reference` to that rule. A
    /// `Constrained` wrapper is preserved around the rewritten base so a
    /// field-level constraint on an inline choice is not silently dropped. Any
    /// other shape (scalar, `Reference`, an already-named choice's own arms)
    /// passes through unchanged. This is the op-signature-scoped remnant of what
    /// was, pre-migration, this crate's only hoist pass — the general
    /// `GroupDef`/`TypeDef`/`TypeChoice`/`GroupChoice` traversal now lives in
    /// `csilgen_common::hoist_inline_composites` (see `hoist_inline`).
    fn rewrite_hoisted_choices(
        ty: CsilTypeExpression,
        synth_stem: &str,
        synthesized: &mut Vec<CsilRule>,
    ) -> CsilTypeExpression {
        match ty {
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => {
                let base = Self::rewrite_hoisted_choices(*base_type, synth_stem, synthesized);
                CsilTypeExpression::Constrained {
                    base_type: Box::new(base),
                    constraints,
                }
            }
            CsilTypeExpression::Choice(arms) => {
                synthesized.push(CsilRule {
                    name: synth_stem.to_string(),
                    rule_type: CsilRuleType::TypeChoice(arms),
                    position: CsilPosition {
                        line: 0,
                        column: 0,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                });
                CsilTypeExpression::Reference(synth_stem.to_string())
            }
            // An inline (anonymous) group has exactly the same problem an inline
            // choice does: `generate_struct`/`map_type_to_rust` have no nominal
            // route for it (no `Reference` name to hang a `csil_enc_`/`csil_dec_`
            // pair off), so it fell to the generic `serde_json::Value` fallback —
            // a type the generated crate never declares a dependency on. Hoist it
            // to a synthesized `GroupDef` the exact same way, recursing into the
            // new group's own fields first (with `synth_stem` as their owner) so a
            // doubly-nested inline group/choice gets the same treatment one level
            // deeper — mirrors the OCaml generator's `hoist_inline_composites`.
            CsilTypeExpression::Group(mut group) => {
                Self::hoist_group_fields(synth_stem, &mut group, synthesized);
                synthesized.push(CsilRule {
                    name: synth_stem.to_string(),
                    rule_type: CsilRuleType::GroupDef(group),
                    position: CsilPosition {
                        line: 0,
                        column: 0,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                });
                CsilTypeExpression::Reference(synth_stem.to_string())
            }
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let elem = Self::rewrite_hoisted_choices(
                    *element_type,
                    &format!("{synth_stem}_item"),
                    synthesized,
                );
                CsilTypeExpression::Array {
                    element_type: Box::new(elem),
                    occurrence,
                }
            }
            CsilTypeExpression::Map {
                key,
                value,
                occurrence,
            } => {
                let k =
                    Self::rewrite_hoisted_choices(*key, &format!("{synth_stem}_key"), synthesized);
                let v = Self::rewrite_hoisted_choices(
                    *value,
                    &format!("{synth_stem}_value"),
                    synthesized,
                );
                CsilTypeExpression::Map {
                    key: Box::new(k),
                    value: Box::new(v),
                    occurrence,
                }
            }
            CsilTypeExpression::Tuple(group) => {
                let entries = group
                    .entries
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut entry)| {
                        entry.value_type = Self::rewrite_hoisted_choices(
                            entry.value_type,
                            &format!("{synth_stem}_{i}"),
                            synthesized,
                        );
                        entry
                    })
                    .collect();
                CsilTypeExpression::Tuple(CsilGroupExpression { entries })
            }
            other => other,
        }
    }

    fn generate(&mut self) -> Result<Vec<GeneratedFile>, String> {
        // Dispatch on the requested target. The base `rust` (and explicit
        // `rust-server`) target emits the server-side trait surface;
        // `rust-client` emits a transport-agnostic client; `rust-typesonly`
        // emits the types alone. An unrecognized sub-target is an error rather
        // than a silent fall-through.
        let surface = match self.input.config.target.as_str() {
            "rust" | "rust-server" => Surface::Server,
            "rust-client" => Surface::Client,
            "rust-typesonly" => Surface::TypesOnly,
            other => {
                return Err(format!(
                    "Unknown rust sub-target '{other}'. Supported: rust, rust-server, rust-client, rust-typesonly"
                ));
            }
        };

        // Validate the decimal mapping before emitting anything so a bad option
        // fails the run rather than producing code referencing a missing type.
        self.decimal_mapping = decimal_mapping(self.input)?;

        // Validate `client_style` for every target (not just the client surface) so
        // a misconfiguration fails the run early, before any file is emitted.
        let style = client_style(self.input)?;

        let mut files = Vec::new();

        // Generate types.rs for structs and enums
        let types_content = self.generate_types()?;
        if !types_content.is_empty() {
            files.push(GeneratedFile {
                path: "types.rs".to_string(),
                content: types_content,
            });
        }

        // The self-contained per-type CBOR codec is emitted whenever the spec
        // declares a record type, independent of the service surface: the types
        // need (de)serialization regardless of whether a client or server rides
        // on top. Nothing else in the generated crate owns the payload wire.
        if let Some(codec) = self.generate_codec()? {
            files.push(GeneratedFile {
                path: "codec.gen.rs".to_string(),
                content: codec,
            });
        }

        // In self-contained package mode the genquickstart demonstrates both the calling
        // side (CSIL-RPC/Datagrams over the client) and the handling side (CSIL-Events
        // over the channel router), so the package must carry both surfaces for its own
        // quickstart to compile — regardless of which surface the target requested. Flat
        // (non-package) output stays byte-identical: it emits only the requested surface.
        // Mirrors the OCaml generator.
        let package = emit_rust_package(self.input);
        let want_client = matches!(surface, Surface::Client)
            || (package && !matches!(surface, Surface::TypesOnly));
        let want_server = matches!(surface, Surface::Server)
            || (package && !matches!(surface, Surface::TypesOnly));

        if self.spec.service_count > 0 {
            if want_client {
                // `Both` (default) ships the blocking client at the canonical
                // `client.rs` plus an async twin (marked symbols) at
                // `client_async.rs`; `Async` makes the async client a drop-in at
                // `client.rs` with canonical names; `Sync` is the original output.
                let sync = ClientShape {
                    is_async: false,
                    marker: "",
                };
                let async_drop_in = ClientShape {
                    is_async: true,
                    marker: "",
                };
                let async_twin = ClientShape {
                    is_async: true,
                    marker: "Async",
                };
                match style {
                    ClientStyle::Sync => files.push(GeneratedFile {
                        path: "client.rs".to_string(),
                        content: self.generate_client(sync)?,
                    }),
                    ClientStyle::Async => files.push(GeneratedFile {
                        path: "client.rs".to_string(),
                        content: self.generate_client(async_drop_in)?,
                    }),
                    ClientStyle::Both => {
                        files.push(GeneratedFile {
                            path: "client.rs".to_string(),
                            content: self.generate_client(sync)?,
                        });
                        files.push(GeneratedFile {
                            path: "client_async.rs".to_string(),
                            content: self.generate_client(async_twin)?,
                        });
                    }
                }
            }
            if want_server {
                files.push(GeneratedFile {
                    path: "services.rs".to_string(),
                    content: self.generate_services()?,
                });
            }
        }

        // Generate module root file to tie everything together. In package mode the
        // root is the crate root (`lib.rs`) so the emitted directory is itself a
        // buildable crate; otherwise it is a plain module (`mod.rs` by default) the
        // consumer drops into their own crate.
        let root_filename = if package {
            "lib.rs".to_string()
        } else {
            self.input
                .config
                .options
                .get("module_root_filename")
                .and_then(|v| v.as_str())
                .unwrap_or("mod.rs")
                .to_string()
        };
        let lib_content = self.generate_lib_file(&files)?;
        files.push(GeneratedFile {
            path: root_filename,
            content: lib_content,
        });

        // In package mode the generated `.rs` files become the crate's `src/`, and a
        // `Cargo.toml` is added so the output directory is a publishable crate a
        // consumer adds as a path/git dependency. The module declarations in
        // `lib.rs` (`pub mod types;`, `#[path = "codec.gen.rs"]`) resolve relative to
        // `src/lib.rs`, so prefixing every emitted file with `src/` is the whole of
        // the restructuring — no content changes. The non-package output is untouched.
        if package {
            for file in &mut files {
                file.path = format!("src/{}", file.path);
            }
            files.push(GeneratedFile {
                path: "Cargo.toml".to_string(),
                content: self.generate_cargo_toml(),
            });
            // The README (at the crate root, beside `Cargo.toml`, not under `src/`)
            // carries a copy-paste Quickstart only when this run emits a *sync* client
            // for a carrier to ride on — the canonical blocking `client.rs` exists
            // under `Sync`/`Both` style and the `Client` surface. Otherwise it falls
            // back to a types/codec section, mirroring the TypeScript generator.
            // Package mode emits the client surface for every target (`want_client`), so
            // the typed RPC example is meaningful regardless of the requested target; it
            // still needs a *blocking* client for the carrier to ride, hence Sync|Both.
            let client_quickstart =
                want_client && matches!(style, ClientStyle::Sync | ClientStyle::Both);
            // The README is opt-out: only an explicit `emit_readme: false` suppresses
            // it. Absent / non-bool / `true` all keep the prior behavior so existing
            // consumers see no change.
            let emit_readme = self
                .input
                .config
                .options
                .get("emit_readme")
                .and_then(|v| v.as_bool())
                != Some(false);
            if emit_readme {
                files.push(GeneratedFile {
                    path: "genquickstart.md".to_string(),
                    content: self.generate_readme(client_quickstart)?,
                });
            }
        }

        for file in &mut files {
            if file.path.ends_with(".rs") {
                file.content = format!("{}\n", file.content.trim_end());
            }
        }

        Ok(files)
    }

    /// Build the `Cargo.toml` for package mode. The crate is kept dependency-free
    /// whenever the spec allows it (the self-contained CBOR codec owns the wire); a
    /// dependency is listed only when the in-memory type for a builtin demands it
    /// (`chrono` for timestamp, `rust_decimal` only under the library mapping,
    /// `regex` only when emitted validation needs it). Those versions resolve from
    /// the local cargo cache, so the crate still builds offline.
    fn generate_cargo_toml(&self) -> String {
        let name = package_name(self.input);
        let version = package_version(self.input);
        let mut content = String::new();
        content.push_str("[package]\n");
        content.push_str(&format!("name = \"{name}\"\n"));
        content.push_str(&format!("version = \"{version}\"\n"));
        content.push_str("edition = \"2021\"\n");

        let deps = self.crate_dependencies();
        if !deps.is_empty() {
            content.push_str("\n[dependencies]\n");
            for (crate_name, req) in deps {
                content.push_str(&format!("{crate_name} = \"{req}\"\n"));
            }
        }
        content
    }

    /// The external crates the generated code needs, as `(name, version-req)` pairs.
    /// Shared by the package-mode `Cargo.toml` and the non-package dependency note so
    /// the two never drift. Empty for a spec whose codec is fully self-contained.
    fn crate_dependencies(&self) -> Vec<(&'static str, &'static str)> {
        let mut deps = Vec::new();
        if self.spec_uses_builtin("timestamp") {
            deps.push(("chrono", "0.4"));
        }
        if self.spec_uses_builtin("decimal") && self.decimal_mapping == DecimalMapping::Library {
            deps.push(("rust_decimal", "1"));
        }
        if self.uses_regex {
            deps.push(("regex", "1"));
        }
        deps
    }

    /// The package README: a transport-by-transport **Quickstart** built on the official
    /// `csilgen-transport` library. The generated codec owns CBOR (de)serialization of
    /// your types and the library owns the envelope, framing, and connection lifecycle;
    /// you supply only a *carrier* that moves bytes, so the same typed surface rides
    /// HTTP, TLS, a WebSocket, QUIC, or raw UDP unchanged. Each requested section
    /// (CSIL-RPC over HTTP, CSIL-Events over TLS, CSIL-Datagrams over UDP) is a complete,
    /// copy-paste program built on the library.
    fn generate_readme(&mut self, client_quickstart: bool) -> Result<String, String> {
        let name = package_name(self.input);
        // The crate is referenced in Rust paths by its identifier form (hyphens become
        // underscores), so `use` lines and the example resolve regardless of the name.
        let krate = name.replace('-', "_");
        let mut out = format!(
            "# {name}\n\n\
             Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
             (de)serialization and the `csilgen-transport` library owns the envelope,\n\
             framing, and connection lifecycle. You supply only a *carrier* that moves\n\
             bytes, so the same typed surface rides HTTP, TLS, a WebSocket, QUIC, or raw\n\
             UDP unchanged.\n\n\
             ## Add to your project\n\n\
             ```toml\n\
             [dependencies]\n\
             {name} = {{ path = \"./{name}\" }} # TODO: point at the published/vendored crate\n\
             csilgen-transport = \"0.1\"        # TODO: not yet published — vendor or git for now\n\
             ureq = \"2\"                       # the CSIL-RPC carrier's blocking HTTP client\n\
             ```\n\n"
        );

        let (rpc, events, datagrams) = wanted_transports(self.input);
        // The typed RPC client (and so a meaningful CSIL-RPC example) only exists for a
        // sync client surface; the per-type codec the Events/Datagrams sections ride is
        // emitted for every target, so those sections render regardless of surface.
        let unary = if client_quickstart {
            self.first_unary_example()
        } else {
            None
        };
        let channel = self.first_channel_example();
        if rpc {
            out.push_str(&self.rust_rpc_section(&krate, unary.as_ref()));
        }
        if events {
            out.push_str(&self.rust_events_section(&krate, channel.as_ref())?);
        }
        if datagrams {
            out.push_str(&self.rust_datagrams_section(&krate, unary.as_ref()));
        }
        Ok(out)
    }

    /// CSIL-RPC over HTTP: a carrier implementing the generated sync `Transport` that
    /// builds the request with the library's `RpcRequest` and parses the reply with
    /// `RpcResponse` (never hand-rolled), POSTing to `{base_url}/csil/v1/rpc`. The typed
    /// client decodes the success payload; a non-zero transport status and the
    /// `ServiceError` arm are surfaced distinctly. Rendered only when the package emits a
    /// sync client; otherwise a short note points at the `rust-client` target.
    fn rust_rpc_section(&self, krate: &str, ex: Option<&RustExample>) -> String {
        let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
        out.push_str(
            "Request/response. The library owns the envelope (`RpcRequest`/`RpcResponse`);\n\
             you bring a carrier that moves bytes. The HTTP carrier below is just one\n\
             example — swap `ureq` for any client (it implements the generated `Transport`\n\
             seam).\n\n",
        );
        let Some(ex) = ex else {
            out.push_str(
                "This package emits no sync RPC client (generate the `rust-client` target\n\
                 for one), so there is no CSIL-RPC call to make here.\n\n",
            );
            return out;
        };
        out.push_str("```rust\n");
        out.push_str(&format!(
            "use {krate}::*;\nuse csilgen_transport::rpc::{{RpcRequest, RpcResponse}};\nuse std::io::Read;\n\n"
        ));
        out.push_str(RPC_CARRIER_RUST);
        out.push('\n');
        out.push_str("fn main() {\n");
        out.push_str(&format!(
            "    let client = {}::new(HttpRpcCarrier::new(\"http://localhost:5080\"));\n",
            ex.client_struct
        ));
        if ex.null_input {
            out.push_str(&format!("    let resp = client.{}();\n", ex.method));
        } else {
            out.push_str(&format!(
                "    let resp = client.{}({});\n",
                ex.method, ex.sample
            ));
        }
        out.push_str("    match resp {\n");
        out.push_str("        Ok(value) => println!(\"{value:?}\"),\n");
        out.push_str("        Err(err) => eprintln!(\"call failed: {err}\"),\n");
        out.push_str("    }\n");
        out.push_str("}\n```\n\n");
        out
    }

    /// CSIL-Events over TLS: a full session example. Opens a TLS byte stream wrapped as
    /// the library's `StreamCarrier` (CSIL length-prefix framing), performs the
    /// `$hello`/`$hello-ack` handshake, sends one outbound event via the generated
    /// encoder, and runs a recv loop that decodes each frame to an `Event`, answers
    /// `$ping` with `$pong`, and dispatches typed channel events into the generated
    /// `route_<service>_channel`. When the spec has no usable channel op the typed
    /// dispatch is replaced with a note (the handshake + heartbeat still apply).
    fn rust_events_section(
        &mut self,
        krate: &str,
        ch: Option<&RustChannelExample>,
    ) -> Result<String, String> {
        let mut out = String::from("## CSIL-Events (TLS)\n\n");
        out.push_str(
            "Typed, bidirectional event streams over a long-lived connection. The library\n\
             owns the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and\n\
             framing; the generated channel router dispatches typed events. The TLS\n\
             carrier below is just one example — a WebSocket/WebTransport/QUIC carrier\n\
             drops in unchanged.\n\n",
        );
        out.push_str("```rust\n");
        out.push_str(&format!("use {krate}::*;\n"));
        out.push_str(
            "use csilgen_transport::carrier::{FrameCarrier, StreamCarrier};\n\
             use csilgen_transport::events::{control, Event, Heartbeat, Hello, HelloAck, Profile};\n\
             use csilgen_transport::{MAX_FRAME_DEFAULT, VERSION};\n\
             use std::net::TcpStream;\n\n",
        );
        out.push_str(EVENTS_CARRIER_RUST);
        out.push('\n');
        match ch {
            Some(ch) => out.push_str(&self.rust_events_session(ch)?),
            None => out.push_str(EVENTS_NO_CHANNEL_SESSION_RUST),
        }
        out.push_str("```\n\n");
        Ok(out)
    }

    /// A quickstart `impl <Trait> for QuickstartHandlers` whose only real body is the
    /// demonstrated channel method (it prints the decoded event); every other trait
    /// method is a never-reached `unimplemented!()` stub so the handler satisfies the
    /// full trait the router requires without fabricating return values. The signatures
    /// are derived exactly as `generate_service_trait` does, so the impl always matches.
    fn rust_handler_impl(
        &mut self,
        ch: &RustChannelExample,
        operations: &[CsilServiceOperation],
    ) -> Result<String, String> {
        let demo_snake = Self::to_snake_case(&ch.op_name);
        let mut out = format!(
            "struct QuickstartHandlers;\n\nimpl {} for QuickstartHandlers {{\n    type Context = ();\n",
            ch.service_trait
        );
        for op in operations {
            let op_snake = Self::to_snake_case(&op.name);
            match op.direction {
                CsilServiceDirection::Unidirectional => {
                    let output_type =
                        self.map_type_to_rust(&success_type(&op.output_type), &None)?;
                    if is_null_input(&op.input_type) {
                        out.push_str(&format!(
                            "    fn {op_snake}(&self, _ctx: &Self::Context) -> Result<{output_type}, ServiceError> {{\n        unimplemented!(\"request/response op; see the CSIL-RPC section\")\n    }}\n"
                        ));
                    } else {
                        let input_type = self.map_type_to_rust(&op.input_type, &None)?;
                        out.push_str(&format!(
                            "    fn {op_snake}(&self, _ctx: &Self::Context, _input: {input_type}) -> Result<{output_type}, ServiceError> {{\n        unimplemented!(\"request/response op; see the CSIL-RPC section\")\n    }}\n"
                        ));
                    }
                }
                CsilServiceDirection::Bidirectional => {
                    let is_demo = op_snake == demo_snake;
                    if is_null_input(&op.input_type) {
                        let body = if is_demo {
                            format!(
                                "        println!(\"channel event {op_snake}\");\n        Ok(())\n"
                            )
                        } else {
                            "        unimplemented!()\n".to_string()
                        };
                        out.push_str(&format!(
                            "    fn {op_snake}(&self, _ctx: &Self::Context) -> Result<(), ServiceError> {{\n{body}    }}\n"
                        ));
                    } else {
                        let input_type = self.map_type_to_rust(&op.input_type, &None)?;
                        if is_demo {
                            out.push_str(&format!(
                                "    fn {op_snake}(&self, _ctx: &Self::Context, msg: {input_type}) -> Result<(), ServiceError> {{\n        println!(\"channel event {op_snake}: {{msg:?}}\");\n        Ok(())\n    }}\n"
                            ));
                        } else {
                            out.push_str(&format!(
                                "    fn {op_snake}(&self, _ctx: &Self::Context, _msg: {input_type}) -> Result<(), ServiceError> {{\n        unimplemented!()\n    }}\n"
                            ));
                        }
                    }
                }
                // Reverse is server-pushed only: no inbound trait method.
                CsilServiceDirection::Reverse => {}
            }
        }
        out.push_str("}\n");
        Ok(out)
    }

    /// The channel session body for an Events connection that has a `<->` op: a handler
    /// implementing the generated service trait, a handshake, one outbound event built by
    /// the generated encoder, and the recv loop that heartbeats and dispatches inbound
    /// typed events into the generated channel router.
    fn rust_events_session(&mut self, ch: &RustChannelExample) -> Result<String, String> {
        // Clone the operations so the handler impl can call `&mut self` mapping helpers
        // without holding a borrow on `self.input`.
        let operations: Vec<CsilServiceOperation> = self
            .spec
            .rules
            .iter()
            .find_map(|r| match &r.rule_type {
                CsilRuleType::ServiceDef(s) if r.name == ch.service_trait => {
                    Some(s.operations.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        let handler_impl = self.rust_handler_impl(ch, &operations)?;
        let service_snake = Self::to_snake_case(&ch.service_trait);
        let op_snake = Self::to_snake_case(&ch.op_name);
        Ok(format!(
            r#"{handler_impl}
fn session(carrier: &mut impl FrameCarrier) -> Result<(), Box<dyn std::error::Error>> {{
    let service = "{wire_service}";
    let handlers = QuickstartHandlers;

    // $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the
    // wire profile for the connection's lifetime.
    let hello = Hello {{
        versions: vec![VERSION],
        profiles: vec![Profile::Verbose.as_str().to_string()],
        service: Some(service.to_string()),
        auth: None,
    }};
    carrier.send_frame(&hello.encode()?)?;
    let ack_frame = carrier
        .recv_frame()?
        .ok_or_else(|| "connection closed during handshake".to_string())?;
    let profile =
        Profile::parse(&HelloAck::decode(&ack_frame)?.profile).ok_or_else(|| "unsupported profile".to_string())?;

    // Send one outbound event: the generated encoder serializes the typed payload, the
    // library frames it as a verbose Event.
    let (method, payload) = encode_{service_snake}_{op_snake}(&{outbound_sample});
    carrier.send_frame(&Event::verbose(Some(service.to_string()), method, payload).encode(profile)?)?;

    // Recv loop: decode each frame to an Event, answer $ping with $pong (the library
    // heartbeat), and dispatch typed channel events into the generated router.
    while let Some(frame) = carrier.recv_frame()? {{
        let ev = Event::decode(&frame, profile)?;
        match ev.event.as_deref() {{
            Some(control::PING_NAME) => {{
                let ping = Heartbeat::decode(&ev.payload)?;
                let pong = Heartbeat {{
                    nonce: ping.nonce,
                    at: None,
                }};
                carrier.send_frame(
                    &Event::verbose(Some(service.to_string()), control::PONG_NAME, pong.encode()?)
                        .encode(profile)?,
                )?;
            }}
            Some(method) => {{
                route_{service_snake}_channel(&handlers, &(), method, &ev.payload)?;
            }}
            None => {{}}
        }}
    }}
    Ok(())
}}

fn main() {{
    let mut carrier = open_tls_carrier("localhost:7443").expect("connect");
    if let Err(err) = session(&mut carrier) {{
        eprintln!("session failed: {{err}}");
    }}
}}
"#,
            handler_impl = handler_impl,
            wire_service = ch.wire_service,
            service_snake = service_snake,
            op_snake = op_snake,
            outbound_sample = ch.outbound_sample,
        ))
    }

    /// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it
    /// in the library's `Datagram`, and `send_datagram` it fire-and-forget. The recv
    /// path `Datagram::decode`s an inbound datagram and decodes its payload with the
    /// generated codec into the response type — there is NO synchronous response.
    fn rust_datagrams_section(&self, krate: &str, ex: Option<&RustExample>) -> String {
        let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
        out.push_str(
            "Unreliable, unordered, message-oriented. The library owns the `Datagram`\n\
             envelope; you bring a datagram carrier. The UDP carrier below is one example\n\
             — a WebRTC unreliable channel or QUIC datagrams drop in unchanged.\n\n",
        );
        let Some(ex) = ex else {
            out.push_str(
                "This package declares no record `->` operations, so there is no datagram\n\
                 payload to encode.\n\n",
            );
            return out;
        };
        let (Some(req_snake), Some(res_snake)) = (&ex.req_snake, &ex.res_snake) else {
            out.push_str(
                "This package's `->` operations have null or non-record payloads;\n\
                 (de)serialize them manually before framing.\n\n",
            );
            return out;
        };
        out.push_str("```rust\n");
        out.push_str(&format!("use {krate}::*;\n"));
        out.push_str(
            "use csilgen_transport::carrier::DatagramCarrier;\n\
             use csilgen_transport::datagrams::Datagram;\n\
             use csilgen_transport::udp::UdpDatagramCarrier;\n\
             use std::net::UdpSocket;\n\n",
        );
        out.push_str(&format!(
            r#"// The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
const OP_ORD: u64 = {op_ord};

// run_datagrams sends one `->` request as a Datagram fire-and-forget, then tries to
// decode a late inbound response. There is NO synchronous response: a datagram of the
// response type MAY arrive later — or never — so the caller must tolerate loss and
// reordering.
fn run_datagrams(carrier: &mut impl DatagramCarrier) -> Result<(), Box<dyn std::error::Error>> {{
    let req = {req_sample};
    // seq 0 marks an unsequenced datagram.
    carrier.send_datagram(&Datagram::new(OP_ORD, 0, encode_{req_snake}(&req)).encode()?)?;

    if let Some(inbound) = carrier.recv_datagram()? {{
        let dg = Datagram::decode(&inbound)?;
        let resp = decode_{res_snake}(&dg.payload)?;
        println!("late response: {{resp:?}}");
    }}
    Ok(())
}}

fn main() {{
    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind");
    socket.connect("127.0.0.1:9000").expect("connect");
    let mut carrier = UdpDatagramCarrier::new(socket);
    if let Err(err) = run_datagrams(&mut carrier) {{
        eprintln!("datagrams failed: {{err}}");
    }}
}}
"#,
            op_ord = ex.op_ord,
            req_sample = ex.sample,
            req_snake = req_snake,
            res_snake = res_snake,
        ));
        out.push_str("```\n\n");
        out
    }

    /// The first service operation the typed sync client actually exposes, reduced to
    /// an example call: which client struct + method to invoke and a compiling sample
    /// request literal. `None` when no operation qualifies (so the README falls back to
    /// the types-only section). The filter mirrors `generate_client_struct` exactly so
    /// the example never names a method the client did not emit.
    fn first_unary_example(&self) -> Option<RustExample> {
        let records = self.record_names();
        for rule in &self.spec.rules {
            let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
                continue;
            };
            for op in &service.operations {
                if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                    continue;
                }
                let success = success_type(&op.output_type);
                let null_input = is_null_input(&op.input_type);
                if !Self::is_record_ref(&success, &records)
                    || !(null_input || Self::is_record_ref(&op.input_type, &records))
                {
                    continue;
                }
                let base = Self::service_base(&rule.name);
                return Some(RustExample {
                    client_struct: format!("{base}Client"),
                    method: Self::to_snake_case(&op.name),
                    null_input,
                    sample: if null_input {
                        String::new()
                    } else {
                        self.rust_sample(&op.input_type)
                    },
                    req_snake: (!null_input)
                        .then(|| Self::to_snake_case(&Self::type_ref_name(&op.input_type))),
                    res_snake: Some(Self::to_snake_case(&Self::type_ref_name(&success))),
                    // The datagram ordinal is the op's @wire-id when present; otherwise a
                    // channel-agreed placeholder the user fills in.
                    op_ord: op.wire_id.unwrap_or(1),
                });
            }
        }
        None
    }

    /// The first service (in declaration order) with a `<->` op whose inbound and
    /// outbound are both records, so the generated per-type codec helpers exist for a
    /// compiling Events session. `None` when no service has a usable channel op — the
    /// Events section then shows the handshake/heartbeat without typed dispatch.
    fn first_channel_example(&self) -> Option<RustChannelExample> {
        let records = self.record_names();
        for rule in &self.spec.rules {
            let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
                continue;
            };
            for op in &service.operations {
                if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                    continue;
                }
                let success = success_type(&op.output_type);
                if !Self::is_record_ref(&success, &records)
                    || !Self::is_record_ref(&op.input_type, &records)
                {
                    continue;
                }
                // The Events session dispatches through the generated server-side channel
                // router + encoder: the router decodes the op's input (inbound, handed to
                // a handler) and the encoder serializes the op's success output (outbound).
                return Some(RustChannelExample {
                    service_trait: rule.name.clone(),
                    // The wire carries the CSIL service name verbatim
                    // (docs/cbor-wire-contract.md "RPC call naming").
                    wire_service: rule.name.clone(),
                    op_name: op.name.clone(),
                    outbound_sample: self.rust_sample(&success),
                });
            }
        }
        None
    }

    /// A compiling Rust literal for a value of `ty`: real values for scalars and
    /// collections and a recursive literal for a nested record (every field present —
    /// required fields sampled, optionals `None`). Anything a generic sample can't
    /// fabricate (a choice/enum, a tuple, decimal, timestamp) becomes `todo!()`, whose
    /// `!` type unifies with the field's type so the snippet always compiles while
    /// flagging the spot a user must fill in.
    fn rust_sample(&self, ty: &CsilTypeExpression) -> String {
        match ty {
            CsilTypeExpression::Builtin(name) => match name.as_str() {
                "text" | "tstr" | "string" => "\"example\".to_string()".to_string(),
                "bool" | "boolean" => "false".to_string(),
                "bytes" | "bstr" => "Vec::new()".to_string(),
                "float" | "float16" | "float32" | "float64" => "0.0".to_string(),
                "int" | "uint" | "integer" | "number" | "int8" | "int16" | "int32" | "int64"
                | "uint8" | "uint16" | "uint32" | "uint64" => "0".to_string(),
                _ => "todo!()".to_string(),
            },
            CsilTypeExpression::Array { .. } => "Vec::new()".to_string(),
            CsilTypeExpression::Map { .. } => "std::collections::HashMap::new()".to_string(),
            CsilTypeExpression::Constrained { base_type, .. } => self.rust_sample(base_type),
            CsilTypeExpression::Reference(name) => {
                if let Some(group) = self.find_record(name) {
                    self.rust_record_literal(name, group)
                } else if let Some(alias) = self.codec_aliases().get(name) {
                    // A transparent alias maps to its underlying type, so a map/array
                    // alias still gets a real empty literal rather than `todo!()`.
                    match alias {
                        CsilTypeExpression::Map { .. } => {
                            "std::collections::HashMap::new()".to_string()
                        }
                        CsilTypeExpression::Array { .. } => "Vec::new()".to_string(),
                        other => self.rust_sample(other),
                    }
                } else {
                    "todo!()".to_string()
                }
            }
            _ => "todo!()".to_string(),
        }
    }

    /// `Name { field: <sample>, ... }` over every field of a record: required fields
    /// get a sampled value, optionals get `None`, and a group-spread entry gets the
    /// referenced record — matching `generate_struct`'s field set so the literal names
    /// exactly the fields the generated struct declares.
    fn rust_record_literal(&self, name: &str, group: &CsilGroupExpression) -> String {
        let mut fields: Vec<String> = Vec::new();
        for entry in &group.entries {
            if let Some(field_name) = self.extract_field_name(&entry.key) {
                let value = if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    "None".to_string()
                } else {
                    self.rust_sample(&entry.value_type)
                };
                fields.push(format!("{field_name}: {value}"));
            } else if let Some(spread) = Self::group_spread_reference(&entry.value_type) {
                let field_name = Self::escape_rust_ident(&Self::to_snake_case(&spread));
                let value = self.rust_sample(&CsilTypeExpression::Reference(spread));
                fields.push(format!("{field_name}: {value}"));
            }
        }
        if fields.is_empty() {
            format!("{name} {{}}")
        } else {
            format!("{name} {{ {} }}", fields.join(", "))
        }
    }

    /// The record a type reference names, if any. `Name = {{ ... }}` parses as a
    /// `TypeDef(Group)` while a bare group rule is a `GroupDef`; both are records.
    fn find_record(&self, name: &str) -> Option<&CsilGroupExpression> {
        self.spec
            .rules
            .iter()
            .filter(|r| r.name == name)
            .find_map(|r| match &r.rule_type {
                CsilRuleType::GroupDef(g) => Some(g),
                CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
                _ => None,
            })
    }

    fn generate_types(&mut self) -> Result<String, String> {
        // Build the type bodies first: emitting structs is what sets the
        // `needs_validation_error` / `uses_regex` flags that decide which shared
        // helpers to inject ahead of them.
        let mut body = String::new();
        // Cloned so the loop body can call `&mut self` emitters: `self.spec` is now
        // owned data (the hoisted spec), so `&self.spec.rules` would otherwise
        // borrow `self` itself for the loop's whole lifetime.
        let rules = self.spec.rules.clone();
        for rule in &rules {
            match &rule.rule_type {
                CsilRuleType::GroupDef(group) => {
                    let struct_code = self.generate_struct(&rule.name, group)?;
                    body.push_str(&struct_code);
                    body.push_str("\n\n");
                    self.type_definitions.insert(rule.name.clone());
                }
                CsilRuleType::TypeChoice(choices) => {
                    let enum_code = self.generate_enum(&rule.name, choices)?;
                    body.push_str(&enum_code);
                    body.push_str("\n\n");
                    self.type_definitions.insert(rule.name.clone());
                }
                CsilRuleType::TypeDef(type_expr) => {
                    let type_alias_code = self.generate_type_alias(&rule.name, type_expr)?;
                    body.push_str(&type_alias_code);
                    body.push_str("\n\n");
                    self.type_definitions.insert(rule.name.clone());
                }
                _ => {} // Services handled separately
            }
        }

        let mut content = String::new();
        content.push_str("//! Generated types from CSIL specification\n\n");
        // Every CSIL rule name — and every `<Record>_<field>`/`<Service>_<op>_...`
        // name `hoist_inline` synthesizes for a hoisted inline choice — is
        // emitted verbatim as the Rust type identifier (see `Reference(name) =>
        // name.clone()` in `map_type_to_rust`, and the struct/enum declarations
        // above, which both use `rule.name` as-is with no PascalCasing). That is
        // deliberate: CSIL is CDDL-flavored, so rule names are conventionally
        // snake_case or kebab-case, and Rust-casing them here would mean every
        // codec/client/server emitter also has to re-derive and agree on the same
        // casing at every reference site — a much larger, riskier change than
        // allowing the lint once. Unconditional like `codec.gen.rs`'s own
        // `#![allow(dead_code, ...)]`: cheap to always emit, and correct whether or
        // not this particular spec happens to use non-CamelCase rule names.
        //
        // `clippy::large_enum_variant` is allowed for the same reason a union's
        // shape is not ours to choose: a type-choice's variant payload sizes come
        // straight from the CSIL spec (e.g. a large record next to a small error
        // type), and the wire format is a tagged sum over the *value*, not a
        // pointer — boxing a variant to appease the lint would mean the codec
        // encode/decode paths route through `Box<T>` for exactly one variant of
        // exactly the enums whose sibling arms happen to be small, an asymmetry
        // that buys nothing at the wire and would ripple through every
        // `rust_enc_value`/`rust_dec_func` call site that touches a union.
        content.push_str("#![allow(non_camel_case_types, clippy::large_enum_variant)]\n\n");

        // The exact-decimal helper is emitted only when the spec uses `decimal`
        // under the default mapping; under `library` mode the type is
        // `rust_decimal::Decimal` and no helper is needed. Its CBOR tag-4 wire form
        // lives in `codec.gen.rs`, so the type itself carries no serde impl.
        if self.decimal_mapping == DecimalMapping::Csil && self.spec_uses_builtin("decimal") {
            content.push_str(CSIL_DECIMAL);
            content.push_str("\n\n");
        }

        if self.needs_validation_error {
            content.push_str(VALIDATION_ERROR);
            content.push_str("\n\n");
        }

        content.push_str(&body);

        Ok(content)
    }

    fn generate_struct(
        &mut self,
        name: &str,
        group: &CsilGroupExpression,
    ) -> Result<String, String> {
        let mut content = String::new();
        // The payload wire is owned by `codec.gen.rs`, so the type itself derives no
        // serde traits — only the in-memory conveniences a consumer expects.
        let mut derive_attrs = vec!["Debug", "Clone"];

        // Add struct documentation if any field has descriptions
        let has_descriptions = group.entries.iter().any(|e| {
            e.metadata
                .iter()
                .any(|m| matches!(m, CsilFieldMetadata::Description(_)))
        });

        if has_descriptions {
            content.push_str(&format!("/// {name}\n"));
        }

        // Check for PartialEq derive based on metadata
        if self.should_derive_partial_eq(group) {
            derive_attrs.push("PartialEq");
        }

        content.push_str(&format!("#[derive({})]\n", derive_attrs.join(", ")));

        if group.entries.is_empty() {
            content.push_str(&format!("pub struct {name} {{}}"));

            if let Some(validate_impl) = self.generate_validate_impl(name, group) {
                content.push_str("\n\n");
                content.push_str(&validate_impl);
            }

            return Ok(content);
        }

        content.push_str(&format!("pub struct {name} {{\n"));

        for entry in &group.entries {
            if let Some(field_name) = self.extract_field_name(&entry.key) {
                // Add field documentation
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Description(desc) = metadata {
                        content.push_str(&format!("    /// {desc}\n"));
                    }
                }

                // Surface every control operator as a doc line so encoding-only
                // operators (`.json`/`.cbor`/`.bits`/`.and`/`.within`) and
                // `.default` remain visible even when they carry no runtime check.
                for line in Self::constraint_doc_lines(&entry.value_type) {
                    content.push_str(&format!("    /// {line}\n"));
                }

                // A `@depends-on` is a conditional-requirement relationship between
                // fields with no Rust-type effect, so surface it as a doc line rather
                // than dropping it silently.
                for line in Self::depends_on_doc_lines(&entry.metadata) {
                    content.push_str(&format!("    /// {line}\n"));
                }

                let rust_type = self.map_type_to_rust(&entry.value_type, &entry.occurrence)?;
                content.push_str(&format!("    pub {field_name}: {rust_type},\n"));
            } else if let Some(spread) = Self::group_spread_reference(&entry.value_type) {
                // A keyless entry that references another group is a CDDL-style
                // group spread. Rust structs cannot inline another struct's
                // fields, so surface the referenced group as a named field rather
                // than dropping it silently (which would leave the spread's fields
                // unrepresentable). The field is named after the referenced type.
                let field_name = Self::escape_rust_ident(&Self::to_snake_case(&spread));
                content.push_str("    /// Inlined from group spread; flattened on the wire.\n");
                content.push_str(&format!("    pub {field_name}: {spread},\n"));
            }
        }

        content.push('}');

        if let Some(validate_impl) = self.generate_validate_impl(name, group) {
            content.push_str("\n\n");
            content.push_str(&validate_impl);
        }

        Ok(content)
    }

    /// Human-readable doc lines describing the control operators on a field's
    /// type. Used to document constraints that have no runtime enforcement (or in
    /// addition to it), so nothing about a `.csil` constraint is silently lost.
    fn constraint_doc_lines(value_type: &CsilTypeExpression) -> Vec<String> {
        let CsilTypeExpression::Constrained { constraints, .. } = value_type else {
            return Vec::new();
        };
        constraints
            .iter()
            .map(|op| match op {
                CsilControlOperator::Size(s) => format!("constraint: size {}", Self::size_doc(s)),
                CsilControlOperator::Regex(p) => format!("constraint: matches regex {p:?}"),
                CsilControlOperator::Default(v) => {
                    format!("default: {}", Self::literal_doc(v))
                }
                CsilControlOperator::GreaterEqual(v) => {
                    format!("constraint: >= {}", Self::literal_doc(v))
                }
                CsilControlOperator::LessEqual(v) => {
                    format!("constraint: <= {}", Self::literal_doc(v))
                }
                CsilControlOperator::GreaterThan(v) => {
                    format!("constraint: > {}", Self::literal_doc(v))
                }
                CsilControlOperator::LessThan(v) => {
                    format!("constraint: < {}", Self::literal_doc(v))
                }
                CsilControlOperator::Equal(v) => format!("constraint: == {}", Self::literal_doc(v)),
                CsilControlOperator::NotEqual(v) => {
                    format!("constraint: != {}", Self::literal_doc(v))
                }
                CsilControlOperator::Bits(b) => format!("constraint: bits {b}"),
                CsilControlOperator::And(_) => "constraint: type intersection (.and)".to_string(),
                CsilControlOperator::Within(_) => "constraint: subset (.within)".to_string(),
                CsilControlOperator::Json => "encoding: nested JSON (.json)".to_string(),
                CsilControlOperator::Cbor => "encoding: embedded CBOR (.cbor)".to_string(),
                CsilControlOperator::Cborseq => "encoding: CBOR sequence (.cborseq)".to_string(),
            })
            .collect()
    }

    fn size_doc(size: &CsilSizeConstraint) -> String {
        match size {
            CsilSizeConstraint::Exact(n) => format!("== {n}"),
            CsilSizeConstraint::Range { min, max } => format!("in {min}..={max}"),
            CsilSizeConstraint::Min(n) => format!(">= {n}"),
            CsilSizeConstraint::Max(n) => format!("<= {n}"),
        }
    }

    /// Doc lines for any `@depends-on` metadata on a field. Both the simple
    /// single-comparison `DependsOn` and the boolean-tree `DependsOnExpr` render to
    /// the same readable form so the conditional relationship is never lost.
    fn depends_on_doc_lines(metadata: &[CsilFieldMetadata]) -> Vec<String> {
        metadata
            .iter()
            .filter_map(|m| match m {
                CsilFieldMetadata::DependsOn { field, value } => Some(format!(
                    "depends-on: {}",
                    Self::render_simple_depends(field, value)
                )),
                CsilFieldMetadata::DependsOnExpr(condition) => Some(format!(
                    "depends-on: {}",
                    Self::render_depends_condition(condition)
                )),
                _ => None,
            })
            .collect()
    }

    /// The simple `field = value` (or bare-presence) dependency as a readable string,
    /// matching the comparison form used by the boolean-tree renderer.
    fn render_simple_depends(field: &str, value: &Option<CsilLiteralValue>) -> String {
        match value {
            Some(v) => format!("{field} == {}", Self::literal_doc(v)),
            None => field.to_string(),
        }
    }

    /// Render a `@depends-on` boolean condition to a readable string: `All` joins with
    /// `&&`, `Any` with `||`, and a nested group is parenthesized so the precedence the
    /// author wrote survives. A bare `Compare` with no op is a presence check.
    fn render_depends_condition(condition: &CsilDependsCondition) -> String {
        match condition {
            CsilDependsCondition::Compare { field, op, value } => match (op, value) {
                (Some(op), Some(v)) => {
                    format!(
                        "{field} {} {}",
                        Self::depends_op_str(op),
                        Self::literal_doc(v)
                    )
                }
                (Some(op), None) => format!("{field} {}", Self::depends_op_str(op)),
                (None, _) => field.clone(),
            },
            CsilDependsCondition::All(conds) => Self::join_depends_conditions(conds, " && "),
            CsilDependsCondition::Any(conds) => Self::join_depends_conditions(conds, " || "),
        }
    }

    fn join_depends_conditions(conds: &[CsilDependsCondition], sep: &str) -> String {
        conds
            .iter()
            .map(|c| {
                let rendered = Self::render_depends_condition(c);
                // Parenthesize a nested boolean group so mixed `&&`/`||` stays unambiguous.
                if matches!(
                    c,
                    CsilDependsCondition::All(_) | CsilDependsCondition::Any(_)
                ) {
                    format!("({rendered})")
                } else {
                    rendered
                }
            })
            .collect::<Vec<_>>()
            .join(sep)
    }

    fn depends_op_str(op: &CsilDependsCompareOp) -> &'static str {
        match op {
            CsilDependsCompareOp::Eq => "==",
            CsilDependsCompareOp::Ne => "!=",
            CsilDependsCompareOp::Lt => "<",
            CsilDependsCompareOp::Le => "<=",
            CsilDependsCompareOp::Gt => ">",
            CsilDependsCompareOp::Ge => ">=",
        }
    }

    fn literal_doc(value: &CsilLiteralValue) -> String {
        match value {
            CsilLiteralValue::Integer(i) => i.to_string(),
            CsilLiteralValue::Float(f) => f.to_string(),
            CsilLiteralValue::Text(t) => format!("{t:?}"),
            CsilLiteralValue::Bool(b) => b.to_string(),
            CsilLiteralValue::Null => "null".to_string(),
            CsilLiteralValue::Bytes(_) => "<bytes>".to_string(),
            CsilLiteralValue::Array(_) => "<array>".to_string(),
        }
    }

    /// The choices of a named type-choice rule (`X = a / b / ...`), whether the
    /// parser modeled it as a `TypeChoice` rule or a `TypeDef` of a `Choice`.
    fn type_choice(&self, name: &str) -> Option<&Vec<CsilTypeExpression>> {
        self.spec.rules.iter().find_map(|r| {
            if r.name != name {
                return None;
            }
            match &r.rule_type {
                CsilRuleType::TypeChoice(choices) => Some(choices),
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => Some(choices),
                _ => None,
            }
        })
    }

    // `choice_arm_literal` is shared machinery now (see `csilgen_common::choice`,
    // THE normative classification contract): it sees through a trailing
    // control-operator wrapper the same way this crate's former local copy did
    // (`text / "a" / "b" .default "b"` parses its last arm as `Constrained {
    // base_type: Literal("b"), .. }`, not a bare `Literal` — the `.default` binds
    // to that one arm, not to the choice as a whole), so every existing
    // `choice_arm_literal(...)` call site below keeps its exact behavior via the
    // `use` import at the top of this file.

    /// A type-choice whose every variant is a literal — of any kind, or a MIX of
    /// kinds (`"a" / 1` is a text literal and an int literal, both literals) — is
    /// an *enum*: it carries no payload, so each literal is its own wire value and
    /// discriminant. Returns the literals (in declaration order) when so, else
    /// `None` — a `None` choice is a *union* (a tagged sum, see
    /// `emit_choice_codec`). Delegates the classification itself to
    /// `csilgen_common::classify_choice` (THE normative contract): a mixed-kind
    /// literal choice used to fall through here to `None` (this function required
    /// EVERY literal to be text, or EVERY literal to be int) and land in the
    /// generic union path, where `map_type_to_rust` wraps each literal in a
    /// per-arm payload type instead of emitting the unit-variant bare-literal enum
    /// its all-literal vocabulary actually is.
    fn enum_literals(choices: &[CsilTypeExpression]) -> Option<Vec<CsilLiteralValue>> {
        match classify_choice(choices) {
            ChoiceClass::Enum(lits) => Some(lits.into_iter().cloned().collect()),
            ChoiceClass::Union(_) => None,
        }
    }

    /// A Rust unit-variant identifier for an enum literal: the text PascalCased, or
    /// `V<n>` / `VNeg<n>` for integers; positional `Variant<idx>` as a collision-proof
    /// fallback. The codec pairs each emitted variant with its literal by index, so the
    /// name is cosmetic — only its uniqueness within the enum matters.
    fn enum_variant_ident(lit: &CsilLiteralValue, idx: usize) -> String {
        let candidate = match lit {
            CsilLiteralValue::Text(s) => {
                let p = Self::to_pascal_case(s);
                if p.is_empty() || p.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    String::new()
                } else {
                    p
                }
            }
            CsilLiteralValue::Integer(n) if *n < 0 => format!("VNeg{}", n.unsigned_abs()),
            CsilLiteralValue::Integer(n) => format!("V{n}"),
            _ => String::new(),
        };
        if candidate.is_empty() {
            format!("Variant{idx}")
        } else {
            candidate
        }
    }

    /// The unique unit-variant identifiers for an enum, de-duplicating any two literals
    /// that PascalCase to the same name by falling back to the positional form.
    fn enum_variant_idents(lits: &[CsilLiteralValue]) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::with_capacity(lits.len());
        for (i, lit) in lits.iter().enumerate() {
            let mut ident = Self::enum_variant_ident(lit, i);
            if !seen.insert(ident.clone()) {
                ident = format!("Variant{i}");
                seen.insert(ident.clone());
            }
            out.push(ident);
        }
        out
    }

    fn generate_enum(
        &mut self,
        name: &str,
        choices: &[CsilTypeExpression],
    ) -> Result<String, String> {
        let mut content = String::new();

        content.push_str(&format!("/// {name} variants\n"));
        content.push_str("#[derive(Debug, Clone, PartialEq)]\n");
        content.push_str(&format!("pub enum {name} {{\n"));

        // A literal-only choice is an enum (unit variants, bare-literal wire); a
        // choice with type-bearing variants is a union (a tagged sum, payload per
        // variant). Both share this declaration; their codecs differ.
        if let Some(lits) = Self::enum_literals(choices) {
            for ident in Self::enum_variant_idents(&lits) {
                content.push_str(&format!("    {ident},\n"));
            }
        } else {
            for (i, choice) in choices.iter().enumerate() {
                let variant_name = format!("Variant{i}");
                let rust_type = self.map_type_to_rust(choice, &None)?;
                content.push_str(&format!("    {variant_name}({rust_type}),\n"));
            }
        }

        content.push('}');
        Ok(content)
    }

    fn generate_type_alias(
        &mut self,
        name: &str,
        type_expr: &CsilTypeExpression,
    ) -> Result<String, String> {
        match type_expr {
            CsilTypeExpression::Group(group) => self.generate_struct(name, group),
            CsilTypeExpression::Choice(choices) => self.generate_enum(name, choices),
            _ => {
                let rust_type = self.map_type_to_rust(type_expr, &None)?;
                Ok(format!("pub type {name} = {rust_type};"))
            }
        }
    }

    fn generate_services(&mut self) -> Result<String, String> {
        // Built before the header so the `use super::codec::*;` import (below) can
        // be gated on whether the body actually ended up calling into the codec —
        // "the spec has a channel op" over-approximates that: a channel op whose
        // request type the router can't decode (e.g. a bare scalar/`Reference`
        // boundary `generate_service_router` has no dispatch arm for) still emits
        // a router stub that touches no codec symbol, leaving the import unused.
        let mut body = String::new();

        // Only emit the fallback `ServiceError` when the spec doesn't declare its
        // own; otherwise it collides with the type from `types.rs` (both are
        // re-exported through `mod.rs`). A spec-defined `ServiceError` is used
        // verbatim via the `use super::types::*` import above.
        if !self.spec_defines_service_error() {
            self.generate_service_error(&mut body);
            body.push('\n');
        }

        let rules = self.spec.rules.clone();
        for rule in &rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let trait_code = self.generate_service_trait(&rule.name, service)?;
                body.push_str(&trait_code);
                body.push_str("\n\n");

                // Purely additive: only specs carrying `@wire-id(N)` ordinals get
                // a `wire_ids` module, so wire-id-free specs stay byte-identical.
                if let Some(wire_ids) = self.generate_wire_ids(&rule.name, service) {
                    body.push_str(&wire_ids);
                    body.push_str("\n\n");
                }

                if Self::service_has_channel_ops(service) {
                    body.push_str(&self.generate_service_router(&rule.name, service)?);
                    body.push('\n');
                    // Compact-profile twin, emitted only for wire-id-bearing
                    // services so wire-id-free specs stay byte-identical.
                    if let Some(compact) =
                        self.generate_service_router_compact(&rule.name, service)?
                    {
                        body.push_str(&compact);
                        body.push('\n');
                    }
                    body.push_str(&self.generate_service_encoders(&rule.name, service)?);
                    body.push('\n');
                }
            }
        }

        let mut content = String::new();
        content.push_str("//! Generated service traits from CSIL specification\n\n");
        // The channel router/encoders ride the generated per-type CBOR codec
        // directly (it owns the wire); a textual scan of the body for any codec
        // symbol (see `body_uses_codec`) pins the import to exactly the specs
        // whose generated trait/router/encoder code actually references it.
        if Self::body_uses_codec(&body) {
            content.push_str("use super::codec::*;\n");
        }
        content.push_str("use super::types::*;\n");
        content.push('\n');
        content.push_str(&body);

        Ok(content)
    }

    /// Emit `client.rs`: a transport-agnostic, typed client per service. Each
    /// unary operation becomes a method that hands `(service, method, req)` to a
    /// caller-supplied `Transport` and returns the typed success response, with
    /// the `/ ServiceError` half surfaced through `ClientError`.
    fn generate_client(&mut self, shape: ClientShape) -> Result<String, String> {
        // Built before the header for the same reason `generate_services` builds
        // its body first: a service whose every operation is a channel op has no
        // unary method to encode/decode a request/response through, so nothing in
        // `body` would call the codec — `client_prelude` (the `ClientError`/
        // `Transport` trait boilerplate) never references it either.
        let mut body = String::new();
        body.push_str(&client_prelude(shape));
        body.push('\n');

        let rules = self.spec.rules.clone();
        for rule in &rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let client_code = self.generate_client_struct(&rule.name, service, shape)?;
                body.push_str(&client_code);
                body.push_str("\n\n");
            }
        }

        let mut content = String::new();
        content.push_str(
            "//! Generated transport-agnostic service clients from CSIL specification\n\n",
        );
        // `async fn` in a public trait is intentional here (the consumer drives the
        // returned future); silence the discouragement lint in the emitted crate.
        if shape.is_async {
            content.push_str("#![allow(async_fn_in_trait)]\n\n");
        }
        // The marked twin shares the sync client's `ClientError`; import it rather
        // than redefine it so both clients re-export cleanly from the module root.
        if !shape.marker.is_empty() {
            content.push_str("use super::client::ClientError;\n");
        }
        // The client owns (de)serialization through the generated per-type codec —
        // but only import it when a generated method actually calls into it (see
        // `body_uses_codec`, shared with `generate_services`).
        if Self::body_uses_codec(&body) {
            content.push_str("use super::codec::*;\n");
        }
        content.push_str("use super::types::*;\n");
        content.push('\n');
        content.push_str(&body);

        Ok(content)
    }

    /// Whether `body` (an already-rendered `services.rs`/`client.rs` body) calls
    /// into the generated codec module. Every codec symbol it could reference is
    /// either the internal `csil_enc_*`/`csil_dec_*`/`Csil*` surface (see
    /// `CODEC_RUNTIME_RUST`) or the public `encode_*`/`decode_*` wrappers
    /// (`emit_record_codec`/`emit_choice_codec`/`emit_op_codec_pair`) — a router
    /// stub with no dispatchable op, or a client whose service has no unary op,
    /// calls neither, and must not import a module it never uses.
    fn body_uses_codec(body: &str) -> bool {
        body.contains("csil_")
            || body.contains("Csil")
            || body.contains("cbor_")
            || body.contains("encode_")
            || body.contains("decode_")
    }

    fn generate_client_struct(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
        shape: ClientShape,
    ) -> Result<String, String> {
        let base = Self::service_base(name);
        let client = shape.client_name(&base);
        let transport = shape.transport_trait();

        let mut content = String::new();
        content.push_str(&format!("/// Typed client for the {name} service.\n"));
        content.push_str(&format!("pub struct {client}<T: {transport}> {{\n"));
        // A service whose every operation is a channel op (`<->`/`<-`) has no
        // unary method to call `self.transport` from (channel ops ride the
        // router/encoder surface below, not the client struct) — `transport` is
        // still stored so `new(transport: T)` and the `T: Transport` bound stay
        // uniform across every service, but clippy has no way to know a
        // *different* service in the same spec might use it, so allow rather than
        // special-case the struct shape per service.
        content.push_str("    #[allow(dead_code)]\n");
        content.push_str("    transport: T,\n");
        content.push_str("}\n\n");

        content.push_str(&format!("impl<T: {transport}> {client}<T> {{\n"));
        content.push_str("    pub fn new(transport: T) -> Self {\n");
        content.push_str("        Self { transport }\n");
        content.push_str("    }\n");

        let records = self.record_names();
        let aliases = self.codec_aliases();
        // The client hands its transport seam the CSIL service and operation names
        // verbatim, so a Rust client reaches the same endpoint as its peers
        // (docs/cbor-wire-contract.md "RPC call naming").
        let wire_service = name;
        for operation in &service.operations {
            // Only unary request/response operations belong on the RPC client;
            // channel (`<->`/`<-`) ops ride the router/encoder surface instead.
            if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
                content.push_str(&format!(
                    "\n    // channel operation `{}` is not part of the RPC client\n",
                    operation.name
                ));
                continue;
            }
            let success = success_type(&operation.output_type);
            let null_input = is_null_input(&operation.input_type);
            let req_ok = null_input
                || self.op_boundary_expressible(&operation.input_type, &records, &aliases);
            // Only a genuinely inexpressible boundary (an inline multi-variant choice
            // with no wire discriminator, or an unmodeled reference) is skipped now;
            // scalar/array/map/tuple shapes ride the per-op codec helpers, so every
            // other op gets a typed method.
            if !req_ok || !self.op_boundary_expressible(&success, &records, &aliases) {
                content.push_str(&format!(
                    "\n    // operation `{}` has a payload csilgen can't (de)serialize; handle it manually\n",
                    operation.name
                ));
                continue;
            }
            let method = Self::to_snake_case(&operation.name);
            let wire_method = &operation.name;
            let output_type = self.map_type_to_rust(&success, &None)?;
            let stem = self.op_codec_stem(name, &operation.name);
            // A record success reuses its `decode_<t>` wrapper; any other shape uses the
            // op's per-op response decoder.
            let resp_dec = if Self::is_record_ref(&success, &records) {
                format!(
                    "decode_{}",
                    Self::to_snake_case(&Self::type_ref_name(&success))
                )
            } else {
                format!("decode_{stem}_response")
            };
            content.push('\n');
            Self::write_op_doc(&mut content, operation, "request/response");
            // A push-style op (`op: -> Event`) takes no request payload: emit a
            // parameterless method and send empty request bytes on the wire.
            let async_kw = shape.async_kw();
            let dot_await = shape.dot_await();
            if null_input {
                content.push_str(&Self::rust_client_method_sig(
                    async_kw,
                    &method,
                    None,
                    &output_type,
                ));
                content.push_str(&Self::rust_client_call(
                    wire_service,
                    wire_method,
                    "&[]",
                    dot_await,
                ));
            } else {
                let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                // A record request reuses its `encode_<t>` wrapper; any other shape uses
                // the op's per-op request encoder.
                let req_enc = if Self::is_record_ref(&operation.input_type, &records) {
                    format!(
                        "encode_{}",
                        Self::to_snake_case(&Self::type_ref_name(&operation.input_type))
                    )
                } else {
                    format!("encode_{stem}_request")
                };
                content.push_str(&Self::rust_client_method_sig(
                    async_kw,
                    &method,
                    Some(&input_type),
                    &output_type,
                ));
                content.push_str(&Self::rust_client_call(
                    wire_service,
                    wire_method,
                    &format!("&{req_enc}(&req)"),
                    dot_await,
                ));
            }
            let decode_line = format!(
                "        {resp_dec}(&csil_resp).map_err(|e| ClientError::Transport(e.to_string()))\n"
            );
            if decode_line.trim_end().len() <= 100 {
                content.push_str(&decode_line);
            } else {
                content.push_str(&format!(
                    "        {resp_dec}(&csil_resp)\n\
                     \x20           .map_err(|e| ClientError::Transport(e.to_string()))\n"
                ));
            }
            content.push_str("    }\n");
        }

        content.push('}');
        Ok(content)
    }

    /// The bare type name of a record `Reference`. Only called after
    /// `is_record_ref` has confirmed the type is a record reference.
    fn type_ref_name(ty: &CsilTypeExpression) -> String {
        match ty {
            CsilTypeExpression::Reference(name) => name.clone(),
            _ => String::new(),
        }
    }

    /// Whether a type is a reference to a record the codec can (de)serialize, so a
    /// typed client method can call the generated `encode_`/`decode_` directly.
    fn is_record_ref(ty: &CsilTypeExpression, records: &HashSet<String>) -> bool {
        matches!(ty, CsilTypeExpression::Reference(name) if records.contains(name))
    }

    /// Whether `rust_enc_value`/`rust_dec_func` model an op-boundary type faithfully,
    /// so an op carrying it can get a real client method (a record reuses its own
    /// `encode_`/`decode_`; anything else rides a per-op codec helper). Records,
    /// builtins, transparent aliases, named enums/unions, arrays, maps, and tuples all
    /// resolve to real codec building blocks. An inline multi-variant choice has no
    /// wire discriminator and an unmodeled reference has no codec, so those two stay on
    /// the skip-with-note path the client falls back to.
    fn op_boundary_expressible(
        &self,
        ty: &CsilTypeExpression,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> bool {
        match Self::value_base(ty) {
            CsilTypeExpression::Builtin(_) => true,
            CsilTypeExpression::Reference(name) => {
                records.contains(name)
                    || aliases.contains_key(name)
                    || self.type_choice(name).is_some()
            }
            CsilTypeExpression::Array { element_type, .. } => {
                self.op_boundary_expressible(element_type, records, aliases)
            }
            CsilTypeExpression::Map { key, value, .. } => {
                self.op_boundary_expressible(key, records, aliases)
                    && self.op_boundary_expressible(value, records, aliases)
            }
            CsilTypeExpression::Tuple(_) => true,
            _ => false,
        }
    }

    /// The `<service_base>_<method>` stem shared by an op's per-op codec helpers and
    /// the client method that calls them, so the two never drift.
    fn op_codec_stem(&self, service_name: &str, op_name: &str) -> String {
        format!(
            "{}_{}",
            Self::to_snake_case(&Self::service_base(service_name)),
            Self::to_snake_case(op_name)
        )
    }

    /// The verbatim CSIL wire name for one group entry — the key as written in the
    /// `.csil` source, which is the map key on the wire. `None` for a keyless entry
    /// (a group spread) or a non-text key, neither of which the per-type codec can
    /// (de)serialize field-by-field.
    fn entry_wire_name(entry: &CsilGroupEntry) -> Option<String> {
        match entry.key.as_ref()? {
            CsilGroupKey::Bare(name) => Some(name.clone()),
            CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => Some(name.clone()),
            _ => None,
        }
    }

    /// The names of every record the codec can (de)serialize: a `GroupDef` (or a
    /// `TypeDef` wrapping a `Group`) whose every entry carries a text wire key. A
    /// record with a group-spread entry has a field with no wire key, so it gets no
    /// codec and is not treated as a record reference anywhere.
    /// The transparent type aliases the codec resolves through: a `TypeDef` whose
    /// target is a map / array / scalar / reference / tuple / constrained (NOT a
    /// record group or a choice, which have their own handling). A field referencing
    /// one must encode as the underlying type rather than the `null` stub a bare
    /// non-record reference would yield. The Rust alias is a transparent `pub type`,
    /// so the underlying codec's value is assignable to/from the named field.
    fn codec_aliases(&self) -> HashMap<String, CsilTypeExpression> {
        self.spec
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

    fn record_names(&self) -> HashSet<String> {
        self.spec
            .rules
            .iter()
            .filter_map(|rule| {
                let group = match &rule.rule_type {
                    CsilRuleType::GroupDef(g) => g,
                    CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => g,
                    _ => return None,
                };
                if group
                    .entries
                    .iter()
                    .all(|e| Self::entry_wire_name(e).is_some())
                {
                    Some(rule.name.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// The CBOR encoding of a text key. Comparing these byte slices lexicographically
    /// is exactly RFC 8949 §4.2.1 canonical key ordering, computed here at generation
    /// time so the emitted encoder lays a record's map keys down in canonical order.
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

    /// A Rust expression building a `CsilCborValue` from `expr` (a value of the
    /// field's mapped type). `by_ref` marks `expr` as already a `&T` reference (a
    /// closure binding); otherwise it is an owned place (`csil_v.field`) the scalar
    /// constructors copy and the reference constructors borrow. Composites recurse
    /// through the generic `cbor_enc_array`/`cbor_enc_map` runtime helpers.
    fn rust_enc_value(
        &self,
        ty: &CsilTypeExpression,
        expr: &str,
        by_ref: bool,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> RustExpr {
        // A scalar constructor takes the value by copy; a `&T` binding is deref'd.
        let scalar = |ctor: &str| {
            if by_ref {
                RustExpr::call(ctor, vec![RustExpr::Atom(format!("*{expr}"))])
            } else {
                RustExpr::call(ctor, vec![RustExpr::Atom(expr.to_string())])
            }
        };
        // A reference constructor borrows an owned place but takes a binding as-is.
        let refed = |ctor: &str| {
            if by_ref {
                RustExpr::call(ctor, vec![RustExpr::Atom(expr.to_string())])
            } else {
                RustExpr::call(ctor, vec![RustExpr::Atom(format!("&{expr}"))])
            }
        };
        // The reference passed to a composite helper (already a ref, or borrowed).
        let as_ref = || {
            if by_ref {
                RustExpr::Atom(expr.to_string())
            } else {
                RustExpr::Atom(format!("&{expr}"))
            }
        };
        match Self::value_base(ty) {
            CsilTypeExpression::Builtin(name) => match name.as_str() {
                "int" | "nint" => scalar("cbor_int"),
                "uint" => scalar("cbor_uint"),
                "float" | "float16" | "float32" | "float64" => scalar("cbor_float"),
                "bool" => scalar("cbor_bool"),
                "text" | "tstr" => refed("cbor_text"),
                "bytes" | "bstr" => refed("cbor_bytes"),
                "timestamp" => refed("csil_enc_timestamp"),
                "decimal" => refed("csil_enc_decimal"),
                "null" | "nil" => RustExpr::Atom("CsilCborValue::Null".to_string()),
                // `any` already is a CBOR value tree; carry it through by clone.
                "any" => RustExpr::Atom(format!("{expr}.clone()")),
                _ => RustExpr::Atom("CsilCborValue::Null".to_string()),
            },
            CsilTypeExpression::Reference(name) if records.contains(name) => {
                refed(&format!("csil_enc_{}", Self::to_snake_case(name)))
            }
            // A reference to a transparent alias (`StringInt64Map = {* text => int}`,
            // `Tags = [* text]`, `Uuid = text`) has no codec of its own; encode it as
            // its underlying type. The Rust alias is a transparent `pub type`, so the
            // named-typed `expr` is the underlying type and flows through unchanged.
            CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
                self.rust_enc_value(&aliases[name], expr, by_ref, records, aliases)
            }
            // A named type-choice (enum or union) has its own value-tree codec.
            CsilTypeExpression::Reference(name) if self.type_choice(name).is_some() => {
                refed(&format!("csil_enc_{}", Self::to_snake_case(name)))
            }
            CsilTypeExpression::Array { element_type, .. } => {
                let elem = match self.rust_enc_func(element_type, records, aliases) {
                    Some(func) => RustExpr::Atom(func),
                    None => {
                        let inner =
                            self.rust_enc_value(element_type, "csil_elem", true, records, aliases);
                        RustExpr::closure("|csil_elem|", inner)
                    }
                };
                RustExpr::call("cbor_enc_array", vec![as_ref(), elem])
            }
            CsilTypeExpression::Map { key, value, .. } => {
                let kenc = match self.rust_enc_func(key, records, aliases) {
                    Some(func) => RustExpr::Atom(func),
                    None => {
                        let inner = self.rust_enc_value(key, "csil_mk", true, records, aliases);
                        RustExpr::closure("|csil_mk|", inner)
                    }
                };
                let venc = match self.rust_enc_func(value, records, aliases) {
                    Some(func) => RustExpr::Atom(func),
                    None => {
                        let inner = self.rust_enc_value(value, "csil_mv", true, records, aliases);
                        RustExpr::closure("|csil_mv|", inner)
                    }
                };
                RustExpr::call("cbor_enc_map", vec![as_ref(), kenc, venc])
            }
            // A fixed-shape tuple maps to a Rust tuple; encode positionally into a CBOR
            // array. An absent optional element is held in place as null so the array
            // length is fixed (the decoder reads positionally).
            CsilTypeExpression::Tuple(group) => {
                let mut parts = Vec::with_capacity(group.entries.len());
                for (i, entry) in group.entries.iter().enumerate() {
                    let place = format!("{expr}.{i}");
                    if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                        let inner = self.rust_enc_value(
                            &entry.value_type,
                            "csil_t",
                            true,
                            records,
                            aliases,
                        );
                        parts.push(RustExpr::MatchOpt {
                            place,
                            inner: Box::new(inner),
                        });
                    } else {
                        parts.push(self.rust_enc_value(
                            &entry.value_type,
                            &place,
                            false,
                            records,
                            aliases,
                        ));
                    }
                }
                RustExpr::ArrayVec(parts)
            }
            CsilTypeExpression::Literal(literal) => {
                RustExpr::Atom(Self::rust_literal_cbor_expr(literal))
            }
            // A shape the codec cannot model precisely (a non-record reference, `any`)
            // is carried as null rather than emitting code that would not compile.
            _ => RustExpr::Atom("CsilCborValue::Null".to_string()),
        }
    }

    /// A bare encoder function path when the typed helper's argument exactly matches
    /// the generic array/map helper's borrowed element. Returning a path instead of a
    /// closure keeps generated code clippy-clean without changing the fallback path
    /// needed for scalar conversions such as `String` to `&str`.
    fn rust_enc_func(
        &self,
        ty: &CsilTypeExpression,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> Option<String> {
        match Self::value_base(ty) {
            CsilTypeExpression::Reference(name) if records.contains(name) => {
                Some(format!("csil_enc_{}", Self::to_snake_case(name)))
            }
            CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
                self.rust_enc_func(&aliases[name], records, aliases)
            }
            CsilTypeExpression::Reference(name) if self.type_choice(name).is_some() => {
                Some(format!("csil_enc_{}", Self::to_snake_case(name)))
            }
            CsilTypeExpression::Builtin(name) => match name.as_str() {
                "timestamp" => Some("csil_enc_timestamp".to_string()),
                "decimal" => Some("csil_enc_decimal".to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Lay out `expr` the way rustfmt would: the returned first line carries no
    /// indentation (the caller has already written `col` characters on it) and
    /// continuation lines are indented to `indent`; `tail` characters will follow
    /// on the final line.
    fn render_expr(expr: &RustExpr, indent: usize, col: usize, tail: usize) -> String {
        if let Some(f) = expr.flat()
            && col + f.len() + tail <= 100
        {
            return f;
        }
        let pad = " ".repeat(indent);
        let pad4 = " ".repeat(indent + 4);
        match expr {
            RustExpr::Atom(s) => s.clone(),
            RustExpr::Closure { params, body } => {
                let inner = match body.as_ref() {
                    RustExpr::TupleDec { arity, elems } => {
                        Self::render_tuple_dec(indent + 4, *arity, elems)
                    }
                    RustExpr::LitDec { expected, value } => {
                        format!("cbor_expect_value(csil_v, &{expected})?;\n{pad4}Ok({value})")
                    }
                    other => Self::render_expr(other, indent + 4, indent + 4, 0),
                };
                format!("{params} {{\n{pad4}{inner}\n{pad}}}")
            }
            RustExpr::Call { .. } => Self::render_call(expr, indent, col, tail),
            RustExpr::ArrayVec(elems) => {
                let mut out = String::from("CsilCborValue::Array(vec![\n");
                for elem in elems {
                    out.push_str(&pad4);
                    out.push_str(&Self::render_expr(elem, indent + 4, indent + 4, 1));
                    out.push_str(",\n");
                }
                out.push_str(&format!("{pad}])"));
                out
            }
            RustExpr::MatchOpt { place, inner } => {
                let arm = match inner.flat() {
                    Some(f) if indent + 4 + 16 + f.len() < 100 => {
                        format!("Some(csil_t) => {f},")
                    }
                    _ => {
                        let pad8 = " ".repeat(indent + 8);
                        let rendered = Self::render_expr(inner, indent + 8, indent + 8, 0);
                        format!("Some(csil_t) => {{\n{pad8}{rendered}\n{pad4}}}")
                    }
                };
                format!(
                    "match &{place} {{\n{pad4}{arm}\n{pad4}None => CsilCborValue::Null,\n{pad}}}"
                )
            }
            RustExpr::TupleDec { arity, elems } => Self::render_tuple_dec(indent, *arity, elems),
            RustExpr::LitDec { expected, value } => {
                format!("cbor_expect_value(csil_v, &{expected})?;\n{pad}Ok({value})")
            }
        }
    }

    /// A call that did not fit flat: rustfmt flattens a chain of single-argument
    /// calls into one combined head and stacks the innermost argument list;
    /// otherwise a lone trailing closure overflows with a block body, and failing
    /// that every argument stacks on its own line.
    fn render_call(expr: &RustExpr, indent: usize, col: usize, _tail: usize) -> String {
        let pad = " ".repeat(indent);
        let pad4 = " ".repeat(indent + 4);
        let RustExpr::Call {
            head,
            args,
            macro_like,
        } = expr
        else {
            unreachable!("render_call takes only RustExpr::Call");
        };
        let outer_macro = *macro_like;

        // Chain-of-single-argument flattening (`Err(CsilCborError(` ...). A macro
        // is never folded into the head chain so its own no-trailing-comma
        // argument stacking stays in charge of the innermost list.
        let mut heads = String::new();
        let mut depth = 0usize;
        let mut cur = expr;
        while let RustExpr::Call {
            head,
            args,
            macro_like,
        } = cur
            && args.len() == 1
            && !*macro_like
        {
            heads.push_str(head);
            heads.push('(');
            depth += 1;
            cur = &args[0];
        }
        if depth > 0 {
            let closing = ")".repeat(depth);
            match cur {
                RustExpr::Call {
                    head: inner_head,
                    args: inner_args,
                    macro_like,
                } => {
                    let mut out = format!("{heads}{inner_head}(\n");
                    for (idx, arg) in inner_args.iter().enumerate() {
                        out.push_str(&pad4);
                        out.push_str(&Self::render_expr(arg, indent + 4, indent + 4, 1));
                        if !*macro_like || idx + 1 < inner_args.len() {
                            out.push(',');
                        }
                        out.push('\n');
                    }
                    out.push_str(&format!("{pad}){closing}"));
                    return out;
                }
                other => {
                    let rendered = Self::render_expr(other, indent + 4, indent + 4, 1);
                    return format!("{heads}\n{pad4}{rendered},\n{pad}{closing}");
                }
            }
        }

        // Lone trailing closure: overflow it with a block body when everything
        // before it stays flat on the head line.
        let closure_count = args
            .iter()
            .filter(|a| matches!(a, RustExpr::Closure { .. }))
            .count();
        if closure_count == 1
            && let Some(RustExpr::Closure { params, body }) = args.last()
        {
            let leading: Option<Vec<String>> =
                args[..args.len() - 1].iter().map(RustExpr::flat).collect();
            if let Some(leading) = leading {
                let head_line = format!("{head}({}, {params} {{", leading.join(", "));
                if col + head_line.len() <= 100 {
                    let inner = match body.as_ref() {
                        RustExpr::TupleDec { arity, elems } => {
                            Self::render_tuple_dec(indent + 4, *arity, elems)
                        }
                        RustExpr::LitDec { expected, value } => {
                            format!("cbor_expect_value(csil_v, &{expected})?;\n{pad4}Ok({value})")
                        }
                        other => Self::render_expr(other, indent + 4, indent + 4, 0),
                    };
                    return format!("{head_line}\n{pad4}{inner}\n{pad}}})");
                }
            }
        }

        // Every argument on its own line.
        let mut out = format!("{head}(\n");
        for (idx, arg) in args.iter().enumerate() {
            out.push_str(&pad4);
            out.push_str(&Self::render_expr(arg, indent + 4, indent + 4, 1));
            if !outer_macro || idx + 1 < args.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str(&format!("{pad})"));
        out
    }

    /// The positional tuple decoder's statements, first line unindented per the
    /// `render_expr` contract.
    fn render_tuple_dec(indent: usize, arity: usize, elems: &[(RustExpr, bool)]) -> String {
        let pad = " ".repeat(indent);
        let pad4 = " ".repeat(indent + 4);
        let pad8 = " ".repeat(indent + 8);
        let pad12 = " ".repeat(indent + 12);
        let mut out = format!(
            "let csil_arr = match csil_v {{\n\
             {pad4}CsilCborValue::Array(csil_a) => csil_a,\n\
             {pad4}_ => {{\n\
             {pad8}return Err(CsilCborError(\n\
             {pad12}\"csil cbor: tuple expects an array\".to_string(),\n\
             {pad8}))\n\
             {pad4}}}\n\
             {pad}}};\n\
             {pad}if csil_arr.len() != {arity} {{\n\
             {pad4}return Err(CsilCborError(format!(\n\
             {pad8}\"csil cbor: tuple expects {arity} elements, got {{}}\",\n\
             {pad8}csil_arr.len()\n\
             {pad4})));\n\
             {pad}}}\n\
             {pad}Ok((\n"
        );
        for (i, (dec, optional)) in elems.iter().enumerate() {
            out.push_str(&format!("{pad4}{{\n"));
            if *optional {
                out.push_str(&format!(
                    "{pad8}if matches!(csil_arr[{i}], CsilCborValue::Null) {{\n\
                     {pad12}None\n\
                     {pad8}}} else {{\n"
                ));
                out.push_str(&Self::render_let(indent + 12, "csil_d", dec));
                out.push_str(&format!("{pad12}Some(csil_d(&csil_arr[{i}])?)\n{pad8}}}\n"));
            } else {
                out.push_str(&Self::render_let(indent + 8, "csil_d", dec));
                out.push_str(&format!("{pad8}csil_d(&csil_arr[{i}])?\n"));
            }
            out.push_str(&format!("{pad4}}},\n"));
        }
        out.push_str(&format!("{pad}))"));
        out
    }

    /// Lay out `let {var} = {expr};` at `indent` down rustfmt's ladder: same line
    /// when the flat form fits, broken after `=` when the flat form fits four
    /// deeper, otherwise the expression wraps in place.
    fn render_let(indent: usize, var: &str, expr: &RustExpr) -> String {
        let pad = " ".repeat(indent);
        if let Some(f) = expr.flat() {
            if indent + 4 + var.len() + 3 + f.len() < 100 {
                return format!("{pad}let {var} = {f};\n");
            }
            if indent + 4 + f.len() < 100 {
                return format!("{pad}let {var} =\n{pad}    {f};\n");
            }
        }
        let col = indent + 4 + var.len() + 3;
        let rendered = Self::render_expr(expr, indent, col, 1);
        format!("{pad}let {var} = {rendered};\n")
    }

    /// A Rust expression of type `impl Fn(&CsilCborValue) -> Result<T, CsilCborError>`
    /// decoding a typed value from a `CsilCborValue`. Builtins resolve to a bare
    /// runtime accessor path; composites wrap the generic `cbor_dec_array`/
    /// `cbor_dec_map` helpers; an unmodelable shape errors at runtime rather than
    /// fabricating a value of a type the codec cannot reconstruct.
    fn rust_dec_func(
        &self,
        ty: &CsilTypeExpression,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> RustExpr {
        match Self::value_base(ty) {
            CsilTypeExpression::Builtin(name) => match name.as_str() {
                "int" | "nint" => RustExpr::Atom("cbor_as_i64".to_string()),
                "uint" => RustExpr::Atom("cbor_as_u64".to_string()),
                "float" | "float16" | "float32" | "float64" => {
                    RustExpr::Atom("cbor_as_f64".to_string())
                }
                "bool" => RustExpr::Atom("cbor_as_bool".to_string()),
                "text" | "tstr" => RustExpr::Atom("cbor_as_text".to_string()),
                "bytes" | "bstr" => RustExpr::Atom("cbor_as_bytes".to_string()),
                "timestamp" => RustExpr::Atom("csil_as_timestamp".to_string()),
                "decimal" => RustExpr::Atom("csil_as_decimal".to_string()),
                // `any` is the CBOR value tree itself; clone it through.
                "any" => RustExpr::closure(
                    "|csil_v: &CsilCborValue|",
                    RustExpr::Atom("Ok(csil_v.clone())".to_string()),
                ),
                _ => Self::dec_unsupported(),
            },
            CsilTypeExpression::Reference(name) if records.contains(name) => {
                RustExpr::Atom(format!("csil_dec_{}", Self::to_snake_case(name)))
            }
            // A reference to a transparent alias decodes as its underlying type; the
            // Rust alias is a transparent `pub type`, so the value the underlying
            // map/array/scalar decoder returns already is the alias-typed field.
            CsilTypeExpression::Reference(name) if aliases.contains_key(name) => {
                self.rust_dec_func(&aliases[name], records, aliases)
            }
            // A named type-choice (enum or union) decodes via its own value-tree codec.
            CsilTypeExpression::Reference(name) if self.type_choice(name).is_some() => {
                RustExpr::Atom(format!("csil_dec_{}", Self::to_snake_case(name)))
            }
            CsilTypeExpression::Array { element_type, .. } => {
                let inner = self.rust_dec_func(element_type, records, aliases);
                RustExpr::closure(
                    "|csil_v|",
                    RustExpr::call(
                        "cbor_dec_array",
                        vec![RustExpr::Atom("csil_v".to_string()), inner],
                    ),
                )
            }
            CsilTypeExpression::Map { key, value, .. } => {
                let kf = self.rust_dec_func(key, records, aliases);
                let vf = self.rust_dec_func(value, records, aliases);
                RustExpr::closure(
                    "|csil_v|",
                    RustExpr::call(
                        "cbor_dec_map",
                        vec![RustExpr::Atom("csil_v".to_string()), kf, vf],
                    ),
                )
            }
            // Decode a fixed-shape tuple positionally from a CBOR array; an optional
            // element reads `null` as `None`.
            CsilTypeExpression::Tuple(group) => {
                let elems: Vec<(RustExpr, bool)> = group
                    .entries
                    .iter()
                    .map(|entry| {
                        (
                            self.rust_dec_func(&entry.value_type, records, aliases),
                            matches!(entry.occurrence, Some(CsilOccurrence::Optional)),
                        )
                    })
                    .collect();
                RustExpr::closure(
                    "|csil_v: &CsilCborValue|",
                    RustExpr::TupleDec {
                        arity: group.entries.len(),
                        elems,
                    },
                )
            }
            CsilTypeExpression::Literal(literal) => RustExpr::closure(
                "|csil_v|",
                RustExpr::LitDec {
                    expected: Self::rust_literal_cbor_expr(literal),
                    value: Self::rust_literal_value_expr(literal),
                },
            ),
            _ => Self::dec_unsupported(),
        }
    }

    /// The decode fallback for a payload shape the codec cannot reconstruct: a
    /// closure that errors. Its `Ok` type is inferred from the field it fills, so it
    /// compiles against any field type without needing a `Default` to fabricate.
    fn dec_unsupported() -> RustExpr {
        RustExpr::closure(
            "|_csil_v|",
            RustExpr::call(
                "Err",
                vec![RustExpr::call(
                    "CsilCborError",
                    vec![RustExpr::Atom(
                        "\"csil cbor: unsupported field type\".to_string()".to_string(),
                    )],
                )],
            ),
        )
    }

    /// One `csil_entries.push((cbor_text(<wire>), <value>));` statement at
    /// `indent`, laid out on one line only while the tuple stays inside rustfmt's
    /// width and stacked (with the value wrapping in place) otherwise.
    fn rust_push_entry(indent: usize, wire_lit: &str, value: &RustExpr) -> String {
        let pad = " ".repeat(indent);
        let pad4 = " ".repeat(indent + 4);
        if let Some(flat) = value.flat() {
            let tuple = format!("cbor_text({wire_lit}), {flat}");
            let one_line = format!("{pad}csil_entries.push(({tuple}));\n");
            if tuple.len() <= 60 && one_line.trim_end().len() <= 100 {
                return one_line;
            }
        }
        let rendered = Self::render_expr(value, indent + 4, indent + 4, 1);
        format!(
            "{pad}csil_entries.push((\n{pad4}cbor_text({wire_lit}),\n{pad4}{rendered},\n{pad}));\n"
        )
    }

    fn rust_literal_cbor_expr(literal: &CsilLiteralValue) -> String {
        match literal {
            CsilLiteralValue::Integer(n) => format!("cbor_int({n})"),
            CsilLiteralValue::Float(f) => format!("cbor_float({f:?})"),
            CsilLiteralValue::Text(s) => format!("cbor_text({s:?})"),
            CsilLiteralValue::Bool(b) => format!("cbor_bool({b})"),
            CsilLiteralValue::Null => "CsilCborValue::Null".to_string(),
            CsilLiteralValue::Bytes(bytes) => {
                let values = bytes
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("CsilCborValue::Bytes(vec![{values}])")
            }
            CsilLiteralValue::Array(_) => "CsilCborValue::Null".to_string(),
        }
    }

    /// The boolean guard for one arm of a MIXED-kind literal enum's decode match
    /// (see `emit_choice_codec`'s `else` branch): does `csil_v` hold this literal's
    /// own value, checked in a CBOR-major-type-appropriate way. Every kind except
    /// `Integer` compares `csil_v` directly against the literal's own rendering
    /// (`rust_literal_cbor_expr`) — safe because encode and decode always agree on
    /// which `CsilCborValue` variant a given CBOR major type produces, for every
    /// kind except integers. `Integer` is the one exception: `cbor_int` always
    /// encodes to `CsilCborValue::Int`, but `cbor_dec` decodes a non-negative
    /// integer (CBOR major type 0) to `CsilCborValue::Uint`, so a direct `csil_v ==
    /// &cbor_int(n)` would reject a valid non-negative match on the wire —
    /// `cbor_as_i64` already normalizes that Uint/Int aliasing (see its own
    /// two-armed match over both variants), so route through it instead of a raw
    /// equality check.
    fn rust_enum_decode_guard(lit: &CsilLiteralValue) -> String {
        match lit {
            CsilLiteralValue::Integer(n) => format!("matches!(cbor_as_i64(csil_v), Ok({n}))"),
            other => format!("csil_v == &{}", Self::rust_literal_cbor_expr(other)),
        }
    }

    fn rust_literal_value_expr(literal: &CsilLiteralValue) -> String {
        match literal {
            CsilLiteralValue::Integer(n) => n.to_string(),
            CsilLiteralValue::Float(f) => format!("{f:?}"),
            CsilLiteralValue::Text(s) => format!("{s:?}.to_string()"),
            CsilLiteralValue::Bool(b) => b.to_string(),
            CsilLiteralValue::Null => "()".to_string(),
            CsilLiteralValue::Bytes(bytes) => {
                let values = bytes
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("vec![{values}]")
            }
            CsilLiteralValue::Array(_) => "Vec::<serde_json::Value>::new()".to_string(),
        }
    }

    fn rust_struct_ok(name: &str, members: &[String]) -> String {
        if members.is_empty() {
            return format!("    Ok({name} {{}})\n");
        }

        let one_line = format!("    Ok({name} {{ {} }})\n", members.join(", "));
        if format!("{{ {} }}", members.join(", ")).len() <= 22 {
            return one_line;
        }

        let mut out = format!("    Ok({name} {{\n");
        for member in members {
            out.push_str(&format!("        {member},\n"));
        }
        out.push_str("    })\n");
        out
    }

    /// One `pat => value,` match arm at codec indent. rustfmt keeps the arm on one
    /// line up to the width limit and otherwise braces the body on its own line (a
    /// braced arm carries no trailing comma).
    fn rust_match_arm(pat: &str, value: &str) -> String {
        let one_line = format!("        {pat} => {value},\n");
        if one_line.trim_end().len() <= 100 {
            return one_line;
        }
        format!("        {pat} => {{\n            {value}\n        }}\n")
    }

    /// The `cbor_encode(&<enc>(<arg>))` wrapper body. Past rustfmt's call width the
    /// nested single-argument calls flatten into one combined head and only the
    /// innermost argument drops to its own line.
    fn rust_encode_wrapper_body(call_head: &str, call_arg: &str) -> String {
        let one_line = format!("    cbor_encode(&{call_head}({call_arg}))\n");
        if format!("&{call_head}({call_arg})").len() <= 60 && one_line.trim_end().len() <= 100 {
            return one_line;
        }
        format!("    cbor_encode(&{call_head}(\n        {call_arg},\n    ))\n")
    }

    /// A single-parameter fn signature, wrapped param-on-its-own-line once the
    /// one-line form passes rustfmt's width.
    fn rust_sig(prefix: &str, name: &str, param: &str, ret: &str) -> String {
        let one_line = format!("{prefix}fn {name}({param}) -> {ret} {{\n");
        if one_line.trim_end().len() <= 100 {
            return one_line;
        }
        format!("{prefix}fn {name}(\n    {param},\n) -> {ret} {{\n")
    }

    fn rust_fn_header(name: &str, arg: &str, ret: &str) -> String {
        Self::rust_sig(
            "",
            name,
            &format!("{arg}: &CsilCborValue"),
            &format!("Result<{ret}, CsilCborError>"),
        )
    }

    fn rust_pub_decode_header(name: &str, ret: &str) -> String {
        Self::rust_sig(
            "pub ",
            name,
            "csil_data: &[u8]",
            &format!("Result<{ret}, CsilCborError>"),
        )
    }

    fn rust_decode_binding(indent: usize, dec: &RustExpr) -> String {
        Self::render_let(indent, "csil_decode", dec)
    }

    fn rust_trait_method(name: &str, args: &[String], ret: &str) -> String {
        let params = if args.is_empty() {
            "&self".to_string()
        } else {
            format!("&self, {}", args.join(", "))
        };
        let one_line = format!("    fn {name}({params}) -> Result<{ret}, ServiceError>;\n");
        // rustfmt's ladder for a bodyless trait fn: one line through 99 columns,
        // return type alone pushed to the next line at exactly 100 (the reserved
        // terminator column), params one-per-line beyond that.
        if one_line.trim_end().len() <= 99 {
            return one_line;
        }
        if one_line.trim_end().len() == 100 {
            return format!("    fn {name}({params})\n        -> Result<{ret}, ServiceError>;\n");
        }

        let mut out = format!("    fn {name}(\n        &self,\n");
        for arg in args {
            out.push_str("        ");
            out.push_str(arg);
            out.push_str(",\n");
        }
        out.push_str(&format!("    ) -> Result<{ret}, ServiceError>;\n"));
        out
    }

    /// A client method signature at impl indent: one line through rustfmt's
    /// width, params one-per-line past it.
    fn rust_client_method_sig(
        async_kw: &str,
        method: &str,
        input: Option<&str>,
        output: &str,
    ) -> String {
        let params = match input {
            Some(t) => format!("&self, req: {t}"),
            None => "&self".to_string(),
        };
        let one_line = format!(
            "    pub {async_kw}fn {method}({params}) -> Result<{output}, ClientError> {{\n"
        );
        if one_line.trim_end().len() <= 100 {
            return one_line;
        }
        let mut out = format!("    pub {async_kw}fn {method}(\n        &self,\n");
        if let Some(t) = input {
            out.push_str(&format!("        req: {t},\n"));
        }
        out.push_str(&format!("    ) -> Result<{output}, ClientError> {{\n"));
        out
    }

    /// The `let csil_resp = self.transport.call(...)` statement, following the
    /// ladder rustfmt applies to the chain (probed against rustfmt 1.x defaults):
    /// one line while the whole chain stays inside `chain_width`; then one chain
    /// element per line while the last element stays inside `chain_width`; then —
    /// sync only — the visual form that keeps `self.transport` together; and once
    /// the argument list itself passes `fn_call_width`, stacked arguments.
    fn rust_client_call(
        wire_service: &str,
        wire_method: &str,
        request_expr: &str,
        dot_await: &str,
    ) -> String {
        let args = format!("\"{wire_service}\", \"{wire_method}\", {request_expr}");
        let arg_width = args.len();
        if dot_await.is_empty() {
            if arg_width + 22 <= 60 {
                return format!("        let csil_resp = self.transport.call({args})?;\n");
            }
            if arg_width + 9 <= 60 {
                return format!(
                    "        let csil_resp = self\n\
                     \x20           .transport\n\
                     \x20           .call({args})?;\n"
                );
            }
            if arg_width <= 60 {
                return format!(
                    "        let csil_resp =\n\
                     \x20           self.transport\n\
                     \x20               .call({args})?;\n"
                );
            }
            return format!(
                "        let csil_resp = self.transport.call(\n\
                 \x20           \"{wire_service}\",\n\
                 \x20           \"{wire_method}\",\n\
                 \x20           {request_expr},\n\
                 \x20       )?;\n"
            );
        }

        if arg_width + 29 <= 60 {
            return format!("        let csil_resp = self.transport.call({args}).await?;\n");
        }
        if arg_width <= 60 {
            return format!(
                "        let csil_resp = self\n\
                 \x20           .transport\n\
                 \x20           .call({args})\n\
                 \x20           .await?;\n"
            );
        }
        format!(
            "        let csil_resp = self\n\
             \x20           .transport\n\
             \x20           .call(\n\
             \x20               \"{wire_service}\",\n\
             \x20               \"{wire_method}\",\n\
             \x20               {request_expr},\n\
             \x20           )\n\
             \x20           .await?;\n"
        )
    }

    /// Emit the `csil_enc_<t>`/`csil_dec_<t>` pair plus the public `encode_<t>`/
    /// `decode_<t>` byte wrappers for one record. The encoder lays keys in canonical
    /// RFC 8949 order; the decoder reads by key in declaration order (irrelevant on
    /// decode) and builds the struct literal directly.
    fn emit_record_codec(
        &self,
        name: &str,
        group: &CsilGroupExpression,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> String {
        // (member, wire, entry) in declaration order, plus a canonical-key-order copy
        // for the encoder so the emitted map is deterministic across languages.
        let named: Vec<(String, String, &CsilGroupEntry)> = group
            .entries
            .iter()
            .filter_map(|e| {
                let wire = Self::entry_wire_name(e)?;
                let member = Self::escape_rust_ident(&Self::to_snake_case(&wire));
                Some((member, wire, e))
            })
            .collect();
        let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
        canonical.sort_by_key(|f| Self::cbor_text_key_bytes(&f.1));

        let snake = Self::to_snake_case(name);
        let mut out = String::new();
        let encoder_reads_value = canonical.iter().any(|(_, _, entry)| {
            matches!(entry.occurrence, Some(CsilOccurrence::Optional))
                || !matches!(
                    Self::value_base(&entry.value_type),
                    CsilTypeExpression::Literal(_)
                )
        });
        let value_param = if encoder_reads_value {
            "csil_v"
        } else {
            "_csil_v"
        };
        let root_param = if named.is_empty() {
            "_csil_root"
        } else {
            "csil_root"
        };

        out.push_str(&format!(
            "/// Build the canonical CBOR value tree for a {name}.\n"
        ));
        out.push_str(&Self::rust_sig(
            "",
            &format!("csil_enc_{snake}"),
            &format!("{value_param}: &{name}"),
            "CsilCborValue",
        ));
        if named.is_empty() {
            out.push_str("    CsilCborValue::Map(Vec::new())\n}\n\n");
        } else {
            out.push_str(&format!(
                "    let mut csil_entries: Vec<(CsilCborValue, CsilCborValue)> = Vec::with_capacity({});\n",
                named.len()
            ));
            for (member, wire, entry) in &canonical {
                let wire_lit = format!("{wire:?}");
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    // An absent optional is omitted from the map entirely (wire contract).
                    let enc = self.rust_enc_value(
                        &entry.value_type,
                        "csil_inner",
                        true,
                        records,
                        aliases,
                    );
                    out.push_str(&format!(
                        "    if let Some(csil_inner) = &csil_v.{member} {{\n"
                    ));
                    out.push_str(&Self::rust_push_entry(8, &wire_lit, &enc));
                    out.push_str("    }\n");
                } else {
                    let place = format!("csil_v.{member}");
                    let enc =
                        self.rust_enc_value(&entry.value_type, &place, false, records, aliases);
                    out.push_str(&Self::rust_push_entry(4, &wire_lit, &enc));
                }
            }
            out.push_str("    CsilCborValue::Map(csil_entries)\n}\n\n");
        }

        out.push_str(&format!(
            "/// Reconstruct a {name} from a decoded CBOR value tree.\n"
        ));
        out.push_str(&Self::rust_fn_header(
            &format!("csil_dec_{snake}"),
            root_param,
            name,
        ));
        for (member, wire, entry) in &named {
            let wire_lit = format!("{wire:?}");
            // The decoder is bound to a local before being called: a composite field's
            // decoder is a closure, and calling it inline where declared would trip
            // `clippy::redundant_closure_call`. The binding works for the bare accessor
            // path too (a function item assigned then called).
            let dec = self.rust_dec_func(&entry.value_type, records, aliases);
            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                // A missing optional key leaves the field None; a present one decodes.
                // rustfmt's ladder for an overlong head: first only the brace drops
                // to its own line (the head itself may then still exceed the width —
                // rustfmt accepts that); only when even the braceless head overflows
                // does it break after `=`, shifting the whole match four deeper.
                let map_get = format!("cbor_map_get(csil_root, {wire_lit})");
                let head = format!("    let {member} = match {map_get} {{");
                let extra = if head.len() <= 100 {
                    out.push_str(&head);
                    out.push('\n');
                    0
                } else if head.len() - 2 <= 100 {
                    out.push_str(&format!("    let {member} = match {map_get}\n    {{\n"));
                    0
                } else {
                    out.push_str(&format!("    let {member} =\n        match {map_get} {{\n"));
                    4
                };
                let pad = " ".repeat(extra);
                out.push_str(&format!("{pad}        Some(csil_field) => {{\n"));
                out.push_str(&Self::rust_decode_binding(12 + extra, &dec));
                out.push_str(&format!(
                    "{pad}            Some(csil_decode(csil_field)?)\n\
                     {pad}        }}\n\
                     {pad}        None => None,\n\
                     {pad}    }};\n"
                ));
            } else {
                out.push_str(&format!(
                    "    let {member} = {{\n\
                     \x20       let csil_field = cbor_require(csil_root, {wire_lit})?;\n"
                ));
                out.push_str(&Self::rust_decode_binding(8, &dec));
                out.push_str(
                    "        csil_decode(csil_field)?\n\
                     \x20   };\n",
                );
            }
        }
        let members: Vec<String> = named.iter().map(|(member, _, _)| member.clone()).collect();
        out.push_str(&Self::rust_struct_ok(name, &members));
        out.push_str("}\n\n");

        out.push_str(&format!(
            "/// Encode a {name} to canonical CSIL CBOR bytes.\n"
        ));
        out.push_str(&Self::rust_sig(
            "pub ",
            &format!("encode_{snake}"),
            &format!("csil_v: &{name}"),
            "Vec<u8>",
        ));
        out.push_str(&Self::rust_encode_wrapper_body(
            &format!("csil_enc_{snake}"),
            "csil_v",
        ));
        out.push_str("}\n\n");
        out.push_str(&format!(
            "/// Decode canonical CSIL CBOR bytes into a {name}.\n"
        ));
        out.push_str(&Self::rust_pub_decode_header(
            &format!("decode_{snake}"),
            name,
        ));
        out.push_str(&format!(
            "    let csil_root = cbor_decode(csil_data)?;\n    csil_dec_{snake}(&csil_root)\n}}\n\n"
        ));
        out
    }

    /// Exported `encode_<stem>_request`/`decode_<stem>_request` (and `_response`)
    /// pairs for every op whose boundary is expressible but NOT a record or null — the
    /// scalar-id requests and `[*T]`/map/scalar responses the record-only filter used
    /// to drop. They reuse the same `rust_enc_value`/`rust_dec_func` building blocks the
    /// record codec uses for fields, so the client (and a consumer-side server) own one
    /// codec surface for every op. Record and null boundaries already have their own
    /// `encode_`/`decode_` (or no body), so they emit nothing here — an all-record spec
    /// produces byte-identical codec output.
    fn emit_op_codecs(
        &mut self,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> Result<String, String> {
        // Collect owned (helper, type) pairs first: the rule walk borrows
        // `self.input` immutably, while `map_type_to_rust` needs `&mut self`, so the two
        // must not overlap.
        let mut targets: Vec<(String, CsilTypeExpression)> = Vec::new();
        for rule in &self.spec.rules {
            let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
                continue;
            };
            for op in &service.operations {
                if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                    continue;
                }
                let success = success_type(&op.output_type);
                let stem = self.op_codec_stem(&rule.name, &op.name);
                if !is_null_input(&op.input_type)
                    && !Self::is_record_ref(&op.input_type, records)
                    && self.op_boundary_expressible(&op.input_type, records, aliases)
                {
                    targets.push((format!("{stem}_request"), op.input_type.clone()));
                }
                if !Self::is_record_ref(&success, records)
                    && self.op_boundary_expressible(&success, records, aliases)
                {
                    targets.push((format!("{stem}_response"), success));
                }
            }
        }

        let mut out = String::new();
        for (helper, ty) in &targets {
            out.push_str(&self.emit_op_codec_pair(helper, ty, records, aliases)?);
        }
        Ok(out)
    }

    /// One `encode_<helper>`/`decode_<helper>` pair over the value builders the record
    /// codec already uses, giving an arbitrary op-boundary shape the same byte seam a
    /// record type has.
    fn emit_op_codec_pair(
        &mut self,
        helper: &str,
        ty: &CsilTypeExpression,
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> Result<String, String> {
        let rust_type = self.map_type_to_rust(ty, &None)?;
        let enc = self.rust_enc_value(ty, "csil_v", true, records, aliases);
        let dec = self.rust_dec_func(ty, records, aliases);
        let decode_binding = Self::rust_decode_binding(4, &dec);
        let mut out = String::new();
        out.push_str(&format!(
            "/// Encode the {helper} payload to canonical CSIL CBOR bytes.\n"
        ));
        out.push_str(&Self::rust_sig(
            "pub ",
            &format!("encode_{helper}"),
            &format!("csil_v: &{rust_type}"),
            "Vec<u8>",
        ));
        out.push_str(&Self::rust_op_encode_body(&enc));
        out.push_str("}\n\n");
        out.push_str(&format!(
            "/// Decode canonical CSIL CBOR bytes into the {helper} payload.\n"
        ));
        out.push_str(&Self::rust_pub_decode_header(
            &format!("decode_{helper}"),
            &rust_type,
        ));
        out.push_str("    let csil_root = cbor_decode(csil_data)?;\n");
        out.push_str(&decode_binding);
        out.push_str("    csil_decode(&csil_root)\n}\n\n");
        Ok(out)
    }

    /// The op-codec `cbor_encode(&<enc>)` body: the borrow rides the encoder's
    /// head so the nested single-argument flattening sees one chain.
    fn rust_op_encode_body(enc: &RustExpr) -> String {
        let borrowed = match enc.clone() {
            RustExpr::Call {
                head,
                args,
                macro_like,
            } => RustExpr::Call {
                head: format!("&{head}"),
                args,
                macro_like,
            },
            RustExpr::Atom(s) => RustExpr::Atom(format!("&{s}")),
            // An op-boundary encoder root is always a call or an atom.
            other => other,
        };
        let whole = RustExpr::call("cbor_encode", vec![borrowed]);
        let rendered = Self::render_expr(&whole, 4, 4, 0);
        format!("    {rendered}\n")
    }

    /// Emit the value-tree codec (`csil_enc_<t>`/`csil_dec_<t>`) for a named
    /// type-choice. An enum (literal-only) encodes as its bare literal — the literal
    /// is its own discriminant. A union (type-bearing variants) encodes as a tagged
    /// sum `[variant_index, value]`, the index being the 0-based declaration order, so
    /// any variant types (records, tuples, nested unions) round-trip unambiguously.
    fn emit_choice_codec(
        &self,
        name: &str,
        choices: &[CsilTypeExpression],
        records: &HashSet<String>,
        aliases: &HashMap<String, CsilTypeExpression>,
    ) -> String {
        let snake = Self::to_snake_case(name);
        let mut out = String::new();

        if let Some(lits) = Self::enum_literals(choices) {
            let idents = Self::enum_variant_idents(&lits);
            // Encode: each unit variant to its bare literal CBOR value, via the
            // same `rust_literal_cbor_expr` every other literal-rendering call site
            // in this file uses — it already covers every `CsilLiteralValue` kind
            // (not just text/int), so a mixed-kind enum's variants each encode to
            // their own kind-appropriate CBOR value with no extra casing here. The
            // signature is built through `rust_sig` (not a hardcoded one-liner) so
            // a long synthesized hoisted-choice name (`<Record>_<field>`, which can
            // run well past a source-declared rule name) still wraps like rustfmt
            // instead of leaving a fixed line rustfmt would reformat.
            out.push_str(&format!(
                "/// Encode a {name} enum as its bare literal value.\n"
            ));
            out.push_str(&Self::rust_sig(
                "",
                &format!("csil_enc_{snake}"),
                &format!("csil_v: &{name}"),
                "CsilCborValue",
            ));
            out.push_str("    match csil_v {\n");
            for (ident, lit) in idents.iter().zip(lits.iter()) {
                let value = Self::rust_literal_cbor_expr(lit);
                out.push_str(&Self::rust_match_arm(&format!("{name}::{ident}"), &value));
            }
            out.push_str("    }\n}\n\n");

            // Decode: read the bare scalar and match it back to the variant. A
            // uniform-kind vocabulary (all-text or all-int — the overwhelming
            // common case) keeps its historical single-extraction shape
            // (`cbor_as_text`/`cbor_as_i64` once, then a plain scalar match) so
            // this stays byte-identical to the pre-fix output for every spec that
            // has no mixed-kind choice. A genuinely mixed vocabulary (`"a" / 1`, or
            // any other kind mix per `csilgen_common::classify_choice`'s contract)
            // has no single extractor that fits every arm, so it matches directly
            // on `csil_v` with a per-arm, kind-appropriate guard instead — this is
            // the defect fix: previously `enum_literals` rejected a mixed
            // vocabulary outright (required a uniform text-only or int-only
            // choice) and it fell through to the union path below, wrapping each
            // literal in a per-arm payload type it didn't have.
            let all_text = lits.iter().all(|l| matches!(l, CsilLiteralValue::Text(_)));
            let all_int = lits
                .iter()
                .all(|l| matches!(l, CsilLiteralValue::Integer(_)));
            out.push_str(&format!(
                "/// Decode a bare literal value into a {name} enum.\n"
            ));
            out.push_str(&Self::rust_fn_header(
                &format!("csil_dec_{snake}"),
                "csil_v",
                name,
            ));
            if all_text {
                out.push_str(
                    "    let csil_val = cbor_as_text(csil_v)?;\n    match csil_val.as_str() {\n",
                );
                for (ident, lit) in idents.iter().zip(lits.iter()) {
                    if let CsilLiteralValue::Text(s) = lit {
                        out.push_str(&Self::rust_match_arm(
                            &format!("{s:?}"),
                            &format!("Ok({name}::{ident})"),
                        ));
                    }
                }
            } else if all_int {
                out.push_str("    let csil_val = cbor_as_i64(csil_v)?;\n    match csil_val {\n");
                for (ident, lit) in idents.iter().zip(lits.iter()) {
                    if let CsilLiteralValue::Integer(n) = lit {
                        out.push_str(&Self::rust_match_arm(
                            &n.to_string(),
                            &format!("Ok({name}::{ident})"),
                        ));
                    }
                }
            } else {
                out.push_str("    match csil_v {\n");
                for (ident, lit) in idents.iter().zip(lits.iter()) {
                    let guard = Self::rust_enum_decode_guard(lit);
                    out.push_str(&Self::rust_match_arm(
                        &format!("_ if {guard}"),
                        &format!("Ok({name}::{ident})"),
                    ));
                }
            }
            out.push_str(&format!(
                "        csil_other => Err(CsilCborError(format!(\n\
                 \x20           \"csil cbor: unknown {name} value {{csil_other:?}}\"\n\
                 \x20       ))),\n    }}\n}}\n\n"
            ));
            return out;
        }

        // Union: tagged sum [index, value].
        out.push_str(&format!(
            "/// Encode a {name} union as a tagged sum `[variant_index, value]`.\n"
        ));
        out.push_str(&Self::rust_sig(
            "",
            &format!("csil_enc_{snake}"),
            &format!("csil_v: &{name}"),
            "CsilCborValue",
        ));
        out.push_str("    match csil_v {\n");
        for (i, choice) in choices.iter().enumerate() {
            let enc = self.rust_enc_value(choice, "csil_x", true, records, aliases);
            let value = RustExpr::ArrayVec(vec![
                RustExpr::Atom(format!("CsilCborValue::Uint({i})")),
                enc,
            ]);
            // A literal arm (bare, or `.default`/other-control-operator-wrapped —
            // see `choice_arm_literal`) encodes its constant, never the binding —
            // bind `_` so the generated match arm carries no unused-variable
            // warning.
            let binding = if choice_arm_literal(choice).is_some() {
                "_"
            } else {
                "csil_x"
            };
            let pat = format!("{name}::Variant{i}({binding})");
            // rustfmt's arm ladder: whole arm on one line when it fits; a flat
            // value that only overruns with the pattern gets a braced body (whose
            // budget reserves six columns for the arm scaffolding — probed at
            // inner line <= 94); a non-flat value hangs its `CsilCborValue::Array(vec![`
            // opener directly off `=>` as long as THAT line still fits — a long
            // synthesized hoisted-choice pattern (`<Record>_<field>::VariantN(...)` /
            // `<Service>_<op>_response::VariantN(...)`) can push even the opener
            // past rustfmt's width, at which point it braces the body instead
            // (empirical breakpoint verified against real `rustfmt` output; not
            // simply `max_width`, since the opener line ending in `[` doesn't
            // carry a trailing comma the way a flat arm's does).
            let hang_open = format!("        {pat} => CsilCborValue::Array(vec![");
            match value.flat() {
                Some(f) if 8 + pat.len() + 4 + f.len() < 100 => {
                    out.push_str(&format!("        {pat} => {f},\n"));
                }
                Some(f) if 12 + f.len() <= 94 => {
                    out.push_str(&format!(
                        "        {pat} => {{\n            {f}\n        }}\n"
                    ));
                }
                _ if hang_open.len() <= 98 => {
                    let rendered = Self::render_expr(&value, 8, 8 + pat.len() + 4, 1);
                    out.push_str(&format!("        {pat} => {rendered},\n"));
                }
                _ => {
                    // NOT `render_expr(&value, 12, 12, 0)`: at col 12 an 84-char
                    // flat array would satisfy render_expr's own `col + f.len() <=
                    // 100` shortcut and come back flat again — that shortcut uses
                    // `max_width` (100), but this call arrived here precisely
                    // because the flat form already failed the narrower
                    // `fn_call_width`-based budgets above. Force the multi-line
                    // `vec![...]` layout directly instead of re-asking
                    // `render_expr`, which has no way to know this element list
                    // was already rejected.
                    let RustExpr::ArrayVec(elems) = &value else {
                        unreachable!("emit_choice_codec's union arm value is always an ArrayVec");
                    };
                    let mut body = String::from("CsilCborValue::Array(vec![\n");
                    for elem in elems {
                        body.push_str("                ");
                        body.push_str(&Self::render_expr(elem, 16, 16, 1));
                        body.push_str(",\n");
                    }
                    body.push_str("            ])");
                    out.push_str(&format!(
                        "        {pat} => {{\n            {body}\n        }}\n"
                    ));
                }
            }
        }
        out.push_str("    }\n}\n\n");

        out.push_str(&format!(
            "/// Decode a tagged sum `[variant_index, value]` into a {name} union.\n"
        ));
        out.push_str(&Self::rust_fn_header(
            &format!("csil_dec_{snake}"),
            "csil_v",
            name,
        ));
        out.push_str(
            "    let csil_arr = match csil_v {\n\
             \x20       CsilCborValue::Array(csil_a) => csil_a,\n\
             \x20       _ => {\n\
             \x20           return Err(CsilCborError(\n\
             \x20               \"csil cbor: union expects a 2-element array\".to_string(),\n\
             \x20           ))\n\
             \x20       }\n\
             \x20   };\n\
             \x20   if csil_arr.len() != 2 {\n\
             \x20       return Err(CsilCborError(format!(\n\
             \x20           \"csil cbor: union array has {} elements, expected 2\",\n\
             \x20           csil_arr.len()\n\
             \x20       )));\n\
             \x20   }\n\
             \x20   let csil_idx = cbor_as_u64(&csil_arr[0])?;\n\
             \x20   match csil_idx {\n",
        );
        for (i, choice) in choices.iter().enumerate() {
            let dec = self.rust_dec_func(choice, records, aliases);
            out.push_str(&format!("        {i} => {{\n"));
            out.push_str(&Self::rust_decode_binding(12, &dec));
            // A long synthesized hoisted-choice name (`<Record>_<field>` or
            // `<Service>_<op>_response`) can push this call past rustfmt's width.
            // For this exact fixed shape (`Ok(Name::VariantN(csil_decode(&csil_arr[1])?))`
            // at a fixed indent of 12), rustfmt's actual choice among flat /
            // break-the-inner-call's-argument / break-the-outer-call's-argument
            // was reverse-engineered by trial against real `rustfmt` at varying
            // name lengths — it is not simply `max_width`(100) or
            // `fn_call_width`(60), so the two breakpoints below (76, 90) are
            // empirical for this one shape, not general-purpose constants.
            let flat = format!("            Ok({name}::Variant{i}(csil_decode(&csil_arr[1])?))\n");
            let flat_len = flat.trim_end().len();
            if flat_len <= 76 {
                out.push_str(&flat);
            } else if flat_len <= 90 {
                out.push_str(&format!(
                    "            Ok({name}::Variant{i}(csil_decode(\n\
                     \x20               &csil_arr[1],\n\
                     \x20           )?))\n"
                ));
            } else {
                out.push_str(&format!(
                    "            Ok({name}::Variant{i}(\n\
                     \x20               csil_decode(&csil_arr[1])?,\n\
                     \x20           ))\n"
                ));
            }
            out.push_str("        }\n");
        }
        out.push_str(&format!(
            "        csil_other => Err(CsilCborError(format!(\n\
             \x20           \"csil cbor: unknown {name} variant {{csil_other}}\"\n\
             \x20       ))),\n    }}\n}}\n\n"
        ));
        out
    }

    /// Build `codec.gen.rs`: the self-contained canonical-CBOR runtime plus an
    /// `encode_`/`decode_` pair per record. `None` when the spec declares no record
    /// the codec can model.
    fn generate_codec(&mut self) -> Result<Option<String>, String> {
        let records = self.record_names();
        if records.is_empty() {
            return Ok(None);
        }
        let aliases = self.codec_aliases();

        let mut content = String::new();
        content.push_str(
            "//! Generated self-contained canonical-CBOR codec from CSIL specification.\n\
             //!\n\
             //! CSIL is the CBOR Service Interface Language; this codec owns the payload\n\
             //! wire (a CBOR map keyed by the verbatim CSIL field name in canonical RFC\n\
             //! 8949 order) so the generated types need no serde derive. One\n\
             //! `encode_`/`decode_` pair is emitted per record type.\n",
        );
        // Not every runtime helper is exercised by every spec; the codec is a fixed
        // self-contained block, so silence the unused-helper lint rather than prune.
        // A record whose fields are all required builds its entry list with `push`
        // calls after a sized `Vec` (capacity matches the field count); the
        // optional-field case interleaves conditional pushes, so one canonical shape
        // is kept rather than special-casing each record.
        content.push_str("#![allow(dead_code, clippy::vec_init_then_push)]\n\n");
        content.push_str("use super::types::*;\n\n");

        content.push_str(CODEC_RUNTIME_RUST);
        content.push_str("\n\n");

        if self.spec_uses_builtin("timestamp") {
            content.push_str(CODEC_TIMESTAMP_RUST);
            content.push_str("\n\n");
        }
        if self.spec_uses_builtin("decimal") {
            content.push_str(CODEC_BIGINT_RUST);
            content.push_str("\n\n");
            content.push_str(match self.decimal_mapping {
                DecimalMapping::Csil => CODEC_DECIMAL_CSIL_RUST,
                DecimalMapping::Library => CODEC_DECIMAL_LIBRARY_RUST,
            });
            content.push_str("\n\n");
        }

        for rule in &self.spec.rules {
            let group = match &rule.rule_type {
                CsilRuleType::GroupDef(g) => Some(g),
                CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
                _ => None,
            };
            if let Some(group) = group
                && records.contains(&rule.name)
            {
                content.push_str(&self.emit_record_codec(&rule.name, group, &records, &aliases));
            }
        }

        // Value-tree codecs for named type-choices (enums + unions) so record fields
        // referencing them encode/decode through `csil_enc_`/`csil_dec_`.
        for rule in &self.spec.rules {
            let choices = match &rule.rule_type {
                CsilRuleType::TypeChoice(c) => c,
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(c)) => c,
                _ => continue,
            };
            content.push_str(&self.emit_choice_codec(&rule.name, choices, &records, &aliases));
        }

        // Per-op byte helpers for non-record (non-null) op boundaries, so the client and
        // a consumer-side server share one codec surface for every op.
        content.push_str(&self.emit_op_codecs(&records, &aliases)?);

        Ok(Some(content))
    }

    /// Strip a trailing `Service` suffix and PascalCase the remainder — a Rust
    /// identifier stem (client struct name, crate/package name, per-op codec
    /// stem). The wire carries the CSIL service name verbatim
    /// (docs/cbor-wire-contract.md "RPC call naming"); this is not that.
    fn service_base(name: &str) -> String {
        let pascal = Self::to_pascal_case(name);
        pascal
            .strip_suffix("Service")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(pascal)
    }

    fn to_pascal_case(s: &str) -> String {
        let mut out = String::new();
        let mut cap = true;
        for ch in s.chars() {
            if ch == '-' || ch == '_' {
                cap = true;
            } else if cap {
                out.push(ch.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// Whether the spec declares its own `ServiceError` type (group/type rule),
    /// in which case the generator must not emit its hardcoded fallback.
    fn spec_defines_service_error(&self) -> bool {
        self.spec.rules.iter().any(|r| {
            r.name == "ServiceError" && !matches!(r.rule_type, CsilRuleType::ServiceDef(_))
        })
    }

    fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
        def.operations
            .iter()
            .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
    }

    fn generate_service_error(&self, code: &mut String) {
        code.push_str("#[derive(Debug, Clone)]\n");
        code.push_str("pub struct ServiceError {\n");
        code.push_str("    pub code: i32,\n");
        code.push_str("    pub message: String,\n");
        code.push_str("}\n\n");

        code.push_str("impl std::fmt::Display for ServiceError {\n");
        code.push_str("    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n");
        code.push_str("        write!(f, \"service error {}: {}\", self.code, self.message)\n");
        code.push_str("    }\n");
        code.push_str("}\n\n");

        code.push_str("impl std::error::Error for ServiceError {}\n");
    }

    fn generate_service_trait(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let mut content = String::new();

        content.push_str(&format!("/// {name} service trait\n"));
        content.push_str(&format!("pub trait {name} {{\n"));
        content.push_str("    type Context;\n");

        for operation in &service.operations {
            let op_name = Self::to_snake_case(&operation.name);
            match operation.direction {
                CsilServiceDirection::Unidirectional => {
                    // `Res / ServiceError` rides the `Result` error channel, so the
                    // success signature uses the response half only — otherwise the
                    // whole union falls back to the untyped `serde_json::Value`.
                    let output_type =
                        self.map_type_to_rust(&success_type(&operation.output_type), &None)?;
                    Self::write_op_doc(&mut content, operation, "request/response");
                    // A push-style op (`op: -> Event`) has a `null` input: emit no
                    // request parameter rather than a meaningless `input: ()`.
                    if is_null_input(&operation.input_type) {
                        content.push_str(&Self::rust_trait_method(
                            &op_name,
                            &["ctx: &Self::Context".to_string()],
                            &output_type,
                        ));
                    } else {
                        let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                        content.push_str(&Self::rust_trait_method(
                            &op_name,
                            &[
                                "ctx: &Self::Context".to_string(),
                                format!("input: {input_type}"),
                            ],
                            &output_type,
                        ));
                    }
                }
                CsilServiceDirection::Bidirectional => {
                    // Server-side inbound: receive the client's pushed message.
                    // Outbound (Output) is encoded via the generated helper and
                    // pushed by the implementer's connection plumbing — the
                    // generator never owns the wire.
                    Self::write_op_doc(&mut content, operation, "channel inbound (bidirectional)");
                    // A null inbound payload means there is no message to receive, so
                    // omit the `msg` parameter rather than emit `msg: ()`.
                    if is_null_input(&operation.input_type) {
                        content.push_str(&Self::rust_trait_method(
                            &op_name,
                            &["ctx: &Self::Context".to_string()],
                            "()",
                        ));
                    } else {
                        let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                        content.push_str(&Self::rust_trait_method(
                            &op_name,
                            &[
                                "ctx: &Self::Context".to_string(),
                                format!("msg: {input_type}"),
                            ],
                            "()",
                        ));
                    }
                }
                CsilServiceDirection::Reverse => {
                    // Reverse is server-pushed only: no inbound on the server
                    // side, just an outbound encoder emitted below.
                }
            }
        }

        content.push('}');
        Ok(content)
    }

    /// Emit a `wire_ids` module exposing the `@wire-id(N)` ordinals as `u64`
    /// constants so a host can reference them instead of hardcoding. Returns
    /// `None` when the service carries no wire-id, keeping output unchanged for
    /// every wire-id-free spec.
    fn generate_wire_ids(&self, name: &str, service: &CsilServiceDefinition) -> Option<String> {
        let service_id = service.wire_id?;
        let mod_name = format!("{}_wire_ids", Self::to_snake_case(name));
        let mut content = String::new();
        content.push_str(&format!(
            "/// Wire-id ordinals for the {name} service (transport compact profiles).\n"
        ));
        content.push_str(&format!("pub mod {mod_name} {{\n"));
        content.push_str(&format!("    pub const SERVICE: u64 = {service_id};\n"));
        for operation in &service.operations {
            if let Some(op_id) = operation.wire_id {
                // Prefix operation constants with `OP_` so an op literally named
                // `service` emits `OP_SERVICE` rather than colliding with the
                // `SERVICE` service ordinal (which would fail to compile).
                let const_name = Self::to_snake_case(&operation.name).to_ascii_uppercase();
                content.push_str(&format!("    pub const OP_{const_name}: u64 = {op_id};\n"));
            }
        }
        content.push('}');
        Some(content)
    }

    fn write_op_doc(content: &mut String, op: &CsilServiceOperation, fallback: &str) {
        if op.doc_comments.is_empty() {
            content.push_str(&format!("    /// {} ({fallback}).\n", op.name));
        } else {
            for line in &op.doc_comments {
                content.push_str(&format!("    /// {line}\n"));
            }
        }
    }

    /// One channel router match arm: decode the op's inbound payload with the generated
    /// per-type codec and hand it to the trait method. A null inbound payload carries no
    /// message, so the handler is called without one. Returns `None` for a non-record,
    /// non-null payload — the per-type codec only covers records, so such an op is left
    /// unrouted (it falls through to the `unknown channel` arm) rather than emitting an
    /// uncompilable decode. The label is the match scrutinee (`"Wire"` or an ordinal).
    fn channel_route_arm(&self, op: &CsilServiceOperation, label: &str) -> Option<String> {
        let op_snake = Self::to_snake_case(&op.name);
        if is_null_input(&op.input_type) {
            return Some(format!("        {label} => handlers.{op_snake}(ctx),\n"));
        }
        if !Self::is_record_ref(&op.input_type, &self.record_names()) {
            return None;
        }
        let decode_fn = format!(
            "decode_{}",
            Self::to_snake_case(&Self::type_ref_name(&op.input_type))
        );
        // rustfmt's ladder for this statement, probed against the defaults: hang
        // the struct literal off `.map_err` while its head line fits; then break
        // after `=`; then give the closure a block body (which reserves one extra
        // column); then break after `=` with the block body; then break the chain.
        let d = decode_fn.len();
        let msg_stmt = if 58 + d <= 100 {
            format!(
                "            let msg = {decode_fn}(bytes).map_err(|err| ServiceError {{\n\
                 \x20               code: 400,\n\
                 \x20               message: err.to_string(),\n\
                 \x20           }})?;\n"
            )
        } else if 52 + d <= 100 {
            format!(
                "            let msg =\n\
                 \x20               {decode_fn}(bytes).map_err(|err| ServiceError {{\n\
                 \x20                   code: 400,\n\
                 \x20                   message: err.to_string(),\n\
                 \x20               }})?;\n"
            )
        } else if 45 + d <= 99 {
            format!(
                "            let msg = {decode_fn}(bytes).map_err(|err| {{\n\
                 \x20               ServiceError {{\n\
                 \x20                   code: 400,\n\
                 \x20                   message: err.to_string(),\n\
                 \x20               }}\n\
                 \x20           }})?;\n"
            )
        } else if 39 + d <= 99 {
            format!(
                "            let msg =\n\
                 \x20               {decode_fn}(bytes).map_err(|err| {{\n\
                 \x20                   ServiceError {{\n\
                 \x20                       code: 400,\n\
                 \x20                       message: err.to_string(),\n\
                 \x20                   }}\n\
                 \x20               }})?;\n"
            )
        } else {
            format!(
                "            let msg = {decode_fn}(bytes)\n\
                 \x20               .map_err(|err| ServiceError {{\n\
                 \x20                   code: 400,\n\
                 \x20                   message: err.to_string(),\n\
                 \x20               }})?;\n"
            )
        };
        Some(format!(
            "        {label} => {{\n\
             {msg_stmt}\
             \x20           handlers.{op_snake}(ctx, msg)\n\
             \x20       }}\n"
        ))
    }

    /// For services with any `<->` op, emit `route_<service>_channel` that decodes
    /// inbound bytes (keyed by the wire method name) with the generated per-type codec
    /// and dispatches to the trait method. Reverse ops never have an inbound route.
    fn generate_service_router(
        &mut self,
        service_name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let inbound_ops: Vec<&CsilServiceOperation> = service
            .operations
            .iter()
            .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
            .collect();
        let arms: Vec<String> = inbound_ops
            .iter()
            .filter_map(|op| {
                // The router matches the verbose wire's event name, which is the CSIL
                // operation name verbatim (docs/cbor-wire-contract.md "RPC call naming").
                let wire = &op.name;
                self.channel_route_arm(op, &format!("\"{wire}\""))
            })
            .collect();

        let mut content = String::new();
        let fn_name = format!("route_{}_channel", Self::to_snake_case(service_name));
        let (handlers, ctx, bytes) = if arms.is_empty() {
            ("_handlers", "_ctx", "_bytes")
        } else {
            ("handlers", "ctx", "bytes")
        };
        content.push_str(&format!(
            "/// Decode one inbound channel frame for {service_name} (with the generated\n\
             /// per-type codec) and dispatch to the matching trait method. The implementer\n\
             /// feeds raw bytes from its connection here; we never own the wire.\n\
             pub fn {fn_name}<H>(\n\
             \x20   {handlers}: &H,\n\
             \x20   {ctx}: &H::Context,\n\
             \x20   method: &str,\n\
             \x20   {bytes}: &[u8],\n\
             ) -> Result<(), ServiceError>\n\
             where\n\
             \x20   H: {service_name},\n\
             {{\n"
        ));
        if arms.is_empty() {
            content.push_str("    Err(ServiceError {\n");
            content.push_str("        code: 404,\n");
            content.push_str("        message: format!(\"unknown channel {method}\"),\n");
            content.push_str("    })\n");
        } else {
            content.push_str("    match method {\n");
            for arm in arms {
                content.push_str(&arm);
            }
            content.push_str("        other => Err(ServiceError {\n");
            content.push_str("            code: 404,\n");
            content.push_str("            message: format!(\"unknown channel {other}\"),\n");
            content.push_str("        }),\n");
            content.push_str("    }\n");
        }
        content.push_str("}\n");
        Ok(content)
    }

    /// The compact-profile twin of `generate_service_router`: when the service
    /// carries `@wire-id` ordinals, emit `route_<service>_channel_compact` that
    /// dispatches on the operation ordinal (`u64`) instead of the wire name.
    /// The profile is negotiated on the wire (never declared in CSIL), so a host
    /// keeps both routers and calls whichever the peer selected. Returns `None`
    /// for wire-id-free specs, keeping their output byte-identical.
    fn generate_service_router_compact(
        &mut self,
        service_name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<Option<String>, String> {
        if service.wire_id.is_none() {
            return Ok(None);
        }
        let inbound_ops: Vec<&CsilServiceOperation> = service
            .operations
            .iter()
            .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
            .collect();
        let arms: Vec<String> = inbound_ops
            .iter()
            .filter_map(|op| {
                let op_id = op.wire_id?;
                self.channel_route_arm(op, &op_id.to_string())
            })
            .collect();

        let mut content = String::new();
        let fn_name = format!(
            "route_{}_channel_compact",
            Self::to_snake_case(service_name)
        );
        let (handlers, ctx, bytes) = if arms.is_empty() {
            ("_handlers", "_ctx", "_bytes")
        } else {
            ("handlers", "ctx", "bytes")
        };
        content.push_str(&format!(
            "/// Decode one inbound channel frame for {service_name} by its\n\
             /// `@wire-id` ordinal (compact transport profile) and dispatch to the\n\
             /// matching trait method. The verbose-profile twin is\n\
             /// `route_{}_channel`; the host calls whichever matches the profile\n\
             /// negotiated on the wire.\n\
             pub fn {fn_name}<H>(\n\
             \x20   {handlers}: &H,\n\
             \x20   {ctx}: &H::Context,\n\
             \x20   op: u64,\n\
             \x20   {bytes}: &[u8],\n\
             ) -> Result<(), ServiceError>\n\
             where\n\
             \x20   H: {service_name},\n\
             {{\n",
            Self::to_snake_case(service_name)
        ));
        if arms.is_empty() {
            content.push_str("    Err(ServiceError {\n");
            content.push_str("        code: 404,\n");
            content.push_str("        message: format!(\"unknown channel ordinal {op}\"),\n");
            content.push_str("    })\n");
        } else {
            content.push_str("    match op {\n");
            for arm in arms {
                content.push_str(&arm);
            }
            content.push_str("        other => Err(ServiceError {\n");
            content.push_str("            code: 404,\n");
            content
                .push_str("            message: format!(\"unknown channel ordinal {other}\"),\n");
            content.push_str("        }),\n");
            content.push_str("    }\n");
        }
        content.push_str("}\n");
        Ok(Some(content))
    }

    /// For each `<->` and `<-` op, emit `encode_<service>_<op>` that returns
    /// `(method, bytes)` — the wire method name and the op's outbound payload encoded
    /// with the generated per-type codec — for the implementer to put on the wire.
    /// Unidirectional ops already return a value from their trait method, so no encoder.
    /// A null outbound payload encodes to empty bytes; a non-record, non-null payload has
    /// no per-type codec helper and so emits no encoder.
    fn generate_service_encoders(
        &mut self,
        service_name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let mut content = String::new();
        let svc_snake = Self::to_snake_case(service_name);
        let records = self.record_names();
        for op in &service.operations {
            if !matches!(
                op.direction,
                CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
            ) {
                continue;
            }
            let op_snake = Self::to_snake_case(&op.name);
            // Verbatim CSIL operation name: what the encoder returns as the wire
            // event name (docs/cbor-wire-contract.md "RPC call naming").
            let wire = &op.name;
            let fn_name = format!("encode_{svc_snake}_{op_snake}");
            let doc = format!(
                "/// Encode a `{wire}` message pushed from {service_name}'s server\n\
                 /// side; the implementer frames `(method, bytes)` onto its connection.\n"
            );
            if is_null_input(&op.output_type) {
                content.push_str(&doc);
                content.push_str(&format!("pub fn {fn_name}() -> (String, Vec<u8>) {{\n"));
                content.push_str(&Self::rust_wire_tuple(wire, "Vec::new()"));
                content.push_str("}\n");
                continue;
            }
            if !Self::is_record_ref(&op.output_type, &records) {
                continue;
            }
            let output_type = self.map_type_to_rust(&op.output_type, &None)?;
            let encode_fn = format!(
                "encode_{}",
                Self::to_snake_case(&Self::type_ref_name(&op.output_type))
            );
            content.push_str(&doc);
            content.push_str(&Self::rust_sig(
                "pub ",
                &fn_name,
                &format!("msg: &{output_type}"),
                "(String, Vec<u8>)",
            ));
            content.push_str(&Self::rust_wire_tuple(wire, &format!("{encode_fn}(msg)")));
            content.push_str("}\n");
        }
        Ok(content)
    }

    /// The `("Wire".to_string(), <bytes expr>)` body of a channel encoder; the
    /// tuple stacks one element per line past rustfmt's width.
    fn rust_wire_tuple(wire: &str, bytes_expr: &str) -> String {
        let inner = format!("\"{wire}\".to_string(), {bytes_expr}");
        let one_line = format!("    ({inner})\n");
        if inner.len() <= 60 && one_line.trim_end().len() <= 100 {
            return one_line;
        }
        format!("    (\n        \"{wire}\".to_string(),\n        {bytes_expr},\n    )\n")
    }

    fn generate_lib_file(&mut self, files: &[GeneratedFile]) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("//! Generated Rust code from CSIL specification\n\n");

        // In the default (non-package) mode this generator emits no Cargo.toml, so
        // the crates the generated code needs are documented here for the consuming
        // crate to add. The self-contained `codec.gen.rs` owns the CBOR wire, so no
        // CBOR library is required; only the in-memory types for `timestamp`/`decimal`
        // (and `regex` for validation) pull a dependency. In package mode the same
        // list lands in the emitted `Cargo.toml` instead, so the note is suppressed.
        let deps = self.crate_dependencies();
        if !emit_rust_package(self.input) && !deps.is_empty() {
            content.push_str("//! ## Additional dependencies for the consuming crate\n//!\n");
            for (crate_name, req) in &deps {
                content.push_str(&format!("//! {crate_name} = \"{req}\"\n"));
            }
            content.push_str("//!\n");
        }

        // Add module declarations
        if files.iter().any(|f| f.path == "types.rs") {
            content.push_str("pub mod types;\n");
            content.push_str("pub use types::*;\n\n");
        }

        // The codec file carries a `.gen.rs` infix (matching the other generators'
        // `codec.gen.*`), which is not a bare module name, so it is wired in through
        // an explicit `#[path]`.
        if files.iter().any(|f| f.path == "codec.gen.rs") {
            content.push_str("#[path = \"codec.gen.rs\"]\n");
            content.push_str("pub mod codec;\n");
            content.push_str("pub use codec::*;\n\n");
        }

        if files.iter().any(|f| f.path == "services.rs") {
            content.push_str("pub mod services;\n");
            content.push_str("pub use services::*;\n\n");
        }

        if files.iter().any(|f| f.path == "client.rs") {
            content.push_str("pub mod client;\n");
            content.push_str("pub use client::*;\n\n");
        }

        // The async twin (emitted under the default `both` style) is a sibling module
        // whose `Async`-marked symbols coexist with the sync client's in one glob.
        if files.iter().any(|f| f.path == "client_async.rs") {
            content.push_str("pub mod client_async;\n");
            content.push_str("pub use client_async::*;\n\n");
        }

        Ok(content)
    }

    /// The referenced type name when a value expression is a bare group
    /// reference (optionally wrapped in constraints), used to recognize a
    /// keyless group-spread entry. Returns `None` for any other shape so only
    /// genuine spreads gain a synthesized field.
    fn group_spread_reference(value_type: &CsilTypeExpression) -> Option<String> {
        match value_type {
            CsilTypeExpression::Reference(name) => Some(name.clone()),
            CsilTypeExpression::Constrained { base_type, .. } => {
                Self::group_spread_reference(base_type)
            }
            _ => None,
        }
    }

    fn extract_field_name(&self, key: &Option<CsilGroupKey>) -> Option<String> {
        match key {
            Some(CsilGroupKey::Bare(name)) => {
                Some(Self::escape_rust_ident(&Self::to_snake_case(name)))
            }
            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                Some(Self::escape_rust_ident(&Self::to_snake_case(name)))
            }
            _ => None,
        }
    }

    /// Make `ident` usable as a Rust binding/field identifier when a CSIL field name
    /// collides with a keyword (`type`, `match`, `move`, …). Most keywords take the
    /// raw-identifier form (`r#type`), which keeps the name readable and stable; the
    /// four that `r#` forbids (`crate`/`self`/`super`/`Self`) fall back to a trailing
    /// underscore. Only standalone identifiers are escaped — the wire key keeps the
    /// original spelling, so the rename never reaches the CBOR map.
    fn escape_rust_ident(ident: &str) -> String {
        const RAW_FORBIDDEN: [&str; 4] = ["crate", "self", "super", "Self"];
        const KEYWORDS: [&str; 51] = [
            "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
            "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
            "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe",
            "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
            "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
            "Self",
        ];
        if !KEYWORDS.contains(&ident) {
            ident.to_string()
        } else if RAW_FORBIDDEN.contains(&ident) {
            format!("{ident}_")
        } else {
            format!("r#{ident}")
        }
    }

    fn map_type_to_rust(
        &mut self,
        type_expr: &CsilTypeExpression,
        occurrence: &Option<CsilOccurrence>,
    ) -> Result<String, String> {
        let base_type = match type_expr {
            CsilTypeExpression::Builtin(name) => self.map_builtin_type(name),
            CsilTypeExpression::Reference(name) => name.clone(),
            CsilTypeExpression::Array { element_type, .. } => {
                let element = self.map_type_to_rust(element_type, &None)?;
                format!("Vec<{element}>")
            }
            CsilTypeExpression::Map { key, value, .. } => {
                let key_type = self.map_type_to_rust(key, &None)?;
                let value_type = self.map_type_to_rust(value, &None)?;
                format!("std::collections::HashMap<{key_type}, {value_type}>")
            }
            // A fixed-shape array maps to a Rust tuple. Entries are positional, so
            // any keys are names only and carry no Rust-type effect (a tuple has no
            // field names); they are dropped here.
            CsilTypeExpression::Tuple(group) => {
                let mut elems = Vec::with_capacity(group.entries.len());
                for entry in &group.entries {
                    elems.push(self.map_type_to_rust(&entry.value_type, &entry.occurrence)?);
                }
                match elems.len() {
                    // A 1-tuple needs the trailing comma or it's just a parenthesized type.
                    1 => format!("({},)", elems[0]),
                    _ => format!("({})", elems.join(", ")),
                }
            }
            CsilTypeExpression::Choice(choices) => {
                if choices.len() == 2
                    && choices
                        .iter()
                        .any(|c| matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Null)))
                {
                    // Handle optional types (T | null)
                    let non_null = choices.iter().find(|c| {
                        !matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Null))
                    });
                    if let Some(inner_type) = non_null {
                        let inner = self.map_type_to_rust(inner_type, &None)?;
                        format!("Option<{inner}>")
                    } else {
                        "serde_json::Value".to_string()
                    }
                } else {
                    "serde_json::Value".to_string() // General choice fallback
                }
            }
            // A constrained type maps to its base type; the constraints are
            // enforced at runtime by the generated `validate` method, not by the
            // Rust type. Occurrence is applied by the outer wrapper below.
            CsilTypeExpression::Constrained { base_type, .. } => {
                self.map_type_to_rust(base_type, &None)?
            }
            CsilTypeExpression::Literal(literal) => match literal {
                CsilLiteralValue::Integer(_) => "i64".to_string(),
                CsilLiteralValue::Float(_) => "f64".to_string(),
                CsilLiteralValue::Text(_) => "String".to_string(),
                CsilLiteralValue::Bool(_) => "bool".to_string(),
                CsilLiteralValue::Bytes(_) => "Vec<u8>".to_string(),
                CsilLiteralValue::Null => "()".to_string(),
                CsilLiteralValue::Array(_) => "Vec<serde_json::Value>".to_string(),
            },
            _ => {
                self.warnings.push(GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: format!("Unsupported type expression: {type_expr:?}"),
                    location: None,
                    suggestion: Some("Consider using basic CDDL types".to_string()),
                });
                "serde_json::Value".to_string()
            }
        };

        // Apply occurrence modifiers
        let final_type = match occurrence {
            Some(CsilOccurrence::Optional) => format!("Option<{base_type}>"),
            _ => base_type,
        };

        Ok(final_type)
    }

    fn map_builtin_type(&mut self, name: &str) -> String {
        match name {
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "Vec<u8>".to_string(),
            "bool" => "bool".to_string(),
            "int" => "i64".to_string(),
            "uint" => "u64".to_string(),
            // A negative integer (CBOR major type 1); the codec already routes
            // `nint` through the signed-integer path, so it shares `i64`.
            "nint" => "i64".to_string(),
            "float" | "float16" | "float32" | "float64" => "f64".to_string(),
            // A UTC-typed instant; the tag-0 RFC3339 wire form is the codec's job.
            "timestamp" => "chrono::DateTime<chrono::Utc>".to_string(),
            // The injected `CsilDecimal` holds the exact tag-4 value by default;
            // `decimal_mapping = "library"` swaps in `rust_decimal::Decimal`.
            "decimal" => match self.decimal_mapping {
                DecimalMapping::Csil => "CsilDecimal".to_string(),
                DecimalMapping::Library => "rust_decimal::Decimal".to_string(),
            },
            "null" => "()".to_string(),
            // `any` is carried as the codec's own CBOR value tree so it round-trips
            // losslessly without a serde dependency.
            "any" => "crate::codec::CsilCborValue".to_string(),
            _ => {
                self.warnings.push(GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: format!("Unknown builtin type '{name}', using serde_json::Value"),
                    location: None,
                    suggestion: None,
                });
                "serde_json::Value".to_string()
            }
        }
    }

    /// Emit `impl Type { pub fn validate(&self) -> Result<(), ValidationError> }`
    /// enforcing every runtime-meaningful constraint on the struct's fields, from
    /// both the `Constrained` control-operator system and the `@`-annotation
    /// `ValidationConstraint` system. Returns `None` (and emits no impl) when no
    /// field carries an enforceable constraint, so unconstrained types stay clean.
    fn generate_validate_impl(
        &mut self,
        name: &str,
        group: &CsilGroupExpression,
    ) -> Option<String> {
        let mut blocks = String::new();

        for entry in &group.entries {
            let Some(field_name) = self.extract_field_name(&entry.key) else {
                continue;
            };

            let checks = self.field_checks(&field_name, entry);
            if checks.is_empty() {
                continue;
            }

            let is_optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
            if is_optional {
                blocks.push_str(&format!("        if let Some(v) = &self.{field_name} {{\n"));
                for c in &checks {
                    blocks.push_str(c);
                }
                blocks.push_str("        }\n");
            } else {
                blocks.push_str("        {\n");
                blocks.push_str(&format!("            let v = &self.{field_name};\n"));
                for c in &checks {
                    blocks.push_str(c);
                }
                blocks.push_str("        }\n");
            }
        }

        if blocks.is_empty() {
            return None;
        }

        self.needs_validation_error = true;

        let mut out = String::new();
        out.push_str(&format!("impl {name} {{\n"));
        out.push_str(
            "    /// Validate this value against the constraints declared in the CSIL spec.\n",
        );
        out.push_str("    pub fn validate(&self) -> Result<(), ValidationError> {\n");
        out.push_str(&blocks);
        out.push_str("        Ok(())\n");
        out.push_str("    }\n");
        out.push('}');
        Some(out)
    }

    /// All runtime guard statements for a single field, already indented for the
    /// body of a `let v = ...` / `if let Some(v) = ...` block. Length checks bind
    /// to string/bytes/collection shapes; numeric comparisons to numeric shapes.
    fn field_checks(&mut self, field_name: &str, entry: &CsilGroupEntry) -> Vec<String> {
        let mut checks = Vec::new();
        let base = Self::value_base(&entry.value_type);
        let len_shape = Self::is_len_shape(base);
        let string_shape = Self::is_string_shape(base);
        // Disambiguates the per-pattern `OnceLock` static name when one field carries
        // more than one regex, so two hoisted statics in the same block never collide.
        let mut regex_idx = 0usize;

        let control_ops: &[CsilControlOperator] = match &entry.value_type {
            CsilTypeExpression::Constrained { constraints, .. } => constraints,
            _ => &[],
        };

        for op in control_ops {
            match op {
                CsilControlOperator::Size(s) if len_shape => {
                    Self::push_size_check(&mut checks, field_name, s);
                }
                CsilControlOperator::Regex(pattern) if string_shape => {
                    self.uses_regex = true;
                    checks.push(Self::regex_check(field_name, pattern, regex_idx));
                    regex_idx += 1;
                }
                CsilControlOperator::GreaterEqual(v) => {
                    self.push_comparison(&mut checks, field_name, base, "<", v, "is below minimum");
                }
                CsilControlOperator::LessEqual(v) => {
                    self.push_comparison(&mut checks, field_name, base, ">", v, "is above maximum");
                }
                CsilControlOperator::GreaterThan(v) => {
                    self.push_comparison(&mut checks, field_name, base, "<=", v, "must be greater");
                }
                CsilControlOperator::LessThan(v) => {
                    self.push_comparison(&mut checks, field_name, base, ">=", v, "must be less");
                }
                CsilControlOperator::Equal(v) => {
                    self.push_comparison(
                        &mut checks,
                        field_name,
                        base,
                        "!=",
                        v,
                        "must equal bound",
                    );
                }
                CsilControlOperator::NotEqual(v) => {
                    self.push_comparison(
                        &mut checks,
                        field_name,
                        base,
                        "==",
                        v,
                        "must not equal bound",
                    );
                }
                // Encoding-only / non-runtime operators are documented on the
                // field instead of enforced here; ignore them and never error.
                _ => {}
            }
        }

        for meta in &entry.metadata {
            let CsilFieldMetadata::Constraint(c) = meta else {
                continue;
            };
            match c {
                CsilValidationConstraint::MinLength(n) | CsilValidationConstraint::MinItems(n)
                    if len_shape =>
                {
                    if let Some(cond) = Self::len_min_cond(*n) {
                        checks.push(Self::len_check(
                            field_name,
                            &cond,
                            &format!("length is below minimum {n}"),
                        ));
                    }
                }
                CsilValidationConstraint::MaxLength(n) | CsilValidationConstraint::MaxItems(n)
                    if len_shape =>
                {
                    checks.push(Self::len_check(
                        field_name,
                        &Self::len_max_cond(*n),
                        &format!("length is above maximum {n}"),
                    ));
                }
                CsilValidationConstraint::MinValue(v) => {
                    self.push_comparison(&mut checks, field_name, base, "<", v, "is below minimum");
                }
                CsilValidationConstraint::MaxValue(v) => {
                    self.push_comparison(&mut checks, field_name, base, ">", v, "is above maximum");
                }
                _ => {}
            }
        }

        checks
    }

    fn push_size_check(checks: &mut Vec<String>, field: &str, size: &CsilSizeConstraint) {
        match size {
            CsilSizeConstraint::Exact(n) => checks.push(Self::len_check(
                field,
                &Self::len_ne_cond(*n),
                &format!("length must equal {n}"),
            )),
            CsilSizeConstraint::Range { min, max } => {
                let max_cond = Self::len_max_cond(*max);
                let cond = match Self::len_min_cond(*min) {
                    Some(min_cond) => format!("{min_cond} || {max_cond}"),
                    None => max_cond,
                };
                checks.push(Self::len_check(
                    field,
                    &cond,
                    &format!("length must be in {min}..={max}"),
                ));
            }
            CsilSizeConstraint::Min(n) => {
                if let Some(cond) = Self::len_min_cond(*n) {
                    checks.push(Self::len_check(
                        field,
                        &cond,
                        &format!("length is below minimum {n}"),
                    ));
                }
            }
            CsilSizeConstraint::Max(n) => checks.push(Self::len_check(
                field,
                &Self::len_max_cond(*n),
                &format!("length is above maximum {n}"),
            )),
        }
    }

    /// Condition for "length below minimum `n`", built to dodge two clippy lints
    /// rather than emit a bare comparison: `n == 1` is exactly the `is_empty()`
    /// case (`len_zero`), and `n == 0` can never hold for a `usize` length, so
    /// that bound is dropped entirely instead of emitting dead-code clippy would
    /// flag (`absurd_extreme_comparisons`). `None` means "no check to emit".
    fn len_min_cond(n: u64) -> Option<String> {
        match n {
            0 => None,
            1 => Some("v.is_empty()".to_string()),
            _ => Some(format!("v.len() < {n}usize")),
        }
    }

    /// Condition for "length above maximum `n`", dodging clippy's `len_zero`:
    /// `n == 0` is exactly the `!is_empty()` case.
    fn len_max_cond(n: u64) -> String {
        if n == 0 {
            "!v.is_empty()".to_string()
        } else {
            format!("v.len() > {n}usize")
        }
    }

    /// Condition for "length not exactly `n`", dodging clippy's `len_zero`:
    /// `n == 0` is exactly the `!is_empty()` case.
    fn len_ne_cond(n: u64) -> String {
        if n == 0 {
            "!v.is_empty()".to_string()
        } else {
            format!("v.len() != {n}usize")
        }
    }

    /// Emit a comparison guard for one bound, dispatching on the field's base type
    /// so the constraint is enforced (never silently dropped) for every comparable
    /// shape. Plain numbers widen to `f64`; `decimal` builds the bound through the
    /// in-memory decimal type's ordering; `timestamp` parses an RFC 3339 instant and
    /// compares chronologically. The bound is escaped into the generated source via
    /// `{:?}` so an embedded quote can never break out of the emitted string literal.
    fn push_comparison(
        &self,
        checks: &mut Vec<String>,
        field: &str,
        base: &CsilTypeExpression,
        fail_op: &str,
        bound: &CsilLiteralValue,
        msg: &str,
    ) {
        if Self::is_numeric_shape(base) {
            Self::push_numeric_check(checks, field, base, fail_op, bound, msg);
            return;
        }
        let typed = if Self::is_decimal_shape(base) {
            self.decimal_compare_check(field, fail_op, bound, msg)
        } else if Self::is_timestamp_shape(base) {
            Self::timestamp_compare_check(field, fail_op, bound, msg)
        } else {
            None
        };
        if let Some(check) = typed {
            checks.push(check);
        }
    }

    /// A `decimal` comparison guard. Core guarantees a `decimal` bound is always an
    /// integer literal or a well-formed decimal text literal (float bounds are
    /// rejected). Both are valid decimal strings, so an integer like `0` is rendered
    /// to `"0"` and parsed into the in-memory decimal type the same way a text bound
    /// is — never silently dropped. The parsed bound compares via that type's
    /// `Ord`/`PartialEq`. Any other literal kind yields `None`.
    fn decimal_compare_check(
        &self,
        field: &str,
        fail_op: &str,
        bound: &CsilLiteralValue,
        msg: &str,
    ) -> Option<String> {
        let text = match bound {
            CsilLiteralValue::Text(t) => t.clone(),
            CsilLiteralValue::Integer(i) => i.to_string(),
            _ => return None,
        };
        // A `from_str(...)` receiver is a plain call, so its lone `.map_err` hangs
        // off it with the struct literal overflowing; the `library` mapping's
        // literal-receiver chain (`"0".parse::<..>().map_err(..)`) is two links, which
        // rustfmt always breaks one per line.
        let binding = match self.decimal_mapping {
            DecimalMapping::Csil => format!(
                "                let bound = CsilDecimal::from_str({text:?}).map_err(|e| ValidationError {{\n\
                 \x20                   field: {field:?}.to_string(),\n\
                 \x20                   message: format!(\"invalid decimal bound: {{e}}\"),\n\
                 \x20               }})?;\n"
            ),
            // At this nesting rustfmt drops a short literal receiver (probed: up
            // to the width of `let bound = `) to its own line after the `=`,
            // pushing the chain a level deeper; a longer receiver stays inline.
            DecimalMapping::Library if text.len() + 2 <= 12 => format!(
                "                let bound =\n\
                 \x20                   {text:?}\n\
                 \x20                       .parse::<rust_decimal::Decimal>()\n\
                 \x20                       .map_err(|e| ValidationError {{\n\
                 \x20                           field: {field:?}.to_string(),\n\
                 \x20                           message: format!(\"invalid decimal bound: {{e}}\"),\n\
                 \x20                       }})?;\n"
            ),
            DecimalMapping::Library => format!(
                "                let bound = {text:?}\n\
                 \x20                   .parse::<rust_decimal::Decimal>()\n\
                 \x20                   .map_err(|e| ValidationError {{\n\
                 \x20                       field: {field:?}.to_string(),\n\
                 \x20                       message: format!(\"invalid decimal bound: {{e}}\"),\n\
                 \x20                   }})?;\n"
            ),
        };
        Some(format!(
            "            {{\n\
             {binding}\
             \x20               if *v {fail_op} bound {{\n\
             \x20                   return Err(ValidationError {{\n\
             \x20                       field: {field:?}.to_string(),\n\
             \x20                       message: {msg:?}.to_string(),\n\
             \x20                   }});\n\
             \x20               }}\n\
             \x20           }}\n"
        ))
    }

    /// A `timestamp` comparison guard. The bound is an RFC 3339 TEXT literal; it is
    /// parsed into `chrono::DateTime<chrono::Utc>` and compared chronologically. A
    /// non-text bound yields `None` so nothing broken is emitted.
    fn timestamp_compare_check(
        field: &str,
        fail_op: &str,
        bound: &CsilLiteralValue,
        msg: &str,
    ) -> Option<String> {
        let CsilLiteralValue::Text(text) = bound else {
            return None;
        };
        Some(format!(
            "            {{\n\
             \x20               let bound = chrono::DateTime::parse_from_rfc3339({text:?})\n\
             \x20                   .map_err(|e| ValidationError {{\n\
             \x20                       field: {field:?}.to_string(),\n\
             \x20                       message: format!(\"invalid timestamp bound: {{e}}\"),\n\
             \x20                   }})?\n\
             \x20                   .with_timezone(&chrono::Utc);\n\
             \x20               if *v {fail_op} bound {{\n\
             \x20                   return Err(ValidationError {{\n\
             \x20                       field: {field:?}.to_string(),\n\
             \x20                       message: {msg:?}.to_string(),\n\
             \x20                   }});\n\
             \x20               }}\n\
             \x20           }}\n"
        ))
    }

    /// A numeric comparison guard. Integer fields (`int`/`uint`) with an integer
    /// bound compare in their native width, so values past 2^53 keep full precision
    /// instead of being mangled by an `f64` round-trip that could wrongly accept or
    /// reject a value near the boundary. The bound is emitted with no type suffix so
    /// it infers the field's own type (`i64` or `u64`). Genuinely floating
    /// comparisons (a `float*` field, or a float bound) still widen to `f64`.
    fn push_numeric_check(
        checks: &mut Vec<String>,
        field: &str,
        base: &CsilTypeExpression,
        fail_op: &str,
        bound: &CsilLiteralValue,
        msg: &str,
    ) {
        if Self::is_integer_shape(base)
            && let CsilLiteralValue::Integer(i) = bound
        {
            let cond = format!("*v {fail_op} {i}");
            checks.push(Self::len_check(field, &cond, msg));
            return;
        }
        let Some(rendered) = Self::literal_as_f64(bound) else {
            return;
        };
        // Every `float*` builtin already maps to Rust `f64` (see
        // `map_builtin_type`), so `*v` is already an `f64` there — casting it to
        // `f64` again is a clippy `unnecessary_cast`. Only an integer field
        // reaching this branch (an integer field with a *float* bound, since an
        // integer bound already returned above) genuinely needs the widening cast.
        let lhs = if Self::is_float_shape(base) {
            "*v".to_string()
        } else {
            "(*v as f64)".to_string()
        };
        let cond = format!("{lhs} {fail_op} {rendered}");
        checks.push(Self::len_check(field, &cond, msg));
    }

    fn is_integer_shape(base: &CsilTypeExpression) -> bool {
        matches!(base, CsilTypeExpression::Builtin(n) if matches!(n.as_str(), "int" | "uint"))
    }

    fn is_float_shape(base: &CsilTypeExpression) -> bool {
        matches!(
            base,
            CsilTypeExpression::Builtin(n)
                if matches!(n.as_str(), "float" | "float16" | "float32" | "float64")
        )
    }

    /// Render a literal as an `f64` literal token usable on the right side of a
    /// comparison. Non-numeric literals yield `None` so the constraint is skipped.
    fn literal_as_f64(value: &CsilLiteralValue) -> Option<String> {
        match value {
            CsilLiteralValue::Integer(i) => Some(format!("{i}f64")),
            CsilLiteralValue::Float(f) => Some(format!("{f:?}")),
            _ => None,
        }
    }

    fn len_check(field: &str, cond: &str, msg: &str) -> String {
        format!(
            "            if {cond} {{\n\
             \x20               return Err(ValidationError {{\n\
             \x20                   field: \"{field}\".to_string(),\n\
             \x20                   message: \"{msg}\".to_string(),\n\
             \x20               }});\n\
             \x20           }}\n"
        )
    }

    /// A regex guard whose `regex::Regex` is compiled once via a `OnceLock` static
    /// rather than rebuilt on every `validate()` call. The pattern is a spec
    /// constant, so an invalid one still surfaces as a `ValidationError` (not a
    /// panic) by caching the `Result` and re-borrowing it on each call.
    fn regex_check(field: &str, pattern: &str, idx: usize) -> String {
        let static_name = format!("RE_{}_{idx}", field.to_ascii_uppercase());
        // The `get_or_init` line follows rustfmt's own fallback ladder for an
        // overlong `let`: break after `=`, then break the method chain, then give
        // the closure a block body, then stack the call's argument — and when even
        // that cannot fit (the pattern literal itself is too wide) rustfmt keeps
        // the original line untouched, so the one-liner is the give-up form too.
        let pat = format!("{pattern:?}");
        let init_stmt = {
            let one_line = format!(
                "                let re = {static_name}.get_or_init(|| regex::Regex::new({pat}));\n"
            );
            let after_eq = format!(
                "                    {static_name}.get_or_init(|| regex::Regex::new({pat}));"
            );
            let chain_elem =
                format!("                    .get_or_init(|| regex::Regex::new({pat}));");
            let block_call = format!("                    regex::Regex::new({pat})");
            let stacked_arg = format!("                        {pat},");
            if one_line.trim_end().len() <= 100 {
                one_line
            } else if after_eq.len() <= 100 {
                format!("                let re =\n{after_eq}\n")
            } else if chain_elem.len() <= 100 {
                format!("                let re = {static_name}\n{chain_elem}\n")
            } else if block_call.len() <= 100 {
                format!(
                    "                let re = {static_name}.get_or_init(|| {{\n{block_call}\n                }});\n"
                )
            } else if stacked_arg.len() <= 100 {
                format!(
                    "                let re = {static_name}.get_or_init(|| {{\n\
                     \x20                   regex::Regex::new(\n{stacked_arg}\n\
                     \x20                   )\n\
                     \x20               }});\n"
                )
            } else {
                one_line
            }
        };
        format!(
            "            {{\n\
             \x20               static {static_name}: std::sync::OnceLock<Result<regex::Regex, regex::Error>> =\n\
             \x20                   std::sync::OnceLock::new();\n\
             {init_stmt}\
             \x20               let re = re.as_ref().map_err(|e| ValidationError {{\n\
             \x20                   field: \"{field}\".to_string(),\n\
             \x20                   message: format!(\"invalid regex: {{e}}\"),\n\
             \x20               }})?;\n\
             \x20               if !re.is_match(v) {{\n\
             \x20                   return Err(ValidationError {{\n\
             \x20                       field: \"{field}\".to_string(),\n\
             \x20                       message: \"value does not match required pattern\".to_string(),\n\
             \x20                   }});\n\
             \x20               }}\n\
             \x20           }}\n"
        )
    }

    /// Unwrap any `Constrained` wrapper to reach the underlying type whose Rust
    /// shape (string/collection vs. numeric) decides which checks make sense.
    fn value_base(expr: &CsilTypeExpression) -> &CsilTypeExpression {
        match expr {
            CsilTypeExpression::Constrained { base_type, .. } => Self::value_base(base_type),
            other => other,
        }
    }

    fn is_len_shape(base: &CsilTypeExpression) -> bool {
        match base {
            CsilTypeExpression::Array { .. } | CsilTypeExpression::Map { .. } => true,
            CsilTypeExpression::Builtin(n) => {
                matches!(n.as_str(), "text" | "tstr" | "bytes" | "bstr")
            }
            _ => false,
        }
    }

    fn is_string_shape(base: &CsilTypeExpression) -> bool {
        matches!(base, CsilTypeExpression::Builtin(n) if matches!(n.as_str(), "text" | "tstr"))
    }

    fn is_numeric_shape(base: &CsilTypeExpression) -> bool {
        matches!(
            base,
            CsilTypeExpression::Builtin(n)
                if matches!(
                    n.as_str(),
                    "int" | "uint" | "float" | "float16" | "float32" | "float64"
                )
        )
    }

    fn is_decimal_shape(base: &CsilTypeExpression) -> bool {
        matches!(base, CsilTypeExpression::Builtin(n) if n == "decimal")
    }

    fn is_timestamp_shape(base: &CsilTypeExpression) -> bool {
        matches!(base, CsilTypeExpression::Builtin(n) if n == "timestamp")
    }

    /// Whether any rule in the spec references the named builtin anywhere in its
    /// type tree (group fields, arrays, maps, choices, constrained bases, service
    /// operation signatures). Drives both helper injection and the dep note.
    fn spec_uses_builtin(&self, target: &str) -> bool {
        self.spec.rules.iter().any(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(g) | CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => g
                .entries
                .iter()
                .any(|e| Self::type_mentions_builtin(&e.value_type, target)),
            CsilRuleType::TypeDef(t) => Self::type_mentions_builtin(t, target),
            CsilRuleType::TypeChoice(choices) => choices
                .iter()
                .any(|c| Self::type_mentions_builtin(c, target)),
            CsilRuleType::GroupChoice(groups) => groups.iter().any(|g| {
                g.entries
                    .iter()
                    .any(|e| Self::type_mentions_builtin(&e.value_type, target))
            }),
            CsilRuleType::ServiceDef(def) => def.operations.iter().any(|op| {
                Self::type_mentions_builtin(&op.input_type, target)
                    || Self::type_mentions_builtin(&op.output_type, target)
            }),
        })
    }

    fn type_mentions_builtin(expr: &CsilTypeExpression, target: &str) -> bool {
        match expr {
            CsilTypeExpression::Builtin(n) => n == target,
            CsilTypeExpression::Array { element_type, .. } => {
                Self::type_mentions_builtin(element_type, target)
            }
            CsilTypeExpression::Map { key, value, .. } => {
                Self::type_mentions_builtin(key, target)
                    || Self::type_mentions_builtin(value, target)
            }
            CsilTypeExpression::Group(g) | CsilTypeExpression::Tuple(g) => g
                .entries
                .iter()
                .any(|e| Self::type_mentions_builtin(&e.value_type, target)),
            CsilTypeExpression::Choice(choices) => choices
                .iter()
                .any(|c| Self::type_mentions_builtin(c, target)),
            CsilTypeExpression::Constrained { base_type, .. } => {
                Self::type_mentions_builtin(base_type, target)
            }
            _ => false,
        }
    }

    fn should_derive_partial_eq(&self, _group: &CsilGroupExpression) -> bool {
        // For now, always derive PartialEq for structs
        true
    }

    /// A pure function of `s` (no generator state), so it is callable from
    /// `hoist_inline` inside `new()` before `self` exists.
    fn to_snake_case(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let mut result = String::new();

        for (i, &ch) in chars.iter().enumerate() {
            if ch == '-' || ch == '_' {
                result.push('_');
                continue;
            }

            // Insert a boundary only at real word starts so acronym runs stay
            // intact: `GetTaskStateByID` -> `get_task_state_by_id`, not
            // `..._by_i_d`. A boundary is a lower/digit->Upper transition, or the
            // tail of an acronym handing off to a new word (Upper->Upper-then-lower).
            if ch.is_ascii_uppercase() && !result.is_empty() && !result.ends_with('_') {
                let prev = chars[i - 1];
                let next_is_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
                if prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase() && next_is_lower)
                {
                    result.push('_');
                }
            }

            result.push(ch.to_ascii_lowercase());
        }

        result
    }
}

/// Reduce an operation's output to its success type by dropping a top-level
/// `ServiceError` member of a `Res / ServiceError` union — that error half is the
/// `Result::Err` channel, not part of the returned value. Non-union outputs and
/// unions without a `ServiceError` member pass through unchanged.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !is_service_error(c))
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

fn is_service_error(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Reference(name) if name == "ServiceError")
}

/// Whether an operation's input is the empty/`null` payload used by push-style ops
/// (`op: <- Event` / `op: -> Event`). Such ops carry no request value, so the
/// generated signature must omit the request parameter rather than emit a bogus
/// `()` argument.
fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(n) if n == "null")
        || matches!(
            type_expr,
            CsilTypeExpression::Literal(CsilLiteralValue::Null)
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::*;
    use std::collections::HashMap;

    fn create_test_input() -> WasmGeneratorInput {
        let metadata = GeneratorMetadata {
            name: "rust-code-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test Rust generator".to_string(),
            target: "rust".to_string(),
            capabilities: vec![
                GeneratorCapability::BasicTypes,
                GeneratorCapability::Services,
            ],
            author: None,
            homepage: None,
        };

        let config = GeneratorConfig {
            target: "rust".to_string(),
            output_dir: "/tmp/output".to_string(),
            options: HashMap::new(),
        };

        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Description(
                                "User's name".to_string(),
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("email".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::SendOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 2,
        };

        WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        }
    }

    #[test]
    fn test_struct_generation() {
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);

        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct User"));
        assert!(types_content.contains("pub name: String"));
        assert!(types_content.contains("pub email: Option<String>"));
        // The payload wire is owned by the generated codec, so the type derives no
        // serde traits and carries no serde attribute.
        assert!(types_content.contains("#[derive(Debug, Clone, PartialEq)]"));
        assert!(!types_content.contains("Serialize"));
        assert!(!types_content.contains("#[serde"));
    }

    #[test]
    fn keyword_field_names_are_escaped() {
        // A CSIL field named after a Rust keyword must become a valid identifier; most
        // take the raw form, the four `r#` forbids take a trailing underscore, and a
        // non-keyword is untouched.
        assert_eq!(RustCodeGenerator::escape_rust_ident("type"), "r#type");
        assert_eq!(RustCodeGenerator::escape_rust_ident("match"), "r#match");
        assert_eq!(RustCodeGenerator::escape_rust_ident("self"), "self_");
        assert_eq!(RustCodeGenerator::escape_rust_ident("crate"), "crate_");
        assert_eq!(RustCodeGenerator::escape_rust_ident("house_id"), "house_id");
    }

    #[test]
    fn keyword_field_emits_raw_identifier_in_struct() {
        use csilgen_common::{CsilGroupEntry, CsilGroupExpression, CsilGroupKey};
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);
        let group = CsilGroupExpression {
            entries: vec![CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("type".to_string())),
                value_type: CsilTypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![],
                doc_comments: vec![],
            }],
        };
        let out = generator.generate_struct("Node", &group).unwrap();
        assert!(out.contains("pub r#type: String"), "got {out}");
    }

    #[test]
    fn test_type_mapping() {
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);

        assert_eq!(generator.map_builtin_type("text"), "String");
        assert_eq!(generator.map_builtin_type("int"), "i64");
        assert_eq!(generator.map_builtin_type("bool"), "bool");
        assert_eq!(generator.map_builtin_type("bytes"), "Vec<u8>");
    }

    #[test]
    fn test_snake_case_conversion() {
        assert_eq!(RustCodeGenerator::to_snake_case("CamelCase"), "camel_case");
        // Acronym runs stay intact: the boundary lands where a new word starts.
        assert_eq!(
            RustCodeGenerator::to_snake_case("HTTPResponse"),
            "http_response"
        );
        assert_eq!(
            RustCodeGenerator::to_snake_case("GetTaskStateByID"),
            "get_task_state_by_id"
        );
        assert_eq!(RustCodeGenerator::to_snake_case("simple"), "simple");
        assert_eq!(
            RustCodeGenerator::to_snake_case("create-entry"),
            "create_entry"
        );
        assert_eq!(
            RustCodeGenerator::to_snake_case("MyService-operation"),
            "my_service_operation"
        );
        assert_eq!(RustCodeGenerator::to_snake_case("a--b"), "a__b");
    }

    #[test]
    fn test_service_generation_with_service() {
        let mut input = create_test_input();

        // Add a service to the spec
        input.csil_spec.rules.push(CsilRule {
            name: "UserService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "create_user".to_string(),
                    input_type: CsilTypeExpression::Reference("User".to_string()),
                    output_type: CsilTypeExpression::Reference("User".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 5,
                        column: 4,
                        offset: 100,
                    },
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 4,
                column: 1,
                offset: 80,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services_content = generator.generate_services().unwrap();

        assert!(services_content.contains("pub struct ServiceError {"));
        assert!(services_content.contains("pub code: i32"));
        assert!(services_content.contains("pub message: String"));
        assert!(services_content.contains("impl std::fmt::Display for ServiceError"));
        assert!(services_content.contains("impl std::error::Error for ServiceError"));
        assert!(services_content.contains("pub trait UserService"));
        assert!(services_content.contains("type Context;"));
        assert!(services_content.contains(
            "fn create_user(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>;"
        ));
    }

    /// An optional `bytes` field carries three distinct states — absent,
    /// present-and-empty, present-and-non-empty — and the codec must decide presence by
    /// whether the value is set, never by whether it is non-empty (cbor-wire-contract.md
    /// "Optional fields"). An `is_empty()` guard would collapse present-empty into absent
    /// and silently lose a caller's "replace this with nothing".
    #[test]
    fn optional_bytes_encodes_on_presence_not_emptiness() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "UpdateRequest".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("id".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("payload".to_string())),
                        value_type: CsilTypeExpression::Builtin("bytes".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];
        input.csil_spec.service_count = 0;

        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().expect("generation ok");
        let file = |suffix: &str| {
            files
                .iter()
                .find(|f| f.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("no file ending in {suffix}"))
                .content
                .clone()
        };
        let types = file("types.rs");
        let codec = file("codec.gen.rs");

        // `Option` distinguishes None (absent) from `Some(vec![])` (present-and-empty).
        assert!(
            types.contains("pub payload: Option<Vec<u8>>"),
            "optional bytes needs a presence-carrying type:\n{types}"
        );
        // Encode gates on presence (`if let Some`), not on emptiness.
        assert!(
            codec.contains("if let Some(csil_inner) = &csil_v.payload {"),
            "encode must gate on presence, not emptiness:\n{codec}"
        );
        assert!(
            !codec.contains("!csil_v.payload.as_ref().is_some_and(|p| p.is_empty())"),
            "encode must not gate on emptiness:\n{codec}"
        );
        // Decode gates on the key being present in the map, so a present empty byte
        // string stays `Some` rather than collapsing to `None`.
        assert!(
            codec.contains("let payload = match cbor_map_get(csil_root, \"payload\")"),
            "decode must gate on key presence:\n{codec}"
        );
    }

    /// A single-field group record rule, for fixtures that need the request/response
    /// payloads to be real records the codec (and the typed client) can resolve.
    fn group_rule(name: &str, field: &str, builtin: &str) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare(field.to_string())),
                    value_type: CsilTypeExpression::Builtin(builtin.to_string()),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    fn make_unary_service_input(target: &str) -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.config.target = target.to_string();
        // The request/response payloads are real records so the typed-codec client
        // can resolve their `encode_`/`decode_` pair.
        input
            .csil_spec
            .rules
            .push(group_rule("SubmitTaskRequest", "queue", "text"));
        input
            .csil_spec
            .rules
            .push(group_rule("SubmitTaskResponse", "uuid", "text"));
        input.csil_spec.rules.push(CsilRule {
            name: "CorndogsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "SubmitTask".to_string(),
                    input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                    // `Res / ServiceError` union: the success type must be stripped.
                    output_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                        CsilTypeExpression::Reference("ServiceError".to_string()),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;
        input
    }

    #[test]
    fn test_typed_response_strips_service_error() {
        // The `rust` server trait must return the concrete success type, not the
        // untyped `serde_json::Value` the whole union would otherwise map to.
        let input = make_unary_service_input("rust");
        let mut generator = RustCodeGenerator::new(&input);
        let services = generator.generate_services().unwrap();
        assert!(services.contains(
            "fn submit_task(\n        &self,\n        ctx: &Self::Context,\n        input: SubmitTaskRequest,\n    ) -> Result<SubmitTaskResponse, ServiceError>;"
        ));
        assert!(!services.contains("serde_json::Value"));
    }

    #[test]
    fn test_rust_client_target_emits_typed_client() {
        let input = make_unary_service_input("rust-client");
        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();

        let client = files
            .iter()
            .find(|f| f.path == "client.rs")
            .expect("client.rs emitted");
        // The transport is now a dumb byte seam: bytes in, bytes out.
        assert!(client.content.contains(
            "fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError>;"
        ));
        assert!(client.content.contains("pub enum ClientError"));
        assert!(
            client
                .content
                .contains("pub struct CorndogsClient<T: Transport>")
        );
        assert!(client.content.contains(
            "pub fn submit_task(&self, req: SubmitTaskRequest) -> Result<SubmitTaskResponse, ClientError>"
        ));
        // The client encodes the request, calls over the byte seam, then decodes.
        // service/op are the verbatim CSIL names (docs/cbor-wire-contract.md
        // "RPC call naming") — no suffix stripping, no case change. The longer
        // "CorndogsService" pushes past the flat-arg width, so rustfmt's ladder
        // stacks each argument on its own line.
        assert!(client.content.contains(
            "let csil_resp = self.transport.call(\n            \"CorndogsService\",\n            \"SubmitTask\",\n            &encode_submit_task_request(&req),\n        )?;"
        ));
        assert!(
            client
                .content
                .contains("decode_submit_task_response(&csil_resp).map_err(|e| ClientError::Transport(e.to_string()))")
        );
        // The codec ships alongside the client.
        assert!(files.iter().any(|f| f.path == "codec.gen.rs"));
        // The server surface must not leak into the client target.
        assert!(!files.iter().any(|f| f.path == "services.rs"));
    }

    fn make_nonrecord_ops_input() -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.config.target = "rust-client".to_string();
        let pos = || CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        let r#ref = |n: &str| CsilTypeExpression::Reference(n.to_string());
        let alias = |name: &str, ty: CsilTypeExpression| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::TypeDef(ty),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let field = |name: &str, ty: CsilTypeExpression, optional: bool| CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: optional.then_some(CsilOccurrence::Optional),
            metadata: vec![],
            doc_comments: Vec::new(),
        };
        let record = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
            position: pos(),
            doc_comments: Vec::new(),
        };
        let op = |name: &str, input: CsilTypeExpression, output: CsilTypeExpression| {
            CsilServiceOperation {
                name: name.to_string(),
                input_type: input,
                output_type: output,
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: Vec::new(),
                wire_id: None,
            }
        };
        let text = || CsilTypeExpression::Builtin("text".to_string());
        let rules = &mut input.csil_spec.rules;
        rules.push(alias("MemberID", text()));
        rules.push(alias("TaskID", text()));
        rules.push(record(
            "Member",
            vec![
                field("id", r#ref("MemberID"), false),
                field("name", text(), false),
            ],
        ));
        rules.push(record(
            "ListMembersRequest",
            vec![field(
                "limit",
                CsilTypeExpression::Builtin("uint".to_string()),
                true,
            )],
        ));
        rules.push(CsilRule {
            name: "MemberService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    op("create-member", r#ref("Member"), r#ref("Member")),
                    op("get-member", r#ref("MemberID"), r#ref("Member")),
                    op(
                        "list-members",
                        r#ref("ListMembersRequest"),
                        CsilTypeExpression::Array {
                            element_type: Box::new(r#ref("Member")),
                            occurrence: Some(CsilOccurrence::ZeroOrMore),
                        },
                    ),
                    op(
                        "delete-task",
                        r#ref("TaskID"),
                        CsilTypeExpression::Builtin("bool".to_string()),
                    ),
                    op(
                        "member-names",
                        r#ref("ListMembersRequest"),
                        CsilTypeExpression::Map {
                            key: Box::new(text()),
                            value: Box::new(text()),
                            occurrence: None,
                        },
                    ),
                ],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;
        input
    }

    #[test]
    fn non_record_op_boundaries_get_client_methods_and_per_op_codecs() {
        let input = make_nonrecord_ops_input();
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let client = files
            .iter()
            .find(|f| f.path == "client.rs")
            .expect("client.rs emitted");
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");

        // Every op gets a method — scalar-id request, bare-array, scalar, and map
        // responses included, not only the record↔record op the old filter kept.
        assert!(
            client
                .content
                .contains("pub fn get_member(&self, req: MemberID) -> Result<Member, ClientError>")
        );
        assert!(client.content.contains(
            "pub fn list_members(&self, req: ListMembersRequest) -> Result<Vec<Member>, ClientError>"
        ));
        assert!(
            client
                .content
                .contains("pub fn delete_task(&self, req: TaskID) -> Result<bool, ClientError>")
        );
        // Too wide for one line, so it is emitted pre-wrapped the rustfmt way.
        assert!(client.content.contains(
            "pub fn member_names(\n        &self,\n        req: ListMembersRequest,\n    ) -> Result<std::collections::HashMap<String, String>, ClientError> {"
        ));
        // No op is dropped with a note anymore.
        assert!(!client.content.contains("handle it manually"));

        // The record boundary keeps its `encode_<t>`/`decode_<t>` wrapper byte-for-byte.
        assert!(client.content.contains("&encode_member(&req)"));
        // Non-record boundaries ride the op's per-op helpers.
        assert!(
            client
                .content
                .contains("&encode_member_get_member_request(&req)")
        );
        assert!(
            client
                .content
                .contains("decode_member_list_members_response(&csil_resp)")
        );
        assert!(
            client
                .content
                .contains("decode_member_delete_task_response(&csil_resp)")
        );
        assert!(
            client
                .content
                .contains("decode_member_member_names_response(&csil_resp)")
        );

        // The per-op helpers are exported from the codec, so a consumer-side server can
        // compose decode(request)/encode(response) for every op.
        assert!(codec.content.contains(
            "pub fn decode_member_get_member_request(csil_data: &[u8]) -> Result<MemberID, CsilCborError>"
        ));
        assert!(codec.content.contains(
            "pub fn encode_member_list_members_response(csil_v: &Vec<Member>) -> Vec<u8>"
        ));
        assert!(
            codec
                .content
                .contains("pub fn encode_member_delete_task_response(csil_v: &bool) -> Vec<u8>")
        );
        assert!(codec.content.contains(
            "pub fn encode_member_member_names_response(\n    csil_v: &std::collections::HashMap<String, String>,\n) -> Vec<u8> {"
        ));
        // The record op needs no per-op helper (its record codec already covers it).
        assert!(
            !codec
                .content
                .contains("encode_member_create_member_request")
        );
    }

    #[test]
    fn async_twin_emitted_by_default_with_marked_symbols() {
        // Default (`both`): the sync client is unchanged and an async twin rides
        // alongside it in a separate file with `Async`-marked public symbols.
        let input = make_unary_service_input("rust-client");
        let files = RustCodeGenerator::new(&input).generate().unwrap();

        let sync = files
            .iter()
            .find(|f| f.path == "client.rs")
            .expect("sync client.rs emitted");
        // The sync client keeps the canonical names and stays blocking.
        assert!(
            sync.content
                .contains("pub struct CorndogsClient<T: Transport>")
        );
        assert!(sync.content.contains(
            "pub fn submit_task(&self, req: SubmitTaskRequest) -> Result<SubmitTaskResponse, ClientError>"
        ));
        assert!(!sync.content.contains("async"));

        let twin = files
            .iter()
            .find(|f| f.path == "client_async.rs")
            .expect("async twin client_async.rs emitted");
        // The twin's symbols all carry the `Async` marker so it coexists with the
        // sync client, and it reuses the sync module's `ClientError`.
        assert!(twin.content.contains("use super::client::ClientError;"));
        assert!(twin.content.contains("pub trait AsyncTransport"));
        assert!(twin.content.contains(
            "async fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError>;"
        ));
        assert!(
            twin.content
                .contains("pub struct CorndogsAsyncClient<T: AsyncTransport>")
        );
        // The one-line signature would be 101 columns, so it is emitted the way
        // rustfmt would wrap it: params one per line.
        assert!(twin.content.contains(
            "pub async fn submit_task(\n        &self,\n        req: SubmitTaskRequest,\n    ) -> Result<SubmitTaskResponse, ClientError> {"
        ));
        // The seam is awaited before its `?`. service/op are verbatim CSIL names;
        // the longer "CorndogsService" pushes past the flat-arg width so
        // rustfmt's ladder stacks each argument on its own line.
        assert!(twin.content.contains(
            ".call(\n                \"CorndogsService\",\n                \"SubmitTask\",\n                &encode_submit_task_request(&req),\n            )\n            .await?;"
        ));
        // The twin must not redefine the shared error type.
        assert!(!twin.content.contains("pub enum ClientError"));

        // The module root registers both clients so both re-export cleanly.
        let root = files
            .iter()
            .find(|f| f.path == "mod.rs")
            .expect("mod.rs emitted");
        assert!(root.content.contains("pub mod client;"));
        assert!(root.content.contains("pub mod client_async;"));
    }

    #[test]
    fn client_style_async_is_drop_in_at_canonical_path() {
        // `async`: the async client takes the canonical filename AND the canonical
        // symbol names — a drop-in a consumer swaps in by adding `.await`.
        let mut input = make_unary_service_input("rust-client");
        input.config.options.insert(
            "client_style".to_string(),
            serde_json::Value::String("async".to_string()),
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();

        assert!(!files.iter().any(|f| f.path == "client_async.rs"));
        let client = files
            .iter()
            .find(|f| f.path == "client.rs")
            .expect("client.rs emitted");
        // Canonical names, owns its own `ClientError`, async seam + methods.
        assert!(client.content.contains("pub enum ClientError"));
        assert!(client.content.contains("pub trait Transport"));
        assert!(client.content.contains(
            "async fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError>;"
        ));
        assert!(
            client
                .content
                .contains("pub struct CorndogsClient<T: Transport>")
        );
        assert!(client.content.contains(
            "pub async fn submit_task(\n        &self,\n        req: SubmitTaskRequest,\n    ) -> Result<SubmitTaskResponse, ClientError> {"
        ));
        assert!(client.content.contains(".await?;"));
        // No `Async`-marked symbols in the drop-in shape.
        assert!(!client.content.contains("AsyncClient"));
        assert!(!client.content.contains("AsyncTransport"));
    }

    #[test]
    fn client_style_sync_suppresses_the_twin() {
        // `sync`: today's output, byte-identical, with no twin.
        let mut input = make_unary_service_input("rust-client");
        input.config.options.insert(
            "client_style".to_string(),
            serde_json::Value::String("sync".to_string()),
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();

        assert!(!files.iter().any(|f| f.path == "client_async.rs"));
        let client = files
            .iter()
            .find(|f| f.path == "client.rs")
            .expect("client.rs emitted");
        assert!(
            client
                .content
                .contains("pub struct CorndogsClient<T: Transport>")
        );
        assert!(client.content.contains(
            "pub fn submit_task(&self, req: SubmitTaskRequest) -> Result<SubmitTaskResponse, ClientError>"
        ));
        assert!(!client.content.contains("async"));
    }

    #[test]
    fn client_style_invalid_value_is_rejected() {
        let mut input = make_unary_service_input("rust-client");
        input.config.options.insert(
            "client_style".to_string(),
            serde_json::Value::String("eventual".to_string()),
        );
        let err = RustCodeGenerator::new(&input)
            .generate()
            .expect_err("invalid client_style must fail generation");
        assert!(
            err.contains("client_style"),
            "error must mention the option: {err}"
        );
    }

    #[test]
    fn test_unknown_rust_subtarget_errors() {
        let input = make_unary_service_input("rust-bogus");
        let mut generator = RustCodeGenerator::new(&input);
        assert!(generator.generate().is_err());
    }

    #[test]
    fn test_rust_server_alias_and_typesonly() {
        // `rust-server` is an explicit alias for the base server surface.
        let input = make_unary_service_input("rust-server");
        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();
        assert!(files.iter().any(|f| f.path == "services.rs"));
        assert!(!files.iter().any(|f| f.path == "client.rs"));

        // `rust-typesonly` emits the types (and mod) but no service surface.
        let input = make_unary_service_input("rust-typesonly");
        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();
        assert!(files.iter().any(|f| f.path == "types.rs"));
        assert!(!files.iter().any(|f| f.path == "services.rs"));
        assert!(!files.iter().any(|f| f.path == "client.rs"));
    }

    #[test]
    fn package_mode_emits_both_client_and_server_surfaces() {
        // The genquickstart's RPC/Datagrams sections ride the client surface and its
        // Events section rides the server-side channel router, so a self-contained
        // package must carry both — for either requested target. Flat mode stays
        // single-surface (covered by test_rust_server_alias_and_typesonly).
        for target in ["rust-client", "rust"] {
            let mut input = make_unary_service_input(target);
            input
                .config
                .options
                .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
            let files = RustCodeGenerator::new(&input).generate().unwrap();
            assert!(
                files.iter().any(|f| f.path == "src/client.rs"),
                "{target}: package mode must emit the client surface"
            );
            assert!(
                files.iter().any(|f| f.path == "src/services.rs"),
                "{target}: package mode must emit the server/router surface"
            );
        }
    }

    #[test]
    fn test_spec_defined_service_error_not_duplicated() {
        // When the spec declares its own `ServiceError`, the generator must not
        // emit its hardcoded fallback (which would collide via `mod.rs`).
        let mut input = make_unary_service_input("rust");
        input.csil_spec.rules.push(CsilRule {
            name: "ServiceError".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("code".to_string())),
                    value_type: CsilTypeExpression::Builtin("uint".to_string()),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let services = generator.generate_services().unwrap();
        // The fallback struct (with `pub code: i32`) must be absent; the
        // spec's own definition lives in types.rs and is imported.
        assert!(!services.contains("pub struct ServiceError"));
        // The trait still references the type.
        assert!(services.contains("Result<SubmitTaskResponse, ServiceError>"));
    }

    #[test]
    fn test_success_type_helper() {
        let union = CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Reference("Res".to_string()),
            CsilTypeExpression::Reference("ServiceError".to_string()),
        ]);
        assert!(matches!(success_type(&union), CsilTypeExpression::Reference(n) if n == "Res"));
        let plain = CsilTypeExpression::Reference("Res".to_string());
        assert!(matches!(success_type(&plain), CsilTypeExpression::Reference(n) if n == "Res"));
    }

    /// Regression for the `examples/build-integration/npm-project/api.csil` shape:
    /// a hoisted op-boundary union (`delete-notification: NotificationID ->
    /// DeleteResponse / NotificationError`, neither arm named `ServiceError`)
    /// whose synthesized name (`NotificationAPI_delete_notification_response`) is
    /// long enough that the union encoder's per-arm value no longer fits
    /// rustfmt's single-line-in-braces budget and must render as a fully
    /// multi-line `CsilCborValue::Array(vec![...])`. This previously came out
    /// flat-in-braces because the fallback called `render_expr` at a reset column
    /// that let its own `max_width`-based shortcut re-flatten an element list the
    /// caller had already rejected on the narrower `fn_call_width` budget.
    #[test]
    fn union_arm_with_long_hoisted_name_forces_multiline_array() {
        let mut input = create_test_input();
        input.csil_spec.rules.clear();
        input.csil_spec.rules.push(CsilRule {
            name: "DeleteResponse".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("success".to_string())),
                    value_type: CsilTypeExpression::Builtin("bool".to_string()),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.rules.push(CsilRule {
            name: "NotificationError".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("code".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("message".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.rules.push(CsilRule {
            name: "NotificationAPI".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "delete-notification".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Reference("DeleteResponse".to_string()),
                        CsilTypeExpression::Reference("NotificationError".to_string()),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let _ = generator.generate_types().unwrap();
        let codec = generator.generate_codec().unwrap().unwrap();
        assert!(
            codec.contains(
                "NotificationAPI_delete_notification_response::Variant0(csil_x) => {\n            \
                 CsilCborValue::Array(vec![\n                CsilCborValue::Uint(0),\n                \
                 csil_enc_delete_response(csil_x),\n            ])\n        }\n"
            ),
            "long hoisted-choice union arm should render the array fully multi-line:\n{codec}"
        );
        assert!(
            !codec.contains(
                "CsilCborValue::Array(vec![CsilCborValue::Uint(0), csil_enc_delete_response(csil_x)])"
            ),
            "must not fall back to the flat single-line array inside the braces"
        );
    }

    #[test]
    fn test_service_with_hyphenated_operations() {
        let mut input = create_test_input();

        input.csil_spec.rules.push(CsilRule {
            name: "Guestbook".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    CsilServiceOperation {
                        name: "create-entry".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 2,
                            column: 4,
                            offset: 20,
                        },
                        doc_comments: Vec::new(),
                        wire_id: None,
                    },
                    CsilServiceOperation {
                        name: "list-entries".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 3,
                            column: 4,
                            offset: 40,
                        },
                        doc_comments: Vec::new(),
                        wire_id: None,
                    },
                ],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services_content = generator.generate_services().unwrap();

        assert!(services_content.contains(
            "fn create_entry(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>;"
        ));
        assert!(!services_content.contains("fn create-entry("));
        assert!(services_content.contains(
            "fn list_entries(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>;"
        ));
        // Unidirectional ops get a generic (request/response) doc when the CSIL
        // has no `;;;` doc comments on the operation itself.
        assert!(services_content.contains("/// create-entry (request/response)."));
        assert!(services_content.contains("/// list-entries (request/response)."));
    }

    fn service_with_directions(
        name: &str,
        ops: &[(&str, &str, &str, CsilServiceDirection)],
    ) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: ops
                    .iter()
                    .map(|(n, i, o, d)| CsilServiceOperation {
                        name: n.to_string(),
                        input_type: CsilTypeExpression::Reference(i.to_string()),
                        output_type: CsilTypeExpression::Reference(o.to_string()),
                        direction: d.clone(),
                        position: CsilPosition {
                            line: 1,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                        wire_id: None,
                    })
                    .collect(),
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn bidirectional_op_emits_inbound_trait_method_router_and_outbound_encoder() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(service_with_directions(
            "Match",
            &[
                (
                    "list-events",
                    "User",
                    "User",
                    CsilServiceDirection::Unidirectional,
                ),
                ("play", "User", "User", CsilServiceDirection::Bidirectional),
            ],
        ));
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services = generator.generate_services().unwrap();

        // The router/encoders ride the generated per-type codec directly (no serde
        // `Codec` trait), so the module pulls in the codec rather than defining one.
        assert!(
            services.contains("use super::codec::*;"),
            "channel service must import the per-type codec"
        );
        assert!(
            !services.contains("pub trait Codec"),
            "the serde-generic Codec trait must be gone"
        );

        // Unidirectional kept as request/response.
        assert!(services.contains(
            "fn list_events(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>;"
        ));

        // Bidirectional is a fire-and-forget inbound handler (no return value).
        assert!(services.contains(
            "fn play(&self, ctx: &Self::Context, msg: User) -> Result<(), ServiceError>;"
        ));

        // Router decodes the inbound bytes with the per-type codec and dispatches by
        // wire method name.
        assert!(services.contains("pub fn route_match_channel<H>"));
        assert!(services.contains("\"play\" => {"));
        assert!(services.contains("let msg = decode_user(bytes)"));
        assert!(services.contains("handlers.play(ctx, msg)"));

        // Outbound encoder for the bidirectional op, over the per-type codec. The
        // wire event name is the CSIL operation name verbatim, not PascalCased.
        assert!(services.contains("pub fn encode_match_play(msg: &User) -> (String, Vec<u8>)"));
        assert!(services.contains("(\"play\".to_string(), encode_user(msg))"));
    }

    #[test]
    fn reverse_op_emits_only_outbound_encoder_no_trait_method() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(service_with_directions(
            "Callbacks",
            &[("notify", "User", "User", CsilServiceDirection::Reverse)],
        ));
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services = generator.generate_services().unwrap();

        // Reverse has no server-side inbound — no trait method at all.
        assert!(
            !services.contains("fn notify("),
            "reverse must not emit a trait method"
        );

        // Router exists but its match must NOT include a Notify arm.
        assert!(services.contains("pub fn route_callbacks_channel"));
        let router_start = services.find("pub fn route_callbacks_channel").unwrap();
        let router_block = &services[router_start..];
        assert!(!router_block.contains("\"notify\" =>"));

        // The encoder for the reverse op (server pushes Output to the client).
        assert!(
            services.contains("pub fn encode_callbacks_notify(msg: &User) -> (String, Vec<u8>)")
        );
    }

    #[test]
    fn services_without_channel_ops_skip_codec_and_router() {
        // create_test_input has no service rules; create_test_input + add a
        // single unidirectional op should not pull in the channel scaffolding.
        let mut input = create_test_input();
        input.csil_spec.rules.push(service_with_directions(
            "Auth",
            &[(
                "login",
                "User",
                "User",
                CsilServiceDirection::Unidirectional,
            )],
        ));
        input.csil_spec.service_count = 1;

        let services = RustCodeGenerator::new(&input).generate_services().unwrap();
        assert!(!services.contains("pub trait Codec"));
        assert!(!services.contains("route_auth_channel"));
        assert!(!services.contains("encode_auth_login"));
    }

    #[test]
    fn test_module_root_filename_default() {
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();

        let root_file = files.iter().find(|f| f.path == "mod.rs");
        assert!(root_file.is_some());
    }

    #[test]
    fn test_module_root_filename_custom() {
        let mut input = create_test_input();
        input.config.options.insert(
            "module_root_filename".to_string(),
            serde_json::Value::String("lib.rs".to_string()),
        );

        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();

        let root_file = files.iter().find(|f| f.path == "lib.rs");
        assert!(root_file.is_some());
        assert!(files.iter().all(|f| f.path != "mod.rs"));
    }

    #[test]
    fn test_full_generation_workflow() {
        let input = create_test_input();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.files.is_empty());
        assert_eq!(output.stats.fields_with_metadata_count, 2);

        // Check that types.rs and lib.rs are generated
        let type_file = output.files.iter().find(|f| f.path == "types.rs");
        assert!(type_file.is_some());

        let mod_file = output.files.iter().find(|f| f.path == "mod.rs");
        assert!(mod_file.is_some());
    }

    #[test]
    fn test_error_handling() {
        let result = process_generation(std::ptr::null(), 0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::INVALID_INPUT);

        let invalid_json = b"not json";
        let result = process_generation(invalid_json.as_ptr(), invalid_json.len());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::SERIALIZATION_ERROR);
    }

    #[test]
    fn test_memory_management() {
        let size = 1024;
        let ptr = allocate(size);
        assert!(!ptr.is_null());

        deallocate(ptr, size);
        // Test passes if no crash occurs
    }

    #[test]
    fn test_enum_from_typedef_wrapping_choice() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "CheckValue".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("int".to_string()),
                CsilTypeExpression::Builtin("float".to_string()),
            ])),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content.contains("pub enum CheckValue"),
            "Choice wrapped in TypeDef should generate an enum, not a type alias"
        );
        assert!(types_content.contains("Variant0(String)"));
        assert!(types_content.contains("Variant1(i64)"));
        assert!(types_content.contains("Variant2(f64)"));
        // The enum no longer derives serde; the payload wire is the codec's job.
        assert!(!types_content.contains("#[serde"));
        assert!(
            !types_content.contains("pub type CheckValue = serde_json::Value"),
            "Should not fall back to serde_json::Value"
        );
    }

    #[test]
    fn test_struct_from_typedef_wrapping_group() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "CheckResult".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("result".to_string())),
                        value_type: CsilTypeExpression::Builtin("bool".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("entries".to_string())),
                        value_type: CsilTypeExpression::Reference("CheckEntries".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content.contains("pub struct CheckResult"),
            "Group wrapped in TypeDef should generate a struct, not a type alias"
        );
        assert!(types_content.contains("pub result: bool"));
        assert!(types_content.contains("pub entries: CheckEntries"));
        assert!(
            !types_content.contains("pub type CheckResult = serde_json::Value"),
            "Should not fall back to serde_json::Value"
        );
    }

    /// A `Widget` record exercising every inline-choice position the contract
    /// covers: a plain field, an array element, a map value, and a tuple element —
    /// plus a `.default`-suffixed trailing literal arm (the parser's
    /// last-arm-gets-the-control-operator quirk). `OtherThing` gives the mixed
    /// choices a non-literal arm that is a genuine `Reference`, not the `text`/
    /// `tstr` open-base idiom — Rust has no special-cased "open enum" shape (a
    /// named choice with a `text` base already lowers to a tagged-sum union; see
    /// `OrderStatus` in `examples/real-world-api/e-commerce-api.csil`), so an
    /// inline choice must land on the same union/enum split a same-shaped named
    /// choice would, not on some inline-only third shape.
    fn widget_hoisting_input() -> WasmGeneratorInput {
        let mut input = create_test_input();
        let synth_position = CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        };
        input.csil_spec.rules.push(CsilRule {
            name: "OtherThing".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("note".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: synth_position.clone(),
            doc_comments: Vec::new(),
        });
        input.csil_spec.rules.push(CsilRule {
            name: "Widget".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    // Plain field, inline mixed choice, optional: `Reference / literal`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("tag".to_string())),
                        value_type: CsilTypeExpression::Choice(vec![
                            CsilTypeExpression::Reference("OtherThing".to_string()),
                            CsilTypeExpression::Literal(CsilLiteralValue::Text(
                                "unset".to_string(),
                            )),
                        ]),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Plain field, inline all-literal choice, last arm `.default`-wrapped:
                    // `"low" / "high" .default "normal"`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("mode".to_string())),
                        value_type: CsilTypeExpression::Choice(vec![
                            CsilTypeExpression::Literal(CsilLiteralValue::Text("low".to_string())),
                            CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Literal(
                                    CsilLiteralValue::Text("high".to_string()),
                                )),
                                constraints: vec![CsilControlOperator::Default(
                                    CsilLiteralValue::Text("normal".to_string()),
                                )],
                            },
                        ]),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Array element, inline mixed choice: `[* (int / "auto")]`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("codes".to_string())),
                        value_type: CsilTypeExpression::Array {
                            element_type: Box::new(CsilTypeExpression::Choice(vec![
                                CsilTypeExpression::Builtin("int".to_string()),
                                CsilTypeExpression::Literal(CsilLiteralValue::Text(
                                    "auto".to_string(),
                                )),
                            ])),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Map value, inline all-literal choice: `{* text => ("a" / "b")}`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("labels".to_string())),
                        value_type: CsilTypeExpression::Map {
                            key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            value: Box::new(CsilTypeExpression::Choice(vec![
                                CsilTypeExpression::Literal(CsilLiteralValue::Text(
                                    "a".to_string(),
                                )),
                                CsilTypeExpression::Literal(CsilLiteralValue::Text(
                                    "b".to_string(),
                                )),
                            ])),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Tuple element, inline mixed choice: `[(text / "x"), int]`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("pair".to_string())),
                        value_type: CsilTypeExpression::Tuple(CsilGroupExpression {
                            entries: vec![
                                CsilGroupEntry {
                                    key: None,
                                    value_type: CsilTypeExpression::Choice(vec![
                                        CsilTypeExpression::Builtin("text".to_string()),
                                        CsilTypeExpression::Literal(CsilLiteralValue::Text(
                                            "x".to_string(),
                                        )),
                                    ]),
                                    occurrence: None,
                                    metadata: vec![],
                                    doc_comments: Vec::new(),
                                },
                                CsilGroupEntry {
                                    key: None,
                                    value_type: CsilTypeExpression::Builtin("int".to_string()),
                                    occurrence: None,
                                    metadata: vec![],
                                    doc_comments: Vec::new(),
                                },
                            ],
                        }),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: synth_position,
            doc_comments: Vec::new(),
        });
        input
    }

    #[test]
    fn test_inline_choice_field_hoisted_to_named_union() {
        let input = widget_hoisting_input();
        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        // The field's type is the synthesized `<Record>_<field>` name, exactly as
        // if `Widget_tag` had been declared as its own named choice rule.
        assert!(
            types_content.contains("pub tag: Option<Widget_tag>"),
            "inline choice field should be hoisted to a synthesized named type:\n{types_content}"
        );
        assert!(types_content.contains("pub enum Widget_tag"));
        assert!(types_content.contains("Variant0(OtherThing)"));
        assert!(types_content.contains("Variant1(String)"));
        // No inline choice should ever fall back to the untyped escape hatch.
        assert!(!types_content.contains("serde_json::Value"));
    }

    #[test]
    fn test_inline_all_literal_choice_hoisted_to_named_enum() {
        let input = widget_hoisting_input();
        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub mode: Widget_mode"));
        assert!(types_content.contains("pub enum Widget_mode"));
        // A `.default`-wrapped literal arm still contributes a unit variant, not a
        // payload-carrying one — it stayed classified as a literal despite the
        // `Constrained` wrapper the parser attached to it.
        assert!(types_content.contains("Low,"));
        assert!(types_content.contains("High,"));
        assert!(!types_content.contains("Widget_mode::Variant"));
    }

    #[test]
    fn test_inline_choice_hoisted_in_array_map_tuple_positions() {
        let input = widget_hoisting_input();
        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content.contains("pub codes: Vec<Widget_codes_item>"),
            "array element inline choice should hoist:\n{types_content}"
        );
        assert!(types_content.contains("pub enum Widget_codes_item"));

        assert!(
            types_content
                .contains("pub labels: std::collections::HashMap<String, Widget_labels_value>"),
            "map value inline choice should hoist:\n{types_content}"
        );
        assert!(types_content.contains("pub enum Widget_labels_value"));
        assert!(types_content.contains("A,"));
        assert!(types_content.contains("B,"));

        assert!(
            types_content.contains("pub pair: (Widget_pair_0, i64)"),
            "tuple element inline choice should hoist:\n{types_content}"
        );
        assert!(types_content.contains("pub enum Widget_pair_0"));
    }

    #[test]
    fn test_inline_choice_codec_matches_named_choice_shape() {
        let input = widget_hoisting_input();
        let mut generator = RustCodeGenerator::new(&input);
        // `generate_types` must run first: it is what sets `type_definitions`, and
        // `generate_codec` is what actually needs to see the hoisted rules (it
        // reads `self.spec` independently, so the ordering only matters for
        // `type_definitions`-driven behavior elsewhere, not for correctness here).
        let _ = generator.generate_types().unwrap();
        let codec = generator.generate_codec().unwrap().unwrap();

        // The union field: encoding routes through the hoisted type's own codec
        // function, with the optional-field binding actually used (not the
        // `CsilCborValue::Null` stub the pre-fix fallback emitted).
        assert!(codec.contains("fn csil_enc_widget_tag(csil_v: &Widget_tag)"));
        assert!(codec.contains("fn csil_dec_widget_tag(csil_v: &CsilCborValue)"));
        assert!(codec.contains("if let Some(csil_inner) = &csil_v.tag {"));
        assert!(codec.contains("csil_enc_widget_tag(csil_inner)"));

        // The all-literal field: bare-literal wire (no `[variant_index, value]`
        // tagged-sum wrapper), decode dispatches by string match, and the
        // `.default`-wrapped "high" arm encodes its constant with no unused
        // binding.
        assert!(codec.contains("fn csil_enc_widget_mode(csil_v: &Widget_mode)"));
        assert!(codec.contains("Widget_mode::Low => cbor_text(\"low\"),"));
        assert!(codec.contains("Widget_mode::High => cbor_text(\"high\"),"));
        assert!(codec.contains("\"low\" => Ok(Widget_mode::Low)"));
        assert!(codec.contains("\"high\" => Ok(Widget_mode::High)"));
        assert!(codec.contains("unknown Widget_mode value"));

        // Nested positions: element/value/tuple-slot codecs exist under their
        // synthesized names.
        assert!(codec.contains("fn csil_enc_widget_codes_item(csil_v: &Widget_codes_item)"));
        assert!(codec.contains("fn csil_enc_widget_labels_value(csil_v: &Widget_labels_value)"));
        assert!(codec.contains("fn csil_enc_widget_pair_0(csil_v: &Widget_pair_0)"));
    }

    /// `Order.status: "pending" / "shipped" / 0 / 1` — a mixed text+int literal
    /// choice as a record field. Pins the shared `classify_choice` contract now
    /// flowing through this generator: `csilgen_common::classify_choice` classifies
    /// ANY all-literal vocabulary as an `Enum`, mixed kind or not, so this must
    /// hoist to a named unit-variant enum exactly like an all-text or all-int
    /// choice does — not fall through to the union path (the pre-fix
    /// `enum_literals` required a uniform text-only or int-only vocabulary and
    /// this mixed one failed both checks).
    fn mixed_kind_choice_input() -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "Order".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("status".to_string())),
                    value_type: CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("pending".to_string())),
                        CsilTypeExpression::Literal(CsilLiteralValue::Text("shipped".to_string())),
                        CsilTypeExpression::Literal(CsilLiteralValue::Integer(0)),
                        CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
                    ]),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];
        input.csil_spec.service_count = 0;
        input
    }

    #[test]
    fn mixed_kind_literal_choice_hoists_to_unit_variant_enum() {
        let input = mixed_kind_choice_input();
        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub status: Order_status"));
        assert!(
            types_content.contains("pub enum Order_status"),
            "mixed-kind literal choice must hoist to a named enum:\n{types_content}"
        );
        // Unit variants — one per literal, regardless of kind — never a payload
        // (`Variant0(String)`-shaped) arm, which is what the pre-fix union
        // fallback produced.
        assert!(types_content.contains("Pending,"));
        assert!(types_content.contains("Shipped,"));
        assert!(types_content.contains("V0,"));
        assert!(types_content.contains("V1,"));
        assert!(!types_content.contains("Order_status::Variant"));
        assert!(!types_content.contains("Variant0(String)"));
    }

    #[test]
    fn mixed_kind_literal_choice_codec_is_bare_wire_enum_not_tagged_union() {
        let input = mixed_kind_choice_input();
        let mut generator = RustCodeGenerator::new(&input);
        let _ = generator.generate_types().unwrap();
        let codec = generator.generate_codec().unwrap().unwrap();

        // Encode: bare literal per variant (via `rust_literal_cbor_expr`, kind-
        // appropriate per arm), never a `[variant_index, value]` tagged sum.
        assert!(codec.contains("fn csil_enc_order_status(csil_v: &Order_status)"));
        assert!(codec.contains("Order_status::Pending => cbor_text(\"pending\"),"));
        assert!(codec.contains("Order_status::Shipped => cbor_text(\"shipped\"),"));
        assert!(codec.contains("Order_status::V0 => cbor_int(0),"));
        assert!(codec.contains("Order_status::V1 => cbor_int(1),"));

        // Decode: neither the all-text (`cbor_as_text` + `.as_str()` match) nor the
        // all-int (`cbor_as_i64` + scalar match) shape applies to a mixed
        // vocabulary, so it matches directly on `csil_v` with a per-arm,
        // kind-appropriate guard — text arms compare the decoded CBOR value
        // directly, int arms route through `cbor_as_i64` so a non-negative literal
        // (decoded as `CsilCborValue::Uint`, not the `Int` an `Integer` literal's
        // own rendering would produce) still matches.
        assert!(codec.contains("fn csil_dec_order_status(csil_v: &CsilCborValue)"));
        assert!(
            codec.contains("_ if csil_v == &cbor_text(\"pending\") => Ok(Order_status::Pending),")
        );
        assert!(
            codec.contains("_ if csil_v == &cbor_text(\"shipped\") => Ok(Order_status::Shipped),")
        );
        assert!(
            codec.contains("_ if matches!(cbor_as_i64(csil_v), Ok(0)) => Ok(Order_status::V0),")
        );
        assert!(
            codec.contains("_ if matches!(cbor_as_i64(csil_v), Ok(1)) => Ok(Order_status::V1),")
        );
        // Out-of-vocabulary membership check: an unrecognized value of a declared
        // kind is rejected, not silently coerced.
        assert!(codec.contains("unknown Order_status value"));
        // No tagged-sum union shape leaked through for this field.
        assert!(!codec.contains("union expects a 2-element array"));
    }

    #[test]
    fn test_keyless_group_spread_emits_named_field() {
        // `r = { g, b: bool }` has a keyless entry that spreads group `g`. Rust
        // cannot inline another struct's fields, so the spread must surface as a
        // constructible field named after the referenced type rather than being
        // dropped (which would leave `r` missing the spread) or emitted with a
        // garbage/empty field name (which would not compile).
        let mut input = create_test_input();
        input.csil_spec.rules = vec![
            CsilRule {
                name: "g".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("x".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "r".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: None,
                            value_type: CsilTypeExpression::Reference("g".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("b".to_string())),
                            value_type: CsilTypeExpression::Builtin("bool".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            },
        ];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct r {"));
        assert!(
            types_content.contains("pub g: g,"),
            "group spread must surface as a named field, got:\n{types_content}"
        );
        assert!(types_content.contains("pub b: bool,"));
        // A dropped or unnamed spread would leave an empty/garbage field; guard
        // against both so the struct stays constructible.
        assert!(
            !types_content.contains("pub : ") && !types_content.contains("pub  :"),
            "no empty/garbage field name may be emitted, got:\n{types_content}"
        );
    }

    #[test]
    fn test_struct_with_optional_fields() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "HelloRequest".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("name".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct HelloRequest"));
        assert!(types_content.contains("pub name: Option<String>"));
        // No serde attribute: an optional field is just `Option<T>`, and the codec
        // omits it from the wire map when absent.
        assert!(!types_content.contains("#[serde"));
        assert!(
            !types_content.contains("default"),
            "optional text field must not emit serde `default`"
        );
    }

    #[test]
    fn test_struct_with_optional_bytes_field() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "DomainPublicKey".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("key_signature".to_string())),
                    value_type: CsilTypeExpression::Builtin("bytes".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        // The payload wire is owned by `codec.gen.rs`; the type field carries no
        // serde attribute. An optional bytes field is simply `Option<Vec<u8>>`, and
        // the generated codec omits it when absent / decodes a present one.
        assert!(types_content.contains("pub key_signature: Option<Vec<u8>>"));
        assert!(!types_content.contains("#[serde"));
        assert!(!types_content.contains("serde_bytes"));
    }

    #[test]
    fn optional_bytes_codec_omits_absent_and_uses_byte_string() {
        // The optional bytes field's codec guards on `Some`, pushes the verbatim
        // wire key, and routes through `cbor_bytes` (CBOR major type 2), never an
        // integer array.
        let input = single_field_spec(
            "DomainPublicKey",
            CsilTypeExpression::Builtin("bytes".to_string()),
            vec![],
            Some(CsilOccurrence::Optional),
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(
            codec
                .content
                .contains("if let Some(csil_inner) = &csil_v.field")
        );
        assert!(
            codec
                .content
                .contains("cbor_text(\"field\"), cbor_bytes(csil_inner)")
        );
        // The decoder leaves an absent optional as None.
        assert!(
            codec
                .content
                .contains("match cbor_map_get(csil_root, \"field\")")
        );
    }

    #[test]
    fn test_struct_with_receive_only_visibility() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "GuestbookEntry".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("id".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::ReceiveOnly,
                        )],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("created_at".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::ReceiveOnly,
                        )],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct GuestbookEntry"));
        assert!(types_content.contains("pub id: String"));
        assert!(types_content.contains("pub name: String"));
        assert!(types_content.contains("pub created_at: String"));
        // Field visibility no longer maps onto a serde attribute now that the payload
        // wire is owned by the generated codec; the type stays attribute-free.
        assert!(!types_content.contains("#[serde"));
    }

    #[test]
    fn test_map_type_still_works() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "CheckEntries".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Reference("CheckValue".to_string())),
                occurrence: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content
                .contains("pub type CheckEntries = std::collections::HashMap<String, CheckValue>;")
        );
    }

    /// Build a single-struct spec whose one field has the given type expression
    /// and metadata, for exercising type mapping and constraint emission.
    fn single_field_spec(
        type_name: &str,
        value_type: CsilTypeExpression,
        metadata: Vec<CsilFieldMetadata>,
        occurrence: Option<CsilOccurrence>,
    ) -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: type_name.to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("field".to_string())),
                    value_type,
                    occurrence,
                    metadata,
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];
        input
    }

    #[test]
    fn overlong_codec_signatures_wrap_like_rustfmt() {
        // The linkkeys regression: `SignedLocalRpCallbackPayload` pushes the
        // one-line `csil_enc_...` signature to 103 columns, which rustfmt wraps
        // one param per line — so the generator emits exactly that wrapped form
        // and a fresh `rustfmt --check` after `csilgen generate` stays diff-free.
        let input = single_field_spec(
            "SignedLocalRpCallbackPayload",
            CsilTypeExpression::Builtin("bytes".to_string()),
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(codec.content.contains(
            "fn csil_enc_signed_local_rp_callback_payload(\n\
             \x20   csil_v: &SignedLocalRpCallbackPayload,\n\
             ) -> CsilCborValue {"
        ));

        // A short record name keeps the one-line signature rustfmt would keep.
        let input = single_field_spec(
            "User",
            CsilTypeExpression::Builtin("bytes".to_string()),
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(
            codec
                .content
                .contains("fn csil_enc_user(csil_v: &User) -> CsilCborValue {")
        );
    }

    #[test]
    fn unsupported_field_decoder_wraps_like_rustfmt() {
        // The erroring fallback closure exceeds rustfmt's call width, so rustfmt
        // gives it a block body with the nested single-argument calls flattened;
        // the generator must emit that exact shape.
        let input = single_field_spec(
            "Wrapper",
            CsilTypeExpression::Reference("NotModeled".to_string()),
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(codec.content.contains(
            "        let csil_decode = |_csil_v| {\n\
             \x20           Err(CsilCborError(\n\
             \x20               \"csil cbor: unsupported field type\".to_string(),\n\
             \x20           ))\n\
             \x20       };"
        ));
    }

    #[test]
    fn wide_map_decoder_overflows_its_trailing_closure_like_rustfmt() {
        // map<text, any>: the decode call's argument list passes fn_call_width,
        // so rustfmt block-bodies the binding closure and overflows the trailing
        // `any` closure inside the call.
        let input = single_field_spec(
            "Wrapper",
            CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Builtin("any".to_string())),
                occurrence: None,
            },
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(codec.content.contains(
            "        let csil_decode = |csil_v| {\n\
             \x20           cbor_dec_map(csil_v, cbor_as_text, |csil_v: &CsilCborValue| {\n\
             \x20               Ok(csil_v.clone())\n\
             \x20           })\n\
             \x20       };"
        ));
    }

    #[test]
    fn validation_error_returns_are_emitted_vertically() {
        // Even the shortest `ValidationError` literal passes rustfmt's
        // struct_lit_width, so every guard return is emitted in the vertical
        // form rustfmt would produce.
        let input = single_field_spec(
            "Doc",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MaxLength(10),
            )],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains(
            "                return Err(ValidationError {\n\
             \x20                   field: \"field\".to_string(),\n\
             \x20                   message: \"length is above maximum 10\".to_string(),\n\
             \x20               });"
        ));
    }

    #[test]
    fn trait_method_signature_follows_rustfmt_ladder() {
        // One line through 99 columns; at exactly 100 rustfmt drops only the
        // return type to the next line; past that, params go one per line.
        let short = RustCodeGenerator::rust_trait_method(
            "get",
            &["input: serde_json::Value".to_string()],
            "Cart",
        );
        assert_eq!(
            short,
            "    fn get(&self, input: serde_json::Value) -> Result<Cart, ServiceError>;\n"
        );

        // 4 + len("fn get_cart(&self, ctx: &Self::Context, input: serde_json::Value) -> Result<Cart, ServiceError>;") == 100.
        let exactly_100 = RustCodeGenerator::rust_trait_method(
            "get_cart",
            &[
                "ctx: &Self::Context".to_string(),
                "input: serde_json::Value".to_string(),
            ],
            "Cart",
        );
        assert_eq!(
            exactly_100,
            "    fn get_cart(&self, ctx: &Self::Context, input: serde_json::Value)\n\
             \x20       -> Result<Cart, ServiceError>;\n"
        );

        let long = RustCodeGenerator::rust_trait_method(
            "get_cart_x",
            &[
                "ctx: &Self::Context".to_string(),
                "input: serde_json::Value".to_string(),
            ],
            "Cart",
        );
        assert_eq!(
            long,
            "    fn get_cart_x(\n\
             \x20       &self,\n\
             \x20       ctx: &Self::Context,\n\
             \x20       input: serde_json::Value,\n\
             \x20   ) -> Result<Cart, ServiceError>;\n"
        );
    }

    #[test]
    fn timestamp_maps_to_chrono_utc() {
        let input = single_field_spec(
            "Event",
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("pub field: chrono::DateTime<chrono::Utc>"));

        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        // `chrono` is now a plain in-memory dependency; no serde feature, no codec
        // library (the generated codec owns the tag-0 wire).
        assert!(root.content.contains("chrono = \"0.4\""));
        assert!(!root.content.contains("ciborium"));
    }

    #[test]
    fn timestamp_field_codec_emits_tag0_rfc3339() {
        let input = single_field_spec(
            "Event",
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();
        // The type field carries no serde attribute.
        assert!(!types.content.contains("#[serde"));

        // The tag-0 RFC3339 wire form lives in the generated codec, not the type.
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(codec.content.contains("csil_enc_timestamp"));
        assert!(codec.content.contains("CsilCborValue::Tag(0,"));
        assert!(codec.content.contains("to_rfc3339_opts"));
        // The field routes through the timestamp codec on both sides.
        assert!(codec.content.contains("csil_enc_timestamp(&csil_v.field)"));
    }

    #[test]
    fn optional_timestamp_field_codec_guards_and_keeps_type() {
        let input = single_field_spec(
            "Event",
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![],
            Some(CsilOccurrence::Optional),
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();
        assert!(
            types
                .content
                .contains("pub field: Option<chrono::DateTime<chrono::Utc>>")
        );
        assert!(!types.content.contains("#[serde"));

        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        // An optional timestamp is guarded on encode and routed through the codec.
        assert!(
            codec
                .content
                .contains("if let Some(csil_inner) = &csil_v.field")
        );
        assert!(codec.content.contains("csil_enc_timestamp(csil_inner)"));
    }

    #[test]
    fn library_decimal_field_codec_emits_tag4_and_maps_to_rust_decimal() {
        let mut input = single_field_spec(
            "Money",
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![],
            None,
        );
        input.config.options.insert(
            "decimal_mapping".to_string(),
            serde_json::Value::String("library".to_string()),
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();

        assert!(types.content.contains("pub field: rust_decimal::Decimal"));
        assert!(!types.content.contains("#[serde"));
        // No `CsilDecimal` helper in library mode.
        assert!(!types.content.contains("pub struct CsilDecimal"));

        // The tag-4 wire form lives in the codec, keyed off `rust_decimal::Decimal`.
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(
            codec
                .content
                .contains("fn csil_enc_decimal(d: &rust_decimal::Decimal)")
        );
        assert!(codec.content.contains("CsilCborValue::Tag(\n        4,"));

        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(root.content.contains("rust_decimal = \"1\""));
        assert!(!root.content.contains("ciborium"));
    }

    #[test]
    fn regex_check_is_hoisted_to_oncelock() {
        let input = single_field_spec(
            "Tag",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                constraints: vec![CsilControlOperator::Regex("^[a-z]+$".to_string())],
            },
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        // Compiled once into a static, not rebuilt on every call.
        assert!(types.contains(
            "static RE_FIELD_0: std::sync::OnceLock<Result<regex::Regex, regex::Error>>"
        ));
        assert!(types.contains("get_or_init(|| regex::Regex::new(\"^[a-z]+$\"))"));
        assert!(types.contains("if !re.is_match(v) {"));
    }

    #[test]
    fn decimal_csil_mode_injects_helper_only_when_used() {
        // Used: the CsilDecimal helper is emitted and the field maps to it.
        let input = single_field_spec(
            "Money",
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("pub field: CsilDecimal"));
        assert!(types.contains("pub struct CsilDecimal"));
        // The helper carries no serde impl now; its tag-4 wire form is the codec's.
        assert!(!types.contains("impl Serialize for CsilDecimal"));
        assert!(!types.contains("impl<'de> Deserialize<'de> for CsilDecimal"));
        assert!(!types.contains("#[serde"));
        // The value-string conversions remain (the lossless bridge to a decimal lib).
        assert!(types.contains("pub fn from_str"));
        assert!(types.contains("pub fn as_str"));

        // The tag-4 wire form lives in the codec, keyed off `CsilDecimal`.
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");
        assert!(
            codec
                .content
                .contains("fn csil_enc_decimal(d: &CsilDecimal)")
        );
        assert!(codec.content.contains("csil_enc_bigint(d.mantissa)"));
        // No CBOR library is assumed; the codec is self-contained.
        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(!root.content.contains("ciborium"));

        // Not used: no helper leaks into an unrelated spec.
        let plain = single_field_spec(
            "Plain",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![],
            None,
        );
        let mut gen2 = RustCodeGenerator::new(&plain);
        let plain_types = gen2.generate_types().unwrap();
        assert!(!plain_types.contains("pub struct CsilDecimal"));
    }

    #[test]
    fn decimal_library_mode_uses_rust_decimal_and_no_helper() {
        let mut input = single_field_spec(
            "Money",
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![],
            None,
        );
        input.config.options.insert(
            "decimal_mapping".to_string(),
            serde_json::Value::String("library".to_string()),
        );

        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();
        assert!(types.content.contains("pub field: rust_decimal::Decimal"));
        assert!(!types.content.contains("pub struct CsilDecimal"));

        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(root.content.contains("rust_decimal = \"1\""));
    }

    #[test]
    fn decimal_mapping_rejects_unknown_value() {
        let mut input = single_field_spec(
            "Money",
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![],
            None,
        );
        input.config.options.insert(
            "decimal_mapping".to_string(),
            serde_json::Value::String("bignum".to_string()),
        );
        assert!(RustCodeGenerator::new(&input).generate().is_err());
    }

    #[test]
    fn constrained_size_emits_length_validation() {
        // `field: text .size (3..10)` -> a validate() with min/max length checks.
        let input = single_field_spec(
            "Name",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                    min: 3,
                    max: 10,
                })],
            },
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();

        // Base type still maps to String despite the constraint wrapper.
        assert!(types.contains("pub field: String"));
        assert!(types.contains("pub struct ValidationError"));
        assert!(types.contains("pub fn validate(&self) -> Result<(), ValidationError>"));
        assert!(types.contains("v.len() < 3usize || v.len() > 10usize"));
    }

    #[test]
    fn annotation_constraints_and_numeric_bounds_validate() {
        // MinValue/MaxValue annotations on an integer field compare in the native
        // integer width (no `f64` cast), so large values keep full precision.
        let input = single_field_spec(
            "Score",
            CsilTypeExpression::Builtin("int".to_string()),
            vec![
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinValue(
                    CsilLiteralValue::Integer(0),
                )),
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxValue(
                    CsilLiteralValue::Integer(100),
                )),
            ],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("*v < 0"));
        assert!(types.contains("*v > 100"));
        // The integer path must never round-trip through f64.
        assert!(!types.contains("as f64"));
    }

    #[test]
    fn integer_bounds_compare_natively_past_f64_precision() {
        // A bound above 2^53 (9_007_199_254_740_993) cannot be represented exactly
        // by f64, so an `as f64` comparison would mis-handle values near it. The
        // native comparison must emit the exact integer with no f64 cast.
        let big = 9_007_199_254_740_993i64;
        let input = single_field_spec(
            "Big",
            CsilTypeExpression::Builtin("uint".to_string()),
            vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(big)),
            )],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains(&format!("*v < {big}")));
        assert!(!types.contains("as f64"));
    }

    #[test]
    fn float_field_bounds_compare_natively_with_no_cast() {
        // A genuinely floating field's Rust type is already `f64` (see
        // `map_builtin_type`), so the comparison must not cast it to its own type —
        // `(*v as f64) > 1.5` on an already-`f64` `v` is clippy's
        // `unnecessary_cast`, not just a style nit: it fails `-D warnings`.
        let input = single_field_spec(
            "Ratio",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("float64".to_string())),
                constraints: vec![CsilControlOperator::LessEqual(CsilLiteralValue::Float(1.5))],
            },
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("*v > 1.5"));
        assert!(!types.contains("as f64"));
    }

    #[test]
    fn integer_field_with_float_bound_still_widens_to_f64() {
        // An integer field constrained by a *float* bound (`int .le 1.5`) has no
        // native integer/float comparison, so it genuinely needs the widening cast
        // — only a field whose own Rust type is already `f64` skips it.
        let input = single_field_spec(
            "Score",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                constraints: vec![CsilControlOperator::LessEqual(CsilLiteralValue::Float(1.5))],
            },
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("(*v as f64) > 1.5"));
    }

    #[test]
    fn decimal_integer_bound_is_not_dropped() {
        // Core may hand a `decimal` bound as an integer literal (e.g. `decimal .ge 0`).
        // It must be rendered to a decimal string and enforced, never silently dropped.
        let input = single_field_spec(
            "Balance",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("decimal".to_string())),
                constraints: vec![CsilControlOperator::GreaterEqual(
                    CsilLiteralValue::Integer(0),
                )],
            },
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("pub fn validate(&self) -> Result<(), ValidationError>"));
        assert!(types.contains("CsilDecimal::from_str(\"0\")"));
        assert!(types.contains("if *v < bound {"));
    }

    #[test]
    fn regex_constraint_emits_regex_check_and_dep_note() {
        let input = single_field_spec(
            "Tag",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                constraints: vec![CsilControlOperator::Regex("^[a-z]+$".to_string())],
            },
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();
        assert!(types.content.contains("regex::Regex::new(\"^[a-z]+$\")"));
        assert!(
            types
                .content
                .contains("value does not match required pattern")
        );

        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(root.content.contains("regex = \"1\""));
    }

    #[test]
    fn optional_constrained_field_guards_with_if_let() {
        let input = single_field_spec(
            "Maybe",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(2))],
            },
            vec![],
            Some(CsilOccurrence::Optional),
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("pub field: Option<String>"));
        assert!(types.contains("if let Some(v) = &self.field {"));
        assert!(types.contains("v.len() < 2usize"));
    }

    #[test]
    fn encoding_only_constraints_document_but_do_not_validate() {
        let input = single_field_spec(
            "Blob",
            CsilTypeExpression::Constrained {
                base_type: Box::new(CsilTypeExpression::Builtin("bytes".to_string())),
                constraints: vec![CsilControlOperator::Cbor],
            },
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        // Documented on the field, but no validate() / ValidationError is forced.
        assert!(types.contains("encoding: embedded CBOR (.cbor)"));
        assert!(!types.contains("pub struct ValidationError"));
        assert!(!types.contains("fn validate"));
    }

    #[test]
    fn unconstrained_struct_has_no_validate() {
        let input = single_field_spec(
            "Plain",
            CsilTypeExpression::Builtin("text".to_string()),
            vec![],
            None,
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(!types.contains("fn validate"));
        assert!(!types.contains("pub struct ValidationError"));
    }

    /// Mirrors the injected `CsilDecimal` parse/format algorithm so its
    /// correctness is exercised here even though the emitted helper itself is
    /// compiled by the consumer. Kept identical to `CSIL_DECIMAL`'s bodies.
    mod csil_decimal_algo {
        pub fn as_str(exponent: i64, mantissa: i128) -> String {
            let digits = mantissa.unsigned_abs().to_string();
            let body = if exponent >= 0 {
                let mut out = digits;
                out.push_str(&"0".repeat(exponent as usize));
                out
            } else {
                let scale = (-exponent) as usize;
                if digits.len() > scale {
                    let point = digits.len() - scale;
                    format!("{}.{}", &digits[..point], &digits[point..])
                } else {
                    let pad = "0".repeat(scale - digits.len());
                    format!("0.{pad}{digits}")
                }
            };
            if mantissa < 0 {
                format!("-{body}")
            } else {
                body
            }
        }

        pub fn from_str(s: &str) -> Result<(i64, i128), String> {
            let t = s.trim();
            let (negative, rest) = match t.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            let (int_part, frac_part) = match rest.split_once('.') {
                Some((i, f)) => (i, f),
                None => (rest, ""),
            };
            if frac_part.contains('.') || (int_part.is_empty() && frac_part.is_empty()) {
                return Err(format!("invalid decimal: {s}"));
            }
            let mut digits = String::with_capacity(int_part.len() + frac_part.len());
            digits.push_str(int_part);
            digits.push_str(frac_part);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid decimal: {s}"));
            }
            let magnitude: i128 = digits
                .parse()
                .map_err(|e| format!("decimal out of range: {e}"))?;
            let mantissa = if negative { -magnitude } else { magnitude };
            Ok((-(frac_part.len() as i64), mantissa))
        }

        /// Mirrors the injected `CsilDecimal::cmp` so the by-value ordering is
        /// exercised here even though the emitted helper is compiled by the
        /// consumer. Kept identical to `CSIL_DECIMAL`'s `Ord`/`cmp_magnitude` bodies.
        pub fn cmp(a: (i64, i128), b: (i64, i128)) -> std::cmp::Ordering {
            use std::cmp::Ordering;
            let (ea, ma) = a;
            let (eb, mb) = b;
            let sign_a = ma.signum();
            let sign_b = mb.signum();
            if sign_a != sign_b {
                return sign_a.cmp(&sign_b);
            }
            if sign_a == 0 {
                return Ordering::Equal;
            }
            let magnitude = cmp_magnitude(ma.unsigned_abs(), ea, mb.unsigned_abs(), eb);
            if sign_a < 0 {
                magnitude.reverse()
            } else {
                magnitude
            }
        }

        fn cmp_magnitude(
            mut ma: u128,
            mut ea: i64,
            mut mb: u128,
            mut eb: i64,
        ) -> std::cmp::Ordering {
            while ma.is_multiple_of(10) {
                ma /= 10;
                ea += 1;
            }
            while mb.is_multiple_of(10) {
                mb /= 10;
                eb += 1;
            }
            let digits_a = ma.to_string();
            let digits_b = mb.to_string();
            let weight_a = digits_a.len() as i64 + ea;
            let weight_b = digits_b.len() as i64 + eb;
            if weight_a != weight_b {
                return weight_a.cmp(&weight_b);
            }
            let width = digits_a.len().max(digits_b.len());
            let padded_a = format!("{digits_a:0<width$}");
            let padded_b = format!("{digits_b:0<width$}");
            padded_a.cmp(&padded_b)
        }
    }

    #[test]
    fn csil_decimal_string_round_trips_exact_value() {
        use csil_decimal_algo::{as_str, from_str};
        // exponent/mantissa -> string
        assert_eq!(as_str(-2, 12345), "123.45");
        assert_eq!(as_str(-3, -1), "-0.001");
        assert_eq!(as_str(0, 100), "100");
        assert_eq!(as_str(2, 5), "500");

        // string -> exact (exponent, mantissa)
        assert_eq!(from_str("123.45").unwrap(), (-2, 12345));
        assert_eq!(from_str("-0.001").unwrap(), (-3, -1));
        assert_eq!(from_str("+.5").unwrap(), (-1, 5));
        assert_eq!(from_str("100").unwrap(), (0, 100));

        // value-preserving round trip through the string form
        for (e, m) in [(-2i64, 12345i128), (-3, -1), (0, 7), (-4, 1000000)] {
            let parsed = from_str(&as_str(e, m)).unwrap();
            // The value mantissa*10^exp is preserved even if representation normalizes.
            let original = m as f64 * 10f64.powi(e as i32);
            let round = parsed.1 as f64 * 10f64.powi(parsed.0 as i32);
            assert!(
                (original - round).abs() < 1e-9,
                "value preserved for ({e}, {m})"
            );
        }

        assert!(from_str("1.2.3").is_err());
        assert!(from_str("abc").is_err());
        assert!(from_str("-").is_err());
    }

    #[test]
    fn csil_decimal_orders_by_value_not_representation() {
        use csil_decimal_algo::{cmp, from_str};
        use std::cmp::Ordering;

        // Differing exponents that encode the same value compare Equal.
        assert_eq!(cmp((-2, 0), (-1, 0)), Ordering::Equal); // "0.00" == "0.0"
        assert_eq!(cmp((-2, 100), (0, 1)), Ordering::Equal); // "1.00" == "1"
        assert_eq!(cmp((-3, 1500), (-1, 15)), Ordering::Equal); // "1.500" == "1.5"

        // Magnitude ordering across exponents.
        assert_eq!(cmp((-1, 5), (-2, 5)), Ordering::Greater); // 0.5 > 0.05
        assert_eq!(cmp((-2, 145), (-1, 15)), Ordering::Less); // 1.45 < 1.5
        assert_eq!(cmp((-2, 12345), (-3, 12345)), Ordering::Greater); // 123.45 > 12.345

        // Sign handling.
        assert_eq!(cmp((-2, -1), (-2, 1)), Ordering::Less); // -0.01 < 0.01
        assert_eq!(cmp((-2, -1), (-3, -1)), Ordering::Less); // -0.01 < -0.001
        assert_eq!(cmp((0, 0), (5, 0)), Ordering::Equal); // zero is zero at any exponent

        // Consistent with the canonical parse of literal bounds.
        let zero = from_str("0.00").unwrap();
        let positive = from_str("0.01").unwrap();
        assert_eq!(cmp(zero, positive), Ordering::Less);
        assert_eq!(cmp(zero, from_str("0").unwrap()), Ordering::Equal);
    }

    #[test]
    fn decimal_and_timestamp_bounds_construct_typed_comparands() {
        // `balance: decimal .ge "0.00"`, `created_at: timestamp .ge "1970-01-01T00:00:00Z"`.
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "User".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("balance".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin("decimal".to_string())),
                            constraints: vec![CsilControlOperator::GreaterEqual(
                                CsilLiteralValue::Text("0.00".to_string()),
                            )],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("created_at".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin(
                                "timestamp".to_string(),
                            )),
                            constraints: vec![CsilControlOperator::GreaterEqual(
                                CsilLiteralValue::Text("1970-01-01T00:00:00Z".to_string()),
                            )],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();

        // A validate() is emitted (the constraint is not silently dropped).
        assert!(types.contains("pub fn validate(&self) -> Result<(), ValidationError>"));

        // decimal bound: built through CsilDecimal::from_str and compared by Ord.
        assert!(types.contains("CsilDecimal::from_str(\"0.00\")"));
        assert!(types.contains("if *v < bound {"));

        // timestamp bound: parsed RFC 3339 into chrono UTC and compared.
        assert!(types.contains("chrono::DateTime::parse_from_rfc3339(\"1970-01-01T00:00:00Z\")"));
        assert!(types.contains(".with_timezone(&chrono::Utc)"));

        // The bounds are escaped as Rust string literals, so the surrounding source
        // stays balanced: equal count of `"` characters means no quote broke out.
        assert_eq!(
            types.matches('"').count() % 2,
            0,
            "generated source has unbalanced quotes"
        );
    }

    #[test]
    fn test_linkkeys_end_to_end() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![
            CsilRule {
                name: "CheckValue".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Builtin("text".to_string()),
                    CsilTypeExpression::Builtin("int".to_string()),
                    CsilTypeExpression::Builtin("float".to_string()),
                ])),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "CheckEntries".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                    key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                    value: Box::new(CsilTypeExpression::Reference("CheckValue".to_string())),
                    occurrence: None,
                }),
                position: CsilPosition {
                    line: 3,
                    column: 1,
                    offset: 30,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "CheckResult".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("result".to_string())),
                            value_type: CsilTypeExpression::Builtin("bool".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("entries".to_string())),
                            value_type: CsilTypeExpression::Reference("CheckEntries".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: CsilPosition {
                    line: 5,
                    column: 1,
                    offset: 60,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "HelloRequest".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                })),
                position: CsilPosition {
                    line: 10,
                    column: 1,
                    offset: 120,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "GuestbookEntry".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("id".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("created_at".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("updated_at".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: CsilPosition {
                    line: 14,
                    column: 1,
                    offset: 160,
                },
                doc_comments: Vec::new(),
            },
        ];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        // CheckValue is an enum with 3 variants
        assert!(types_content.contains("pub enum CheckValue"));
        assert!(types_content.contains("Variant0(String)"));
        assert!(types_content.contains("Variant1(i64)"));
        assert!(types_content.contains("Variant2(f64)"));

        // CheckEntries is a HashMap
        assert!(
            types_content
                .contains("pub type CheckEntries = std::collections::HashMap<String, CheckValue>")
        );

        // CheckResult is a struct
        assert!(types_content.contains("pub struct CheckResult"));
        assert!(types_content.contains("pub result: bool"));
        assert!(types_content.contains("pub entries: CheckEntries"));

        // HelloRequest has optional name
        assert!(types_content.contains("pub struct HelloRequest"));
        assert!(types_content.contains("pub name: Option<String>"));

        // GuestbookEntry is a struct with 4 fields
        assert!(types_content.contains("pub struct GuestbookEntry"));
        assert!(types_content.contains("pub id: String"));
        assert!(types_content.contains("pub name: String"));
        assert!(types_content.contains("pub created_at: String"));
        assert!(types_content.contains("pub updated_at: String"));

        // No serde_json::Value type aliases (except for 'any' typed fields)
        assert!(
            !types_content.contains("pub type CheckValue = serde_json::Value"),
            "CheckValue should be an enum"
        );
        assert!(
            !types_content.contains("pub type CheckResult = serde_json::Value"),
            "CheckResult should be a struct"
        );
        assert!(
            !types_content.contains("pub type HelloRequest = serde_json::Value"),
            "HelloRequest should be a struct"
        );
        assert!(
            !types_content.contains("pub type GuestbookEntry = serde_json::Value"),
            "GuestbookEntry should be a struct"
        );
    }

    #[test]
    fn test_tuple_maps_to_rust_tuple() {
        let mut input = create_test_input();
        // A heterogeneous tuple `[text, int, bool]` plus a keyed array
        // `[tag: text, value: any]`; both map to positional Rust tuples.
        input.csil_spec.rules = vec![
            CsilRule {
                name: "Triple".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Tuple(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: None,
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: None,
                            value_type: CsilTypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: None,
                            value_type: CsilTypeExpression::Builtin("bool".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "Keyed".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Tuple(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("tag".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("value".to_string())),
                            value_type: CsilTypeExpression::Builtin("any".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: CsilPosition {
                    line: 2,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            },
        ];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        // Keys are positional names only and drop out of the Rust tuple type.
        assert!(types_content.contains("pub type Triple = (String, i64, bool);"));
        // `any` carries through the codec's own CBOR value tree (no serde dependency).
        assert!(types_content.contains("pub type Keyed = (String, crate::codec::CsilCborValue);"));
    }

    #[test]
    fn test_single_element_tuple_keeps_trailing_comma() {
        // A 1-tuple needs the trailing comma or it degrades to a parenthesized type.
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "Single".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Tuple(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: None,
                    value_type: CsilTypeExpression::Builtin("int".to_string()),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let types_content = RustCodeGenerator::new(&input).generate_types().unwrap();
        assert!(types_content.contains("pub type Single = (i64,);"));
    }

    #[test]
    fn test_tuple_field_in_struct() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "Pair".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("coord".to_string())),
                    value_type: CsilTypeExpression::Tuple(CsilGroupExpression {
                        entries: vec![
                            CsilGroupEntry {
                                key: None,
                                value_type: CsilTypeExpression::Builtin("float".to_string()),
                                occurrence: None,
                                metadata: vec![],
                                doc_comments: Vec::new(),
                            },
                            CsilGroupEntry {
                                key: None,
                                value_type: CsilTypeExpression::Builtin("float".to_string()),
                                occurrence: None,
                                metadata: vec![],
                                doc_comments: Vec::new(),
                            },
                        ],
                    }),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let types_content = RustCodeGenerator::new(&input).generate_types().unwrap();
        assert!(types_content.contains("pub coord: (f64, f64),"));
    }

    #[test]
    fn test_depends_on_expr_renders_doc_lines() {
        // `@depends-on(country == "US" | country == "CA")` and a nested AND tree
        // must surface as readable doc lines on the dependent field.
        let condition = CsilDependsCondition::Any(vec![
            CsilDependsCondition::Compare {
                field: "country".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("US".to_string())),
            },
            CsilDependsCondition::Compare {
                field: "country".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("CA".to_string())),
            },
        ]);

        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "ShippingForm".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("state".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![CsilFieldMetadata::DependsOnExpr(condition)],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let types_content = RustCodeGenerator::new(&input).generate_types().unwrap();
        assert!(
            types_content.contains("/// depends-on: country == \"US\" || country == \"CA\""),
            "expected an Any condition rendered with `||`, got:\n{types_content}"
        );
    }

    #[test]
    fn test_depends_on_condition_rendering_forms() {
        // Presence-only check.
        let presence = CsilDependsCondition::Compare {
            field: "sso_enabled".to_string(),
            op: None,
            value: None,
        };
        assert_eq!(
            RustCodeGenerator::render_depends_condition(&presence),
            "sso_enabled"
        );

        // Mixed AND/OR tree parenthesizes nested groups for unambiguous precedence.
        let mixed = CsilDependsCondition::All(vec![
            CsilDependsCondition::Compare {
                field: "registration_type".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("group".to_string())),
            },
            CsilDependsCondition::Any(vec![
                CsilDependsCondition::Compare {
                    field: "group_size".to_string(),
                    op: Some(CsilDependsCompareOp::Gt),
                    value: Some(CsilLiteralValue::Integer(5)),
                },
                CsilDependsCondition::Compare {
                    field: "vip".to_string(),
                    op: Some(CsilDependsCompareOp::Ne),
                    value: Some(CsilLiteralValue::Bool(false)),
                },
            ]),
        ]);
        assert_eq!(
            RustCodeGenerator::render_depends_condition(&mixed),
            "registration_type == \"group\" && (group_size > 5 || vip != false)"
        );
    }

    #[test]
    fn test_null_input_push_op_server_and_client() {
        // `op: -> Event` parses to a Unidirectional op with a `null` input. The
        // server trait and the client must both omit the request parameter.
        let mut server_input = create_test_input();
        // `Pong` is a real record so the typed client can decode the response.
        server_input
            .csil_spec
            .rules
            .push(group_rule("Pong", "ts", "uint"));
        server_input.csil_spec.rules.push(CsilRule {
            name: "EventsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "heartbeat".to_string(),
                    input_type: CsilTypeExpression::Builtin("null".to_string()),
                    output_type: CsilTypeExpression::Reference("Pong".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        server_input.csil_spec.service_count = 1;

        let services = RustCodeGenerator::new(&server_input)
            .generate_services()
            .unwrap();
        assert!(
            services.contains(
                "fn heartbeat(&self, ctx: &Self::Context) -> Result<Pong, ServiceError>;"
            ),
            "null-input op must omit the `input` parameter, got:\n{services}"
        );
        // No bogus unit-typed request parameter.
        assert!(!services.contains("input: ()"));

        let mut client_input = server_input.clone();
        client_input.config.target = "rust-client".to_string();
        let files = RustCodeGenerator::new(&client_input).generate().unwrap();
        let client = files
            .iter()
            .find(|f| f.path == "client.rs")
            .expect("client.rs emitted");
        assert!(
            client
                .content
                .contains("pub fn heartbeat(&self) -> Result<Pong, ClientError>"),
            "null-input client method must take no req, got:\n{}",
            client.content
        );
        assert!(
            client
                .content
                .contains("self.transport.call(\"EventsService\", \"heartbeat\", &[])?;"),
            "null-input client must send empty request bytes, got:\n{}",
            client.content
        );
        assert!(
            client.content.contains(
                "decode_pong(&csil_resp).map_err(|e| ClientError::Transport(e.to_string()))"
            ),
            "null-input client must decode the typed response, got:\n{}",
            client.content
        );
    }

    fn wire_id_service_input() -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "OrderService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    CsilServiceOperation {
                        name: "place-order".to_string(),
                        input_type: CsilTypeExpression::Reference("Order".to_string()),
                        output_type: CsilTypeExpression::Reference("Receipt".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 1,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                        wire_id: Some(7),
                    },
                    CsilServiceOperation {
                        name: "cancel-order".to_string(),
                        input_type: CsilTypeExpression::Reference("Order".to_string()),
                        output_type: CsilTypeExpression::Reference("Receipt".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 1,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                        wire_id: None,
                    },
                ],
                wire_id: Some(3),
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;
        input
    }

    #[test]
    fn test_wire_ids_emitted_when_present() {
        let input = wire_id_service_input();
        let services = RustCodeGenerator::new(&input).generate_services().unwrap();
        assert!(
            services.contains("pub mod order_service_wire_ids {"),
            "expected wire_ids module, got:\n{services}"
        );
        assert!(
            services.contains("pub const SERVICE: u64 = 3;"),
            "expected service ordinal, got:\n{services}"
        );
        assert!(
            services.contains("pub const OP_PLACE_ORDER: u64 = 7;"),
            "expected operation ordinal, got:\n{services}"
        );
        // Operations without a wire-id contribute no constant.
        assert!(
            !services.contains("CANCEL_ORDER"),
            "operation without wire-id must not emit a constant"
        );
    }

    #[test]
    fn test_wire_id_op_named_service_does_not_collide() {
        let mut input = wire_id_service_input();
        if let CsilRuleType::ServiceDef(service) =
            &mut input.csil_spec.rules.last_mut().unwrap().rule_type
        {
            service.operations[0].name = "service".to_string();
        }
        let services = RustCodeGenerator::new(&input).generate_services().unwrap();
        // The op named `service` becomes OP_SERVICE, distinct from SERVICE.
        assert!(
            services.contains("pub const SERVICE: u64 = 3;"),
            "expected service ordinal, got:\n{services}"
        );
        assert!(
            services.contains("pub const OP_SERVICE: u64 = 7;"),
            "expected op ordinal prefixed to avoid collision, got:\n{services}"
        );
    }

    #[test]
    fn test_wire_ids_absent_when_unset() {
        let mut input = wire_id_service_input();
        if let CsilRuleType::ServiceDef(service) =
            &mut input.csil_spec.rules.last_mut().unwrap().rule_type
        {
            service.wire_id = None;
            for op in &mut service.operations {
                op.wire_id = None;
            }
        }
        let services = RustCodeGenerator::new(&input).generate_services().unwrap();
        assert!(
            !services.contains("wire_ids"),
            "no wire-id output when service has no wire-id, got:\n{services}"
        );
    }

    // Build a channel (bidirectional) service carrying `@wire-id` ordinals so the
    // compact-router twin has something to dispatch on.
    fn wire_id_channel_input() -> WasmGeneratorInput {
        let mut input = create_test_input();
        let mut rule = service_with_directions(
            "Match",
            &[("play", "User", "User", CsilServiceDirection::Bidirectional)],
        );
        if let CsilRuleType::ServiceDef(service) = &mut rule.rule_type {
            service.wire_id = Some(1);
            service.operations[0].wire_id = Some(5);
        }
        input.csil_spec.rules.push(rule);
        input.csil_spec.service_count = 1;
        input
    }

    #[test]
    fn compact_router_emitted_for_wire_id_channel_service() {
        let input = wire_id_channel_input();
        let services = RustCodeGenerator::new(&input).generate_services().unwrap();

        // Verbose router stays byte-identical alongside the compact twin.
        assert!(
            services.contains("pub fn route_match_channel<H>"),
            "verbose router expected, got:\n{services}"
        );
        // Compact twin dispatches on the operation ordinal, not the wire name.
        assert!(
            services.contains("pub fn route_match_channel_compact<H>"),
            "compact router expected, got:\n{services}"
        );
        assert!(
            services.contains("op: u64,"),
            "compact router keys on a u64 ordinal, got:\n{services}"
        );
        assert!(
            services.contains("        5 => {"),
            "compact router matches the op ordinal, got:\n{services}"
        );
        assert!(
            services.contains("handlers.play(ctx, msg)"),
            "compact router dispatches to the handler, got:\n{services}"
        );
        assert!(
            services.contains("unknown channel ordinal {other}"),
            "compact router has an ordinal fallthrough, got:\n{services}"
        );
    }

    #[test]
    fn compact_router_absent_without_wire_id() {
        let mut input = wire_id_channel_input();
        if let CsilRuleType::ServiceDef(service) =
            &mut input.csil_spec.rules.last_mut().unwrap().rule_type
        {
            service.wire_id = None;
            service.operations[0].wire_id = None;
        }
        let services = RustCodeGenerator::new(&input).generate_services().unwrap();
        // The verbose router survives; the compact twin must not appear.
        assert!(
            services.contains("pub fn route_match_channel<H>"),
            "verbose router expected, got:\n{services}"
        );
        assert!(
            !services.contains("_compact"),
            "no compact router without wire-ids, got:\n{services}"
        );
    }

    /// The corndogs `rust-client` fixture from the wire contract: a `Task` with a
    /// text uuid, text current_state, bytes payload, optional int priority, a
    /// `map<text,int>` of labels and a `list<text>` of tags; a `SubmitTaskRequest`
    /// wrapping a `Task` and a queue; a `ServiceError`; and a `CorndogsService` with
    /// `submit-task: SubmitTaskRequest -> Task / ServiceError`.
    fn corndogs_client_input() -> WasmGeneratorInput {
        let task = CsilRule {
            name: "Task".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("uuid".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("current_state".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("payload".to_string())),
                        value_type: CsilTypeExpression::Builtin("bytes".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("priority".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("labels".to_string())),
                        value_type: CsilTypeExpression::Map {
                            key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("tags".to_string())),
                        value_type: CsilTypeExpression::Array {
                            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Fields typed as named map ALIASES — the regression: their codec
                    // must walk the underlying map, not stub it to null. One map-of-int
                    // and one map-of-record.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("queue_counts".to_string())),
                        value_type: CsilTypeExpression::Reference("StringInt64Map".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("state_counts".to_string())),
                        value_type: CsilTypeExpression::Reference(
                            "QueueAndStateCountsMap".to_string(),
                        ),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        // A record used as a map alias value, to prove map-of-record recurses to the
        // record codec rather than stubbing.
        let counts = CsilRule {
            name: "QueueAndStateCounts".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("count".to_string())),
                    value_type: CsilTypeExpression::Builtin("int".to_string()),
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        // Named map aliases (`X = {* text => …}`) parse to a TypeDef carrying a Map.
        let map_alias = |name: &str, value: CsilTypeExpression| CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(value),
                occurrence: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        let str_int_map = map_alias(
            "StringInt64Map",
            CsilTypeExpression::Builtin("int".to_string()),
        );
        let state_map = map_alias(
            "QueueAndStateCountsMap",
            CsilTypeExpression::Reference("QueueAndStateCounts".to_string()),
        );
        let req = CsilRule {
            name: "SubmitTaskRequest".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("task".to_string())),
                        value_type: CsilTypeExpression::Reference("Task".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("queue".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        let err = group_rule("ServiceError", "message", "text");
        let svc = CsilRule {
            name: "CorndogsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
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
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };

        let mut input = create_test_input();
        input.config.target = "rust-client".to_string();
        input.csil_spec.rules = vec![task, counts, str_int_map, state_map, req, err, svc];
        input.csil_spec.service_count = 1;
        input
    }

    #[test]
    fn corndogs_codec_emits_canonical_keys_and_typed_pairs() {
        let files = RustCodeGenerator::new(&corndogs_client_input())
            .generate()
            .unwrap();
        let codec = files
            .iter()
            .find(|f| f.path == "codec.gen.rs")
            .expect("codec.gen.rs emitted");

        // Public byte wrappers per record.
        assert!(
            codec
                .content
                .contains("pub fn encode_task(csil_v: &Task) -> Vec<u8>")
        );
        assert!(
            codec
                .content
                .contains("pub fn decode_task(csil_data: &[u8]) -> Result<Task, CsilCborError>")
        );
        assert!(
            codec.content.contains(
                "pub fn encode_submit_task_request(csil_v: &SubmitTaskRequest) -> Vec<u8>"
            )
        );
        // bytes -> CBOR byte string (major type 2) via the runtime's cbor_bytes.
        assert!(
            codec
                .content
                .contains("cbor_text(\"payload\"), cbor_bytes(&csil_v.payload)")
        );
        // A nested record reference recurses into its codec.
        assert!(codec.content.contains("csil_enc_task(&csil_v.task)"));
        // Optional int: guarded on encode, deref'd into the map.
        assert!(
            codec
                .content
                .contains("if let Some(csil_inner) = &csil_v.priority")
        );
        assert!(codec.content.contains("cbor_int(*csil_inner)"));
        // map<text,int> and list<text> route through the generic helpers.
        assert!(codec.content.contains("cbor_enc_map("));
        assert!(codec.content.contains("&csil_v.labels"));
        assert!(codec.content.contains("cbor_enc_array(&csil_v.tags,"));

        // Keys are laid down in canonical (encoded-key) order: within Task the len-4
        // keys `tags`/`uuid` precede the longer keys, and `current_state` (len 13) is
        // last; `tags` < `uuid` on content.
        let enc_start = codec.content.find("fn csil_enc_task").unwrap();
        let enc = &codec.content[enc_start..];
        let pos_tags = enc.find("\"tags\"").unwrap();
        let pos_uuid = enc.find("\"uuid\"").unwrap();
        let pos_state = enc.find("\"current_state\"").unwrap();
        assert!(
            pos_tags < pos_uuid && pos_uuid < pos_state,
            "fields not in canonical key order"
        );

        // The generated types carry no serde anything.
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();
        assert!(!types.content.contains("serde"));
        assert!(
            !types
                .content
                .contains("#[derive(Debug, Clone, PartialEq, Serialize")
        );
    }

    #[test]
    fn generated_rust_is_rustfmt_and_clippy_clean() {
        let cargo_probe = std::process::Command::new("cargo")
            .arg("--version")
            .output();
        let rustfmt_probe = std::process::Command::new("rustfmt")
            .arg("--version")
            .output();
        if cargo_probe.map(|o| !o.status.success()).unwrap_or(true)
            || rustfmt_probe.map(|o| !o.status.success()).unwrap_or(true)
        {
            eprintln!("skipping: cargo or rustfmt not on PATH");
            return;
        }

        let mut input = corndogs_client_input();
        input.config.options.insert(
            "module_root_filename".to_string(),
            serde_json::Value::String("lib.rs".to_string()),
        );
        input.csil_spec.rules.push(CsilRule {
            name: "EmptyRequest".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.rules.push(CsilRule {
            name: "CheckValue".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("int".to_string()),
                CsilTypeExpression::Builtin("float".to_string()),
            ])),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.rules.push(CsilRule {
            name: "CheckResult".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("result".to_string())),
                        value_type: CsilTypeExpression::Builtin("bool".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("entries".to_string())),
                        value_type: CsilTypeExpression::Map {
                            key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            value: Box::new(CsilTypeExpression::Reference(
                                "CheckValue".to_string(),
                            )),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        for (name, literal) in [("Ping", "ping"), ("Stop", "stop")] {
            input.csil_spec.rules.push(CsilRule {
                name: name.to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("kind".to_string())),
                        value_type: CsilTypeExpression::Literal(CsilLiteralValue::Text(
                            literal.to_string(),
                        )),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            });
        }
        input.csil_spec.rules.push(CsilRule {
            name: "Control".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Reference("Ping".to_string()),
                CsilTypeExpression::Reference("Stop".to_string()),
            ])),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        // The linkkeys regression shape: a record name long enough that the
        // one-line `csil_enc_...` signature would pass rustfmt's width, plus a
        // tuple with optional elements to exercise the positional codec's
        // wrapped closures.
        input.csil_spec.rules.push(CsilRule {
            name: "SignedLocalRpTicketRedemptionRequest".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("request".to_string())),
                        value_type: CsilTypeExpression::Builtin("bytes".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("triple".to_string())),
                        value_type: CsilTypeExpression::Tuple(CsilGroupExpression {
                            entries: vec![
                                CsilGroupEntry {
                                    key: None,
                                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                                    occurrence: None,
                                    metadata: vec![],
                                    doc_comments: Vec::new(),
                                },
                                CsilGroupEntry {
                                    key: None,
                                    value_type: CsilTypeExpression::Builtin("int".to_string()),
                                    occurrence: Some(CsilOccurrence::Optional),
                                    metadata: vec![],
                                    doc_comments: Vec::new(),
                                },
                            ],
                        }),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        // Constraint-heavy shape (mirrors examples/tagged-types/orders.csil): a
        // `decimal` field with a `.ge` bound pulls in the self-contained
        // `CsilDecimal` helper (whose `Ord` impl strips trailing zeros via `% 10 ==
        // 0`), and the length constraints below cover every zero/one boundary the
        // two length-check emitters (`field_checks`'s metadata loop and
        // `push_size_check`'s control-operator match) can produce — regressing
        // either `clippy::manual_is_multiple_of` or `clippy::len_zero` fails this
        // gate.
        input.csil_spec.rules.push(CsilRule {
            name: "ConstraintProbe".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("unit_price".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin("decimal".to_string())),
                            constraints: vec![CsilControlOperator::GreaterEqual(
                                CsilLiteralValue::Text("0.00".to_string()),
                            )],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // `.size (1*)` control operator: min-items-of-one, `push_size_check`'s
                    // `Min` branch.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("items".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Array {
                                element_type: Box::new(CsilTypeExpression::Builtin(
                                    "text".to_string(),
                                )),
                                occurrence: None,
                            }),
                            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(
                                1,
                            ))],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // `@min-items(1)` annotation: the metadata-driven MinItems branch in
                    // `field_checks` rather than the control-operator one above.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("tags".to_string())),
                        value_type: CsilTypeExpression::Array {
                            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            occurrence: None,
                        },
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Constraint(
                            CsilValidationConstraint::MinItems(1),
                        )],
                        doc_comments: Vec::new(),
                    },
                    // `.size (..0)` (must be empty): the zero-boundary of `push_size_check`'s
                    // `Max` branch.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("reserved".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Array {
                                element_type: Box::new(CsilTypeExpression::Builtin(
                                    "text".to_string(),
                                )),
                                occurrence: None,
                            }),
                            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Max(
                                0,
                            ))],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Inline mixed choice (a `Reference` arm plus a literal arm):
                    // must hoist to `ConstraintProbe_tag` and route through that
                    // synthesized union's own codec — regressing the hoist drops
                    // this field to the untyped `serde_json::Value` fallback
                    // (`E0433: cannot find crate serde_json`), and regressing the
                    // optional-field binding fix reintroduces an unused
                    // `csil_inner`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("tag".to_string())),
                        value_type: CsilTypeExpression::Choice(vec![
                            CsilTypeExpression::Reference("Task".to_string()),
                            CsilTypeExpression::Literal(CsilLiteralValue::Text(
                                "untagged".to_string(),
                            )),
                        ]),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // Inline all-literal choice whose LAST arm carries a trailing
                    // `.default` control operator (`"low" / "high" .default
                    // "normal"`) — the parser attaches `.default` to that one arm,
                    // not the choice as a whole, so it parses as `Constrained {
                    // base_type: Literal("high"), .. }`. Both the hoisted-name
                    // (`ConstraintProbe_mode`, tripping `non_camel_case_types`
                    // without the crate-wide allow) and the arm-classification fix
                    // (an unstripped `Constrained` wrapper would misclassify this
                    // as a union and bind-but-never-read the payload) are pinned
                    // by this fixture actually compiling and staying clippy-clean.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("mode".to_string())),
                        value_type: CsilTypeExpression::Choice(vec![
                            CsilTypeExpression::Literal(CsilLiteralValue::Text("low".to_string())),
                            CsilTypeExpression::Constrained {
                                base_type: Box::new(CsilTypeExpression::Literal(
                                    CsilLiteralValue::Text("high".to_string()),
                                )),
                                constraints: vec![CsilControlOperator::Default(
                                    CsilLiteralValue::Text("normal".to_string()),
                                )],
                            },
                        ]),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    // A genuinely floating field with a float bound: the
                    // comparison must stay `*v > 1.5`, not `(*v as f64) > 1.5` —
                    // the latter is clippy's `unnecessary_cast` since `float64`
                    // already maps to Rust `f64`.
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("ratio".to_string())),
                        value_type: CsilTypeExpression::Constrained {
                            base_type: Box::new(CsilTypeExpression::Builtin("float64".to_string())),
                            constraints: vec![CsilControlOperator::LessEqual(
                                CsilLiteralValue::Float(1.5),
                            )],
                        },
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        if let Some(rule) = input
            .csil_spec
            .rules
            .iter_mut()
            .find(|rule| rule.name == "CorndogsService")
            && let CsilRuleType::ServiceDef(service) = &mut rule.rule_type
        {
            // An op boundary union whose second arm is NOT literally
            // `ServiceError` (`Task` vs. the larger `ConstraintProbe`): must hoist
            // to `CorndogsService_peek_response` rather than falling to
            // `serde_json::Value` (undeclared dependency), and the size gap
            // between the two record arms exercises clippy's
            // `large_enum_variant`, which the crate-wide allow must suppress.
            service.operations.push(CsilServiceOperation {
                name: "peek".to_string(),
                input_type: CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                output_type: CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Task".to_string()),
                    CsilTypeExpression::Reference("ConstraintProbe".to_string()),
                ]),
                direction: CsilServiceDirection::Unidirectional,
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
                wire_id: None,
            });
            service.operations.push(CsilServiceOperation {
                name: "control".to_string(),
                input_type: CsilTypeExpression::Reference("Control".to_string()),
                output_type: CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Control".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                direction: CsilServiceDirection::Bidirectional,
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
                wire_id: None,
            });
            // A unary op whose overlong boundary type forces the client method
            // signature, transport call, and decode chain onto rustfmt's wrapped
            // forms.
            service.operations.push(CsilServiceOperation {
                name: "redeem-claim-ticket".to_string(),
                input_type: CsilTypeExpression::Reference(
                    "SignedLocalRpTicketRedemptionRequest".to_string(),
                ),
                output_type: CsilTypeExpression::Reference(
                    "SignedLocalRpTicketRedemptionRequest".to_string(),
                ),
                direction: CsilServiceDirection::Unidirectional,
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
                wire_id: None,
            });
        }

        let files = RustCodeGenerator::new(&input).generate().unwrap();

        // Pin the exact shapes the two fixed lints used to fire on, so a regression
        // in either emitter is caught here even if some other rustfmt-neutral
        // rewrite kept the fixture superficially "clean".
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();
        assert!(
            types.content.contains("while ma.is_multiple_of(10)")
                && types.content.contains("while mb.is_multiple_of(10)"),
            "CsilDecimal::cmp_magnitude should use is_multiple_of, not `% 10 == 0`"
        );
        assert!(
            types.content.contains("if v.is_empty() {"),
            "a `.size (1*)`/`@min-items(1)` bound of exactly one should render as \
             `is_empty()`, not `v.len() < 1usize`"
        );
        assert!(
            types.content.contains("if !v.is_empty() {"),
            "a `.size (..0)` bound of exactly zero should render as `!is_empty()`, \
             not `v.len() > 0usize`"
        );
        assert!(
            !types.content.contains("% 10 == 0"),
            "no raw modulo-by-ten equality should remain in the decimal helper"
        );
        assert!(
            !types.content.contains(".len() < 1usize")
                && !types.content.contains(".len() > 0usize")
                && !types.content.contains(".len() == 0usize")
                && !types.content.contains(".len() != 0usize"),
            "no zero/one length comparison should remain in generated validation"
        );

        let dir = std::env::temp_dir().join(format!("csilgen-rust-tooling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut rust_paths = Vec::new();
        for file in &files {
            let path = src.join(&file.path);
            std::fs::write(&path, &file.content).unwrap();
            if file.path.ends_with(".rs") {
                rust_paths.push(path);
            }
        }
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"csiltoolingclean\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n\n[dependencies]\n",
        )
        .unwrap();

        let rustfmt = std::process::Command::new("rustfmt")
            .arg("--check")
            .arg("--edition")
            .arg("2021")
            .args(&rust_paths)
            .output()
            .unwrap();
        assert!(
            rustfmt.status.success(),
            "rustfmt failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&rustfmt.stdout),
            String::from_utf8_lossy(&rustfmt.stderr)
        );

        let clippy = std::process::Command::new("cargo")
            .arg("clippy")
            .arg("--quiet")
            .arg("--all-targets")
            .arg("--")
            .arg("-D")
            .arg("warnings")
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", dir.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            clippy.status.success(),
            "clippy failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&clippy.stdout),
            String::from_utf8_lossy(&clippy.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);

        let mut server_input = input;
        server_input.config.target = "rust-server".to_string();
        for rule in &mut server_input.csil_spec.rules {
            if rule.name == "ServiceError" {
                rule.rule_type = CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("code".to_string())),
                            value_type: CsilTypeExpression::Builtin("int".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("message".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                });
            }
            if rule.name == "CorndogsService"
                && let CsilRuleType::ServiceDef(service) = &mut rule.rule_type
                && let Some(control) = service
                    .operations
                    .iter_mut()
                    .find(|op| op.name == "control")
            {
                control.output_type = CsilTypeExpression::Reference("ServiceError".to_string());
            }
        }
        let files = RustCodeGenerator::new(&server_input).generate().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "csilgen-rust-server-tooling-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut rust_paths = Vec::new();
        for file in &files {
            let path = src.join(&file.path);
            std::fs::write(&path, &file.content).unwrap();
            if file.path.ends_with(".rs") {
                rust_paths.push(path);
            }
        }
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"csilservertoolingclean\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n\n[dependencies]\n",
        )
        .unwrap();

        let rustfmt = std::process::Command::new("rustfmt")
            .arg("--check")
            .arg("--edition")
            .arg("2021")
            .args(&rust_paths)
            .output()
            .unwrap();
        assert!(
            rustfmt.status.success(),
            "server rustfmt failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&rustfmt.stdout),
            String::from_utf8_lossy(&rustfmt.stderr)
        );

        let clippy = std::process::Command::new("cargo")
            .arg("clippy")
            .arg("--quiet")
            .arg("--all-targets")
            .arg("--")
            .arg("-D")
            .arg("warnings")
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", dir.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            clippy.status.success(),
            "server clippy failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&clippy.stdout),
            String::from_utf8_lossy(&clippy.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compile the generated codec + typed client and round-trip a corndogs request
    /// through a loopback transport with a real `cargo` build. Skips cleanly when no
    /// cargo toolchain is on PATH; with one present, this is the real proof the
    /// output is usable.
    #[test]
    fn codec_round_trips_through_cargo() {
        let probe = std::process::Command::new("cargo")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no cargo toolchain on PATH");
            return;
        }

        let mut input = corndogs_client_input();
        // Emit the module root as `lib.rs` so the temp crate is a library the driver
        // binary can depend on.
        input.config.options.insert(
            "module_root_filename".to_string(),
            serde_json::Value::String("lib.rs".to_string()),
        );
        let files = RustCodeGenerator::new(&input)
            .generate()
            .expect("generation ok");

        let dir = std::env::temp_dir().join(format!("csilgen-rust-codec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for file in &files {
            std::fs::write(src.join(&file.path), &file.content).unwrap();
        }
        // The corndogs spec uses no timestamp/decimal, so the generated codec is fully
        // self-contained and the crate needs no third-party dependency.
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"csilroundtrip\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(src.join("main.rs"), RUST_CODEC_DRIVER).unwrap();

        let run = std::process::Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .current_dir(&dir)
            // Keep the build hermetic and out of the parent's target dir: the
            // generated crate has no deps, so an isolated offline build suffices.
            .env("CARGO_TARGET_DIR", dir.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "cargo run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compile the mixed-kind literal enum (`Order.status: "pending" / "shipped" /
    /// 0 / 1`) and, through a real `cargo run`, (a) prove it compiles cleanly, (b)
    /// round-trip encode -> CBOR bytes -> decode for every declared literal of
    /// both kinds, and (c) prove an out-of-vocabulary value of a declared kind
    /// (an unrecognized text, an unrecognized int) is rejected on decode rather
    /// than silently coerced. This is the live proof behind
    /// `mixed_kind_literal_choice_codec_is_bare_wire_enum_not_tagged_union`'s
    /// source-text assertions. Skips cleanly when no cargo toolchain is on PATH.
    #[test]
    fn mixed_kind_enum_round_trips_through_cargo() {
        let probe = std::process::Command::new("cargo")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no cargo toolchain on PATH");
            return;
        }

        let mut input = mixed_kind_choice_input();
        input.config.target = "rust-typesonly".to_string();
        input.config.options.insert(
            "module_root_filename".to_string(),
            serde_json::Value::String("lib.rs".to_string()),
        );
        let files = RustCodeGenerator::new(&input)
            .generate()
            .expect("generation ok");

        let dir =
            std::env::temp_dir().join(format!("csilgen-rust-mixed-enum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for file in &files {
            std::fs::write(src.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"csilroundtrip\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(src.join("main.rs"), RUST_MIXED_ENUM_DRIVER).unwrap();

        let run = std::process::Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", dir.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "cargo run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compile the default-style output (sync client + async twin) and round-trip a
    /// corndogs request through the async `CorndogsAsyncClient` over an async loopback
    /// transport, driven by a from-scratch dependency-free `block_on`. This proves the
    /// emitted async code is well-formed AND actually awaits to a result — not just a
    /// compile check. Skips cleanly when no cargo toolchain is on PATH.
    #[test]
    fn async_client_round_trips_through_cargo() {
        let probe = std::process::Command::new("cargo")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no cargo toolchain on PATH");
            return;
        }

        let mut input = corndogs_client_input();
        // The default `both` style emits the async twin alongside the sync client, so
        // this crate exercises both clients coexisting in one module tree.
        input.config.options.insert(
            "module_root_filename".to_string(),
            serde_json::Value::String("lib.rs".to_string()),
        );
        let files = RustCodeGenerator::new(&input)
            .generate()
            .expect("generation ok");
        // Guard the premise: the twin must actually be present under the default style.
        assert!(files.iter().any(|f| f.path == "client_async.rs"));

        let dir = std::env::temp_dir().join(format!("csilgen-rust-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for file in &files {
            std::fs::write(src.join(&file.path), &file.content).unwrap();
        }
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"csilroundtrip\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(src.join("main.rs"), RUST_ASYNC_DRIVER).unwrap();

        let run = std::process::Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", dir.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "cargo run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without the `emit_packages` trigger the output is exactly as before: the
    /// module root is `mod.rs` and no `Cargo.toml`/`lib.rs` (and no `src/` prefix)
    /// appears. With `["rust"]` present, the directory becomes a crate: a
    /// `Cargo.toml` plus a `src/lib.rs` entry, every `.rs` file relocated under
    /// `src/`. A token list that omits `rust` leaves the default output intact.
    #[test]
    fn package_mode_emitted_iff_emit_packages_includes_rust() {
        // Default: no package files, flat layout, `mod.rs` root.
        let files = RustCodeGenerator::new(&corndogs_client_input())
            .generate()
            .expect("generation ok");
        assert!(!files.iter().any(|f| f.path == "Cargo.toml"));
        assert!(!files.iter().any(|f| f.path == "src/lib.rs"));
        assert!(files.iter().any(|f| f.path == "mod.rs"));
        assert!(files.iter().any(|f| f.path == "types.rs"));
        // The README rides only with the package; the default output has none.
        assert!(!files.iter().any(|f| f.path == "genquickstart.md"));

        // A token list that does not name rust must not trigger package mode.
        let mut other = corndogs_client_input();
        other.config.options.insert(
            "emit_packages".to_string(),
            serde_json::json!(["go", "python"]),
        );
        let files = RustCodeGenerator::new(&other)
            .generate()
            .expect("generation ok");
        assert!(!files.iter().any(|f| f.path == "Cargo.toml"));
        assert!(files.iter().any(|f| f.path == "mod.rs"));

        // `["rust"]` turns the directory into a crate.
        let mut pkg = corndogs_client_input();
        pkg.config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
        let files = RustCodeGenerator::new(&pkg)
            .generate()
            .expect("generation ok");
        let cargo = files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .expect("Cargo.toml emitted");
        assert!(cargo.content.contains("[package]"));
        assert!(cargo.content.contains("edition = \"2021\""));
        // Default version, and a name derived from the service base.
        assert!(cargo.content.contains("version = \"0.1.0\""));
        assert!(cargo.content.contains("name = \"corndogs\""));
        assert!(files.iter().any(|f| f.path == "src/lib.rs"));
        assert!(files.iter().any(|f| f.path == "src/types.rs"));
        assert!(files.iter().any(|f| f.path == "src/codec.gen.rs"));
        assert!(files.iter().any(|f| f.path == "src/client.rs"));
        // The README sits at the crate root beside `Cargo.toml`, not under `src/`.
        assert!(files.iter().any(|f| f.path == "genquickstart.md"));
        assert!(!files.iter().any(|f| f.path == "src/genquickstart.md"));
        // The flat (non-package) paths and `mod.rs` must be gone.
        assert!(!files.iter().any(|f| f.path == "mod.rs"));
        assert!(!files.iter().any(|f| f.path == "types.rs"));
        // The crate root still declares the relocated modules so it compiles.
        let lib = files.iter().find(|f| f.path == "src/lib.rs").unwrap();
        assert!(lib.content.contains("pub mod types;"));
        assert!(lib.content.contains("#[path = \"codec.gen.rs\"]"));
    }

    /// The README is opt-out: package mode emits it by default, and only an
    /// explicit `emit_readme: false` suppresses it. Everything else about the
    /// package (Cargo.toml, the relocated `src/` files) is unchanged either way.
    #[test]
    fn emit_readme_false_suppresses_only_the_readme() {
        let mut pkg = corndogs_client_input();
        pkg.config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
        let with_readme = RustCodeGenerator::new(&pkg)
            .generate()
            .expect("generation ok");
        assert!(with_readme.iter().any(|f| f.path == "genquickstart.md"));

        let mut off = pkg.clone();
        off.config
            .options
            .insert("emit_readme".to_string(), serde_json::json!(false));
        let without_readme = RustCodeGenerator::new(&off)
            .generate()
            .expect("generation ok");
        assert!(!without_readme.iter().any(|f| f.path == "genquickstart.md"));
        // The rest of the package is untouched: only the README disappears.
        assert!(without_readme.iter().any(|f| f.path == "Cargo.toml"));
        assert!(without_readme.iter().any(|f| f.path == "src/lib.rs"));
        assert!(without_readme.iter().any(|f| f.path == "src/types.rs"));
    }

    /// The dep-free corndogs spec must yield no `[dependencies]`, and the explicit
    /// `package_name`/`package_version` options must override the derived defaults.
    #[test]
    fn package_mode_coordinates_and_dep_free_cargo() {
        let mut input = corndogs_client_input();
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
        input.config.options.insert(
            "package_name".to_string(),
            serde_json::Value::String("acme_client".to_string()),
        );
        input.config.options.insert(
            "package_version".to_string(),
            serde_json::Value::String("2.3.4".to_string()),
        );
        let files = RustCodeGenerator::new(&input)
            .generate()
            .expect("generation ok");
        let cargo = files.iter().find(|f| f.path == "Cargo.toml").unwrap();
        assert!(cargo.content.contains("name = \"acme_client\""));
        assert!(cargo.content.contains("version = \"2.3.4\""));
        // The self-contained codec means a dep-free crate.
        assert!(!cargo.content.contains("[dependencies]"));
    }

    /// Generate a `rust-client` package into a temp dir and prove it is a real,
    /// buildable crate with a hermetic offline `cargo build`. The corndogs spec is
    /// dependency-free, so the build needs nothing from the network. Skips cleanly
    /// when no cargo toolchain is on PATH.
    #[test]
    fn package_mode_crate_builds_offline() {
        let probe = std::process::Command::new("cargo")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no cargo toolchain on PATH");
            return;
        }

        let mut input = corndogs_client_input();
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
        let files = RustCodeGenerator::new(&input)
            .generate()
            .expect("generation ok");

        let dir = std::env::temp_dir().join(format!("csilgen-rust-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in &files {
            let path = dir.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, &file.content).unwrap();
        }

        let build = std::process::Command::new("cargo")
            .arg("build")
            .arg("--quiet")
            .current_dir(&dir)
            // Hermetic, offline, and out of the parent's target dir: the generated
            // crate is dep-free so an isolated offline build must succeed on its own.
            .env("CARGO_TARGET_DIR", dir.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&build.stdout);
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "cargo build of generated package failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The canonical verification spec for the 3-transport genquickstart: two records
    /// and a service with both a `->` op (`ping`) and a record-typed `<->` op (`pulse`),
    /// so the RPC, Events, and Datagrams sections all render against real ops.
    fn transports_input() -> WasmGeneratorInput {
        let ping = group_rule("Ping", "msg", "text");
        let pong = group_rule("Pong", "msg", "text");
        let mk_op = |name: &str, dir: CsilServiceDirection| CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference("Ping".to_string()),
            output_type: CsilTypeExpression::Reference("Pong".to_string()),
            direction: dir,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id: None,
        };
        let svc = CsilRule {
            name: "EchoService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    mk_op("ping", CsilServiceDirection::Unidirectional),
                    mk_op("pulse", CsilServiceDirection::Bidirectional),
                ],
                wire_id: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        };
        let mut input = create_test_input();
        input.config.target = "rust-client".to_string();
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
        input.csil_spec.rules = vec![ping, pong, svc];
        input.csil_spec.service_count = 1;
        input
    }

    fn transports_readme(input: &WasmGeneratorInput) -> String {
        let files = RustCodeGenerator::new(input).generate().unwrap();
        files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .expect("genquickstart.md emitted")
            .content
            .clone()
    }

    /// The slice of `md` from `heading` up to the next `## ` heading (or end).
    fn md_section<'a>(md: &'a str, heading: &str) -> &'a str {
        let start = md.find(heading).expect("section heading present");
        let rest = &md[start..];
        match rest[heading.len()..].find("\n## ") {
            Some(off) => &rest[..heading.len() + off],
            None => rest,
        }
    }

    /// The `rust` fenced block under the given `## ` heading.
    fn section_rust_block(md: &str, heading: &str) -> String {
        let sec = md_section(md, heading);
        let start = sec.find("```rust\n").expect("section has a rust block") + "```rust\n".len();
        let rest = &sec[start..];
        let end = rest.find("\n```").expect("rust block is closed");
        rest[..end].to_string()
    }

    #[test]
    fn genquickstart_has_all_three_sections_by_default() {
        let readme = transports_readme(&transports_input());
        for heading in [
            "## CSIL-RPC (HTTP)",
            "## CSIL-Events (TLS)",
            "## CSIL-Datagrams (UDP)",
        ] {
            assert!(
                readme.contains(heading),
                "default genquickstart must contain {heading}:\n{readme}"
            );
        }
        // The deps block pulls in the transport library alongside the package + ureq.
        assert!(readme.contains("csilgen-transport = \"0.1\""));
        assert!(readme.contains("ureq = \"2\""));
    }

    #[test]
    fn genquickstart_transports_subset_emits_only_listed_sections() {
        let mut input = transports_input();
        input.config.options.insert(
            "genquickstart_transports".to_string(),
            serde_json::json!(["rpc"]),
        );
        let readme = transports_readme(&input);
        assert!(readme.contains("## CSIL-RPC (HTTP)"));
        assert!(
            !readme.contains("## CSIL-Events (TLS)"),
            "events section must be suppressed:\n{readme}"
        );
        assert!(
            !readme.contains("## CSIL-Datagrams (UDP)"),
            "datagrams section must be suppressed:\n{readme}"
        );
    }

    #[test]
    fn genquickstart_transports_unknown_or_empty_falls_back_to_all() {
        for opt in [serde_json::json!([]), serde_json::json!(["bogus"])] {
            let mut input = transports_input();
            input
                .config
                .options
                .insert("genquickstart_transports".to_string(), opt.clone());
            let readme = transports_readme(&input);
            assert!(
                readme.contains("## CSIL-RPC (HTTP)")
                    && readme.contains("## CSIL-Events (TLS)")
                    && readme.contains("## CSIL-Datagrams (UDP)"),
                "{opt} must fall back to all three sections:\n{readme}"
            );
        }

        let mut input = transports_input();
        input.config.options.insert(
            "genquickstart_transports".to_string(),
            serde_json::json!(["datagrams", "bogus"]),
        );
        let readme = transports_readme(&input);
        assert!(readme.contains("## CSIL-Datagrams (UDP)"));
        assert!(!readme.contains("## CSIL-RPC (HTTP)"));
        assert!(!readme.contains("## CSIL-Events (TLS)"));
    }

    #[test]
    fn each_section_names_its_library_imports_and_seam() {
        let readme = transports_readme(&transports_input());
        let rpc = md_section(&readme, "## CSIL-RPC (HTTP)");
        let events = md_section(&readme, "## CSIL-Events (TLS)");
        let datagrams = md_section(&readme, "## CSIL-Datagrams (UDP)");

        // RPC: the library envelope types + the canonical HTTP mount, no hand-rolled CBOR.
        assert!(rpc.contains("use csilgen_transport::rpc::{RpcRequest, RpcResponse};"));
        assert!(rpc.contains("RpcRequest::new(service, op, req.to_vec())"));
        assert!(rpc.contains("RpcResponse::decode(&body)"));
        assert!(rpc.contains("/csil/v1/rpc"));
        assert!(rpc.contains("into_transport_error()"));
        assert!(rpc.contains("== Some(\"ServiceError\")"));
        assert!(rpc.contains("impl Transport for HttpRpcCarrier"));
        assert!(rpc.contains("EchoClient::new(HttpRpcCarrier::new("));
        assert!(rpc.contains("client.ping(Ping { msg: \"example\".to_string() })"));
        assert!(
            !readme.contains("hand-roll") && !readme.contains("CSIL_TAG_EMBEDDED_CBOR"),
            "the lib-based carrier must not hand-roll CBOR:\n{readme}"
        );

        // Events: the lib's handshake/framing/heartbeat surface + the generated channel
        // router. Outbound (the op success output, Pong) rides the generated encoder;
        // inbound dispatch goes through route_<service>_channel into a handler that
        // implements the generated service trait — not codec-direct.
        // The carrier is built with an explicit max-frame limit so an operator can see
        // and change the guard without editing generated source (conventions doc §5).
        assert!(events.contains("StreamCarrier::with_max_frame(stream, MAX_FRAME)?"));
        assert!(events.contains("const MAX_FRAME: usize = MAX_FRAME_DEFAULT;"));
        assert!(events.contains("Hello {"));
        assert!(events.contains("$hello"));
        assert!(events.contains("HelloAck::decode(&ack_frame)"));
        assert!(events.contains("control::PING_NAME"));
        assert!(events.contains("control::PONG_NAME"));
        assert!(
            events.contains("encode_echo_service_pulse(&Pong { msg: \"example\".to_string() })"),
            "outbound must ride the generated encoder:\n{events}"
        );
        assert!(
            events.contains("route_echo_service_channel(&handlers, &(), method, &ev.payload)"),
            "inbound dispatch must go through the generated channel router:\n{events}"
        );
        assert!(
            events.contains("impl EchoService for QuickstartHandlers"),
            "the handler must implement the generated service trait:\n{events}"
        );
        assert!(
            !events.contains("decode_pong(&ev.payload)"),
            "the Events section must not decode payloads directly anymore:\n{events}"
        );

        // Datagrams: the lib's Datagram + carrier seam, and the no-sync-response warning.
        assert!(datagrams.contains("use csilgen_transport::datagrams::Datagram;"));
        assert!(datagrams.contains("Datagram::new(OP_ORD, 0, encode_ping(&req)).encode()"));
        assert!(datagrams.contains("Datagram::decode(&inbound)"));
        assert!(datagrams.contains("decode_pong(&dg.payload)"));
        assert!(datagrams.contains("NO synchronous response"));
    }

    #[test]
    fn rpc_section_renders_for_server_target_in_package_mode() {
        // Package mode emits every surface (client + server) regardless of the requested
        // target — mirroring OCaml — so even the `rust` server target's genquickstart
        // carries a working typed RPC client example rather than a pointer to rust-client.
        let mut input = transports_input();
        input.config.target = "rust".to_string();
        let readme = transports_readme(&input);
        let rpc = md_section(&readme, "## CSIL-RPC (HTTP)");
        assert!(
            rpc.contains("EchoClient::new(") && !rpc.contains("no sync RPC client"),
            "server-target RPC section must render the typed client in package mode:\n{rpc}"
        );
        assert!(readme.contains("## CSIL-Events (TLS)"));
        assert!(readme.contains("## CSIL-Datagrams (UDP)"));
    }

    #[test]
    fn events_section_without_channel_ops_emits_a_note() {
        // The corndogs spec has only a `->` op, so the Events section keeps the handshake
        // but replaces typed dispatch with a note (no generated-codec channel decode).
        let mut input = corndogs_client_input();
        input
            .config
            .options
            .insert("emit_packages".to_string(), serde_json::json!(["rust"]));
        let readme = transports_readme(&input);
        let events = md_section(&readme, "## CSIL-Events (TLS)");
        assert!(events.contains("$hello"));
        assert!(
            events.contains("no <->/<- operations"),
            "must note the absence of channel ops:\n{events}"
        );
        assert!(
            !events.contains("decode_"),
            "no typed channel decode when there are no channel ops:\n{events}"
        );
    }

    /// Stage the transports client package, drop the three README sections as `examples/`
    /// binaries plus an in-process round-trip driver, and drive a hermetic offline cargo.
    /// `cargo build --examples` compiles all three emitted sections (CSIL-Events is
    /// interactive/socket-driven, so this is its compile-check) against the real generated
    /// package + the `csilgen-transport` library; `cargo run --example roundtrip` then
    /// *runs* the CSIL-RPC (typed client over an in-process library-envelope echo) and
    /// CSIL-Datagrams (library loopback carrier) round-trips. Socket-free. Skips no cargo.
    #[test]
    fn genquickstart_sections_compile_and_round_trip() {
        let probe = std::process::Command::new("cargo")
            .arg("--version")
            .output();
        if probe.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: no cargo toolchain on PATH");
            return;
        }

        let files = RustCodeGenerator::new(&transports_input())
            .generate()
            .unwrap();
        let readme = files
            .iter()
            .find(|f| f.path == "genquickstart.md")
            .unwrap()
            .content
            .clone();

        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../transports/rust")
            .canonicalize()
            .expect("transports/rust must exist");

        let dir = std::env::temp_dir().join(format!("csilgen-rust-3t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in &files {
            let path = dir.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            if file.path == "Cargo.toml" {
                // The README example carriers ride two dev-deps: the (unpublished)
                // transport library via a local path, and ureq for the RPC HTTP carrier.
                let mut cargo = file.content.clone();
                cargo.push_str(&format!(
                    "\n[dev-dependencies]\ncsilgen-transport = {{ path = \"{}\" }}\nureq = \"2\"\n",
                    lib.display()
                ));
                std::fs::write(&path, cargo).unwrap();
            } else {
                std::fs::write(&path, &file.content).unwrap();
            }
        }

        let examples = dir.join("examples");
        std::fs::create_dir_all(&examples).unwrap();
        std::fs::write(
            examples.join("rpc.rs"),
            section_rust_block(&readme, "## CSIL-RPC (HTTP)"),
        )
        .unwrap();
        std::fs::write(
            examples.join("events.rs"),
            section_rust_block(&readme, "## CSIL-Events (TLS)"),
        )
        .unwrap();
        std::fs::write(
            examples.join("datagrams.rs"),
            section_rust_block(&readme, "## CSIL-Datagrams (UDP)"),
        )
        .unwrap();
        std::fs::write(examples.join("roundtrip.rs"), RUST_TRANSPORTS_DRIVER).unwrap();

        let target = dir.join("target");
        let build = std::process::Command::new("cargo")
            .arg("build")
            .arg("--examples")
            .arg("--quiet")
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "cargo build of the 3 README sections failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );

        let run = std::process::Command::new("cargo")
            .arg("run")
            .arg("--example")
            .arg("roundtrip")
            .arg("--quiet")
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            run.status.success() && String::from_utf8_lossy(&run.stdout).contains("ok"),
            "RPC + Datagrams round-trip driver failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The in-process round-trip driver for the transports package. It exercises the same
    /// generated codec + typed client + library envelope surfaces the emitted RPC and
    /// Datagrams sections ride, without a socket: CSIL-RPC through an in-process
    /// `RpcRequest`/`RpcResponse` echo, CSIL-Datagrams through the library's loopback
    /// datagram carrier. Prints `ok` on success.
    const RUST_TRANSPORTS_DRIVER: &str = r#"use csilgen_transport::carrier::{DatagramCarrier, LoopbackDatagramCarrier};
use csilgen_transport::datagrams::Datagram;
use csilgen_transport::rpc::{RpcRequest, RpcResponse};

use echo::*;

// A "server" on the far side of the dumb byte seam: it composes the request and reply
// through the library RPC envelope, proving RpcRequest/RpcResponse interop with the
// generated codec.
struct RpcEcho;

impl Transport for RpcEcho {
    fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError> {
        let envelope = RpcRequest::new(service, op, req.to_vec())
            .encode()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let decoded = RpcRequest::decode(&envelope)
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let ping = decode_ping(&decoded.payload).map_err(|e| ClientError::Transport(e.to_string()))?;
        let pong = Pong { msg: ping.msg };
        let resp = RpcResponse::ok("Pong", encode_pong(&pong))
            .encode()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let parsed = RpcResponse::decode(&resp)
            .map_err(|e| ClientError::Transport(e.to_string()))?
            .into_transport_error()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(parsed.payload)
    }
}

fn check(cond: bool, msg: &str) {
    if !cond {
        eprintln!("FAIL: {msg}");
        std::process::exit(1);
    }
}

fn main() {
    // CSIL-RPC: the typed client over the in-process library-envelope echo.
    let client = EchoClient::new(RpcEcho);
    let resp = client.ping(Ping { msg: "hello".to_string() }).expect("ping");
    check(resp.msg == "hello", "rpc round-trip msg");

    // CSIL-Datagrams: send via the library Datagram + generated codec over a loopback,
    // seed a response datagram, and decode it back into the typed response.
    let mut carrier = LoopbackDatagramCarrier::new();
    let req = Ping { msg: "example".to_string() };
    carrier
        .send_datagram(&Datagram::new(1, 0, encode_ping(&req)).encode().unwrap())
        .unwrap();
    carrier.push_inbound(
        Datagram::new(1, 0, encode_pong(&Pong { msg: "late".to_string() }))
            .encode()
            .unwrap(),
    );
    let inbound = carrier.recv_datagram().unwrap().expect("a seeded datagram");
    let dg = Datagram::decode(&inbound).unwrap();
    check(decode_pong(&dg.payload).unwrap().msg == "late", "datagram response decode");
    // The datagram we sent must round-trip through the generated codec too.
    let sent = carrier.take_outbound().expect("a sent datagram");
    let sent_dg = Datagram::decode(&sent).unwrap();
    check(decode_ping(&sent_dg.payload).unwrap().msg == "example", "datagram request round-trip");

    println!("ok");
}
"#;

    /// Driver `main` for the round-trip crate: it round-trips a corndogs request
    /// through the generated codec directly and through the typed client over a
    /// loopback transport, asserting the uuid/payload/absent-optional/map/list all
    /// survive, then prints `ok`.
    const RUST_CODEC_DRIVER: &str = r#"use std::collections::HashMap;

use csilroundtrip::*;

// A "server" on the far side of the dumb byte seam: it decodes the typed request,
// then encodes its task as the typed response, exercising decode and encode across
// the transport boundary.
struct Loopback;

impl Transport for Loopback {
    fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError> {
        if service != "CorndogsService" || op != "submit-task" {
            return Err(ClientError::Transport(format!("unexpected route {service}/{op}")));
        }
        let in_req = decode_submit_task_request(req)
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(encode_task(&in_req.task))
    }
}

fn check(cond: bool, msg: &str) {
    if !cond {
        eprintln!("FAIL: {msg}");
        std::process::exit(1);
    }
}

fn sample_task() -> Task {
    let mut labels = HashMap::new();
    labels.insert("a".to_string(), 1);
    labels.insert("b".to_string(), 2);
    // Named map aliases: a map-of-int and a map-of-record. The transparent `pub type`
    // alias is the underlying HashMap, so it is constructed and indexed in place.
    let mut queue_counts: StringInt64Map = HashMap::new();
    queue_counts.insert("q1".to_string(), 3);
    queue_counts.insert("q2".to_string(), 1);
    let mut state_counts: QueueAndStateCountsMap = HashMap::new();
    state_counts.insert("q1".to_string(), QueueAndStateCounts { count: 5 });
    Task {
        uuid: "u-123".to_string(),
        current_state: "PENDING".to_string(),
        payload: vec![0xde, 0xad, 0xbe],
        priority: Some(7),
        labels,
        tags: vec!["x".to_string(), "y".to_string()],
        queue_counts,
        state_counts,
    }
}

fn main() {
    let task = sample_task();
    let req = SubmitTaskRequest { task: task.clone(), queue: "default".to_string() };

    // Direct codec round-trip through the nested record.
    let back = decode_submit_task_request(&encode_submit_task_request(&req)).expect("decode req");
    check(back.task.uuid == "u-123", "uuid");
    check(back.task.payload == vec![0xde, 0xad, 0xbe], "payload");
    check(back.task.priority == Some(7), "priority");
    check(back.task.labels.get("a") == Some(&1) && back.task.labels.get("b") == Some(&2), "labels");
    check(back.task.tags == vec!["x".to_string(), "y".to_string()], "tags");
    check(back.queue == "default", "queue");
    // Named map aliases must round-trip their entries, not drop them (the regression).
    check(
        back.task.queue_counts.len() == 2
            && back.task.queue_counts.get("q1") == Some(&3)
            && back.task.queue_counts.get("q2") == Some(&1),
        "queue_counts map alias",
    );
    check(
        back.task.state_counts.len() == 1
            && back.task.state_counts.get("q1").map(|c| c.count) == Some(5),
        "state_counts map-of-record alias",
    );

    // An absent optional must round-trip to None, not a zero value.
    let mut task2 = task.clone();
    task2.priority = None;
    let req2 = SubmitTaskRequest { task: task2, queue: "q".to_string() };
    let back2 = decode_submit_task_request(&encode_submit_task_request(&req2)).expect("decode req2");
    check(back2.task.priority.is_none(), "absent optional None");

    // Typed client round-trip over the loopback carrier.
    let client = CorndogsClient::new(Loopback);
    let resp = client.submit_task(req).expect("client call");
    check(resp.uuid == "u-123", "client uuid");
    check(resp.payload == vec![0xde, 0xad, 0xbe], "client payload");
    check(resp.priority == Some(7), "client priority");
    check(resp.tags.len() == 2 && resp.tags[1] == "y", "client tags");

    println!("ok");
}
"#;

    /// Driver `main` for the mixed-kind-literal-enum round-trip crate: round-trips
    /// every declared literal of both kinds through the public per-record
    /// `encode_order`/`decode_order`, then proves an out-of-vocabulary value of a
    /// declared kind is rejected on decode. `csil_enc_order_status`/
    /// `csil_dec_order_status` are module-private (not `pub`), so this drives them
    /// indirectly through the one-field `Order` record's own public codec instead
    /// of calling them directly.
    const RUST_MIXED_ENUM_DRIVER: &str = r#"use csilroundtrip::*;

fn check(cond: bool, msg: &str) {
    if !cond {
        eprintln!("FAIL: {msg}");
        std::process::exit(1);
    }
}

fn main() {
    // Round-trip every declared literal of both kinds.
    for (status, label) in [
        (Order_status::Pending, "pending"),
        (Order_status::Shipped, "shipped"),
        (Order_status::V0, "v0"),
        (Order_status::V1, "v1"),
    ] {
        let order = Order { status: status.clone() };
        let bytes = encode_order(&order);
        let back = decode_order(&bytes).unwrap_or_else(|e| panic!("decode {label}: {e}"));
        check(back.status == status, &format!("round-trip {label}"));
    }

    // Out-of-vocabulary membership check: an unrecognized value of a DECLARED
    // kind (a text that is not "pending"/"shipped", an int that is not 0/1) must
    // be rejected on decode, not silently coerced. `Order` has exactly one
    // field, so its CBOR map is `{status: <literal>}`; reuse the real
    // map-header + key-header + key-bytes prefix from a valid encode (map(1) +
    // text key "status", 8 bytes) rather than hand-deriving that prefix's byte
    // layout independently, and swap in a hand-written CBOR value after it.
    let good = encode_order(&Order { status: Order_status::Pending });
    let prefix = &good[..8];

    let mut bad_text = prefix.to_vec();
    // CBOR text (major type 3), length 9: "cancelled" — not a declared literal.
    bad_text.push(0x69);
    bad_text.extend_from_slice(b"cancelled");
    check(decode_order(&bad_text).is_err(), "out-of-vocab text rejected");

    let mut bad_int = prefix.to_vec();
    // CBOR unsigned int (major type 0), value 2, inline in the header byte — not
    // a declared literal.
    bad_int.push(0x02);
    check(decode_order(&bad_int).is_err(), "out-of-vocab int rejected");

    println!("ok");
}
"#;

    /// Async driver `main` for the round-trip crate: it drives the generated
    /// `CorndogsAsyncClient` over an async loopback transport using a from-scratch,
    /// dependency-free `block_on`. The async seam completes without real I/O, so a
    /// no-op waker and a poll-to-ready loop suffice to await the future to a value.
    const RUST_ASYNC_DRIVER: &str = r#"use std::collections::HashMap;
use std::future::Future;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use csilroundtrip::*;

// A no-op waker: the generated async transport resolves without registering real
// wakeups, so cloning/waking it need do nothing.
fn noop_raw_waker() -> RawWaker {
    fn no_op(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        noop_raw_waker()
    }
    let vtable = &RawWakerVTable::new(clone, no_op, no_op, no_op);
    RawWaker::new(std::ptr::null(), vtable)
}

// A minimal executor: poll the future to completion. The loopback seam is always
// immediately ready, so this returns on the first poll without any runtime crate.
fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = Box::pin(fut);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

// The async transport seam: decode the typed request, then re-encode its task as
// the typed response across the dumb byte boundary — the sync loopback, but async.
struct Loopback;

impl AsyncTransport for Loopback {
    async fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError> {
        if service != "CorndogsService" || op != "submit-task" {
            return Err(ClientError::Transport(format!("unexpected route {service}/{op}")));
        }
        let in_req = decode_submit_task_request(req)
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(encode_task(&in_req.task))
    }
}

fn check(cond: bool, msg: &str) {
    if !cond {
        eprintln!("FAIL: {msg}");
        std::process::exit(1);
    }
}

fn sample_task() -> Task {
    let mut labels = HashMap::new();
    labels.insert("a".to_string(), 1);
    let mut queue_counts: StringInt64Map = HashMap::new();
    queue_counts.insert("q1".to_string(), 3);
    let mut state_counts: QueueAndStateCountsMap = HashMap::new();
    state_counts.insert("q1".to_string(), QueueAndStateCounts { count: 5 });
    Task {
        uuid: "u-123".to_string(),
        current_state: "PENDING".to_string(),
        payload: vec![0xde, 0xad, 0xbe],
        priority: Some(7),
        labels,
        tags: vec!["x".to_string(), "y".to_string()],
        queue_counts,
        state_counts,
    }
}

fn main() {
    let req = SubmitTaskRequest { task: sample_task(), queue: "default".to_string() };

    // Typed async client round-trip over the async loopback carrier, awaited to a
    // value by the hand-written executor.
    let client = CorndogsAsyncClient::new(Loopback);
    let resp = block_on(client.submit_task(req)).expect("client call");
    check(resp.uuid == "u-123", "client uuid");
    check(resp.payload == vec![0xde, 0xad, 0xbe], "client payload");
    check(resp.priority == Some(7), "client priority");
    check(resp.tags.len() == 2 && resp.tags[1] == "y", "client tags");

    println!("ok");
}
"#;
}
