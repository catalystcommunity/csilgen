//! Client/server emission: wire-verbatim op strings, handler stubs, router twins,
//! channel encoders, and wire-id constants.

mod common;

use common::*;
use csilgen_common::*;

fn unary_union_op(name: &str) -> CsilServiceOperation {
    op(
        name,
        reference("submit_task_request"),
        CsilTypeExpression::Choice(vec![
            reference("submit_task_response"),
            reference("ServiceError"),
        ]),
        CsilServiceDirection::Unidirectional,
    )
}

#[test]
fn client_method_is_snake_case_and_wire_strings_are_verbatim() {
    let s = spec(vec![service_rule(
        "corndogs_service",
        vec![unary_union_op("submit-task")],
        None,
    )]);
    let client = file(&s, "ruby-client", "client.rb");
    assert!(client.contains("class CorndogsClient"));
    assert!(client.contains("def initialize(transport)"));
    // snake_case Ruby method name...
    assert!(client.contains("def submit_task(req)"));
    // ...but the verbatim CSIL service/op names on the wire (csil-rpc-transport.md
    // §1.1), over the dumb byte seam: the request self-encodes, the reply self-decodes.
    assert!(client.contains(
        "SubmitTaskResponse.from_cbor(@transport.call(\"corndogs_service\", \"submit-task\", req.to_cbor))"
    ));
    // The ServiceError arm is stripped from the documented success type.
    assert!(client.contains("-> SubmitTaskResponse"));
}

#[test]
fn null_input_op_takes_no_request_param() {
    let s = spec(vec![service_rule(
        "events_service",
        vec![op(
            "ping",
            builtin("null"),
            reference("pong"),
            CsilServiceDirection::Unidirectional,
        )],
        None,
    )]);
    let client = file(&s, "ruby-client", "client.rb");
    assert!(client.contains("def ping\n"));
    // A null-input op sends an empty byte payload (no request body).
    assert!(client.contains("@transport.call(\"events_service\", \"ping\", \"\".b)"));
}

#[test]
fn server_handlers_raise_not_implemented() {
    let s = spec(vec![service_rule(
        "corndogs_service",
        vec![unary_union_op("submit-task")],
        None,
    )]);
    let server = file(&s, "ruby", "server.rb");
    assert!(server.contains("class CorndogsHandlers"));
    assert!(server.contains("def submit_task(req)"));
    assert!(server.contains("raise NotImplementedError, \"CorndogsHandlers#submit_task\""));
}

#[test]
fn unary_only_service_has_no_router() {
    let s = spec(vec![service_rule(
        "auth_service",
        vec![op(
            "login",
            reference("creds"),
            reference("token"),
            CsilServiceDirection::Unidirectional,
        )],
        None,
    )]);
    let server = file(&s, "ruby", "server.rb");
    assert!(!server.contains("module AuthRouter"));
    assert!(!server.contains("route_channel"));
}

#[test]
fn channel_service_emits_verbose_router_and_encoders() {
    let s = spec(vec![service_rule(
        "match_service",
        vec![
            op(
                "list-events",
                reference("query"),
                reference("events"),
                CsilServiceDirection::Unidirectional,
            ),
            op(
                "play",
                reference("move"),
                reference("ack"),
                CsilServiceDirection::Bidirectional,
            ),
        ],
        None,
    )]);
    let server = file(&s, "ruby", "server.rb");
    assert!(server.contains("module MatchRouter"));
    assert!(server.contains("def route_channel(handlers, codec, method, data)"));
    assert!(server.contains("when \"play\""));
    assert!(server.contains("handlers.play(msg)"));
    assert!(server.contains("def encode_play(codec, msg)"));
    assert!(server.contains("[\"play\", codec.encode(msg)]"));
    // No wire-ids -> no compact twin.
    assert!(!server.contains("route_channel_compact"));
}

#[test]
fn compact_router_twin_only_when_wire_ids_present() {
    let s = spec(vec![service_rule(
        "match_service",
        vec![op_wire(
            "play",
            reference("move"),
            reference("ack"),
            CsilServiceDirection::Bidirectional,
            7,
        )],
        Some(3),
    )]);
    let server = file(&s, "ruby", "server.rb");
    assert!(server.contains("def route_channel_compact(handlers, codec, op, data)"));
    assert!(server.contains("when 7"));
    // Wire-id constants surface in the router module.
    assert!(server.contains("SERVICE_WIRE_ID = 3"));
    assert!(server.contains("OP_PLAY_WIRE_ID = 7"));
}

#[test]
fn reverse_op_has_encoder_but_no_inbound_handler() {
    let s = spec(vec![service_rule(
        "callbacks_service",
        vec![op(
            "notify",
            reference("event"),
            reference("event"),
            CsilServiceDirection::Reverse,
        )],
        None,
    )]);
    let server = file(&s, "ruby", "server.rb");
    // Reverse contributes an outbound encoder...
    assert!(server.contains("def encode_notify(codec, msg)"));
    // ...but no inbound handler method or router case.
    assert!(!server.contains("def notify("));
    assert!(!server.contains("when \"notify\""));
}

#[test]
fn client_emits_wire_id_module_when_present() {
    let s = spec(vec![service_rule(
        "match_service",
        vec![op_wire(
            "submit-task",
            reference("req"),
            reference("resp"),
            CsilServiceDirection::Unidirectional,
            9,
        )],
        Some(2),
    )]);
    let client = file(&s, "ruby-client", "client.rb");
    assert!(client.contains("module MatchWireIds"));
    assert!(client.contains("SERVICE = 2"));
    assert!(client.contains("OP_SUBMIT_TASK = 9"));
}
