//! Shared helpers for the TypeScript emitters (types / client / server).

use csilgen_common::{
    CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence, CsilRule, CsilRuleType,
    CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation, CsilSpecSerialized,
    CsilTypeExpression, WasmGeneratorInput,
};
use std::collections::BTreeSet;

/// Bidirectional transport mode declared in the CSIL options block. Generators
/// emit shapes for *messages and routing*, never the wire — these values
/// describe how the consumer's implementation is going to wire up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiTransport {
    /// Long-lived connection (WebSocket/TCP). Channels are push-based:
    /// implementer feeds inbound frames into a router, sends outbound via the
    /// generated encoders. Default.
    Connection,
    /// Request-response only (no persistent channel). Bidirectional ops
    /// degrade to a `check<Op>` poll for inbound + `send<Op>` for outbound,
    /// both riding the existing `ServiceTransport.call`.
    Rpc,
}

/// Read & validate `ts_bidirectional_transport` from the CSIL options block.
/// Any non-`connection`/`rpc` value is rejected so misconfiguration surfaces
/// at generation time instead of silently degrading.
pub fn bidi_transport(input: &WasmGeneratorInput) -> Result<BidiTransport, String> {
    match input.config.options.get("ts_bidirectional_transport") {
        None => Ok(BidiTransport::Connection),
        Some(v) => match v.as_str() {
            Some("connection") => Ok(BidiTransport::Connection),
            Some("rpc") => Ok(BidiTransport::Rpc),
            Some(other) => Err(format!(
                "ts_bidirectional_transport must be \"connection\" or \"rpc\", got {other:?}"
            )),
            None => Err(format!(
                "ts_bidirectional_transport must be a string, got {v:?}"
            )),
        },
    }
}

/// In-memory mapping for the `decimal` core type. The wire form (CBOR tag 4)
/// is identical either way — this only selects the generated TypeScript type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalMapping {
    /// Emit a self-contained `CsilDecimal` helper. Pulls in no third-party
    /// dependency, so it is the default and works in any toolchain. Default.
    Csil,
    /// Use `Decimal` from `decimal.js`. The consumer must install that package;
    /// the generated `types.gen.ts` carries the `import` that requires it.
    Library,
}

/// Read & validate `decimal_mapping` from the CSIL options block. Mirrors
/// `bidi_transport`: any value other than `csil`/`library` is rejected at
/// generation time so misconfiguration fails loudly instead of silently
/// emitting the wrong in-memory type.
pub fn decimal_mapping(input: &WasmGeneratorInput) -> Result<DecimalMapping, String> {
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

/// Which client surface(s) to emit. The transport seam is the only thing that
/// turns async (it owns the I/O round-trip); the codec stays synchronous because
/// it never does I/O. `Both` is the default: every consumer gets the blocking
/// client they had plus an async twin, and can opt down to one shape explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientStyle {
    /// Blocking-only client at `client.gen.ts`. The host owns the I/O loop.
    Sync,
    /// Promise-returning client, a drop-in replacement at `client.gen.ts` (same
    /// symbol names). For hosts whose carrier is async (a browser `fetch`, etc.).
    Async,
    /// Emit both — the sync client at `client.gen.ts` and an async twin at
    /// `client.async.gen.ts` whose symbols carry an `Async` marker so the two
    /// coexist in one package (and one barrel) without name collisions. Default.
    Both,
}

/// Read & validate `client_style` from the CSIL options block. Mirrors
/// `bidi_transport`/`decimal_mapping`: any value other than `sync`/`async`/`both`
/// is rejected at generation time instead of silently degrading. Absent ->
/// `Both`, so the blocking client is preserved and the async twin comes for free.
pub fn client_style(input: &WasmGeneratorInput) -> Result<ClientStyle, String> {
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

/// How the generator writes relative specifiers between its own generated
/// modules (`./types.gen`, `./codec.gen`, ...). The wire format never depends on
/// this — it only changes what a consumer's module resolver sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportExtension {
    /// `./types.gen.ts` — resolvable by Node's ESM loader running the `.ts`
    /// sources directly (type stripping) and by `nodenext`/`node16`
    /// typechecking, which model that same runtime. Requires TypeScript 5.7's
    /// `allowImportingTsExtensions` (or `rewriteRelativeImportExtensions` for a
    /// build) on the consumer's side. Default — preserves the behavior
    /// requested in
    /// docs/csilgen-requests/typescript-codec-import-missing-extension.md.
    Ts,
    /// `./types.gen.js` — the specifier a `tsc`/bundler build actually emits on
    /// disk. Resolves under plain `nodenext`/`node16` on any TypeScript version
    /// without extension-rewriting flags — the pre-diff-compatible path for a
    /// bare (non-package) consumer — at the cost of pointing at a file that does
    /// not exist until the consumer's own build produces it, so running the
    /// `.ts` sources directly under type stripping does not work in this mode.
    Js,
    /// `./types.gen` — no extension. Only resolvable under `moduleResolution`
    /// modes that do not enforce Node ESM extension rules (`bundler`,
    /// `classic`, or plain `node`/CommonJS `require`); the generator's
    /// pre-existing behavior before extensioned specifiers were added.
    None,
}

impl ImportExtension {
    /// Build the relative specifier for a generated module from its
    /// extension-less stem (e.g. `"types.gen"`), so every emitter that imports
    /// another generated module (types/codec/client/server/index barrel) picks
    /// the same suffix. An explicit `*_module` option (`client_types_module`,
    /// `codec_types_module`, ...) is used verbatim and never passes through
    /// here — this only supplies the *default* specifier.
    pub fn specifier(&self, stem: &str) -> String {
        match self {
            ImportExtension::Ts => format!("./{stem}.ts"),
            ImportExtension::Js => format!("./{stem}.js"),
            ImportExtension::None => format!("./{stem}"),
        }
    }
}

/// Read & validate `import_extension` from the CSIL options block. Mirrors
/// `bidi_transport`/`decimal_mapping`/`client_style`: any value other than
/// `ts`/`js`/`none` is rejected at generation time instead of silently
/// degrading. Absent -> `Ts`, preserving the consumer-requested default (Node
/// ESM + `nodenext`, no workaround flags needed for `noEmit` typechecking). A
/// bare `csilgen generate` drop-in on an older TypeScript (or one without
/// `allowImportingTsExtensions`/`rewriteRelativeImportExtensions`), or a
/// consumer who only ever `tsc`-builds the raw sources, can opt down to `js` or
/// `none` to match a pre-existing project's module resolution instead.
pub fn import_extension(input: &WasmGeneratorInput) -> Result<ImportExtension, String> {
    match input.config.options.get("import_extension") {
        None => Ok(ImportExtension::Ts),
        Some(v) => match v.as_str() {
            Some("ts") => Ok(ImportExtension::Ts),
            Some("js") => Ok(ImportExtension::Js),
            Some("none") => Ok(ImportExtension::None),
            Some(other) => Err(format!(
                "import_extension must be \"ts\", \"js\", or \"none\", got {other:?}"
            )),
            None => Err(format!("import_extension must be a string, got {v:?}")),
        },
    }
}

/// The shape of one emitted client file: whether its methods are async and the
/// symbol marker that keeps an async twin distinct from the sync client when both
/// are emitted into the same package. `marker` is empty for a stand-alone client
/// (sync, or async-as-drop-in) and `"Async"` for the twin in `Both` mode.
#[derive(Debug, Clone, Copy)]
pub struct ClientShape {
    pub is_async: bool,
    pub marker: &'static str,
}

impl ClientShape {
    /// `async ` keyword (with trailing space) for method declarations, else empty.
    pub fn async_kw(&self) -> &'static str {
        if self.is_async { "async " } else { "" }
    }

    /// `await ` keyword (with trailing space) for transport calls, else empty.
    pub fn await_kw(&self) -> &'static str {
        if self.is_async { "await " } else { "" }
    }

    /// Wrap a method's return type in `Promise<...>` when async.
    pub fn ret(&self, ty: &str) -> String {
        if self.is_async {
            format!("Promise<{ty}>")
        } else {
            ty.to_string()
        }
    }

    /// The byte-transport interface name (`ServiceTransport`, or `AsyncServiceTransport`
    /// for the twin).
    pub fn transport_name(&self) -> String {
        format!("{}ServiceTransport", self.marker)
    }

    /// The structural `Codec` interface name used by connection-mode channels.
    pub fn codec_name(&self) -> String {
        format!("{}Codec", self.marker)
    }

    /// A per-service client class name (`FooClient`, or `FooAsyncClient` for the twin).
    pub fn class_name(&self, base: &str) -> String {
        format!("{base}{}Client", self.marker)
    }

    /// The aggregate client class name, marker-prefixed so the twin is distinct.
    pub fn aggregate_name(&self, configured: &str) -> String {
        format!("{}{configured}", self.marker)
    }
}

pub fn is_unidirectional(op: &CsilServiceOperation) -> bool {
    matches!(op.direction, CsilServiceDirection::Unidirectional)
}

pub fn is_bidirectional(op: &CsilServiceOperation) -> bool {
    matches!(op.direction, CsilServiceDirection::Bidirectional)
}

pub fn is_reverse(op: &CsilServiceOperation) -> bool {
    matches!(op.direction, CsilServiceDirection::Reverse)
}

/// True if the service has any operation that is not a plain request/response,
/// i.e. it needs the bidirectional channel emissions.
pub fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations.iter().any(|op| !is_unidirectional(op))
}

/// Standard `DO NOT EDIT` banner. `target` distinguishes the emitter.
pub fn header(input: &WasmGeneratorInput, target: &str) -> String {
    let source = input
        .csil_spec
        .source_content
        .as_deref()
        .map(|_| "<csil spec>")
        .unwrap_or("<csil spec>");
    format!(
        "// Code generated by csilgen. DO NOT EDIT.\n// Source: {source}\n// Target: {target}\n\n"
    )
}

/// Whether the spec declares at least one service.
pub fn has_services(spec: &CsilSpecSerialized) -> bool {
    spec.rules
        .iter()
        .any(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
}

/// Services sorted by rule name for deterministic output.
pub fn sorted_services(spec: &CsilSpecSerialized) -> Vec<(&str, &CsilServiceDefinition)> {
    let mut services: Vec<(&str, &CsilServiceDefinition)> = spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::ServiceDef(def) => Some((r.name.as_str(), def)),
            _ => None,
        })
        .collect();
    services.sort_by(|a, b| a.0.cmp(b.0));
    services
}

/// Map a CSIL type expression to a TypeScript type string. `mapping` selects the
/// in-memory type for `decimal` (it is threaded everywhere so an inline `decimal`
/// in an operation signature maps the same way a `decimal` struct field does).
pub fn ts_type(type_expr: &CsilTypeExpression, mapping: DecimalMapping) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => builtin(name, mapping),
        CsilTypeExpression::Reference(name) => to_pascal(name),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("{}[]", ts_type(element_type, mapping))
        }
        CsilTypeExpression::Map { value, .. } => {
            format!("Record<string, {}>", ts_type(value, mapping))
        }
        CsilTypeExpression::Choice(choices) => choices
            .iter()
            .map(|c| ts_type(c, mapping))
            .collect::<Vec<_>>()
            .join(" | "),
        CsilTypeExpression::Range { .. } => "number".to_string(),
        // Constrained types reduce to their base type in TypeScript
        CsilTypeExpression::Constrained { base_type, .. } => ts_type(base_type, mapping),
        CsilTypeExpression::Socket(name) | CsilTypeExpression::Plug(name) => to_pascal(name),
        // A fixed-shape array maps to a TS tuple type.
        CsilTypeExpression::Tuple(group) => tuple_type(group, mapping),
        // Inline groups are uncommon in operation signatures; fall back to a
        // permissive type so output still compiles.
        CsilTypeExpression::Group(_) => "object".to_string(),
        CsilTypeExpression::Literal(value) => ts_literal_type(value),
    }
}

/// Render a literal value as its TypeScript literal type. An enum-style choice of
/// literals (`"active" / "archived"`) is a `Choice` of `Literal`s, so rendering each
/// member precisely makes the union `"active" | "archived"` rather than the useless
/// `unknown | unknown` a blanket fallback produced — which both documents the wire
/// vocabulary and lets the value flow into the codec's `CborValue` without a cast.
pub fn ts_literal_type(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Text(s) => ts_string_literal(s),
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        // A byte-string or array literal has no TS literal-type spelling; keep a
        // permissive type so a spec that uses one still compiles.
        CsilLiteralValue::Bytes(_) => "Uint8Array".to_string(),
        CsilLiteralValue::Array(_) => "unknown[]".to_string(),
    }
}

// `choice_arm_literal` / `all_literal` / `classify_choice` are shared machinery
// now (see `csilgen_common::choice`, THE normative classification contract) —
// re-exported here so every existing `common::choice_arm_literal(...)` /
// `common::all_literal(...)` call site in this crate keeps working unchanged.
pub use csilgen_common::{all_literal, choice_arm_literal};

/// A TypeScript string-literal expression: the string wrapped in double quotes with
/// the JSON control characters escaped. Shared by type rendering and the codec so a
/// wire key or enum value is quoted identically everywhere.
pub fn ts_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Map a fixed-shape array (`[text, int]` / `[tag: text, value: any]`) to a
/// TypeScript tuple. When every entry carries a `Bare` key the result is a
/// labeled tuple so the shape stays self-documenting; otherwise it is positional.
/// TS requires *all* members labeled or none, so a mixed group falls back to
/// positional rather than emit an invalid `[a: T, T]`.
///
/// An optional entry may use the `?` suffix only when every element after it is
/// also optional — TS1257 forbids a required element following an optional one.
/// A non-trailing optional element is therefore rendered as a required slot that
/// admits `undefined` (`T | undefined`) so `[note?: text, id: int]` stays valid.
fn tuple_type(group: &CsilGroupExpression, mapping: DecimalMapping) -> String {
    let entries = &group.entries;
    let all_labeled = !entries.is_empty()
        && entries
            .iter()
            .all(|e| matches!(e.key, Some(CsilGroupKey::Bare(_))));
    let elems: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let ty = ts_type(&e.value_type, mapping);
            let optional = is_optional(&e.occurrence);
            // `?` is only legal when nothing required follows; otherwise keep the
            // slot required but let it accept `undefined`.
            let trailing_optional =
                optional && entries[i + 1..].iter().all(|e| is_optional(&e.occurrence));
            match (all_labeled, &e.key) {
                (true, Some(CsilGroupKey::Bare(label))) => {
                    let label = to_camel(label);
                    if trailing_optional {
                        format!("{label}?: {ty}")
                    } else if optional {
                        format!("{label}: {ty} | undefined")
                    } else {
                        format!("{label}: {ty}")
                    }
                }
                _ => {
                    if trailing_optional {
                        format!("{ty}?")
                    } else if optional {
                        format!("{ty} | undefined")
                    } else {
                        ty
                    }
                }
            }
        })
        .collect();
    format!("[{}]", elems.join(", "))
}

fn builtin(name: &str, mapping: DecimalMapping) -> String {
    match name {
        "text" | "string" => "string".to_string(),
        "int" | "uint" | "nint" | "integer" | "float" | "float16" | "float32" | "float64"
        | "double" | "number" => "number".to_string(),
        "bool" | "boolean" => "boolean".to_string(),
        "bytes" => "Uint8Array".to_string(),
        // `timestamp` is CBOR tag 0 on the wire; in TS it is a UTC-based Date.
        "timestamp" => "Date".to_string(),
        // `decimal` is CBOR tag 4 on the wire; the in-memory type is selectable.
        "decimal" => match mapping {
            DecimalMapping::Csil => "CsilDecimal".to_string(),
            DecimalMapping::Library => "Decimal".to_string(),
        },
        "null" => "null".to_string(),
        "any" => "any".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Does the spec use the `decimal` core type anywhere (so the generator must
/// inject the `CsilDecimal` helper or the `decimal.js` import)? Walks every rule
/// and nested type position.
pub fn spec_uses_decimal(spec: &CsilSpecSerialized) -> bool {
    spec.rules.iter().any(rule_uses_decimal)
}

fn rule_uses_decimal(rule: &CsilRule) -> bool {
    match &rule.rule_type {
        CsilRuleType::TypeDef(t) => type_uses_decimal(t),
        CsilRuleType::TypeChoice(choices) => choices.iter().any(type_uses_decimal),
        CsilRuleType::GroupDef(g) => group_uses_decimal(g),
        CsilRuleType::GroupChoice(groups) => groups.iter().any(group_uses_decimal),
        CsilRuleType::ServiceDef(def) => def
            .operations
            .iter()
            .any(|op| type_uses_decimal(&op.input_type) || type_uses_decimal(&op.output_type)),
    }
}

fn group_uses_decimal(group: &CsilGroupExpression) -> bool {
    group
        .entries
        .iter()
        .any(|e| type_uses_decimal(&e.value_type))
}

fn type_uses_decimal(type_expr: &CsilTypeExpression) -> bool {
    match type_expr {
        CsilTypeExpression::Builtin(name) => name == "decimal",
        CsilTypeExpression::Array { element_type, .. } => type_uses_decimal(element_type),
        CsilTypeExpression::Map { key, value, .. } => {
            type_uses_decimal(key) || type_uses_decimal(value)
        }
        CsilTypeExpression::Choice(choices) => choices.iter().any(type_uses_decimal),
        CsilTypeExpression::Constrained { base_type, .. } => type_uses_decimal(base_type),
        CsilTypeExpression::Group(group) | CsilTypeExpression::Tuple(group) => {
            group_uses_decimal(group)
        }
        CsilTypeExpression::Reference(_)
        | CsilTypeExpression::Range { .. }
        | CsilTypeExpression::Socket(_)
        | CsilTypeExpression::Plug(_)
        | CsilTypeExpression::Literal(_) => false,
    }
}

/// Whether any operation signature across these services places a `decimal`
/// inline (directly, not behind a named `Reference`). Such an inline `decimal`
/// makes `ts_type` emit `CsilDecimal`/`Decimal` straight into client.gen.ts /
/// server.gen.ts, yet `collect_type_refs` only yields named refs and never a
/// builtin, so without this the file would reference an undefined identifier.
/// Output types are reduced to their success form first to match the signatures
/// the emitters actually print.
pub fn services_use_decimal_inline(services: &[(&str, &CsilServiceDefinition)]) -> bool {
    services.iter().any(|(_, def)| {
        def.operations.iter().any(|op| {
            type_uses_decimal(&op.input_type) || type_uses_decimal(&success_type(&op.output_type))
        })
    })
}

/// Collect every user-defined type name referenced by a type expression.
pub fn collect_type_refs(type_expr: &CsilTypeExpression, out: &mut BTreeSet<String>) {
    match type_expr {
        CsilTypeExpression::Reference(name)
        | CsilTypeExpression::Socket(name)
        | CsilTypeExpression::Plug(name) => {
            out.insert(to_pascal(name));
        }
        CsilTypeExpression::Array { element_type, .. } => collect_type_refs(element_type, out),
        CsilTypeExpression::Map { key, value, .. } => {
            collect_type_refs(key, out);
            collect_type_refs(value, out);
        }
        CsilTypeExpression::Choice(choices) => {
            for c in choices {
                collect_type_refs(c, out);
            }
        }
        CsilTypeExpression::Constrained { base_type, .. } => collect_type_refs(base_type, out),
        CsilTypeExpression::Group(group) | CsilTypeExpression::Tuple(group) => {
            for entry in &group.entries {
                collect_type_refs(&entry.value_type, out);
            }
        }
        CsilTypeExpression::Builtin(_)
        | CsilTypeExpression::Range { .. }
        | CsilTypeExpression::Literal(_) => {}
    }
}

/// The transport-level error type name. When an operation's output is written
/// as `Success / ServiceError`, the error half is thrown rather than returned,
/// so it is stripped from the success type the client/server signatures use.
pub const SERVICE_ERROR: &str = "ServiceError";

/// Drop a top-level `ServiceError` member from a choice output. Domain error
/// types (anything not named `ServiceError`) are left in place because they are
/// returned values, not thrown.
pub fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !is_service_error(c))
            .cloned()
            .collect();
        match kept.len() {
            0 => type_expr.clone(),
            1 => kept.into_iter().next().unwrap(),
            _ => CsilTypeExpression::Choice(kept),
        }
    } else {
        type_expr.clone()
    }
}

fn is_service_error(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Reference(name) if to_pascal(name) == SERVICE_ERROR)
}

/// All type names referenced across the request/response signatures of the
/// given services, sorted for deterministic imports. Output types are reduced
/// to their success form first, so a thrown `ServiceError` is not imported.
pub fn referenced_types(services: &[(&str, &CsilServiceDefinition)]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for (_, def) in services {
        for op in &def.operations {
            collect_type_refs(&op.input_type, &mut set);
            collect_type_refs(&success_type(&op.output_type), &mut set);
        }
    }
    set.into_iter().collect()
}

/// Is this occurrence an optional field marker?
pub fn is_optional(occurrence: &Option<CsilOccurrence>) -> bool {
    matches!(occurrence, Some(CsilOccurrence::Optional))
}

/// PascalCase for type names (`house_id` -> `HouseID`-style is not attempted;
/// segments are simply capitalised: `house_id` -> `HouseId`).
pub fn to_pascal(name: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for c in name.chars() {
        if c == '_' || c == '-' {
            cap = true;
        } else if cap {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// camelCase for field and method names.
pub fn to_camel(name: &str) -> String {
    let pascal = to_pascal(name);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Strip a trailing `Service` suffix: `AuthService` -> `Auth`, `Auth` -> `Auth`.
pub fn service_base(name: &str) -> String {
    let pascal = to_pascal(name);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// The string passed to the transport for a service: the CSIL service rule name
/// verbatim, so a transport can place it on the wire unmodified (see
/// docs/cbor-wire-contract.md "RPC call naming"). Any lossy derivation (the old
/// lowercase-and-strip-Service) could not be reversed at the transport seam.
pub fn service_wire(name: &str) -> String {
    name.to_string()
}

/// The string passed to the transport for a method: the CSIL operation name
/// verbatim (kebab-case), matching the `op` field of the CSIL-RPC v1 envelope.
pub fn method_wire(op: &CsilServiceOperation) -> String {
    op.name.clone()
}

/// Render a JSDoc block from doc comments plus any extra trailing lines.
/// Returns an empty string when there is nothing to document.
pub fn jsdoc(doc_comments: &[String], extra: &[String], indent: &str) -> String {
    if doc_comments.is_empty() && extra.is_empty() {
        return String::new();
    }
    let mut out = format!("{indent}/**\n");
    for line in doc_comments {
        out.push_str(&format!("{indent} * {}\n", sanitize_block_comment(line)));
    }
    for line in extra {
        out.push_str(&format!("{indent} * {}\n", sanitize_block_comment(line)));
    }
    out.push_str(&format!("{indent} */\n"));
    out
}

/// Neutralize any `*/` inside text bound for a `/** ... */` block. A value
/// rendered verbatim into a JSDoc note (e.g. a `@depends-on` string literal
/// carrying `*/`) would otherwise close the comment early and break the source.
pub fn sanitize_block_comment(line: &str) -> String {
    line.replace("*/", "*\\/")
}

/// True when a type is the `null` builtin. A push op (`-> Event` / `<- Event`)
/// has a `null` input: there is no request body, so the request parameter is
/// omitted rather than typed `null`.
pub fn is_null_type(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null")
}
