//! Emits `client.gen.ts`: a typed client over a dumb byte transport seam.
//!
//! The client owns (de)serialization via the generated codec; the caller-supplied
//! `ServiceTransport` only moves bytes (`call(service, op, req) -> bytes`). A
//! unary method encodes its request, calls the transport, and decodes the response.
//! The `ClientShape` selects the seam: a sync client returns the decoded value
//! directly (the host owns the I/O loop), an async client `await`s a
//! `Promise`-returning seam and returns a `Promise` (for a `fetch`/WebSocket host).
//! A record request/response uses its byte-level `to<T>Cbor`/`from<T>Cbor`; any other
//! shape (a scalar id, a bare array, a scalar/array/map alias) is encoded/decoded via
//! the codec's generic CBOR, so every op gets a method — none is silently dropped.
//!
//! Per-direction emission shape:
//!
//! - `->` (unidirectional): a method on the per-service `XxxClient` class that
//!   encodes -> `transport.call(...)` -> decodes, returning the success record.
//! - `<->` (bidirectional) — connection mode (default): the service grows a
//!   `XxxChannelHandlers` interface (one method per inbound op), a
//!   `routeXxxChannel` router that decodes inbound frames and dispatches, and
//!   per-op `encodeXxxOp` outbound encoders. The wire (WebSocket/TCP) is the
//!   implementer's responsibility — the generator only produces handler shapes
//!   and routing.
//! - `<-` (reverse) — connection mode: receive-only. Inbound handler entry +
//!   router case, no encoder (server pushes; client only receives).
//! - `<->`/`<-` — rpc mode: degraded poll model. `checkXxxOp(): Out[]` (drain,
//!   decoding the returned CBOR array) and (for `<->`) `sendXxxOp(req): void`
//!   (post, codec-encoded). Both ride the byte-seam `transport.call`.

use crate::{
    codec,
    common::{self, BidiTransport, ClientShape, DecimalMapping},
    types,
};
use csilgen_common::{
    CsilServiceDefinition, CsilServiceOperation, CsilSpecSerialized, WasmGeneratorInput,
};
use std::collections::{BTreeSet, HashSet};

const TYPES_STEM: &str = "types.gen";
const CODEC_STEM: &str = "codec.gen";
const DEFAULT_AGGREGATE: &str = "ApiClient";

// The dumb byte transport seam: the caller-owned carrier performs the call named by
// `(service, op)` with the already-encoded request bytes and returns the response
// bytes. The generated client owns (de)serialization via the codec; the carrier only
// moves bytes. The sync seam returns the bytes directly (the host owns the I/O loop);
// the async seam returns a `Promise` so a `fetch`/WebSocket carrier fits unchanged.
fn transport_iface(shape: ClientShape) -> String {
    let name = shape.transport_name();
    let ret = if shape.is_async {
        "Promise<Uint8Array>"
    } else {
        "Uint8Array"
    };
    format!(
        "export interface {name} {{\n  call(service: string, op: string, req: Uint8Array): {ret};\n}}\n"
    )
}

// Connection mode emits a Codec interface so the channel router can decode raw
// inbound frames. The server's Codec interface is structurally identical, so a
// single implementation satisfies both files. The codec never does I/O, so it stays
// synchronous in both client shapes — only the transport seam turns async.
fn codec_iface(shape: ClientShape) -> String {
    let name = shape.codec_name();
    format!(
        "export interface {name} {{\n  decode<T>(bytes: Uint8Array): T;\n  encode(value: unknown): Uint8Array;\n}}\n"
    )
}

pub fn generate(input: &WasmGeneratorInput, shape: ClientShape) -> Result<String, String> {
    let mode = common::bidi_transport(input)?;
    let mapping = common::decimal_mapping(input)?;
    let ext = common::import_extension(input)?;
    let spec = &input.csil_spec;
    let services = common::sorted_services(spec);

    let mut out = common::header(input, "typescript-client");

    // Emit the per-service classes (and any channel blocks) first so the import
    // headers can name exactly the types and codec helpers actually referenced.
    let mut codec_imports: BTreeSet<String> = BTreeSet::new();
    let mut body = String::new();
    for (name, def) in &services {
        if let Some(class) =
            service_class(spec, name, def, mode, mapping, shape, &mut codec_imports)
        {
            body.push_str(&class);
            body.push('\n');
        }
        if mode == BidiTransport::Connection && common::service_has_channel_ops(def) {
            body.push_str(&channel_block(name, def, mapping, shape));
            body.push('\n');
        }
    }

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
    let mut any_import = false;
    if !imports.is_empty() {
        let default_types_module = ext.specifier(TYPES_STEM);
        let module = string_option(input, "client_types_module", &default_types_module);
        out.push_str(&format!(
            "import type {{ {} }} from \"{module}\";\n",
            imports.join(", ")
        ));
        any_import = true;
    }
    // The typed methods call the generated `to<T>Cbor`/`from<T>Cbor` (and, for the
    // rpc-mode poll path, the value-tree helpers); import exactly those from the codec.
    if !codec_imports.is_empty() {
        let default_codec_module = ext.specifier(CODEC_STEM);
        let module = string_option(input, "client_codec_module", &default_codec_module);
        out.push_str(&format!(
            "import {{ {} }} from \"{module}\";\n",
            codec_imports.into_iter().collect::<Vec<_>>().join(", ")
        ));
        any_import = true;
    }
    if any_import {
        out.push('\n');
    }

    // Emitted by the sync (or async drop-in) client only; the `Both`-mode twin
    // carries a marker, and the unmarked sibling already exports this hint.
    let ws_base_url = shape
        .marker
        .is_empty()
        .then(|| string_option_opt(input, "ts_ws_base_url"))
        .flatten();
    if let Some(url) = ws_base_url {
        // Pure hint: signals the implementer's intent to ride a WebSocket here.
        // The generator never opens this connection itself.
        out.push_str(&format!("export const WS_BASE_URL = {url:?};\n\n"));
    }

    out.push_str(&transport_iface(shape));
    out.push('\n');

    if mode == BidiTransport::Connection
        && services
            .iter()
            .any(|(_, def)| common::service_has_channel_ops(def))
    {
        out.push_str(&codec_iface(shape));
        out.push('\n');
    }

    out.push_str(&body);

    let aggregate = string_option(input, "aggregate_class_name", DEFAULT_AGGREGATE);
    let services_with_class: Vec<&(&str, &CsilServiceDefinition)> = services
        .iter()
        .filter(|(_, def)| service_class_has_methods(def, mode))
        .collect();
    if !aggregate.is_empty() && !services_with_class.is_empty() {
        out.push_str(&aggregate_class(&aggregate, &services_with_class, shape));
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
    spec: &CsilSpecSerialized,
    name: &str,
    def: &CsilServiceDefinition,
    mode: BidiTransport,
    mapping: DecimalMapping,
    shape: ClientShape,
    codec_imports: &mut BTreeSet<String>,
) -> Option<String> {
    if !service_class_has_methods(def, mode) {
        return None;
    }
    let records = codec::record_names(spec);
    let records = &records;
    let class = shape.class_name(&common::service_base(name));
    let transport = shape.transport_name();
    let wire_service = common::service_wire(name);

    let mut out = format!("export class {class} {{\n");
    out.push_str(&format!(
        "  constructor(private readonly t: {transport}) {{}}\n"
    ));

    for op in &def.operations {
        match (mode, &op.direction) {
            (_, csilgen_common::CsilServiceDirection::Unidirectional) => {
                out.push('\n');
                out.push_str(&unary_method(
                    spec,
                    op,
                    &wire_service,
                    mapping,
                    shape,
                    records,
                    codec_imports,
                ));
            }
            (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Bidirectional) => {
                out.push('\n');
                out.push_str(&rpc_send(
                    spec,
                    op,
                    &wire_service,
                    mapping,
                    shape,
                    records,
                    codec_imports,
                ));
                out.push('\n');
                out.push_str(&rpc_check(
                    spec,
                    op,
                    &wire_service,
                    mapping,
                    shape,
                    records,
                    codec_imports,
                ));
            }
            (BidiTransport::Rpc, csilgen_common::CsilServiceDirection::Reverse) => {
                out.push('\n');
                out.push_str(&rpc_check(
                    spec,
                    op,
                    &wire_service,
                    mapping,
                    shape,
                    records,
                    codec_imports,
                ));
            }
            // Connection-mode bidi/reverse are emitted in channel_block instead
            (BidiTransport::Connection, _) => {}
        }
    }

    out.push_str("}\n");
    Some(out)
}

/// A comment emitted in place of a method whose request or response the codec cannot
/// (de)serialize from its exported helpers: a `decimal` payload with no record-level
/// decimal support, or an undecodable multi-variant choice union. The carrier still
/// moves the bytes, so such a payload is handled by the consumer.
fn unsupported_op_note(op: &CsilServiceOperation) -> String {
    format!(
        "\n  // operation '{}' has a non-record payload; (de)serialize it manually\n",
        op.name
    )
}

/// Encode an op request to the wire bytes passed to the transport, plus the method's
/// request parameter declaration. A record uses its byte-level `to<T>Cbor`; a `null`
/// input (a push op) carries no body, so the parameter is dropped and an empty payload
/// sent; any other shape — a scalar id, a bare array, a scalar/array/map alias —
/// encodes via the codec's generic CBOR so the op is never silently dropped.
fn encode_request(
    spec: &CsilSpecSerialized,
    op: &CsilServiceOperation,
    mapping: DecimalMapping,
    records: &HashSet<String>,
    codec_imports: &mut BTreeSet<String>,
) -> (String, String) {
    if common::is_null_type(&op.input_type) {
        return (String::new(), "new Uint8Array()".to_string());
    }
    let param = format!("req: {}", common::ts_type(&op.input_type, mapping));
    if codec::is_record_ref(&op.input_type, records) {
        let to_req = format!("to{}Cbor", codec::record_ref_name(&op.input_type));
        codec_imports.insert(to_req.clone());
        (param, format!("{to_req}(req)"))
    } else {
        let (expr, imports) = codec::op_encode_expr(spec, records, mapping, &op.input_type, "req");
        codec_imports.extend(imports);
        (param, expr)
    }
}

/// Decode a unary response from the transport bytes named by `bytes_expr`. A record
/// uses its byte-level `from<T>Cbor`; any other shape decodes via the codec's generic
/// CBOR.
fn decode_response(
    spec: &CsilSpecSerialized,
    success: &csilgen_common::CsilTypeExpression,
    bytes_expr: &str,
    mapping: DecimalMapping,
    records: &HashSet<String>,
    codec_imports: &mut BTreeSet<String>,
) -> String {
    if codec::is_record_ref(success, records) {
        let from_res = format!("from{}Cbor", codec::record_ref_name(success));
        codec_imports.insert(from_res.clone());
        format!("{from_res}({bytes_expr})")
    } else {
        let (expr, imports) = codec::op_decode_expr(spec, records, mapping, success, bytes_expr);
        codec_imports.extend(imports);
        expr
    }
}

fn unary_method(
    spec: &CsilSpecSerialized,
    op: &CsilServiceOperation,
    wire_service: &str,
    mapping: DecimalMapping,
    shape: ClientShape,
    records: &HashSet<String>,
    codec_imports: &mut BTreeSet<String>,
) -> String {
    let success = common::success_type(&op.output_type);
    if !codec::op_boundary_expressible(spec, records, &op.input_type)
        || !codec::op_boundary_expressible(spec, records, &success)
    {
        return unsupported_op_note(op);
    }
    let method = common::to_camel(&op.name);
    let wire_method = common::method_wire(op);
    let (req_param, req_bytes) = encode_request(spec, op, mapping, records, codec_imports);
    let decode_resp = decode_response(spec, &success, "csilResp", mapping, records, codec_imports);

    let (async_kw, await_kw) = (shape.async_kw(), shape.await_kw());
    let ret = shape.ret(&common::ts_type(&success, mapping));
    let throws = vec![
        "@throws {ServiceError} when the API returns an error response".to_string(),
        "@throws transport errors (network, timeout) raised by the transport".to_string(),
    ];
    let mut out = common::jsdoc(&op.doc_comments, &throws, "  ");
    out.push_str(&format!("  {async_kw}{method}({req_param}): {ret} {{\n"));
    out.push_str(&format!(
        "    const csilResp = {await_kw}this.t.call(\"{wire_service}\", \"{wire_method}\", {req_bytes});\n"
    ));
    out.push_str(&format!("    return {decode_resp};\n"));
    out.push_str("  }\n");
    out
}

/// rpc-mode outbound: `send<Op>` posts an input over a synthetic op name. The input is
/// encoded like a unary request, so a non-record input is no longer dropped.
fn rpc_send(
    spec: &CsilSpecSerialized,
    op: &CsilServiceOperation,
    wire_service: &str,
    mapping: DecimalMapping,
    shape: ClientShape,
    records: &HashSet<String>,
    codec_imports: &mut BTreeSet<String>,
) -> String {
    if !codec::op_boundary_expressible(spec, records, &op.input_type) {
        return unsupported_op_note(op);
    }
    let camel = common::to_camel(&op.name);
    let wire_method = format!("{}Send", common::method_wire(op));
    let (param, req_bytes) = encode_request(spec, op, mapping, records, codec_imports);
    let (async_kw, await_kw) = (shape.async_kw(), shape.await_kw());
    let ret = shape.ret("void");
    let mut out = String::new();
    out.push_str(&format!(
        "  {async_kw}send{}({param}): {ret} {{\n",
        pascal_from_camel(&camel)
    ));
    out.push_str(&format!(
        "    {await_kw}this.t.call(\"{wire_service}\", \"{wire_method}\", {req_bytes});\n"
    ));
    out.push_str("  }\n");
    out
}

/// rpc-mode inbound: `check<Op>` drains the server's pending outbound queue, decoding
/// the CBOR array of records the poll returns.
fn rpc_check(
    spec: &CsilSpecSerialized,
    op: &CsilServiceOperation,
    wire_service: &str,
    mapping: DecimalMapping,
    shape: ClientShape,
    records: &HashSet<String>,
    codec_imports: &mut BTreeSet<String>,
) -> String {
    let success = common::success_type(&op.output_type);
    if !codec::op_boundary_expressible(spec, records, &success) {
        return unsupported_op_note(op);
    }
    let camel = common::to_camel(&op.name);
    let wire_method = format!("{}Check", common::method_wire(op));
    let res = common::ts_type(&success, mapping);
    // The poll returns a CBOR array; each element decodes as the success type. A record
    // element uses its `from<T>CborValue`, any other shape the codec's generic CBOR, so
    // a non-record poll element is no longer dropped.
    let (decode_elem, elem_imports) =
        codec::op_decode_value_expr(spec, records, mapping, &success, "csilE");
    codec_imports.insert("decode".to_string());
    codec_imports.insert("asArray".to_string());
    codec_imports.extend(elem_imports);
    let (async_kw, await_kw) = (shape.async_kw(), shape.await_kw());
    let ret = shape.ret(&format!("{res}[]"));
    let mut out = String::new();
    out.push_str(&format!(
        "  {async_kw}check{}(): {ret} {{\n",
        pascal_from_camel(&camel)
    ));
    out.push_str(&format!(
        "    const csilResp = {await_kw}this.t.call(\"{wire_service}\", \"{wire_method}\", new Uint8Array());\n"
    ));
    out.push_str(&format!(
        "    return asArray(decode(csilResp)).map((csilE) => {decode_elem});\n"
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
fn channel_block(
    name: &str,
    def: &CsilServiceDefinition,
    mapping: DecimalMapping,
    shape: ClientShape,
) -> String {
    let base = common::service_base(name);
    let handlers_iface = format!("{base}{}ChannelHandlers", shape.marker);
    let codec_name = shape.codec_name();
    let wire_service = common::service_wire(name);

    let channel_ops: Vec<&CsilServiceOperation> = def
        .operations
        .iter()
        .filter(|op| !common::is_unidirectional(op))
        .collect();

    let mut out = String::new();

    // Handler interface: client receives output_type for both <-> and <-. An async
    // router awaits the handler, so the async shape lets a handler return either a
    // value or a `Promise` (`void | Promise<void>`).
    let handler_ret = if shape.is_async {
        "void | Promise<void>"
    } else {
        "void"
    };
    out.push_str(&format!("export interface {handlers_iface} {{\n"));
    for op in &channel_ops {
        let method = common::to_camel(&op.name);
        let inbound = common::ts_type(&common::success_type(&op.output_type), mapping);
        out.push_str(&common::jsdoc(&op.doc_comments, &[], "  "));
        out.push_str(&format!("  {method}(msg: {inbound}): {handler_ret};\n"));
    }
    out.push_str("}\n\n");

    // Router: feed inbound frames (method + bytes) in; we decode + dispatch.
    let route_fn = format!("route{base}{}Channel", shape.marker);
    let (async_kw, await_kw) = (shape.async_kw(), shape.await_kw());
    let route_ret = shape.ret("void");
    out.push_str(&format!(
        "/**\n\
         \x20* Dispatch one inbound frame for the {wire_service} channel. The implementer\n\
         \x20* (WebSocket adapter etc.) calls this for each message it pulls off the wire;\n\
         \x20* this generator never owns the connection itself.\n\
         \x20*/\n\
         export {async_kw}function {route_fn}(\n\
         \x20 handlers: {handlers_iface},\n\
         \x20 codec: {codec_name},\n\
         \x20 method: string,\n\
         \x20 bytes: Uint8Array,\n\
         ): {route_ret} {{\n\
         \x20 switch (method) {{\n"
    ));
    for op in &channel_ops {
        let wire_method = common::method_wire(op);
        let method = common::to_camel(&op.name);
        let inbound = common::ts_type(&common::success_type(&op.output_type), mapping);
        out.push_str(&format!("    case \"{wire_method}\":\n"));
        out.push_str(&format!(
            "      {await_kw}handlers.{method}(codec.decode<{inbound}>(bytes));\n"
        ));
        out.push_str("      return;\n");
    }
    out.push_str("    default:\n");
    out.push_str(
        "      throw { code: 404, message: `unknown channel ${method}` } satisfies ServiceError;\n",
    );
    out.push_str("  }\n}\n");

    // Outbound encoders: only `<->` ops have a client-side outbound; reverse is
    // server-pushed and gets no encoder here. Encoders are pure (no I/O), so they
    // stay synchronous in both shapes — only the marker keeps the names distinct.
    for op in &channel_ops {
        if !common::is_bidirectional(op) {
            continue;
        }
        let camel = common::to_camel(&op.name);
        let wire_method = common::method_wire(op);
        let outbound = common::ts_type(&op.input_type, mapping);
        let fn_name = format!("encode{base}{}{}", shape.marker, pascal_from_camel(&camel));
        out.push_str(&format!(
            "\n\
             /**\n\
             \x20* Encode an outbound `{wire_method}` message; hand the resulting bytes to\n\
             \x20* your connection. Returns `{{method, bytes}}` so the implementer can frame\n\
             \x20* both pieces however its protocol requires.\n\
             \x20*/\n\
             export function {fn_name}(codec: {codec_name}, msg: {outbound}): {{ method: string; bytes: Uint8Array }} {{\n\
             \x20 return {{ method: \"{wire_method}\", bytes: codec.encode(msg) }};\n\
             }}\n"
        ));
    }

    out
}

fn aggregate_class(
    name: &str,
    services: &[&(&str, &CsilServiceDefinition)],
    shape: ClientShape,
) -> String {
    let aggregate = shape.aggregate_name(name);
    let transport = shape.transport_name();
    let mut out = format!("export class {aggregate} {{\n");
    for (svc, _) in services {
        let field = common::to_camel(&common::service_base(svc));
        let class = shape.class_name(&common::service_base(svc));
        out.push_str(&format!("  readonly {field}: {class};\n"));
    }
    out.push_str(&format!("  constructor(t: {transport}) {{\n"));
    for (svc, _) in services {
        let field = common::to_camel(&common::service_base(svc));
        let class = shape.class_name(&common::service_base(svc));
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
