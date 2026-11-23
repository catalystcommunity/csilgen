//! Rust interop harness — reference implementation.
//!
//! Runs as a server (binds a loopback TCP/UDP port and serves one transport) or a client
//! (connects, runs the case battery for one transport, prints JSON results).
//! See tests/interop/README.md for the contract this implements.

use std::cell::RefCell;
use std::io::Write;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

use csilgen_transport::carrier::{FrameCarrier, StreamCarrier};
use csilgen_transport::datagrams::Datagram;
use csilgen_transport::events::{control, Event, Hello, HelloAck, Profile};
use csilgen_transport::rpc::{HandlerOutcome, RpcClient, RpcRequest, RpcServer};
use csilgen_transport::Status;
use time::{format_description::well_known::Rfc3339, Date, Month, OffsetDateTime};

use interop_api::{
    decode_collections, decode_constrained, decode_nested, decode_scalars,
    decode_tick, encode_collections, encode_interop_on_tick, encode_scalars,
    route_interop_channel, ClientError, Collections, Color, Constrained,
    CsilCborError, CsilCborValue, EchoNestedResult, IdOrName, Interop, Level, MixedStatus, Nested, OptBytes,
    Priority,
    Scalars, Scalars_note, Scalars_size, Season, ServiceError, ShipMode, Tick, Transport, WireReply,
};

// The wire carries the CSIL service name verbatim as written in interop.csil
// (docs/cbor-wire-contract.md "RPC call naming").
const SERVICE: &str = "Interop";

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see README "Fixed test vectors").
// ---------------------------------------------------------------------------

fn ts() -> OffsetDateTime {
    OffsetDateTime::parse("2026-06-29T12:34:56Z", &Rfc3339).unwrap()
}

fn scalars_ok() -> Scalars {
    Scalars {
        i: -42,
        u: 42,
        n: -7,
        f: 3.5,
        t: "héllo 世界".to_string(),
        raw: vec![0x01, 0x02, 0xf0, 0xff],
        flag: true,
        due: Date::from_calendar_date(2000, Month::February, 29).unwrap(),
        when: ts(),
        // 1782736496.5 on the wire: tag 1 around a float.
        seen: OffsetDateTime::parse("2026-06-29T12:34:56.500Z", &Rfc3339).unwrap(),
        amount: interop_api::CsilDecimal::from_str("123.45").unwrap(),
        // mixed-union coverage: a value equal to a literal arm must wire as
        // [1,"pending"]; a value with no literal match must wire as [0,...].
        status_literal: MixedStatus::Variant1("pending".to_string()),
        status_free: MixedStatus::Variant0("unlisted".to_string()),
        // inline mixed choice (hoisted; literal arm match).
        note: Scalars_note::Variant1("info".to_string()),
        // inline all-literal choice (hoisted to an enum).
        size: Scalars_size::Medium,
        // named enum with a trailing `.default`-constrained last arm.
        level: Level::High,
        // named enum assembled via base rule + `/=` extension.
        season: Season::Autumn,
        // named all-literal enum with mixed literal kinds (text + int arms);
        // wires bare (no tag) with kind+value membership validation on decode.
        ship_text: ShipMode::Ground,
        ship_int: ShipMode::V2,
    }
}

fn collections_ok() -> Collections {
    let mut scores = std::collections::HashMap::new();
    scores.insert("x".to_string(), 1);
    scores.insert("y".to_string(), 2);
    Collections {
        names: vec!["a".to_string(), "b".to_string()],
        at_least_one: vec![1, 2, 3],
        bounded: vec![10, 20],
        exact3: vec![7, 8, 9],
        scores,
        color: Color::Green,
        tone: Color::Blue,
        prio: Priority::V2,
        who: IdOrName::Variant0(4242),
        pair: ("p".to_string(), 5),
        triple: ("t".to_string(), Some(9), Some(true)),
        extra: {
            let mut m = std::collections::HashMap::new();
            m.insert("k".to_string(), CsilCborValue::Text("v".to_string()));
            m
        },
    }
}

fn constrained_ok() -> Constrained {
    Constrained {
        code: "PRD-AB12CD".to_string(),
        qty: 10,
        rate: interop_api::CsilDecimal::from_str("0.25").unwrap(),
        password: "hunter2hunter2".to_string(),
        tags: vec!["one".to_string(), "two".to_string()],
    }
}

fn constrained_bad() -> Constrained {
    Constrained {
        code: "bad".to_string(),
        qty: 0,
        rate: interop_api::CsilDecimal::from_str("9.9").unwrap(),
        password: "x".to_string(),
        tags: vec![],
    }
}

fn nested_ok() -> Nested {
    Nested {
        inner: scalars_ok(),
        maybe: Some(constrained_ok()),
        many: vec![scalars_ok()],
    }
}

// ---------------------------------------------------------------------------
// Result accumulation + JSON output (no serde dependency).
// ---------------------------------------------------------------------------

struct Cases {
    lang: String,
    transport: String,
    out: Vec<(String, bool, String)>,
}

impl Cases {
    fn new(transport: &str) -> Self {
        Self {
            lang: "rust".to_string(),
            transport: transport.to_string(),
            out: Vec::new(),
        }
    }
    fn pass(&mut self, name: &str) {
        self.out.push((name.to_string(), true, String::new()));
    }
    fn fail(&mut self, name: &str, detail: impl Into<String>) {
        self.out.push((name.to_string(), false, detail.into()));
    }
    /// Record `name` as pass when `cond`, else fail with `detail`.
    fn check(&mut self, name: &str, cond: bool, detail: impl Into<String>) {
        if cond {
            self.pass(name);
        } else {
            self.fail(name, detail);
        }
    }
    fn emit(&self) {
        let mut s = String::new();
        s.push_str(&format!(
            "{{\"lang\":\"{}\",\"transport\":\"{}\",\"cases\":[",
            self.lang, self.transport
        ));
        for (i, (name, ok, detail)) in self.out.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"name\":\"{}\",\"ok\":{},\"detail\":{}}}",
                name,
                ok,
                json_str(detail)
            ));
        }
        s.push_str("]}");
        println!("{s}");
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ---------------------------------------------------------------------------
// Server-side handler implementing the generated trait.
// ---------------------------------------------------------------------------

struct Handlers;
impl Interop for Handlers {
    type Context = ();
    fn echo_scalars(&self, _: &(), input: Scalars) -> Result<Scalars, ServiceError> {
        Ok(input)
    }
    fn echo_collections(&self, _: &(), input: Collections) -> Result<Collections, ServiceError> {
        Ok(input)
    }
    fn echo_nested(&self, _: &(), input: Nested) -> Result<EchoNestedResult, ServiceError> {
        Ok(EchoNestedResult {
            ok: true,
            echo: input,
        })
    }
    fn echo_opt_bytes(&self, _: &(), input: OptBytes) -> Result<OptBytes, ServiceError> {
        Ok(input)
    }
    fn validate_constrained(
        &self,
        _: &(),
        input: Constrained,
    ) -> Result<Constrained, ServiceError> {
        match input.validate() {
            Ok(()) => Ok(input),
            Err(e) => Err(ServiceError {
                code: 422,
                message: e.to_string(),
            }),
        }
    }
    fn duplex(&self, _: &(), _msg: Scalars) -> Result<(), ServiceError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

struct RpcTransport {
    client: RefCell<RpcClient<StreamCarrier<TcpStream>>>,
}
impl Transport for RpcTransport {
    fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError> {
        Ok(self.call_reply(service, op, req)?.payload)
    }

    fn call_reply(&self, service: &str, op: &str, req: &[u8]) -> Result<WireReply, ClientError> {
        match self
            .client
            .borrow_mut()
            .call(service, op, req.to_vec(), None)
        {
            Ok(resp) => Ok(WireReply {
                variant: resp.variant,
                payload: resp.payload,
            }),
            Err(e) => Err(ClientError::Transport(e.to_string())),
        }
    }
}

// A reply whose payload cannot be encoded (for example a timestamp outside
// UTC years 0001-9999) is a server-side failure, so it goes back as the
// transport's internal status (docs/csil-rpc-transport.md section 1.2).
fn reply(variant: &str, payload: Result<Vec<u8>, CsilCborError>) -> HandlerOutcome {
    match payload {
        Ok(payload) => HandlerOutcome::Reply {
            variant: variant.to_string(),
            payload,
        },
        Err(e) => HandlerOutcome::Transport(Status::Internal, e.to_string()),
    }
}

fn dispatch<H: Interop<Context = ()>>(handlers: &H, req: &RpcRequest) -> HandlerOutcome {
    // Dispatch keys are the CSIL operation names verbatim (kebab-case, as written
    // in interop.csil) — docs/cbor-wire-contract.md "RPC call naming". The reply
    // `variant` strings below are output-type arm names, unrelated to wire naming.
    match req.op.as_str() {
        "echo-scalars" => match decode_scalars(&req.payload) {
            Ok(v) => match handlers.echo_scalars(&(), v) {
                Ok(r) => reply("Scalars", encode_scalars(&r)),
                Err(e) => HandlerOutcome::Transport(Status::Internal, e.message),
            },
            Err(e) => HandlerOutcome::Transport(Status::MalformedEnvelope, e.to_string()),
        },
        "echo-collections" => match decode_collections(&req.payload) {
            Ok(v) => match handlers.echo_collections(&(), v) {
                Ok(r) => reply("Collections", encode_collections(&r)),
                Err(e) => HandlerOutcome::Transport(Status::Internal, e.message),
            },
            Err(e) => HandlerOutcome::Transport(Status::MalformedEnvelope, e.to_string()),
        },
        "echo-nested" => match decode_nested(&req.payload) {
            Ok(v) => match handlers.echo_nested(&(), v) {
                Ok(r) => reply(
                    "EchoNestedResult",
                    interop_api::encode_echo_nested_result(&r),
                ),
                Err(e) => HandlerOutcome::Transport(Status::Internal, e.message),
            },
            Err(e) => HandlerOutcome::Transport(Status::MalformedEnvelope, e.to_string()),
        },
        "echo-opt-bytes" => match interop_api::decode_opt_bytes(&req.payload) {
            Ok(v) => match handlers.echo_opt_bytes(&(), v) {
                Ok(r) => reply("OptBytes", interop_api::encode_opt_bytes(&r)),
                Err(e) => HandlerOutcome::Transport(Status::Internal, e.message),
            },
            Err(e) => HandlerOutcome::Transport(Status::MalformedEnvelope, e.to_string()),
        },
        "validate-constrained" => match decode_constrained(&req.payload) {
            Ok(v) => {
                let outcome = handlers.validate_constrained(&(), v);
                match interop_api::encode_interop_validate_constrained_reply(outcome) {
                    Ok((variant, payload)) => reply(variant, Ok(payload)),
                    Err(e) => HandlerOutcome::Transport(Status::Internal, e.to_string()),
                }
            }
            Err(e) => HandlerOutcome::Transport(Status::MalformedEnvelope, e.to_string()),
        },
        other => {
            HandlerOutcome::Transport(Status::UnknownServiceOrOp, format!("unknown op {other}"))
        }
    }
}

fn rpc_server(listener: TcpListener) {
    let handlers = Handlers;
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut server = RpcServer::new(StreamCarrier::new(stream));
        let mut handle = |req: &RpcRequest| dispatch(&handlers, req);
        while let Ok(true) = server.serve_one(&mut handle) {}
    }
}

fn rpc_client(stream: TcpStream) -> Cases {
    let mut cases = Cases::new("rpc");
    let transport = RpcTransport {
        client: RefCell::new(RpcClient::new(StreamCarrier::new(stream), false)),
    };
    let client = interop_api::InteropClient::new(transport);

    match client.echo_scalars(scalars_ok()) {
        Ok(r) => cases.check(
            "echo-scalars/success",
            r == scalars_ok(),
            format!("mismatch: {r:?}"),
        ),
        Err(e) => cases.fail("echo-scalars/success", e.to_string()),
    }
    match client.echo_collections(collections_ok()) {
        Ok(r) => cases.check(
            "echo-collections/success",
            r == collections_ok(),
            format!("mismatch: {r:?}"),
        ),
        Err(e) => cases.fail("echo-collections/success", e.to_string()),
    }
    match client.echo_nested(nested_ok()) {
        Ok(r) => cases.check(
            "echo-nested/success",
            r.ok && r.echo == nested_ok(),
            format!("mismatch: {r:?}"),
        ),
        Err(e) => cases.fail("echo-nested/success", e.to_string()),
    }
    match client.validate_constrained(constrained_ok()) {
        Ok(r) => cases.check(
            "validate-constrained/success",
            r == constrained_ok(),
            format!("mismatch: {r:?}"),
        ),
        Err(e) => cases.fail("validate-constrained/success", e.to_string()),
    }
    match client.validate_constrained(constrained_bad()) {
        Ok(_) => cases.fail(
            "validate-constrained/failure",
            "server accepted invalid input",
        ),
        // The typed error arm must surface as a service error, not a transport error.
        Err(ClientError::ApplicationServiceError(_)) => cases.pass("validate-constrained/failure"),
        Err(e) => cases.fail(
            "validate-constrained/failure",
            format!("expected service error, got {e}"),
        ),
    }

    // Optional-bytes presence: absent, present-empty, and present-non-empty are three
    // distinct states that must each survive the round trip unchanged. `Option` is the
    // presence carrier -- `Some(vec![])` must never come back as `None`.
    match client.echo_opt_bytes(OptBytes {
        tag: "absent".into(),
        payload: None,
    }) {
        Ok(r) => cases.check(
            "opt-bytes/absent",
            r.payload.is_none(),
            format!("expected absent, got {:?}", r.payload),
        ),
        Err(e) => cases.fail("opt-bytes/absent", e.to_string()),
    }
    match client.echo_opt_bytes(OptBytes {
        tag: "empty".into(),
        payload: Some(Vec::new()),
    }) {
        Ok(r) => cases.check(
            "opt-bytes/present-empty",
            r.payload.as_deref() == Some(&[][..]),
            format!("expected present-and-empty, got {:?}", r.payload),
        ),
        Err(e) => cases.fail("opt-bytes/present-empty", e.to_string()),
    }
    match client.echo_opt_bytes(OptBytes {
        tag: "full".into(),
        payload: Some(vec![0x01, 0x02, 0xf0, 0xff]),
    }) {
        Ok(r) => cases.check(
            "opt-bytes/present-non-empty",
            r.payload.as_deref() == Some(&[0x01, 0x02, 0xf0, 0xff][..]),
            format!("expected 0102f0ff, got {:?}", r.payload),
        ),
        Err(e) => cases.fail("opt-bytes/present-non-empty", e.to_string()),
    }
    cases
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

const TICKS: u64 = 3;

fn send_event(carrier: &mut StreamCarrier<TcpStream>, ev: &Event) {
    let _ = carrier.send_frame(&ev.encode(Profile::Verbose).unwrap());
}

fn events_server(listener: TcpListener) {
    let handlers = Handlers;
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut carrier = StreamCarrier::new(stream);
        // Handshake: read $hello, reply $hello-ack (verbose).
        match carrier.recv_frame() {
            Ok(Some(frame)) => {
                let ev = Event::decode(&frame, Profile::Verbose).unwrap();
                if ev.event.as_deref() != Some(control::HELLO_NAME) {
                    continue;
                }
                let _ = Hello::decode(&ev.payload);
                let ack = HelloAck {
                    v: 1,
                    profile: "verbose".to_string(),
                    session: None,
                };
                send_event(
                    &mut carrier,
                    &Event::verbose(None, control::HELLO_ACK_NAME, ack.encode().unwrap()),
                );
            }
            _ => continue,
        }
        // Push N ticks (on-tick / server push).
        for seq in 0..TICKS {
            let event = match encode_interop_on_tick(&Tick { seq }) {
                Ok((method, bytes)) => Event::verbose(Some(SERVICE.into()), method, bytes),
                Err(_) => Event::verbose(None, control::ERROR_NAME, vec![]),
            };
            send_event(&mut carrier, &event);
        }
        // React loop.
        while let Ok(Some(frame)) = carrier.recv_frame() {
            let ev = match Event::decode(&frame, Profile::Verbose) {
                Ok(e) => e,
                Err(_) => break,
            };
            let method = ev.event.clone().unwrap_or_default();
            match method.as_str() {
                control::PING_NAME => {
                    send_event(
                        &mut carrier,
                        &Event::verbose(None, control::PONG_NAME, ev.payload.clone()),
                    );
                }
                control::CLOSE_NAME => break,
                "duplex" => {
                    if let Ok(s) = decode_scalars(&ev.payload) {
                        let _ = handlers.duplex(&(), s.clone());
                        let event = match interop_api::encode_interop_duplex(&s) {
                            Ok((m, b)) => Event::verbose(Some(SERVICE.into()), m, b),
                            Err(_) => Event::verbose(None, control::ERROR_NAME, vec![]),
                        };
                        send_event(&mut carrier, &event);
                    }
                }
                other => {
                    // Exercise the generated router's unknown-method path.
                    let _ = route_interop_channel(&handlers, &(), other, &ev.payload);
                    send_event(
                        &mut carrier,
                        &Event::verbose(None, control::ERROR_NAME, vec![]),
                    );
                }
            }
        }
    }
}

fn events_client(stream: TcpStream) -> Cases {
    let mut cases = Cases::new("events");
    let mut carrier = StreamCarrier::new(stream);
    // Handshake.
    let hello = Hello {
        versions: vec![1],
        profiles: vec!["verbose".to_string()],
        service: Some(SERVICE.to_string()),
        auth: None,
    };
    send_event(
        &mut carrier,
        &Event::verbose(None, control::HELLO_NAME, hello.encode().unwrap()),
    );
    let ack_ok = match carrier.recv_frame() {
        Ok(Some(f)) => {
            Event::decode(&f, Profile::Verbose)
                .ok()
                .and_then(|e| e.event)
                .as_deref()
                == Some(control::HELLO_ACK_NAME)
        }
        _ => false,
    };
    cases.check("events/handshake", ack_ok, "no $hello-ack");

    // on-tick: expect TICKS pushes with seq 0..TICKS.
    let mut tick_ok = true;
    let mut detail = String::new();
    for expect in 0..TICKS {
        match carrier.recv_frame() {
            Ok(Some(f)) => match Event::decode(&f, Profile::Verbose) {
                Ok(e) if e.event.as_deref() == Some("on-tick") => match decode_tick(&e.payload) {
                    Ok(t) if t.seq == expect => {}
                    Ok(t) => {
                        tick_ok = false;
                        detail = format!("tick seq {} != {expect}", t.seq);
                    }
                    Err(err) => {
                        tick_ok = false;
                        detail = format!("tick decode: {err}");
                    }
                },
                Ok(e) => {
                    tick_ok = false;
                    detail = format!("expected on-tick got {:?}", e.event);
                }
                Err(err) => {
                    tick_ok = false;
                    detail = format!("decode: {err}");
                }
            },
            _ => {
                tick_ok = false;
                detail = "stream closed during ticks".to_string();
            }
        }
    }
    cases.check("on-tick/success", tick_ok, detail);

    // duplex: send Scalars, expect echo.
    let (m, b) = interop_api::encode_interop_duplex(&scalars_ok()).expect("fixed vector encodes");
    send_event(&mut carrier, &Event::verbose(Some(SERVICE.into()), m, b));
    match carrier.recv_frame() {
        Ok(Some(f)) => match Event::decode(&f, Profile::Verbose) {
            Ok(e) if e.event.as_deref() == Some("duplex") => match decode_scalars(&e.payload) {
                Ok(s) => cases.check("duplex/success", s == scalars_ok(), "echo mismatch"),
                Err(err) => cases.fail("duplex/success", err.to_string()),
            },
            Ok(e) => cases.fail("duplex/success", format!("got {:?}", e.event)),
            Err(err) => cases.fail("duplex/success", err.to_string()),
        },
        _ => cases.fail("duplex/success", "no echo"),
    }

    // unknown-method: send a bogus channel method, expect $error.
    send_event(
        &mut carrier,
        &Event::verbose(Some(SERVICE.into()), "Bogus", vec![]),
    );
    match carrier.recv_frame() {
        Ok(Some(f)) => {
            let is_err = Event::decode(&f, Profile::Verbose)
                .ok()
                .and_then(|e| e.event)
                .as_deref()
                == Some(control::ERROR_NAME);
            cases.check("unknown-method/failure", is_err, "expected $error");
        }
        _ => cases.fail("unknown-method/failure", "no $error"),
    }

    send_event(
        &mut carrier,
        &Event::verbose(None, control::CLOSE_NAME, vec![]),
    );
    cases
}

// ---------------------------------------------------------------------------
// Datagrams
// ---------------------------------------------------------------------------

const OP_ECHO_SCALARS: u64 = 0;
const OP_ECHO_COLLECTIONS: u64 = 1;
const OP_ERROR_SENTINEL: u64 = 255;

fn datagram_server(sock: UdpSocket) {
    let mut buf = vec![0u8; 65536];
    loop {
        let (n, addr) = match sock.recv_from(&mut buf) {
            Ok(x) => x,
            Err(_) => continue,
        };
        let dg = match Datagram::decode(&buf[..n]) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let reply = match dg.op_ord {
            OP_ECHO_SCALARS => match decode_scalars(&dg.payload).map(|s| encode_scalars(&s)) {
                Ok(Ok(b)) => Datagram::new(OP_ECHO_SCALARS, dg.seq, b),
                _ => Datagram::new(OP_ERROR_SENTINEL, dg.seq, vec![]),
            },
            OP_ECHO_COLLECTIONS => {
                match decode_collections(&dg.payload).map(|c| encode_collections(&c)) {
                    Ok(Ok(b)) => Datagram::new(OP_ECHO_COLLECTIONS, dg.seq, b),
                    _ => Datagram::new(OP_ERROR_SENTINEL, dg.seq, vec![]),
                }
            }
            _ => Datagram::new(OP_ERROR_SENTINEL, dg.seq, vec![]),
        };
        let _ = sock.send_to(&reply.encode().unwrap(), addr);
    }
}

fn datagram_client(sock: UdpSocket) -> Cases {
    let mut cases = Cases::new("datagrams");
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let mut buf = vec![0u8; 65536];

    let roundtrip = |sock: &UdpSocket,
                     buf: &mut [u8],
                     op: u64,
                     seq: u64,
                     payload: Vec<u8>|
     -> Result<Datagram, String> {
        let dg = Datagram::new(op, seq, payload);
        sock.send(&dg.encode().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let n = sock.recv(buf).map_err(|e| e.to_string())?;
        Datagram::decode(&buf[..n]).map_err(|e| e.to_string())
    };

    match roundtrip(
        &sock,
        &mut buf,
        OP_ECHO_SCALARS,
        1,
        encode_scalars(&scalars_ok()).expect("fixed vector encodes"),
    ) {
        Ok(d) if d.op_ord == OP_ECHO_SCALARS => match decode_scalars(&d.payload) {
            Ok(s) => cases.check(
                "echo-scalars/success",
                s == scalars_ok(),
                "payload mismatch",
            ),
            Err(e) => cases.fail("echo-scalars/success", e.to_string()),
        },
        Ok(d) => cases.fail("echo-scalars/success", format!("op_ord {}", d.op_ord)),
        Err(e) => cases.fail("echo-scalars/success", e),
    }
    match roundtrip(
        &sock,
        &mut buf,
        OP_ECHO_COLLECTIONS,
        2,
        encode_collections(&collections_ok()).expect("fixed vector encodes"),
    ) {
        Ok(d) if d.op_ord == OP_ECHO_COLLECTIONS => match decode_collections(&d.payload) {
            Ok(c) => cases.check(
                "echo-collections/success",
                c == collections_ok(),
                "payload mismatch",
            ),
            Err(e) => cases.fail("echo-collections/success", e.to_string()),
        },
        Ok(d) => cases.fail("echo-collections/success", format!("op_ord {}", d.op_ord)),
        Err(e) => cases.fail("echo-collections/success", e),
    }
    match roundtrip(&sock, &mut buf, 99, 3, vec![]) {
        Ok(d) => cases.check(
            "bad-op-ord/failure",
            d.op_ord == OP_ERROR_SENTINEL,
            format!("expected error sentinel, got op_ord {}", d.op_ord),
        ),
        Err(e) => cases.fail("bad-op-ord/failure", e),
    }
    cases
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: csil-interop <server|client> <rpc|events|datagrams> <port>");
        std::process::exit(2);
    }
    let mode = args[1].as_str();
    let transport = args[2].as_str();
    let port: u16 = args[3].parse().expect("port");
    let addr = ("127.0.0.1", port);

    match (mode, transport) {
        ("server", "datagrams") => {
            let sock = UdpSocket::bind(addr).expect("bind udp");
            println!("READY");
            let _ = std::io::stdout().flush();
            datagram_server(sock);
        }
        ("server", _) => {
            let listener = bind_retry(port);
            println!("READY");
            let _ = std::io::stdout().flush();
            match transport {
                "rpc" => rpc_server(listener),
                "events" => events_server(listener),
                _ => unreachable!(),
            }
        }
        ("client", "datagrams") => {
            // Bind an ephemeral local UDP port; the server replies to this source.
            let sock = UdpSocket::bind(("127.0.0.1", 0)).expect("bind client udp");
            sock.connect(addr).expect("connect udp");
            let cases = datagram_client(sock);
            cases.emit();
        }
        ("client", _) => {
            let stream = connect_retry(port);
            let cases = match transport {
                "rpc" => rpc_client(stream),
                "events" => events_client(stream),
                _ => unreachable!(),
            };
            cases.emit();
        }
        _ => {
            eprintln!("bad mode/transport");
            std::process::exit(2);
        }
    }
}

/// Bind the TCP listener, retrying briefly so a just-freed port from the previous
/// server in the matrix is tolerated without a SO_REUSEADDR dependency.
fn bind_retry(port: u16) -> TcpListener {
    for _ in 0..200 {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            return l;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    TcpListener::bind(("127.0.0.1", port)).expect("bind tcp")
}

fn connect_retry(port: u16) -> TcpStream {
    for _ in 0..200 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    TcpStream::connect(("127.0.0.1", port)).expect("connect tcp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_transport::carrier::LoopbackFrameCarrier;
    use csilgen_transport::rpc::RpcResponse;
    use std::rc::Rc;

    // A handler whose `echo-scalars` reply holds an instant with no wire form.
    struct BadReplies;
    impl Interop for BadReplies {
        type Context = ();
        fn echo_scalars(&self, _: &(), input: Scalars) -> Result<Scalars, ServiceError> {
            let year_zero = Date::from_calendar_date(0, Month::December, 31).unwrap();
            Ok(Scalars {
                when: year_zero.midnight().assume_utc(),
                ..input
            })
        }
        fn echo_collections(
            &self,
            c: &(),
            input: Collections,
        ) -> Result<Collections, ServiceError> {
            Handlers.echo_collections(c, input)
        }
        fn echo_nested(&self, c: &(), input: Nested) -> Result<EchoNestedResult, ServiceError> {
            Handlers.echo_nested(c, input)
        }
        fn echo_opt_bytes(&self, c: &(), input: OptBytes) -> Result<OptBytes, ServiceError> {
            Handlers.echo_opt_bytes(c, input)
        }
        fn validate_constrained(
            &self,
            c: &(),
            input: Constrained,
        ) -> Result<Constrained, ServiceError> {
            Handlers.validate_constrained(c, input)
        }
        fn duplex(&self, c: &(), msg: Scalars) -> Result<(), ServiceError> {
            Handlers.duplex(c, msg)
        }
    }

    // `RpcServer` owns its carrier, so the test keeps a second handle on the
    // frame queues to read the response the server wrote.
    #[derive(Clone, Default)]
    struct SharedCarrier(Rc<RefCell<LoopbackFrameCarrier>>);
    impl FrameCarrier for SharedCarrier {
        fn send_frame(&mut self, bytes: &[u8]) -> csilgen_transport::Result<()> {
            self.0.borrow_mut().send_frame(bytes)
        }
        fn recv_frame(&mut self) -> csilgen_transport::Result<Option<Vec<u8>>> {
            self.0.borrow_mut().recv_frame()
        }
    }

    #[test]
    fn unencodable_reply_is_an_internal_error_reply() {
        let payload = encode_scalars(&scalars_ok()).unwrap();
        let req = RpcRequest::new(SERVICE, "echo-scalars", payload);
        match dispatch(&BadReplies, &req) {
            HandlerOutcome::Transport(Status::Internal, message) => {
                assert!(message.contains("0001-9999"), "{message}");
            }
            HandlerOutcome::Transport(status, message) => panic!("status {status:?}: {message}"),
            HandlerOutcome::Reply { variant, .. } => panic!("unexpected reply {variant}"),
        }

        // Over the library server, the caller gets one response envelope with
        // the internal status and the connection stays usable.
        let carrier = SharedCarrier::default();
        let mut with_id = req.clone();
        with_id.id = Some(7);
        for frame in [with_id.encode().unwrap(), req.encode().unwrap()] {
            carrier.0.borrow_mut().push_inbound(frame);
        }
        let mut server = RpcServer::new(carrier.clone());
        let mut handle = |req: &RpcRequest| dispatch(&BadReplies, req);
        assert!(server.serve_one(&mut handle).unwrap());
        assert!(server.serve_one(&mut handle).unwrap());
        let first = carrier.0.borrow_mut().take_outbound().unwrap();
        let resp = RpcResponse::decode(&first).unwrap();
        assert_eq!(resp.status, Status::Internal);
        assert_eq!(resp.id, Some(7));
        assert!(resp.payload.is_empty());
        assert!(carrier.0.borrow_mut().take_outbound().is_some());
    }

    #[test]
    fn valid_reply_still_answers_with_the_typed_payload() {
        let payload = encode_scalars(&scalars_ok()).unwrap();
        let req = RpcRequest::new(SERVICE, "echo-scalars", payload.clone());
        match dispatch(&Handlers, &req) {
            HandlerOutcome::Reply {
                variant,
                payload: reply,
            } => {
                assert_eq!(variant, "Scalars");
                assert_eq!(reply, payload);
            }
            HandlerOutcome::Transport(status, message) => panic!("status {status:?}: {message}"),
        }
    }

    #[test]
    fn calendar_date_has_canonical_bytes_and_rejects_an_invalid_day() {
        let mut wire = encode_scalars(&scalars_ok()).unwrap();
        let date = b"\xd9\x03\xec\x6a2000-02-29";
        let start = wire
            .windows(date.len())
            .position(|value| value == date)
            .expect("missing tag 1004 leap-day bytes");
        wire[start + date.len() - 2..start + date.len()].copy_from_slice(b"30");
        assert!(decode_scalars(&wire).is_err());
    }

    #[test]
    fn timestamp_preserves_the_instant_and_fractional_wire_spelling() {
        for (fraction, expected) in [
            ("", ""),
            (".1", ".100"),
            (".0012", ".001200"),
            (".12345678", ".123456780"),
        ] {
            let mut sample = scalars_ok();
            let local = format!("2000-03-01T01:00:00{fraction}+02:00");
            sample.when = OffsetDateTime::parse(&local, &Rfc3339).unwrap();
            let wire = encode_scalars(&sample).unwrap();
            let utc = format!("2000-02-29T23:00:00{expected}Z");
            assert!(wire.windows(utc.len()).any(|value| value == utc.as_bytes()));
            let decoded = decode_scalars(&wire).unwrap().when;
            assert_eq!(decoded, sample.when);
            assert_eq!(decoded.offset(), time::UtcOffset::UTC);
        }
    }

    #[test]
    fn cddl_time_wires_as_tag1_float() {
        let wire = encode_scalars(&scalars_ok()).unwrap();
        let seen = b"\xc1\xfb\x41\xda\x90\x99\x9c\x20\x00\x00";
        assert!(wire.windows(seen.len()).any(|value| value == seen));
        assert_eq!(decode_scalars(&wire).unwrap().seen, scalars_ok().seen);
    }

    #[test]
    fn timestamp_decoder_normalizes_offsets_and_rejects_unrepresentable_input() {
        let wire = encode_scalars(&scalars_ok()).unwrap();
        let tagged = b"\xc0\x742026-06-29T12:34:56Z";
        let start = wire
            .windows(tagged.len())
            .position(|value| value == tagged)
            .expect("missing tag 0 timestamp");
        // Each replacement text is 25 bytes, so its CBOR header is 0x78 0x19.
        let with_offset = |text: &[u8; 25]| {
            let mut bytes = wire[..start].to_vec();
            bytes.extend_from_slice(b"\xc0\x78\x19");
            bytes.extend_from_slice(text);
            bytes.extend_from_slice(&wire[start + tagged.len()..]);
            decode_scalars(&bytes)
        };
        let seen = with_offset(b"2026-06-29T08:34:56-04:00").unwrap().when;
        assert_eq!(seen, ts());
        assert_eq!(seen.offset(), time::UtcOffset::UTC);
        assert!(with_offset(b"2026-06-29x08:34:56-04:00").is_err());
        assert!(with_offset(b"2016-12-31T23:59:60+00:00").is_err());
        assert!(with_offset(b"9999-12-31T23:30:00-01:00").is_err());
    }
}
