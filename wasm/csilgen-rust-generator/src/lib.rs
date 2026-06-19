//! Rust code generator for CSIL specifications (WASM module)
//!
//! This generator produces idiomatic Rust code with serde serialization support,
//! service trait definitions, and proper handling of CSIL metadata.

use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSizeConstraint, CsilTypeExpression, CsilValidationConstraint,
    GeneratedFile, GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning,
    WarningLevel, WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};
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
/// TypeScript, and Python generators. That tag is produced via serde's tag
/// mechanism, which only a tag-aware CBOR codec honours: the generated crate
/// assumes `ciborium` (`ciborium::tag::Required<_, 4>`). A bare-array serialization
/// (what a plain `(i64, i128)` derive would emit) is NOT tag 4 and would not
/// interoperate, so `Serialize`/`Deserialize` are implemented by hand below.
#[derive(Debug, Clone, Copy)]
pub struct CsilDecimal {
    pub exponent: i64,
    pub mantissa: i128,
}

impl Serialize for CsilDecimal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ciborium::tag::Required::<(i64, i128), 4>((self.exponent, self.mantissa))
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CsilDecimal {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let ciborium::tag::Required::<(i64, i128), 4>((exponent, mantissa)) =
            ciborium::tag::Required::<(i64, i128), 4>::deserialize(deserializer)?;
        Ok(Self { exponent, mantissa })
    }
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
        while ma % 10 == 0 {
            ma /= 10;
            ea += 1;
        }
        while mb % 10 == 0 {
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

/// serde `with` adapters for `timestamp` fields, injected into `types.rs` only when
/// the spec uses `timestamp`. A UTC instant rides the wire as a CBOR **tag 0**
/// (RFC 3339 text, always normalized to a `Z` offset) and parses back the same way,
/// matching every other CSIL generator. The tag is produced through serde's tag
/// mechanism, which only a tag-aware CBOR codec honours — the generated crate
/// assumes `ciborium`. A plain `chrono` serde impl would emit an untagged string,
/// which is NOT tag 0, so these adapters wrap it explicitly.
const CSIL_TIMESTAMP: &str = r#"pub mod csil_timestamp {
    use serde::{Deserialize, Serialize};

    pub fn serialize<S: serde::Serializer>(
        value: &chrono::DateTime<chrono::Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // `true` forces the `Z` UTC offset the contract requires; `AutoSi` keeps
        // sub-second precision only when the instant actually carries it.
        let text = value.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
        ciborium::tag::Required::<String, 0>(text).serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<chrono::DateTime<chrono::Utc>, D::Error> {
        let ciborium::tag::Required::<String, 0>(text) =
            ciborium::tag::Required::<String, 0>::deserialize(deserializer)?;
        chrono::DateTime::parse_from_rfc3339(&text)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(serde::de::Error::custom)
    }
}

pub mod csil_timestamp_opt {
    use serde::Deserialize;

    pub fn serialize<S: serde::Serializer>(
        value: &Option<chrono::DateTime<chrono::Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(dt) => super::csil_timestamp::serialize(dt, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, D::Error> {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "super::csil_timestamp")] chrono::DateTime<chrono::Utc>);
        Ok(Option::<Wrap>::deserialize(deserializer)?.map(|w| w.0))
    }
}"#;

/// serde `with` adapters for `decimal` fields under `decimal_mapping = "library"`,
/// where the in-memory type is `rust_decimal::Decimal`. The value rides the wire as
/// a CBOR **tag 4** decimal fraction (`[exponent, mantissa]`), identical to the
/// `CsilDecimal` default mapping and to every other generator. The tag is produced
/// through serde's tag mechanism — the generated crate assumes `ciborium`.
const CSIL_DECIMAL_LIBRARY: &str = r#"pub mod csil_decimal {
    use serde::{Deserialize, Serialize};

    pub fn serialize<S: serde::Serializer>(
        value: &rust_decimal::Decimal,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // `rust_decimal` stores value = mantissa * 10^-scale, i.e. exponent = -scale.
        let exponent = -(value.scale() as i64);
        let mantissa = value.mantissa();
        ciborium::tag::Required::<(i64, i128), 4>((exponent, mantissa)).serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<rust_decimal::Decimal, D::Error> {
        let ciborium::tag::Required::<(i64, i128), 4>((exponent, mantissa)) =
            ciborium::tag::Required::<(i64, i128), 4>::deserialize(deserializer)?;
        // `rust_decimal`'s scale is non-negative, so a non-negative wire exponent
        // (trailing-zero magnitude) is folded into the mantissa rather than the scale.
        if exponent >= 0 {
            let pow = 10i128
                .checked_pow(exponent as u32)
                .ok_or_else(|| serde::de::Error::custom("decimal exponent too large"))?;
            let scaled = mantissa
                .checked_mul(pow)
                .ok_or_else(|| serde::de::Error::custom("decimal mantissa overflow"))?;
            Ok(rust_decimal::Decimal::from_i128_with_scale(scaled, 0))
        } else {
            Ok(rust_decimal::Decimal::from_i128_with_scale(
                mantissa,
                (-exponent) as u32,
            ))
        }
    }
}

pub mod csil_decimal_opt {
    use serde::Deserialize;

    pub fn serialize<S: serde::Serializer>(
        value: &Option<rust_decimal::Decimal>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(d) => super::csil_decimal::serialize(d, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<rust_decimal::Decimal>, D::Error> {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "super::csil_decimal")] rust_decimal::Decimal);
        Ok(Option::<Wrap>::deserialize(deserializer)?.map(|w| w.0))
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
        write!(f, "validation failed for `{}`: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}"#;

/// Shared client scaffolding emitted at the top of `client.rs`: the error type
/// and the caller-supplied `Transport` abstraction every per-service client
/// delegates to. The generator never owns the wire (CBOR-over-HTTP etc.).
const CLIENT_PRELUDE: &str = "\
/// Error from a generated client call: a structured error the service returned,
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
            ClientError::Service { code, message } => write!(f, \"service error {code}: {message}\"),
            ClientError::Transport(msg) => write!(f, \"transport error: {msg}\"),
        }
    }
}

impl std::error::Error for ClientError {}

/// The wire is the caller's concern: an implementation encodes `req` (CBOR over
/// HTTP, say), performs the call named by `(service, method)`, and decodes the
/// response into `Res`, or yields a `ClientError`.
pub trait Transport {
    fn call<Req, Res>(&self, service: &str, method: &str, req: &Req) -> Result<Res, ClientError>
    where
        Req: serde::Serialize,
        Res: serde::de::DeserializeOwned;
}
";

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
            input,
            warnings: Vec::new(),
            type_definitions: HashSet::new(),
            decimal_mapping: DecimalMapping::Csil,
            needs_validation_error: false,
            uses_regex: false,
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

        let mut files = Vec::new();

        // Generate types.rs for structs and enums
        let types_content = self.generate_types()?;
        if !types_content.is_empty() {
            files.push(GeneratedFile {
                path: "types.rs".to_string(),
                content: types_content,
            });
        }

        if self.input.csil_spec.service_count > 0 {
            match surface {
                Surface::Client => {
                    files.push(GeneratedFile {
                        path: "client.rs".to_string(),
                        content: self.generate_client()?,
                    });
                }
                Surface::Server => {
                    files.push(GeneratedFile {
                        path: "services.rs".to_string(),
                        content: self.generate_services()?,
                    });
                }
                Surface::TypesOnly => {}
            }
        }

        // Generate module root file to tie everything together
        let root_filename = self
            .input
            .config
            .options
            .get("module_root_filename")
            .and_then(|v| v.as_str())
            .unwrap_or("mod.rs")
            .to_string();
        let lib_content = self.generate_lib_file(&files)?;
        files.push(GeneratedFile {
            path: root_filename,
            content: lib_content,
        });

        Ok(files)
    }

    fn generate_types(&mut self) -> Result<String, String> {
        // Build the type bodies first: emitting structs is what sets the
        // `needs_validation_error` / `uses_regex` flags that decide which shared
        // helpers to inject ahead of them.
        let mut body = String::new();
        for rule in &self.input.csil_spec.rules {
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
        content.push_str("use serde::{Deserialize, Serialize};\n");

        if self.spec_has_bytes_fields() {
            content.push_str("use serde_bytes;\n");
        }

        content.push('\n');

        // The exact-decimal helper is emitted only when the spec uses `decimal`
        // under the default mapping; under `library` mode the type is
        // `rust_decimal::Decimal` and no helper is needed.
        if self.decimal_mapping == DecimalMapping::Csil && self.spec_uses_builtin("decimal") {
            content.push_str(CSIL_DECIMAL);
            content.push_str("\n\n");
        }

        // The tag-0 timestamp adapters are referenced by every `timestamp` field's
        // `#[serde(with = ...)]`, so inject them whenever the spec uses `timestamp`.
        if self.spec_uses_builtin("timestamp") {
            content.push_str(CSIL_TIMESTAMP);
            content.push_str("\n\n");
        }

        // Under `library` mapping the tag-4 adapters are referenced by every
        // `rust_decimal::Decimal` field's `#[serde(with = ...)]`; the default `csil`
        // mapping needs none because `CsilDecimal` carries its own tag-4 impl.
        if self.decimal_mapping == DecimalMapping::Library && self.spec_uses_builtin("decimal") {
            content.push_str(CSIL_DECIMAL_LIBRARY);
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
        let mut derive_attrs = vec!["Debug", "Clone", "Serialize", "Deserialize"];

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

                // Generate serde attributes based on metadata
                let serde_attrs = self.generate_serde_attributes(
                    &entry.metadata,
                    &entry.occurrence,
                    &entry.value_type,
                );
                if !serde_attrs.is_empty() {
                    content.push_str(&format!("    #[serde({})]\n", serde_attrs.join(", ")));
                }

                let rust_type = self.map_type_to_rust(&entry.value_type, &entry.occurrence)?;
                content.push_str(&format!("    pub {field_name}: {rust_type},\n"));
            } else if let Some(spread) = Self::group_spread_reference(&entry.value_type) {
                // A keyless entry that references another group is a CDDL-style
                // group spread. Rust structs cannot inline another struct's
                // fields, so surface the referenced group as a named field rather
                // than dropping it silently (which would leave the spread's fields
                // unrepresentable). The field is named after the referenced type.
                let field_name = self.to_snake_case(&spread);
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

    fn generate_enum(
        &mut self,
        name: &str,
        choices: &[CsilTypeExpression],
    ) -> Result<String, String> {
        let mut content = String::new();

        content.push_str(&format!("/// {name} enum variants\n"));
        content.push_str("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n");
        content.push_str("#[serde(untagged)]\n");
        content.push_str(&format!("pub enum {name} {{\n"));

        for (i, choice) in choices.iter().enumerate() {
            let variant_name = format!("Variant{i}");
            let rust_type = self.map_type_to_rust(choice, &None)?;
            content.push_str(&format!("    {variant_name}({rust_type}),\n"));
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
        let mut content = String::new();

        content.push_str("//! Generated service traits from CSIL specification\n\n");
        content.push_str("use super::types::*;\n\n");

        // Only emit the fallback `ServiceError` when the spec doesn't declare its
        // own; otherwise it collides with the type from `types.rs` (both are
        // re-exported through `mod.rs`). A spec-defined `ServiceError` is used
        // verbatim via the `use super::types::*` import above.
        if !self.spec_defines_service_error() {
            self.generate_service_error(&mut content);
            content.push('\n');
        }

        if self.spec_has_channel_ops() {
            self.generate_codec_trait(&mut content);
            content.push('\n');
        }

        for rule in &self.input.csil_spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let trait_code = self.generate_service_trait(&rule.name, service)?;
                content.push_str(&trait_code);
                content.push_str("\n\n");

                // Purely additive: only specs carrying `@wire-id(N)` ordinals get
                // a `wire_ids` module, so wire-id-free specs stay byte-identical.
                if let Some(wire_ids) = self.generate_wire_ids(&rule.name, service) {
                    content.push_str(&wire_ids);
                    content.push_str("\n\n");
                }

                if Self::service_has_channel_ops(service) {
                    content.push_str(&self.generate_service_router(&rule.name, service)?);
                    content.push('\n');
                    content.push_str(&self.generate_service_encoders(&rule.name, service)?);
                    content.push('\n');
                }
            }
        }

        Ok(content)
    }

    /// Emit `client.rs`: a transport-agnostic, typed client per service. Each
    /// unary operation becomes a method that hands `(service, method, req)` to a
    /// caller-supplied `Transport` and returns the typed success response, with
    /// the `/ ServiceError` half surfaced through `ClientError`.
    fn generate_client(&mut self) -> Result<String, String> {
        let mut content = String::new();

        content.push_str(
            "//! Generated transport-agnostic service clients from CSIL specification\n\n",
        );
        content.push_str("use super::types::*;\n\n");

        content.push_str(CLIENT_PRELUDE);
        content.push('\n');

        for rule in &self.input.csil_spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let client_code = self.generate_client_struct(&rule.name, service)?;
                content.push_str(&client_code);
                content.push_str("\n\n");
            }
        }

        Ok(content)
    }

    fn generate_client_struct(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let base = Self::service_base(name);
        let client = format!("{base}Client");

        let mut content = String::new();
        content.push_str(&format!("/// Typed client for the {name} service.\n"));
        content.push_str(&format!("pub struct {client}<T: Transport> {{\n"));
        content.push_str("    transport: T,\n");
        content.push_str("}\n\n");

        content.push_str(&format!("impl<T: Transport> {client}<T> {{\n"));
        content.push_str("    pub fn new(transport: T) -> Self {\n");
        content.push_str("        Self { transport }\n");
        content.push_str("    }\n");

        let wire_service = base.to_lowercase();
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
            let method = self.to_snake_case(&operation.name);
            let wire_method = Self::to_pascal_case(&operation.name);
            let output_type =
                self.map_type_to_rust(&success_type(&operation.output_type), &None)?;
            content.push('\n');
            Self::write_op_doc(&mut content, operation, "request/response");
            // A push-style op (`op: -> Event`) takes no request payload: emit a
            // parameterless method and send the empty `()` value on the wire.
            if is_null_input(&operation.input_type) {
                content.push_str(&format!(
                    "    pub fn {method}(&self) -> Result<{output_type}, ClientError> {{\n"
                ));
                content.push_str(&format!(
                    "        self.transport.call(\"{wire_service}\", \"{wire_method}\", &())\n"
                ));
                content.push_str("    }\n");
            } else {
                let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                content.push_str(&format!(
                    "    pub fn {method}(&self, req: {input_type}) -> Result<{output_type}, ClientError> {{\n"
                ));
                content.push_str(&format!(
                    "        self.transport.call(\"{wire_service}\", \"{wire_method}\", &req)\n"
                ));
                content.push_str("    }\n");
            }
        }

        content.push('}');
        Ok(content)
    }

    /// Strip a trailing `Service` suffix and PascalCase the remainder, matching
    /// the wire service base used across the TypeScript/Go/Python clients.
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
        self.input.csil_spec.rules.iter().any(|r| {
            r.name == "ServiceError" && !matches!(r.rule_type, CsilRuleType::ServiceDef(_))
        })
    }

    fn spec_has_channel_ops(&self) -> bool {
        self.input
            .csil_spec
            .rules
            .iter()
            .any(|r| match &r.rule_type {
                CsilRuleType::ServiceDef(def) => Self::service_has_channel_ops(def),
                _ => false,
            })
    }

    fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
        def.operations
            .iter()
            .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
    }

    /// The codec abstraction the user supplies for the message-routing layer.
    /// Same shape across all language targets that emit a router/encoder pair:
    /// the generator never owns serialization or transport, only types and
    /// dispatch.
    fn generate_codec_trait(&self, code: &mut String) {
        code.push_str("/// User-supplied (de)serialization for channel messages. The generator\n");
        code.push_str("/// is codec-agnostic; the implementer wires this to CBOR, JSON, or\n");
        code.push_str("/// anything else its protocol expects.\n");
        code.push_str("pub trait Codec {\n");
        code.push_str("    fn encode<T: serde::Serialize>(&self, value: &T) -> Result<Vec<u8>, ServiceError>;\n");
        code.push_str("    fn decode<T: serde::de::DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, ServiceError>;\n");
        code.push_str("}\n");
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
            let op_name = self.to_snake_case(&operation.name);
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
                        content.push_str(&format!(
                            "    fn {op_name}(&self, ctx: &Self::Context) -> Result<{output_type}, ServiceError>;\n",
                        ));
                    } else {
                        let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                        content.push_str(&format!(
                            "    fn {op_name}(&self, ctx: &Self::Context, input: {input_type}) -> Result<{output_type}, ServiceError>;\n",
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
                        content.push_str(&format!(
                            "    fn {op_name}(&self, ctx: &Self::Context) -> Result<(), ServiceError>;\n",
                        ));
                    } else {
                        let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                        content.push_str(&format!(
                            "    fn {op_name}(&self, ctx: &Self::Context, msg: {input_type}) -> Result<(), ServiceError>;\n",
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
        let mod_name = format!("{}_wire_ids", self.to_snake_case(name));
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
                let const_name = self.to_snake_case(&operation.name).to_ascii_uppercase();
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

    /// For services with any `<->` op, emit `route_<service>_channel` that
    /// decodes inbound bytes (keyed by the wire method name) and dispatches
    /// to the trait method. Reverse ops never have an inbound route.
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

        let mut content = String::new();
        let fn_name = format!("route_{}_channel", self.to_snake_case(service_name));
        content.push_str(&format!(
            "/// Decode one inbound channel frame for {service_name} and dispatch\n\
             /// to the matching trait method. The implementer feeds raw bytes\n\
             /// from its connection here; we never own the wire.\n\
             pub fn {fn_name}<H, C>(\n\
             \x20   handlers: &H,\n\
             \x20   ctx: &H::Context,\n\
             \x20   codec: &C,\n\
             \x20   method: &str,\n\
             \x20   bytes: &[u8],\n\
             ) -> Result<(), ServiceError>\n\
             where\n\
             \x20   H: {service_name},\n\
             \x20   C: Codec,\n\
             {{\n\
             \x20   match method {{\n"
        ));
        for op in &inbound_ops {
            let op_snake = self.to_snake_case(&op.name);
            let input_type = self.map_type_to_rust(&op.input_type, &None)?;
            let wire = Self::pascal_case(&op.name);
            content.push_str(&format!("        \"{wire}\" => {{\n"));
            content.push_str(&format!(
                "            let msg: {input_type} = codec.decode(bytes)?;\n"
            ));
            content.push_str(&format!("            handlers.{op_snake}(ctx, msg)\n"));
            content.push_str("        }\n");
        }
        content.push_str("        other => Err(ServiceError {\n");
        content.push_str("            code: 404,\n");
        content.push_str("            message: format!(\"unknown channel {other}\"),\n");
        content.push_str("        }),\n");
        content.push_str("    }\n");
        content.push_str("}\n");
        Ok(content)
    }

    /// For each `<->` and `<-` op, emit `encode_<service>_<op>` that returns
    /// `(method, bytes)` for the implementer to put on the wire. Unidirectional
    /// ops already have a return value from their trait method, so no encoder.
    fn generate_service_encoders(
        &mut self,
        service_name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let mut content = String::new();
        let svc_snake = self.to_snake_case(service_name);
        for op in &service.operations {
            if !matches!(
                op.direction,
                CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
            ) {
                continue;
            }
            let op_snake = self.to_snake_case(&op.name);
            let wire = Self::pascal_case(&op.name);
            let output_type = self.map_type_to_rust(&op.output_type, &None)?;
            let fn_name = format!("encode_{svc_snake}_{op_snake}");
            content.push_str(&format!(
                "/// Encode a `{wire}` message pushed from {service_name}'s server\n\
                 /// side; the implementer frames `(method, bytes)` onto its connection.\n\
                 pub fn {fn_name}<C: Codec>(codec: &C, msg: &{output_type}) -> Result<(String, Vec<u8>), ServiceError> {{\n\
                 \x20   Ok((\"{wire}\".to_string(), codec.encode(msg)?))\n\
                 }}\n"
            ));
        }
        Ok(content)
    }

    fn pascal_case(s: &str) -> String {
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

    fn generate_lib_file(&mut self, files: &[GeneratedFile]) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("//! Generated Rust code from CSIL specification\n\n");

        // This generator emits no Cargo.toml, so the crates the generated code
        // needs beyond serde are documented here for the consuming crate to add.
        let mut deps: Vec<&str> = Vec::new();
        if self.spec_uses_builtin("timestamp") {
            deps.push("//! chrono = { version = \"0.4\", features = [\"serde\"] }");
        }
        if self.spec_uses_builtin("decimal") && self.decimal_mapping == DecimalMapping::Library {
            deps.push("//! rust_decimal = { version = \"1\", features = [\"serde\"] }");
        }
        // `decimal` (tag 4) and `timestamp` (tag 0) ride CBOR semantic tags emitted
        // through serde's tag mechanism, which only a tag-aware codec honours. This
        // generated code targets `ciborium`; a codec that drops tags will not
        // interoperate with the Go/TypeScript/Python parties.
        if self.spec_uses_builtin("decimal") || self.spec_uses_builtin("timestamp") {
            deps.push("//! ciborium = \"0.2\"  # CBOR codec assumed for decimal (tag 4) / timestamp (tag 0)");
        }
        if self.uses_regex {
            deps.push("//! regex = \"1\"");
        }
        if !deps.is_empty() {
            content.push_str("//! ## Additional dependencies for the consuming crate\n//!\n");
            for line in deps {
                content.push_str(line);
                content.push('\n');
            }
            content.push_str("//!\n");
        }

        // Add module declarations
        if files.iter().any(|f| f.path == "types.rs") {
            content.push_str("pub mod types;\n");
            content.push_str("pub use types::*;\n\n");
        }

        if files.iter().any(|f| f.path == "services.rs") {
            content.push_str("pub mod services;\n");
            content.push_str("pub use services::*;\n\n");
        }

        if files.iter().any(|f| f.path == "client.rs") {
            content.push_str("pub mod client;\n");
            content.push_str("pub use client::*;\n\n");
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
            Some(CsilGroupKey::Bare(name)) => Some(self.to_snake_case(name)),
            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                Some(self.to_snake_case(name))
            }
            _ => None,
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
            "any" => "serde_json::Value".to_string(),
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

    fn generate_serde_attributes(
        &self,
        metadata: &[CsilFieldMetadata],
        occurrence: &Option<CsilOccurrence>,
        value_type: &CsilTypeExpression,
    ) -> Vec<String> {
        let mut attrs = Vec::new();

        for meta in metadata {
            match meta {
                CsilFieldMetadata::Visibility(visibility) => {
                    match visibility {
                        CsilFieldVisibility::SendOnly => {
                            attrs.push("skip_deserializing".to_string());
                        }
                        CsilFieldVisibility::ReceiveOnly => {
                            attrs.push("skip_serializing".to_string());
                        }
                        _ => {} // Bidirectional is default
                    }
                }
                CsilFieldMetadata::Custom { name, parameters } if name == "rust" => {
                    for param in parameters {
                        if let Some(param_name) = &param.name
                            && let CsilLiteralValue::Text(value) = &param.value
                        {
                            attrs.push(format!("{param_name} = \"{value}\""));
                        }
                    }
                }
                _ => {}
            }
        }

        let optional = matches!(occurrence, Some(CsilOccurrence::Optional));

        // `bytes`/`timestamp`/`decimal` each need a custom serde codec to hit their
        // CBOR wire form (major-type-2 byte string, tag 0, tag 4); a bare derive
        // would emit an int array / untagged string / bare array instead.
        if let Some(module) = self.custom_with_module(value_type, optional) {
            attrs.push(format!("with = \"{module}\""));
        }

        if optional {
            // A custom `with` loses serde's automatic "missing -> None", so any field
            // carrying one must also opt into `default` to keep an absent field None.
            if self.custom_with_module(value_type, optional).is_some() {
                attrs.push("default".to_string());
            }
            attrs.push("skip_serializing_if = \"Option::is_none\"".to_string());
        }

        attrs
    }

    /// The serde `with` module a field needs to reach its CBOR wire form, if any.
    /// `bytes` uses `serde_bytes`; `timestamp` and (under `library` mapping)
    /// `decimal` use the injected tag-0 / tag-4 adapters, which split into a bare
    /// and an `_opt` variant because their function signatures differ by `Option`.
    /// The default `csil` decimal mapping returns `None`: `CsilDecimal` carries its
    /// own tag-4 `Serialize`/`Deserialize`, and serde handles `Option<CsilDecimal>`.
    fn custom_with_module(
        &self,
        value_type: &CsilTypeExpression,
        optional: bool,
    ) -> Option<&'static str> {
        if Self::is_bytes_type(value_type) {
            // `serde_bytes` handles both the bare and `Option` shapes itself.
            return Some("serde_bytes");
        }
        if Self::is_timestamp_type(value_type) {
            return Some(if optional {
                "csil_timestamp_opt"
            } else {
                "csil_timestamp"
            });
        }
        if self.decimal_mapping == DecimalMapping::Library && Self::is_decimal_type(value_type) {
            return Some(if optional {
                "csil_decimal_opt"
            } else {
                "csil_decimal"
            });
        }
        None
    }

    fn is_timestamp_type(type_expr: &CsilTypeExpression) -> bool {
        Self::is_timestamp_shape(Self::value_base(type_expr))
    }

    fn is_decimal_type(type_expr: &CsilTypeExpression) -> bool {
        Self::is_decimal_shape(Self::value_base(type_expr))
    }

    fn is_bytes_type(type_expr: &CsilTypeExpression) -> bool {
        match type_expr {
            CsilTypeExpression::Builtin(name) => matches!(name.as_str(), "bytes" | "bstr"),
            CsilTypeExpression::Constrained { base_type, .. } => Self::is_bytes_type(base_type),
            _ => false,
        }
    }

    fn spec_has_bytes_fields(&self) -> bool {
        self.input.csil_spec.rules.iter().any(|rule| {
            if let CsilRuleType::GroupDef(group) = &rule.rule_type {
                group
                    .entries
                    .iter()
                    .any(|e| Self::is_bytes_type(&e.value_type))
            } else {
                false
            }
        })
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
                    checks.push(Self::len_check(
                        field_name,
                        &format!("v.len() < {n}usize"),
                        &format!("length is below minimum {n}"),
                    ));
                }
                CsilValidationConstraint::MaxLength(n) | CsilValidationConstraint::MaxItems(n)
                    if len_shape =>
                {
                    checks.push(Self::len_check(
                        field_name,
                        &format!("v.len() > {n}usize"),
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
                &format!("v.len() != {n}usize"),
                &format!("length must equal {n}"),
            )),
            CsilSizeConstraint::Range { min, max } => checks.push(Self::len_check(
                field,
                &format!("v.len() < {min}usize || v.len() > {max}usize"),
                &format!("length must be in {min}..={max}"),
            )),
            CsilSizeConstraint::Min(n) => checks.push(Self::len_check(
                field,
                &format!("v.len() < {n}usize"),
                &format!("length is below minimum {n}"),
            )),
            CsilSizeConstraint::Max(n) => checks.push(Self::len_check(
                field,
                &format!("v.len() > {n}usize"),
                &format!("length is above maximum {n}"),
            )),
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
        let parse = match self.decimal_mapping {
            DecimalMapping::Csil => format!("CsilDecimal::from_str({text:?})"),
            DecimalMapping::Library => format!("{text:?}.parse::<rust_decimal::Decimal>()"),
        };
        Some(format!(
            "            {{\n                let bound = {parse}\n                    .map_err(|e| ValidationError {{ field: {field:?}.to_string(), message: format!(\"invalid decimal bound: {{e}}\") }})?;\n                if *v {fail_op} bound {{\n                    return Err(ValidationError {{ field: {field:?}.to_string(), message: {msg:?}.to_string() }});\n                }}\n            }}\n"
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
            "            {{\n                let bound = chrono::DateTime::parse_from_rfc3339({text:?})\n                    .map_err(|e| ValidationError {{ field: {field:?}.to_string(), message: format!(\"invalid timestamp bound: {{e}}\") }})?\n                    .with_timezone(&chrono::Utc);\n                if *v {fail_op} bound {{\n                    return Err(ValidationError {{ field: {field:?}.to_string(), message: {msg:?}.to_string() }});\n                }}\n            }}\n"
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
        let cond = format!("(*v as f64) {fail_op} {rendered}");
        checks.push(Self::len_check(field, &cond, msg));
    }

    fn is_integer_shape(base: &CsilTypeExpression) -> bool {
        matches!(base, CsilTypeExpression::Builtin(n) if matches!(n.as_str(), "int" | "uint"))
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
            "            if {cond} {{\n                return Err(ValidationError {{ field: \"{field}\".to_string(), message: \"{msg}\".to_string() }});\n            }}\n"
        )
    }

    /// A regex guard whose `regex::Regex` is compiled once via a `OnceLock` static
    /// rather than rebuilt on every `validate()` call. The pattern is a spec
    /// constant, so an invalid one still surfaces as a `ValidationError` (not a
    /// panic) by caching the `Result` and re-borrowing it on each call.
    fn regex_check(field: &str, pattern: &str, idx: usize) -> String {
        let static_name = format!("RE_{}_{idx}", field.to_ascii_uppercase());
        format!(
            "            {{\n                static {static_name}: std::sync::OnceLock<Result<regex::Regex, regex::Error>> = std::sync::OnceLock::new();\n                let re = {static_name}.get_or_init(|| regex::Regex::new({pattern:?}));\n                let re = re.as_ref().map_err(|e| ValidationError {{ field: \"{field}\".to_string(), message: format!(\"invalid regex: {{e}}\") }})?;\n                if !re.is_match(v) {{\n                    return Err(ValidationError {{ field: \"{field}\".to_string(), message: \"value does not match required pattern\".to_string() }});\n                }}\n            }}\n"
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
        self.input
            .csil_spec
            .rules
            .iter()
            .any(|rule| match &rule.rule_type {
                CsilRuleType::GroupDef(g) | CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => {
                    g.entries
                        .iter()
                        .any(|e| Self::type_mentions_builtin(&e.value_type, target))
                }
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

    fn to_snake_case(&self, s: &str) -> String {
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
        assert!(types_content.contains("#[serde(skip_deserializing"));
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
        let input = create_test_input();
        let generator = RustCodeGenerator::new(&input);

        assert_eq!(generator.to_snake_case("CamelCase"), "camel_case");
        // Acronym runs stay intact: the boundary lands where a new word starts.
        assert_eq!(generator.to_snake_case("HTTPResponse"), "http_response");
        assert_eq!(
            generator.to_snake_case("GetTaskStateByID"),
            "get_task_state_by_id"
        );
        assert_eq!(generator.to_snake_case("simple"), "simple");
        assert_eq!(generator.to_snake_case("create-entry"), "create_entry");
        assert_eq!(
            generator.to_snake_case("MyService-operation"),
            "my_service_operation"
        );
        assert_eq!(generator.to_snake_case("a--b"), "a__b");
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
            "fn create_user(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));
    }

    fn make_unary_service_input(target: &str) -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.config.target = target.to_string();
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
            "fn submit_task(&self, ctx: &Self::Context, input: SubmitTaskRequest) -> Result<SubmitTaskResponse, ServiceError>"
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
        assert!(client.content.contains("pub trait Transport"));
        assert!(client.content.contains("pub enum ClientError"));
        assert!(
            client
                .content
                .contains("pub struct CorndogsClient<T: Transport>")
        );
        assert!(client.content.contains(
            "pub fn submit_task(&self, req: SubmitTaskRequest) -> Result<SubmitTaskResponse, ClientError>"
        ));
        assert!(
            client
                .content
                .contains("self.transport.call(\"corndogs\", \"SubmitTask\", &req)")
        );
        // The server surface must not leak into the client target.
        assert!(!files.iter().any(|f| f.path == "services.rs"));
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
            "fn create_entry(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));
        assert!(!services_content.contains("fn create-entry("));
        assert!(services_content.contains(
            "fn list_entries(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
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

        // Codec trait emitted once at the top of the file.
        assert!(services.contains("pub trait Codec"), "codec trait expected");

        // Unidirectional kept as request/response.
        assert!(services.contains(
            "fn list_events(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));

        // Bidirectional is a fire-and-forget inbound handler (no return value).
        assert!(services.contains(
            "fn play(&self, ctx: &Self::Context, msg: User) -> Result<(), ServiceError>"
        ));

        // Router decodes the inbound bytes and dispatches by wire method name.
        assert!(services.contains("pub fn route_match_channel<H, C>"));
        assert!(services.contains("\"Play\" => {"));
        assert!(services.contains("handlers.play(ctx, msg)"));

        // Outbound encoder for the bidirectional op.
        assert!(services.contains(
            "pub fn encode_match_play<C: Codec>(codec: &C, msg: &User) -> Result<(String, Vec<u8>), ServiceError>"
        ));
        assert!(services.contains("(\"Play\".to_string(), codec.encode(msg)?)"));
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
        assert!(!router_block.contains("\"Notify\" =>"));

        // The encoder for the reverse op (server pushes Output to the client).
        assert!(services.contains(
            "pub fn encode_callbacks_notify<C: Codec>(codec: &C, msg: &User) -> Result<(String, Vec<u8>), ServiceError>"
        ));
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
        assert!(types_content.contains("#[serde(untagged)]"));
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
        assert!(types_content.contains("skip_serializing_if = \"Option::is_none\""));
        // optional non-bytes already round-trips via serde's Option special-casing,
        // so it must not gain `default`.
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

        assert!(types_content.contains("pub key_signature: Option<Vec<u8>>"));
        // A custom `with` disables serde's "missing -> None", so all three must be present.
        let field_pos = types_content.find("pub key_signature:").unwrap();
        let attrs = &types_content[field_pos.saturating_sub(160)..field_pos];
        assert!(
            attrs.contains("default"),
            "optional bytes must emit `default`"
        );
        assert!(attrs.contains("with = \"serde_bytes\""));
        assert!(attrs.contains("skip_serializing_if = \"Option::is_none\""));
    }

    // Reproduces the serde semantics behind the fix: a custom `with`/`deserialize_with`
    // disables serde's automatic "missing Option field -> None", so the bytes-shaped
    // module below stands in for the generated `with = "serde_bytes"`. This is the exact
    // regression that broke linkkeys `DomainPublicKey.key_signature` deserialization.
    mod bytes_with {
        use serde::{Deserialize, Deserializer};

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Vec<u8>>, D::Error> {
            Option::<Vec<u8>>::deserialize(deserializer)
        }
    }

    #[test]
    fn test_optional_bytes_missing_field_round_trips_with_default() {
        #[derive(serde::Deserialize)]
        struct Fixed {
            #[serde(default, with = "bytes_with", skip_serializing_if = "Option::is_none")]
            key_signature: Option<Vec<u8>>,
        }

        #[derive(serde::Deserialize)]
        struct WithoutDefault {
            #[serde(with = "bytes_with", skip_serializing_if = "Option::is_none")]
            #[allow(dead_code)]
            key_signature: Option<Vec<u8>>,
        }

        // A signing key omits `key_signature` entirely.
        let json = "{}";

        // With `default` (what the generator now emits) the field becomes None.
        let fixed: Fixed = serde_json::from_str(json).unwrap();
        assert_eq!(fixed.key_signature, None);

        // Without `default` the custom `with` turns a missing field into a hard error,
        // which is precisely the production breakage the request describes.
        assert!(serde_json::from_str::<WithoutDefault>(json).is_err());
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
        // id and created_at should have skip_serializing, name should not
        let id_section = &types_content
            [types_content.find("pub id:").unwrap() - 80..types_content.find("pub id:").unwrap()];
        assert!(
            id_section.contains("skip_serializing"),
            "receive-only field 'id' should have skip_serializing"
        );
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
        assert!(root.content.contains("chrono = { version = \"0.4\""));
    }

    #[test]
    fn timestamp_field_emits_tag0_serde_with_and_module() {
        let input = single_field_spec(
            "Event",
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![],
            None,
        );
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let types = files.iter().find(|f| f.path == "types.rs").unwrap();

        // The field carries the tag-0 adapter and the adapter module is injected.
        assert!(
            types
                .content
                .contains("#[serde(with = \"csil_timestamp\")]")
        );
        assert!(types.content.contains("pub mod csil_timestamp {"));
        assert!(
            types
                .content
                .contains("ciborium::tag::Required::<String, 0>")
        );
        assert!(types.content.contains("to_rfc3339_opts"));

        // The ciborium codec assumption is documented in the dep note.
        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(root.content.contains("ciborium = \"0.2\""));
    }

    #[test]
    fn optional_timestamp_field_uses_opt_adapter_and_default() {
        let input = single_field_spec(
            "Event",
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![],
            Some(CsilOccurrence::Optional),
        );
        let mut generator = RustCodeGenerator::new(&input);
        let types = generator.generate_types().unwrap();
        assert!(types.contains("pub field: Option<chrono::DateTime<chrono::Utc>>"));
        let field_pos = types.find("pub field:").unwrap();
        let attrs = &types[field_pos.saturating_sub(200)..field_pos];
        // A custom `with` disables serde's missing -> None, so `default` is required.
        assert!(attrs.contains("with = \"csil_timestamp_opt\""));
        assert!(attrs.contains("default"));
        assert!(attrs.contains("skip_serializing_if = \"Option::is_none\""));
        assert!(types.contains("pub mod csil_timestamp_opt {"));
    }

    #[test]
    fn library_decimal_field_emits_tag4_serde_with_and_module() {
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
        assert!(types.content.contains("#[serde(with = \"csil_decimal\")]"));
        assert!(types.content.contains("pub mod csil_decimal {"));
        assert!(
            types
                .content
                .contains("ciborium::tag::Required::<(i64, i128), 4>")
        );
        // No `CsilDecimal` helper in library mode.
        assert!(!types.content.contains("pub struct CsilDecimal"));

        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(root.content.contains("ciborium = \"0.2\""));
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
        // The helper now hand-rolls tag-4 serde rather than a bare `(i64, i128)`
        // array, so the old `into`/`from` shorthand must be gone.
        assert!(!types.contains("#[serde(into = \"(i64, i128)\", from = \"(i64, i128)\")]"));
        assert!(types.contains("impl Serialize for CsilDecimal"));
        assert!(types.contains("impl<'de> Deserialize<'de> for CsilDecimal"));
        assert!(types.contains("ciborium::tag::Required::<(i64, i128), 4>"));
        assert!(types.contains("pub fn from_str"));
        assert!(types.contains("pub fn as_str"));

        // The ciborium codec assumption is documented in the dep note.
        let files = RustCodeGenerator::new(&input).generate().unwrap();
        let root = files.iter().find(|f| f.path == "mod.rs").unwrap();
        assert!(root.content.contains("ciborium = \"0.2\""));

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
        assert!(root.content.contains("rust_decimal = { version = \"1\""));
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
    fn float_field_bounds_still_compare_as_f64() {
        // A genuinely floating field keeps the f64 comparison path.
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

    /// Mirrors the emitted CBOR tag wrappers — `CsilDecimal`'s hand-rolled tag-4
    /// serde, the `csil_timestamp` tag-0 adapter, and the library-mode `csil_decimal`
    /// tag-4 adapter — so the actual wire bytes are exercised here against the same
    /// `ciborium` codec the generated crate assumes. Kept identical to the
    /// `CSIL_DECIMAL` / `CSIL_TIMESTAMP` / `CSIL_DECIMAL_LIBRARY` source bodies.
    mod cbor_tag_wire {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct CsilDecimal {
            pub exponent: i64,
            pub mantissa: i128,
        }

        impl Serialize for CsilDecimal {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                ciborium::tag::Required::<(i64, i128), 4>((self.exponent, self.mantissa))
                    .serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for CsilDecimal {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let ciborium::tag::Required::<(i64, i128), 4>((exponent, mantissa)) =
                    ciborium::tag::Required::<(i64, i128), 4>::deserialize(deserializer)?;
                Ok(Self { exponent, mantissa })
            }
        }

        pub mod csil_timestamp {
            use serde::{Deserialize, Serialize};

            pub fn serialize<S: serde::Serializer>(
                value: &chrono::DateTime<chrono::Utc>,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                let text = value.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
                ciborium::tag::Required::<String, 0>(text).serialize(serializer)
            }

            pub fn deserialize<'de, D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<chrono::DateTime<chrono::Utc>, D::Error> {
                let ciborium::tag::Required::<String, 0>(text) =
                    ciborium::tag::Required::<String, 0>::deserialize(deserializer)?;
                chrono::DateTime::parse_from_rfc3339(&text)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(serde::de::Error::custom)
            }
        }

        pub mod csil_decimal_lib {
            use serde::{Deserialize, Serialize};

            pub fn serialize<S: serde::Serializer>(
                value: &rust_decimal::Decimal,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                let exponent = -(value.scale() as i64);
                let mantissa = value.mantissa();
                ciborium::tag::Required::<(i64, i128), 4>((exponent, mantissa))
                    .serialize(serializer)
            }

            pub fn deserialize<'de, D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<rust_decimal::Decimal, D::Error> {
                let ciborium::tag::Required::<(i64, i128), 4>((exponent, mantissa)) =
                    ciborium::tag::Required::<(i64, i128), 4>::deserialize(deserializer)?;
                if exponent >= 0 {
                    let pow = 10i128
                        .checked_pow(exponent as u32)
                        .ok_or_else(|| serde::de::Error::custom("decimal exponent too large"))?;
                    let scaled = mantissa
                        .checked_mul(pow)
                        .ok_or_else(|| serde::de::Error::custom("decimal mantissa overflow"))?;
                    Ok(rust_decimal::Decimal::from_i128_with_scale(scaled, 0))
                } else {
                    Ok(rust_decimal::Decimal::from_i128_with_scale(
                        mantissa,
                        (-exponent) as u32,
                    ))
                }
            }
        }
    }

    #[test]
    fn csil_decimal_round_trips_as_cbor_tag4() {
        use cbor_tag_wire::CsilDecimal;
        let value = CsilDecimal {
            exponent: -2,
            mantissa: 12345,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&value, &mut buf).unwrap();
        // First byte is the CBOR tag-4 head (major type 6, tag value 4): 0xc4.
        assert_eq!(buf[0], 0xc4, "decimal must serialize under CBOR tag 4");
        let back: CsilDecimal = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(back, value, "decimal must survive encode -> decode");
    }

    #[test]
    fn timestamp_round_trips_as_cbor_tag0() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder(
            #[serde(with = "cbor_tag_wire::csil_timestamp")] chrono::DateTime<chrono::Utc>,
        );

        let when = chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let holder = Holder(when);
        let mut buf = Vec::new();
        ciborium::into_writer(&holder, &mut buf).unwrap();
        // First byte is the CBOR tag-0 head (major type 6, tag value 0): 0xc0.
        assert_eq!(buf[0], 0xc0, "timestamp must serialize under CBOR tag 0");
        let back: Holder = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(back, holder, "timestamp must survive encode -> decode");
    }

    #[test]
    fn library_decimal_round_trips_as_cbor_tag4() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder(#[serde(with = "cbor_tag_wire::csil_decimal_lib")] rust_decimal::Decimal);

        // 123.45 carried as mantissa 12345 with scale 2 (exponent -2).
        let value = rust_decimal::Decimal::from_i128_with_scale(12345, 2);
        let holder = Holder(value);
        let mut buf = Vec::new();
        ciborium::into_writer(&holder, &mut buf).unwrap();
        assert_eq!(
            buf[0], 0xc4,
            "library decimal must serialize under CBOR tag 4"
        );
        let back: Holder = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(
            back.0, value,
            "library decimal must survive encode -> decode"
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
        assert!(types_content.contains("pub type Keyed = (String, serde_json::Value);"));
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
                .contains("self.transport.call(\"events\", \"Heartbeat\", &())"),
            "null-input client must send the empty `()` payload"
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
}
