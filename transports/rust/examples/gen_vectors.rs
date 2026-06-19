//! Generate the CSIL transport conformance vectors from the Rust reference
//! implementation. Run with `cargo run -p csilgen-transport --example gen_vectors`.
//!
//! The vectors are the normative, language-neutral artifact (conventions doc §8):
//! every language's reference library, and anyone implementing a spec, checks
//! their encoders/decoders against these exact bytes. Each entry pairs a
//! structured, language-neutral `input` with the expected `hex` of the encoded
//! envelope, so a consumer can verify both encode (input → hex) and decode
//! (hex → input).

use csilgen_transport::datagrams::*;
use csilgen_transport::events::*;
use csilgen_transport::rpc::*;
use serde_json::{Value, json};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn entry(name: &str, description: &str, input: Value, bytes: &[u8]) -> Value {
    json!({ "name": name, "description": description, "input": input, "hex": hex(bytes) })
}

fn write(path: &str, vectors: Vec<Value>) {
    let dir = format!("{}/../conformance", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let full = format!("{dir}/{path}");
    let doc = json!({ "vectors": vectors });
    std::fs::write(
        &full,
        format!("{}\n", serde_json::to_string_pretty(&doc).unwrap()),
    )
    .unwrap();
    println!("wrote {full}");
}

fn main() {
    // A small, fixed payload reused across cases: the CBOR empty map (0xa0).
    let payload = vec![0xa0u8];
    let phex = hex(&payload);

    // ---- RPC ----
    let mut rpc = Vec::new();
    let req = RpcRequest::new("Attestation", "deposit-claim", payload.clone()).with_id(7);
    rpc.push(entry(
        "request_with_id",
        "A multiplexed request carrying a correlation id.",
        json!({"kind":"request","service":"Attestation","op":"deposit-claim","id":7,"auth":Value::Null,"payload_hex":phex}),
        &req.encode().unwrap(),
    ));
    let req_noid = RpcRequest::new("Attestation", "deposit-claim", payload.clone());
    rpc.push(entry(
        "request_no_id",
        "A one-in-flight request with no correlation id.",
        json!({"kind":"request","service":"Attestation","op":"deposit-claim","id":Value::Null,"auth":Value::Null,"payload_hex":phex}),
        &req_noid.encode().unwrap(),
    ));
    let resp_ok = RpcResponse::ok("DepositClaimResponse", payload.clone()).with_id(Some(7));
    rpc.push(entry(
        "response_success",
        "A status-0 reply; variant names the success arm.",
        json!({"kind":"response","id":7,"status":0,"variant":"DepositClaimResponse","error":Value::Null,"payload_hex":phex}),
        &resp_ok.encode().unwrap(),
    ));
    let resp_app = RpcResponse::ok("ServiceError", payload.clone()).with_id(Some(7));
    rpc.push(entry(
        "response_application_error",
        "An application error: transport status 0, variant names the error arm.",
        json!({"kind":"response","id":7,"status":0,"variant":"ServiceError","error":Value::Null,"payload_hex":phex}),
        &resp_app.encode().unwrap(),
    ));
    let resp_terr =
        RpcResponse::transport_error(csilgen_transport::Status::UnknownServiceOrOp, "no such op")
            .with_id(Some(7));
    rpc.push(entry(
        "response_transport_error",
        "A non-zero transport status with an empty payload.",
        json!({"kind":"response","id":7,"status":2,"variant":Value::Null,"error":"no such op","payload_hex":""}),
        &resp_terr.encode().unwrap(),
    ));
    let push = RpcPush::new("World", "room-delta", payload.clone());
    rpc.push(entry(
        "push",
        "A server push for a <- operation.",
        json!({"kind":"push","service":"World","event":"room-delta","payload_hex":phex}),
        &push.encode().unwrap(),
    ));
    write("rpc.json", rpc);

    // ---- Events ----
    let mut ev = Vec::new();
    let v_single = Event::verbose(None, "chat", payload.clone());
    ev.push(entry(
        "verbose_single_service",
        "A verbose event on a single-service connection (no service key).",
        json!({"profile":"verbose","service":Value::Null,"event":"chat","id":Value::Null,"payload_hex":phex}),
        &v_single.encode(Profile::Verbose).unwrap(),
    ));
    let v_corr = Event::verbose(Some("World".into()), "room-state", payload.clone()).with_id(42);
    ev.push(entry(
        "verbose_multi_service_correlated",
        "A verbose, multi-service, correlated event.",
        json!({"profile":"verbose","service":"World","event":"room-state","id":42,"payload_hex":phex}),
        &v_corr.encode(Profile::Verbose).unwrap(),
    ));
    let c_fire = Event::compact(1, 0, payload.clone());
    ev.push(entry(
        "compact_fire_and_forget",
        "A compact 3-element event [service_ord, op_ord, payload].",
        json!({"profile":"compact","service_ord":1,"op_ord":0,"id":Value::Null,"payload_hex":phex}),
        &c_fire.encode(Profile::Compact).unwrap(),
    ));
    let c_corr = Event::compact(1, 2, payload.clone()).with_id(42);
    ev.push(entry(
        "compact_correlated",
        "A compact 4-element event [service_ord, op_ord, id, payload].",
        json!({"profile":"compact","service_ord":1,"op_ord":2,"id":42,"payload_hex":phex}),
        &c_corr.encode(Profile::Compact).unwrap(),
    ));
    let hello = Hello {
        versions: vec![1],
        profiles: vec!["compact".into(), "verbose".into()],
        service: Some("World".into()),
        auth: None,
    };
    ev.push(entry(
        "control_hello",
        "A $hello control payload.",
        json!({"control":"hello","versions":[1],"profiles":["compact","verbose"],"service":"World","auth":Value::Null}),
        &hello.encode().unwrap(),
    ));
    let ack = HelloAck {
        v: 1,
        profile: "compact".into(),
        session: Some("s1".into()),
    };
    ev.push(entry(
        "control_hello_ack",
        "A $hello-ack selecting the compact profile.",
        json!({"control":"hello_ack","v":1,"profile":"compact","session":"s1"}),
        &ack.encode().unwrap(),
    ));
    let ping = Heartbeat { nonce: 9, at: None };
    ev.push(entry(
        "control_ping",
        "A $ping heartbeat.",
        json!({"control":"ping","nonce":9,"at":Value::Null}),
        &ping.encode().unwrap(),
    ));
    let close = Close {
        status: csilgen_transport::Status::VersionUnsupported,
        reason: Some("bad v".into()),
    };
    ev.push(entry(
        "control_close",
        "A $close with a transport status.",
        json!({"control":"close","status":5,"reason":"bad v"}),
        &close.encode().unwrap(),
    ));
    write("events.json", ev);

    // ---- Datagrams ----
    let mut dg = Vec::new();
    let arr = Datagram::new(0, 5, payload.clone());
    dg.push(entry(
        "cbor_array",
        "A CBOR-array datagram [v, op_ord, seq, payload].",
        json!({"profile":"cbor-array","op_ord":0,"seq":5,"payload_hex":phex}),
        &arr.encode().unwrap(),
    ));
    let arr0 = Datagram::new(1, 0, payload.clone());
    dg.push(entry(
        "cbor_array_unsequenced",
        "A CBOR-array datagram with seq 0 (unsequenced).",
        json!({"profile":"cbor-array","op_ord":1,"seq":0,"payload_hex":phex}),
        &arr0.encode().unwrap(),
    ));
    let comp = CompactDatagram::new(1, 0x1234, vec![1, 2, 3]);
    dg.push(entry(
        "compact_header",
        "A compact fixed-header datagram, no epoch.",
        json!({"profile":"compact-header","op_ord":1,"seq":0x1234,"epoch":Value::Null,"body_hex":"010203"}),
        &comp.encode(),
    ));
    let comp_e = CompactDatagram::new(2, 7, vec![9]).with_epoch(4);
    dg.push(entry(
        "compact_header_epoch",
        "A compact fixed-header datagram with an epoch byte.",
        json!({"profile":"compact-header","op_ord":2,"seq":7,"epoch":4,"body_hex":"09"}),
        &comp_e.encode(),
    ));
    write("datagrams.json", dg);
}
