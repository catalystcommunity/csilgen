//! Emits `server.gen.ts`: framework-agnostic handler interfaces and dispatch.
//!
//! Per-direction emission shape:
//!
//! - `->` (unidirectional): a method on the per-service `XxxHandlers`
//!   interface; routed by the `dispatch` function from raw bytes.
//! - `<->` (bidirectional) — connection mode (default): the service gets a
//!   `XxxChannelHandlers` interface (server-side inbound = input_type), a
//!   `routeXxxChannel` router, and `encodeXxxOp` outbound encoders for
//!   server-pushed output_type messages. The wire is the adapter's job.
//! - `<-` (reverse) — connection mode: server-pushed only. No inbound
//!   handler, just an `encodeXxxOp` for the server's outbound output_type.
//! - `<->`/`<-` — rpc mode: degraded poll model exposed via `dispatch` using
//!   synthetic `<Op>Send` / `<Op>Check` methods that route to per-op
//!   handler methods on `XxxHandlers`.

use crate::{
    common::{self, BidiTransport, DecimalMapping},
    types,
};
use csilgen_common::{CsilServiceDefinition, CsilServiceOperation, WasmGeneratorInput};

const DEFAULT_TYPES_MODULE: &str = "./types.gen";

const PREAMBLE: &str = "\
// The consumer-defined context carries auth, request id, etc. It is opaque to
// generated code; the adapter defines its shape.
export interface RequestContext {
  [key: string]: unknown;
}

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

    let mut out = common::header(input, "typescript-server");

    let declared = types::declared_type_names(spec);
    let mut imports: Vec<String> = common::referenced_types(&services)
        .into_iter()
        .filter(|t| declared.contains(t))
        .collect();
    imports.push("ServiceError".to_string());
    imports.sort();
    imports.dedup();
    let module = string_option(input, "client_types_module", DEFAULT_TYPES_MODULE);
    out.push_str(&format!(
        "import type {{ {} }} from \"{module}\";\n\n",
        imports.join(", ")
    ));

    // An inline `decimal` in an op signature makes `ts_type` print
    // `CsilDecimal`/`Decimal` straight into this file. The type-import block
    // above carries only named refs (never a builtin), so the value import is
    // injected here or the file references an undefined identifier.
    if common::services_use_decimal_inline(&services) {
        match mapping {
            DecimalMapping::Csil => {
                out.push_str(&format!("import {{ CsilDecimal }} from \"{module}\";\n\n"));
            }
            DecimalMapping::Library => {
                out.push_str("import Decimal from \"decimal.js\";\n\n");
            }
        }
    }

    if let Some(url) = string_option_opt(input, "ts_ws_base_url") {
        // Hint only — the generator never opens this connection itself.
        out.push_str(&format!("export const WS_BASE_URL = {url:?};\n\n"));
    }

    out.push_str(PREAMBLE);
    out.push('\n');

    for (name, def) in &services {
        out.push_str(&handlers_interface(name, def, mode, mapping));
        out.push('\n');
        if let Some(consts) = wire_ids_const(name, def) {
            out.push_str(&consts);
            out.push('\n');
        }
        if mode == BidiTransport::Connection && common::service_has_channel_ops(def) {
            out.push_str(&channel_block(name, def, mapping));
            out.push('\n');
        }
    }

    out.push_str(&server_handlers(&services));
    out.push('\n');
    out.push_str(&dispatch(&services, mode, mapping));

    // Compact-profile twin, emitted only when the spec carries wire-ids so
    // wire-id-free specs stay byte-identical.
    if let Some(compact) = dispatch_compact(&services, mapping) {
        out.push('\n');
        out.push_str(&compact);
    }

    Ok(out)
}

/// Emit a `XxxWireIds` const exposing the `@wire-id(N)` ordinals so a host can
/// reference them instead of hardcoding. Purely additive: returns `None` unless
/// the service carries a wire-id, keeping wire-id-free output byte-identical.
fn wire_ids_const(name: &str, def: &CsilServiceDefinition) -> Option<String> {
    let service_id = def.wire_id?;
    let const_name = format!("{}WireIds", common::service_base(name));
    let mut out = format!("export const {const_name} = {{\n");
    out.push_str(&format!("  service: {service_id},\n"));
    // Operations are nested under `ops` so an op named `service` keys into
    // `ops.service` and can never overwrite the top-level `service` ordinal.
    out.push_str("  ops: {\n");
    for op in &def.operations {
        if let Some(op_id) = op.wire_id {
            let key = common::to_camel(&op.name);
            out.push_str(&format!("    {key}: {op_id},\n"));
        }
    }
    out.push_str("  },\n");
    out.push_str("} as const;\n");
    Some(out)
}

fn handlers_interface(
    name: &str,
    def: &CsilServiceDefinition,
    mode: BidiTransport,
    mapping: DecimalMapping,
) -> String {
    let iface = format!("{}Handlers", common::service_base(name));
    let mut out = format!("export interface {iface} {{\n");
    for op in &def.operations {
        match (mode, &op.direction) {
            (_, csilgen_common::CsilServiceDirection::Unidirectional) => {
                out.push_str(&unary_handler_method(op, mapping));
            }
            (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Bidirectional) => {
                out.push_str(&rpc_send_handler_method(op, mapping));
                out.push_str(&rpc_check_handler_method(op, mapping));
            }
            (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Reverse) => {
                out.push_str(&rpc_check_handler_method(op, mapping));
            }
            // Channel ops in connection mode live in XxxChannelHandlers
            (BidiTransport::Connection, _) => {}
        }
    }
    out.push_str("}\n");
    out
}

fn unary_handler_method(op: &CsilServiceOperation, mapping: DecimalMapping) -> String {
    let method = common::to_camel(&op.name);
    let req = common::ts_type(&op.input_type, mapping);
    let res = common::ts_type(&common::success_type(&op.output_type), mapping);
    let mut out = common::jsdoc(&op.doc_comments, &[], "  ");
    out.push_str(&format!(
        "  {method}(req: {req}, ctx: RequestContext): Promise<{res}>;\n"
    ));
    out
}

/// rpc-mode handler: receives a message the client pushed via `send<Op>`.
fn rpc_send_handler_method(op: &CsilServiceOperation, mapping: DecimalMapping) -> String {
    let camel = common::to_camel(&op.name);
    let pascal = pascal_from_camel(&camel);
    let req = common::ts_type(&op.input_type, mapping);
    let mut out = common::jsdoc(&op.doc_comments, &[], "  ");
    out.push_str(&format!(
        "  send{pascal}(req: {req}, ctx: RequestContext): Promise<void>;\n"
    ));
    out
}

/// rpc-mode handler: drains the server's outbound queue for this op.
fn rpc_check_handler_method(op: &CsilServiceOperation, mapping: DecimalMapping) -> String {
    let camel = common::to_camel(&op.name);
    let pascal = pascal_from_camel(&camel);
    let res = common::ts_type(&common::success_type(&op.output_type), mapping);
    let mut out = common::jsdoc(&op.doc_comments, &[], "  ");
    out.push_str(&format!(
        "  check{pascal}(ctx: RequestContext): Promise<{res}[]>;\n"
    ));
    out
}

fn pascal_from_camel(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Connection-mode emission for the server side of channel ops:
/// inbound handlers (for `<->` only) + router + outbound encoders.
fn channel_block(name: &str, def: &CsilServiceDefinition, mapping: DecimalMapping) -> String {
    let base = common::service_base(name);
    let handlers_iface = format!("{base}ChannelHandlers");
    let wire_service = common::service_wire(name);

    let inbound_ops: Vec<&CsilServiceOperation> = def
        .operations
        .iter()
        .filter(|op| common::is_bidirectional(op))
        .collect();
    let outbound_ops: Vec<&CsilServiceOperation> = def
        .operations
        .iter()
        .filter(|op| common::is_bidirectional(op) || common::is_reverse(op))
        .collect();

    let mut out = String::new();

    // Inbound handler interface: only `<->` ops have a server-side inbound;
    // reverse is server-pushed and gets no method here. Emit even when empty
    // so the consumer's `route<Service>Channel` call always typechecks.
    out.push_str(&format!("export interface {handlers_iface} {{\n"));
    for op in &inbound_ops {
        let method = common::to_camel(&op.name);
        let inbound = common::ts_type(&op.input_type, mapping);
        out.push_str(&common::jsdoc(&op.doc_comments, &[], "  "));
        out.push_str(&format!(
            "  {method}(msg: {inbound}, ctx: RequestContext): void;\n"
        ));
    }
    out.push_str("}\n\n");

    // Router for inbound frames. If there are no inbound ops (a reverse-only
    // service) the switch is exhaustive on the empty set and any incoming
    // method is a protocol error.
    let route_fn = format!("route{base}Channel");
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
         \x20 ctx: RequestContext,\n\
         ): void {{\n\
         \x20 switch (method) {{\n"
    ));
    for op in &inbound_ops {
        let wire_method = common::method_wire(op);
        let method = common::to_camel(&op.name);
        let inbound = common::ts_type(&op.input_type, mapping);
        out.push_str(&format!("    case \"{wire_method}\":\n"));
        out.push_str(&format!(
            "      handlers.{method}(codec.decode<{inbound}>(bytes), ctx);\n"
        ));
        out.push_str("      return;\n");
    }
    out.push_str("    default:\n");
    out.push_str(
        "      throw { code: 404, message: `unknown channel ${method}` } satisfies ServiceError;\n",
    );
    out.push_str("  }\n}\n");

    // Compact-profile twin, emitted only for wire-id-bearing services so
    // wire-id-free specs stay byte-identical. It dispatches on the operation
    // ordinal instead of the wire method name; the profile is negotiated on the
    // wire (never declared in CSIL), so the implementer keeps both routers and
    // calls whichever the peer selected.
    if def.wire_id.is_some() {
        let route_fn_compact = format!("route{base}ChannelCompact");
        out.push_str(&format!(
            "\n\
             /**\n\
             \x20* Compact-profile twin of {route_fn}: dispatch one inbound frame by its\n\
             \x20* @wire-id ordinal instead of the wire method name. The host calls whichever\n\
             \x20* twin matches the profile negotiated on the wire.\n\
             \x20*/\n\
             export function {route_fn_compact}(\n\
             \x20 handlers: {handlers_iface},\n\
             \x20 codec: Codec,\n\
             \x20 op: number,\n\
             \x20 bytes: Uint8Array,\n\
             \x20 ctx: RequestContext,\n\
             ): void {{\n\
             \x20 switch (op) {{\n"
        ));
        for op in &inbound_ops {
            // The all-or-nothing wire-id rule (enforced by the validator) means a
            // bidirectional op on a wire-id-bearing service always has an ordinal.
            let Some(op_id) = op.wire_id else {
                continue;
            };
            let method = common::to_camel(&op.name);
            let inbound = common::ts_type(&op.input_type, mapping);
            out.push_str(&format!("    case {op_id}:\n"));
            out.push_str(&format!(
                "      handlers.{method}(codec.decode<{inbound}>(bytes), ctx);\n"
            ));
            out.push_str("      return;\n");
        }
        out.push_str("    default:\n");
        out.push_str(
            "      throw { code: 404, message: `unknown channel ordinal ${op}` } satisfies ServiceError;\n",
        );
        out.push_str("  }\n}\n");
    }

    // Server-side outbound encoders for `<->` and `<-` ops (output_type).
    for op in &outbound_ops {
        let camel = common::to_camel(&op.name);
        let wire_method = common::method_wire(op);
        let outbound = common::ts_type(&common::success_type(&op.output_type), mapping);
        let fn_name = format!("encode{base}{}", pascal_from_camel(&camel));
        out.push_str(&format!(
            "\n\
             /**\n\
             \x20* Encode an outbound `{wire_method}` message the server pushes to a peer;\n\
             \x20* the implementer frames `{{method, bytes}}` onto its connection.\n\
             \x20*/\n\
             export function {fn_name}(codec: Codec, msg: {outbound}): {{ method: string; bytes: Uint8Array }} {{\n\
             \x20 return {{ method: \"{wire_method}\", bytes: codec.encode(msg) }};\n\
             }}\n"
        ));
    }

    out
}

fn server_handlers(services: &[(&str, &CsilServiceDefinition)]) -> String {
    let mut out = String::from("export interface ServerHandlers {\n");
    for (svc, _) in services {
        let field = common::to_camel(&common::service_base(svc));
        let iface = format!("{}Handlers", common::service_base(svc));
        out.push_str(&format!("  {field}: {iface};\n"));
    }
    out.push_str("}\n");
    out
}

fn dispatch(
    services: &[(&str, &CsilServiceDefinition)],
    mode: BidiTransport,
    mapping: DecimalMapping,
) -> String {
    let mut out = String::from(
        "/**\n\
         \x20* Dispatch a single call. The caller decodes service+method from the wire,\n\
         \x20* passes raw request bytes plus a codec, and receives raw response bytes.\n\
         \x20* Throws ServiceError for unknown service/method routes.\n\
         \x20*/\n\
         export async function dispatch(\n\
         \x20 handlers: ServerHandlers,\n\
         \x20 codec: Codec,\n\
         \x20 service: string,\n\
         \x20 method: string,\n\
         \x20 reqBytes: Uint8Array,\n\
         \x20 ctx: RequestContext,\n\
         ): Promise<Uint8Array> {\n\
         \x20 switch (service) {\n",
    );

    for (name, def) in services {
        let wire_service = common::service_wire(name);
        let field = common::to_camel(&common::service_base(name));
        out.push_str(&format!("    case \"{wire_service}\": {{\n"));
        out.push_str("      switch (method) {\n");
        for op in &def.operations {
            match (mode, &op.direction) {
                (_, csilgen_common::CsilServiceDirection::Unidirectional) => {
                    let wire_method = common::method_wire(op);
                    let method = common::to_camel(&op.name);
                    let req = common::ts_type(&op.input_type, mapping);
                    out.push_str(&format!("        case \"{wire_method}\": {{\n"));
                    out.push_str(&format!(
                        "          const req = codec.decode<{req}>(reqBytes);\n"
                    ));
                    out.push_str(&format!(
                        "          const res = await handlers.{field}.{method}(req, ctx);\n"
                    ));
                    out.push_str("          return codec.encode(res);\n");
                    out.push_str("        }\n");
                }
                (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Bidirectional) => {
                    let camel = common::to_camel(&op.name);
                    let pascal = pascal_from_camel(&camel);
                    let req = common::ts_type(&op.input_type, mapping);
                    let wire = common::method_wire(op);
                    // <Op>Send: receive a client-pushed message
                    out.push_str(&format!("        case \"{wire}Send\": {{\n"));
                    out.push_str(&format!(
                        "          const req = codec.decode<{req}>(reqBytes);\n"
                    ));
                    out.push_str(&format!(
                        "          await handlers.{field}.send{pascal}(req, ctx);\n"
                    ));
                    out.push_str("          return codec.encode(null);\n");
                    out.push_str("        }\n");
                    // <Op>Check: drain the server's outbound queue
                    out.push_str(&format!("        case \"{wire}Check\": {{\n"));
                    out.push_str(&format!(
                        "          const res = await handlers.{field}.check{pascal}(ctx);\n"
                    ));
                    out.push_str("          return codec.encode(res);\n");
                    out.push_str("        }\n");
                }
                (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Reverse) => {
                    let camel = common::to_camel(&op.name);
                    let pascal = pascal_from_camel(&camel);
                    let wire = common::method_wire(op);
                    out.push_str(&format!("        case \"{wire}Check\": {{\n"));
                    out.push_str(&format!(
                        "          const res = await handlers.{field}.check{pascal}(ctx);\n"
                    ));
                    out.push_str("          return codec.encode(res);\n");
                    out.push_str("        }\n");
                }
                // Connection-mode channel ops aren't routed through dispatch
                (BidiTransport::Connection, _) => {}
            }
        }
        out.push_str("        default:\n");
        out.push_str(
            "          throw { code: 404, message: `unknown method ${service}.${method}` } satisfies ServiceError;\n",
        );
        out.push_str("      }\n");
        out.push_str("    }\n");
    }

    out.push_str("    default:\n");
    out.push_str(
        "      throw { code: 404, message: `unknown service ${service}` } satisfies ServiceError;\n",
    );
    out.push_str("  }\n}\n");
    out
}

/// The compact-profile twin of `dispatch`: the caller decodes the service and
/// operation `@wire-id` ordinals from the wire and routes on those instead of
/// names. Only unidirectional ops map to a single compact ordinal — channel
/// frames use `route<Service>ChannelCompact`, and the rpc-mode poll fallback
/// (`<Op>Send`/`<Op>Check`) has no single ordinal so it stays verbose. Returns
/// `None` for wire-id-free specs, keeping their output byte-identical.
fn dispatch_compact(
    services: &[(&str, &CsilServiceDefinition)],
    mapping: DecimalMapping,
) -> Option<String> {
    if !services.iter().any(|(_, def)| def.wire_id.is_some()) {
        return None;
    }
    let mut out = String::from(
        "/**\n\
         \x20* Compact-profile twin of dispatch: the caller decodes the service and\n\
         \x20* operation @wire-id ordinals from the wire and routes on those instead of\n\
         \x20* names. The host calls whichever twin matches the negotiated profile. The\n\
         \x20* rpc-mode poll fallback (<Op>Send/<Op>Check) has no single compact ordinal\n\
         \x20* and stays on the verbose `dispatch`.\n\
         \x20*/\n\
         export async function dispatchCompact(\n\
         \x20 handlers: ServerHandlers,\n\
         \x20 codec: Codec,\n\
         \x20 service: number,\n\
         \x20 method: number,\n\
         \x20 reqBytes: Uint8Array,\n\
         \x20 ctx: RequestContext,\n\
         ): Promise<Uint8Array> {\n\
         \x20 switch (service) {\n",
    );

    for (name, def) in services {
        let Some(service_id) = def.wire_id else {
            continue;
        };
        let field = common::to_camel(&common::service_base(name));
        out.push_str(&format!("    case {service_id}: {{\n"));
        out.push_str("      switch (method) {\n");
        for op in &def.operations {
            if !matches!(
                op.direction,
                csilgen_common::CsilServiceDirection::Unidirectional
            ) {
                continue;
            }
            // The all-or-nothing wire-id rule (enforced by the validator) means a
            // unidirectional op on a wire-id-bearing service always has an ordinal.
            let Some(op_id) = op.wire_id else {
                continue;
            };
            let method = common::to_camel(&op.name);
            let req = common::ts_type(&op.input_type, mapping);
            out.push_str(&format!("        case {op_id}: {{\n"));
            out.push_str(&format!(
                "          const req = codec.decode<{req}>(reqBytes);\n"
            ));
            out.push_str(&format!(
                "          const res = await handlers.{field}.{method}(req, ctx);\n"
            ));
            out.push_str("          return codec.encode(res);\n");
            out.push_str("        }\n");
        }
        out.push_str("        default:\n");
        out.push_str(
            "          throw { code: 404, message: `unknown ordinal ${service}.${method}` } satisfies ServiceError;\n",
        );
        out.push_str("      }\n");
        out.push_str("    }\n");
    }

    out.push_str("    default:\n");
    out.push_str(
        "      throw { code: 404, message: `unknown service ordinal ${service}` } satisfies ServiceError;\n",
    );
    out.push_str("  }\n}\n");
    Some(out)
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
