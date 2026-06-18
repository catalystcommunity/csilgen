//! Emits `client.gen.ts`: a transport-agnostic, typed client.
//!
//! Per-direction emission shape:
//!
//! - `->` (unidirectional): a method on the per-service `XxxClient` class that
//!   calls `transport.call(...)` and returns `Promise<Output>`.
//! - `<->` (bidirectional) — connection mode (default): the service grows a
//!   `XxxChannelHandlers` interface (one method per inbound op), a
//!   `routeXxxChannel` router that decodes inbound frames and dispatches, and
//!   per-op `encodeXxxOp` outbound encoders. The wire (WebSocket/TCP) is the
//!   implementer's responsibility — the generator only produces handler shapes
//!   and routing.
//! - `<-` (reverse) — connection mode: receive-only. Inbound handler entry +
//!   router case, no encoder (server pushes; client only receives).
//! - `<->`/`<-` — rpc mode: degraded poll model. `checkXxxOp(): Promise<Out[]>`
//!   (drain) and (for `<->`) `sendXxxOp(req): Promise<void>` (post). Both ride
//!   `transport.call`.

use crate::{
    common::{self, BidiTransport, DecimalMapping},
    types,
};
use csilgen_common::{CsilServiceDefinition, CsilServiceOperation, WasmGeneratorInput};

const DEFAULT_TYPES_MODULE: &str = "./types.gen";
const DEFAULT_AGGREGATE: &str = "ApiClient";

const TRANSPORT: &str = "\
export interface ServiceTransport {
  call<TReq, TRes>(
    service: string,
    method: string,
    req: TReq,
    opts?: { signal?: AbortSignal },
  ): Promise<TRes>;
}
";

// Connection mode emits a Codec interface so the channel router can decode raw
// inbound frames. The server's Codec interface is structurally identical, so a
// single implementation satisfies both files.
const CODEC: &str = "\
export interface Codec {
  decode<T>(bytes: Uint8Array): T;
  encode(value: unknown): Uint8Array;
}
";

pub fn generate(input: &WasmGeneratorInput) -> Result<String, String> {
    let mode = common::bidi_transport(input)?;
    let mapping = common::decimal_mapping(input)?;
    let spec = &input.csil_spec;
    let services = common::sorted_services(spec);

    let mut out = common::header(input, "typescript-client");

    let declared = types::declared_type_names(spec);
    let mut imports: Vec<String> = common::referenced_types(&services)
        .into_iter()
        .filter(|t| declared.contains(t))
        .collect();
    let needs_service_error = mode == BidiTransport::Connection
        && services
            .iter()
            .any(|(_, def)| common::service_has_channel_ops(def));
    if needs_service_error {
        imports.push("ServiceError".to_string());
    }
    imports.sort();
    imports.dedup();
    if !imports.is_empty() {
        let module = string_option(input, "client_types_module", DEFAULT_TYPES_MODULE);
        out.push_str(&format!(
            "import type {{ {} }} from \"{module}\";\n\n",
            imports.join(", ")
        ));
    }

    // An inline `decimal` in an op signature makes `ts_type` print
    // `CsilDecimal`/`Decimal` straight into this file. The type-import block
    // above carries only named refs (never a builtin), so the value import is
    // injected here or the file references an undefined identifier.
    if common::services_use_decimal_inline(&services) {
        match mapping {
            DecimalMapping::Csil => {
                let module = string_option(input, "client_types_module", DEFAULT_TYPES_MODULE);
                out.push_str(&format!("import {{ CsilDecimal }} from \"{module}\";\n\n"));
            }
            DecimalMapping::Library => {
                out.push_str("import Decimal from \"decimal.js\";\n\n");
            }
        }
    }

    if let Some(url) = string_option_opt(input, "ts_ws_base_url") {
        // Pure hint: signals the implementer's intent to ride a WebSocket here.
        // The generator never opens this connection itself.
        out.push_str(&format!("export const WS_BASE_URL = {url:?};\n\n"));
    }

    out.push_str(TRANSPORT);
    out.push('\n');

    if mode == BidiTransport::Connection
        && services
            .iter()
            .any(|(_, def)| common::service_has_channel_ops(def))
    {
        out.push_str(CODEC);
        out.push('\n');
    }

    for (name, def) in &services {
        if let Some(class) = service_class(name, def, mode, mapping) {
            out.push_str(&class);
            out.push('\n');
        }
        if mode == BidiTransport::Connection && common::service_has_channel_ops(def) {
            out.push_str(&channel_block(name, def, mapping));
            out.push('\n');
        }
    }

    let aggregate = string_option(input, "aggregate_class_name", DEFAULT_AGGREGATE);
    let services_with_class: Vec<&(&str, &CsilServiceDefinition)> = services
        .iter()
        .filter(|(_, def)| service_class_has_methods(def, mode))
        .collect();
    if !aggregate.is_empty() && !services_with_class.is_empty() {
        out.push_str(&aggregate_class(&aggregate, &services_with_class));
    }

    Ok(out)
}

/// Whether a per-service client class would have any methods. Connection-mode
/// services with only `<->`/`<-` ops yield nothing here (their interactions go
/// through the channel block instead).
fn service_class_has_methods(def: &CsilServiceDefinition, mode: BidiTransport) -> bool {
    def.operations.iter().any(|op| match mode {
        BidiTransport::Connection => common::is_unidirectional(op),
        BidiTransport::Rpc => true, // all ops route through call() in rpc mode
    })
}

fn service_class(
    name: &str,
    def: &CsilServiceDefinition,
    mode: BidiTransport,
    mapping: DecimalMapping,
) -> Option<String> {
    if !service_class_has_methods(def, mode) {
        return None;
    }
    let class = format!("{}Client", common::service_base(name));
    let wire_service = common::service_wire(name);

    let mut out = format!("export class {class} {{\n");
    out.push_str("  constructor(private readonly t: ServiceTransport) {}\n");

    for op in &def.operations {
        match (mode, &op.direction) {
            (_, csilgen_common::CsilServiceDirection::Unidirectional) => {
                out.push('\n');
                out.push_str(&unary_method(op, &wire_service, mapping));
            }
            (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Bidirectional) => {
                out.push('\n');
                out.push_str(&rpc_send(op, &wire_service, mapping));
                out.push('\n');
                out.push_str(&rpc_check(op, &wire_service, mapping));
            }
            (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Reverse) => {
                out.push('\n');
                out.push_str(&rpc_check(op, &wire_service, mapping));
            }
            // Connection-mode bidi/reverse are emitted in channel_block instead
            (BidiTransport::Connection, _) => {}
        }
    }

    out.push_str("}\n");
    Some(out)
}

fn unary_method(op: &CsilServiceOperation, wire_service: &str, mapping: DecimalMapping) -> String {
    let method = common::to_camel(&op.name);
    let wire_method = common::method_wire(op);
    let res = common::ts_type(&common::success_type(&op.output_type), mapping);
    let throws = vec![
        "@throws {ServiceError} when the API returns an error response".to_string(),
        "@throws transport errors (network, timeout) defined by the transport".to_string(),
    ];
    let mut out = common::jsdoc(&op.doc_comments, &throws, "  ");
    // A push op (`-> Event`) has a `null` input: there is no request body, so the
    // request parameter is omitted rather than emitted as `req: null`, which would
    // force callers to pass a meaningless `null`.
    if common::is_null_type(&op.input_type) {
        out.push_str(&format!(
            "  {method}(opts?: {{ signal?: AbortSignal }}): Promise<{res}> {{\n"
        ));
        out.push_str(&format!(
            "    return this.t.call<undefined, {res}>(\"{wire_service}\", \"{wire_method}\", undefined, opts);\n"
        ));
        out.push_str("  }\n");
        return out;
    }
    let req = common::ts_type(&op.input_type, mapping);
    out.push_str(&format!(
        "  {method}(req: {req}, opts?: {{ signal?: AbortSignal }}): Promise<{res}> {{\n"
    ));
    out.push_str(&format!(
        "    return this.t.call<{req}, {res}>(\"{wire_service}\", \"{wire_method}\", req, opts);\n"
    ));
    out.push_str("  }\n");
    out
}

/// rpc-mode outbound: `send<Op>` posts an input over a synthetic method name.
fn rpc_send(op: &CsilServiceOperation, wire_service: &str, mapping: DecimalMapping) -> String {
    let camel = common::to_camel(&op.name);
    let wire_method = format!("{}Send", common::method_wire(op));
    let req = common::ts_type(&op.input_type, mapping);
    let mut out = String::new();
    out.push_str(&format!(
        "  send{}(req: {req}, opts?: {{ signal?: AbortSignal }}): Promise<void> {{\n",
        pascal_from_camel(&camel)
    ));
    out.push_str(&format!(
        "    return this.t.call<{req}, void>(\"{wire_service}\", \"{wire_method}\", req, opts);\n"
    ));
    out.push_str("  }\n");
    out
}

/// rpc-mode inbound: `check<Op>` drains the server's pending outbound queue.
fn rpc_check(op: &CsilServiceOperation, wire_service: &str, mapping: DecimalMapping) -> String {
    let camel = common::to_camel(&op.name);
    let wire_method = format!("{}Check", common::method_wire(op));
    let res = common::ts_type(&common::success_type(&op.output_type), mapping);
    let mut out = String::new();
    out.push_str(&format!(
        "  check{}(opts?: {{ signal?: AbortSignal }}): Promise<{res}[]> {{\n",
        pascal_from_camel(&camel)
    ));
    out.push_str(&format!(
        "    return this.t.call<undefined, {res}[]>(\"{wire_service}\", \"{wire_method}\", undefined, opts);\n"
    ));
    out.push_str("  }\n");
    out
}

fn pascal_from_camel(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Connection-mode emission for the channel ops of a single service:
/// inbound handler interface, router, and per-`<->`-op outbound encoders.
fn channel_block(name: &str, def: &CsilServiceDefinition, mapping: DecimalMapping) -> String {
    let base = common::service_base(name);
    let handlers_iface = format!("{base}ChannelHandlers");
    let wire_service = common::service_wire(name);

    let channel_ops: Vec<&CsilServiceOperation> = def
        .operations
        .iter()
        .filter(|op| !common::is_unidirectional(op))
        .collect();

    let mut out = String::new();

    // Handler interface: client receives output_type for both <-> and <-.
    out.push_str(&format!("export interface {handlers_iface} {{\n"));
    for op in &channel_ops {
        let method = common::to_camel(&op.name);
        let inbound = common::ts_type(&common::success_type(&op.output_type), mapping);
        out.push_str(&common::jsdoc(&op.doc_comments, &[], "  "));
        out.push_str(&format!("  {method}(msg: {inbound}): void;\n"));
    }
    out.push_str("}\n\n");

    // Router: feed inbound frames (method + bytes) in; we decode + dispatch.
    let route_fn = format!("route{}Channel", base);
    out.push_str(&format!(
        "/**\n\
         \x20* Dispatch one inbound frame for the {wire_service} channel. The implementer\n\
         \x20* (WebSocket adapter etc.) calls this for each message it pulls off the wire;\n\
         \x20* this generator never owns the connection itself.\n\
         \x20*/\n\
         export function {route_fn}(\n\
         \x20 handlers: {handlers_iface},\n\
         \x20 codec: Codec,\n\
         \x20 method: string,\n\
         \x20 bytes: Uint8Array,\n\
         ): void {{\n\
         \x20 switch (method) {{\n"
    ));
    for op in &channel_ops {
        let wire_method = common::method_wire(op);
        let method = common::to_camel(&op.name);
        let inbound = common::ts_type(&common::success_type(&op.output_type), mapping);
        out.push_str(&format!("    case \"{wire_method}\":\n"));
        out.push_str(&format!(
            "      handlers.{method}(codec.decode<{inbound}>(bytes));\n"
        ));
        out.push_str("      return;\n");
    }
    out.push_str("    default:\n");
    out.push_str(
        "      throw { code: 404, message: `unknown channel ${method}` } satisfies ServiceError;\n",
    );
    out.push_str("  }\n}\n");

    // Outbound encoders: only `<->` ops have a client-side outbound; reverse is
    // server-pushed and gets no encoder here.
    for op in &channel_ops {
        if !common::is_bidirectional(op) {
            continue;
        }
        let camel = common::to_camel(&op.name);
        let wire_method = common::method_wire(op);
        let outbound = common::ts_type(&op.input_type, mapping);
        let fn_name = format!("encode{base}{}", pascal_from_camel(&camel));
        out.push_str(&format!(
            "\n\
             /**\n\
             \x20* Encode an outbound `{wire_method}` message; hand the resulting bytes to\n\
             \x20* your connection. Returns `{{method, bytes}}` so the implementer can frame\n\
             \x20* both pieces however its protocol requires.\n\
             \x20*/\n\
             export function {fn_name}(codec: Codec, msg: {outbound}): {{ method: string; bytes: Uint8Array }} {{\n\
             \x20 return {{ method: \"{wire_method}\", bytes: codec.encode(msg) }};\n\
             }}\n"
        ));
    }

    out
}

fn aggregate_class(name: &str, services: &[&(&str, &CsilServiceDefinition)]) -> String {
    let mut out = format!("export class {name} {{\n");
    for (svc, _) in services {
        let field = common::to_camel(&common::service_base(svc));
        let class = format!("{}Client", common::service_base(svc));
        out.push_str(&format!("  readonly {field}: {class};\n"));
    }
    out.push_str("  constructor(t: ServiceTransport) {\n");
    for (svc, _) in services {
        let field = common::to_camel(&common::service_base(svc));
        let class = format!("{}Client", common::service_base(svc));
        out.push_str(&format!("    this.{field} = new {class}(t);\n"));
    }
    out.push_str("  }\n}\n");
    out
}

fn string_option(input: &WasmGeneratorInput, key: &str, default: &str) -> String {
    string_option_opt(input, key).unwrap_or_else(|| default.to_string())
}

fn string_option_opt(input: &WasmGeneratorInput, key: &str) -> Option<String> {
    input
        .config
        .options
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}
