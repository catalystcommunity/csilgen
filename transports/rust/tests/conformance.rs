//! Verify the Rust reference library against the checked-in conformance vectors.
//!
//! This both guards the Rust encoders/decoders against drift and demonstrates the
//! contract every other language's reference library follows: reconstruct each
//! vector's envelope from its language-neutral `input`, assert encode → `hex`, and
//! assert decode(`hex`) → the same envelope.

use csilgen_transport::datagrams::*;
use csilgen_transport::events::*;
use csilgen_transport::rpc::*;
use serde_json::Value;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn load(name: &str) -> Vec<Value> {
    let path = format!("{}/../conformance/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let doc: Value = serde_json::from_str(&text).unwrap();
    doc["vectors"].as_array().unwrap().clone()
}

fn opt_u64(v: &Value) -> Option<u64> {
    v.as_u64()
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(|s| s.to_string())
}

#[test]
fn rpc_vectors() {
    for vec in load("rpc.json") {
        let name = vec["name"].as_str().unwrap();
        let input = &vec["input"];
        let expected_hex = vec["hex"].as_str().unwrap();
        let kind = input["kind"].as_str().unwrap();
        let actual = match kind {
            "request" => {
                let mut r = RpcRequest::new(
                    input["service"].as_str().unwrap(),
                    input["op"].as_str().unwrap(),
                    unhex(input["payload_hex"].as_str().unwrap()),
                );
                r.id = opt_u64(&input["id"]);
                r.auth = opt_str(&input["auth"]);
                let bytes = r.encode().unwrap();
                assert_eq!(RpcRequest::decode(&bytes).unwrap(), r, "decode {name}");
                bytes
            }
            "response" => {
                let r = RpcResponse {
                    id: opt_u64(&input["id"]),
                    status: csilgen_transport::Status::from_code(input["status"].as_i64().unwrap()),
                    variant: opt_str(&input["variant"]),
                    error: opt_str(&input["error"]),
                    payload: unhex(input["payload_hex"].as_str().unwrap()),
                };
                let bytes = r.encode().unwrap();
                assert_eq!(RpcResponse::decode(&bytes).unwrap(), r, "decode {name}");
                bytes
            }
            "push" => {
                let p = RpcPush::new(
                    input["service"].as_str().unwrap(),
                    input["event"].as_str().unwrap(),
                    unhex(input["payload_hex"].as_str().unwrap()),
                );
                let bytes = p.encode().unwrap();
                assert_eq!(RpcPush::decode(&bytes).unwrap(), p, "decode {name}");
                bytes
            }
            other => panic!("unknown rpc kind {other}"),
        };
        assert_eq!(hex(&actual), expected_hex, "encode {name}");
    }
}

#[test]
fn events_vectors() {
    for vec in load("events.json") {
        let name = vec["name"].as_str().unwrap();
        let input = &vec["input"];
        let expected_hex = vec["hex"].as_str().unwrap();

        // Control payloads are keyed by "control"; application events by "profile".
        if let Some(control) = input["control"].as_str() {
            let bytes = match control {
                "hello" => Hello {
                    versions: input["versions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_u64().unwrap())
                        .collect(),
                    profiles: input["profiles"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect(),
                    service: opt_str(&input["service"]),
                    auth: opt_str(&input["auth"]),
                }
                .encode()
                .unwrap(),
                "hello_ack" => HelloAck {
                    v: input["v"].as_u64().unwrap(),
                    profile: input["profile"].as_str().unwrap().to_string(),
                    session: opt_str(&input["session"]),
                }
                .encode()
                .unwrap(),
                "ping" => Heartbeat {
                    nonce: input["nonce"].as_u64().unwrap(),
                    at: opt_u64(&input["at"]),
                }
                .encode()
                .unwrap(),
                "close" => Close {
                    status: csilgen_transport::Status::from_code(input["status"].as_i64().unwrap()),
                    reason: opt_str(&input["reason"]),
                }
                .encode()
                .unwrap(),
                other => panic!("unknown control {other}"),
            };
            assert_eq!(hex(&bytes), expected_hex, "encode {name}");
            continue;
        }

        let profile = match input["profile"].as_str().unwrap() {
            "verbose" => Profile::Verbose,
            "compact" => Profile::Compact,
            other => panic!("unknown profile {other}"),
        };
        let payload = unhex(input["payload_hex"].as_str().unwrap());
        let ev = match profile {
            Profile::Verbose => {
                let mut e = Event::verbose(
                    opt_str(&input["service"]),
                    input["event"].as_str().unwrap(),
                    payload,
                );
                e.id = opt_u64(&input["id"]);
                e
            }
            Profile::Compact => {
                let mut e = Event::compact(
                    input["service_ord"].as_u64().unwrap(),
                    input["op_ord"].as_u64().unwrap(),
                    payload,
                );
                e.id = opt_u64(&input["id"]);
                e
            }
        };
        let bytes = ev.encode(profile).unwrap();
        assert_eq!(hex(&bytes), expected_hex, "encode {name}");
        assert_eq!(Event::decode(&bytes, profile).unwrap(), ev, "decode {name}");
    }
}

#[test]
fn datagram_vectors() {
    for vec in load("datagrams.json") {
        let name = vec["name"].as_str().unwrap();
        let input = &vec["input"];
        let expected_hex = vec["hex"].as_str().unwrap();
        match input["profile"].as_str().unwrap() {
            "cbor-array" => {
                let d = Datagram::new(
                    input["op_ord"].as_u64().unwrap(),
                    input["seq"].as_u64().unwrap(),
                    unhex(input["payload_hex"].as_str().unwrap()),
                );
                let bytes = d.encode().unwrap();
                assert_eq!(hex(&bytes), expected_hex, "encode {name}");
                assert_eq!(Datagram::decode(&bytes).unwrap(), d, "decode {name}");
            }
            "compact-header" => {
                let mut d = CompactDatagram::new(
                    input["op_ord"].as_u64().unwrap() as u8,
                    input["seq"].as_u64().unwrap() as u16,
                    unhex(input["body_hex"].as_str().unwrap()),
                );
                if let Some(e) = opt_u64(&input["epoch"]) {
                    d = d.with_epoch(e as u8);
                }
                let bytes = d.encode();
                assert_eq!(hex(&bytes), expected_hex, "encode {name}");
                assert_eq!(CompactDatagram::decode(&bytes).unwrap(), d, "decode {name}");
            }
            other => panic!("unknown datagram profile {other}"),
        }
    }
}
