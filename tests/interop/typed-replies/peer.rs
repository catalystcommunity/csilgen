#[path = "rust/mod.rs"]
mod api;
#[path = "choices/mod.rs"]
mod choices;
#[path = "rust/services.rs"]
mod services;

use api::*;
use csilgen_transport::{conventions::Status, rpc::RpcResponse};
use services::Rp;
use std::{env, fs};

struct Handler;
impl Rp for Handler {
    type Context = ();

    fn decrypt_token(&self, _: &(), req: Req) -> Result<Resp, RpError> {
        if req.a == "app" {
            Err(RpError {
                code: RpErrorCode::ApplicationUserLimit,
                message: "limit".into(),
            })
        } else {
            Ok(Resp { b: "yes".into() })
        }
    }
}

// A fake transport: it answers every call with the stored response envelope.
struct Carrier(Vec<u8>);
impl Transport for Carrier {
    fn call(&self, service: &str, op: &str, req: &[u8]) -> Result<Vec<u8>, ClientError> {
        Ok(self.call_reply(service, op, req)?.payload)
    }

    fn call_reply(&self, _: &str, _: &str, _: &[u8]) -> Result<WireReply, ClientError> {
        let reply = RpcResponse::decode(&self.0).map_err(|e| ClientError::Decode(e.to_string()))?;
        let reply = reply
            .into_transport_error()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(WireReply {
            variant: reply.variant,
            payload: reply.payload,
        })
    }
}

fn main() {
    let code = RpErrorCode::AccountRateLimited;
    assert_eq!(code.to_number(), 42);
    assert_eq!(RpErrorCode::from_number(42), Some(code));
    assert_eq!(RpErrorCode::from_number(1), None);
    let retry = choices::Retry { delay: 3 };
    let (variant, payload) =
        choices::encode_api_submit_reply(Err(choices::ApiSubmitError::Retry(retry))).unwrap();
    assert!(matches!(
        choices::resolve_rpc_reply("Api", "submit", Some(variant), payload),
        Err(choices::ClientError::ApplicationRetry(e)) if e.delay == 3
    ));
    let value = choices::Reply::Variant1(choices::Other { c: 7 });
    let plain = choices::encode_api_fetch_response(&value).unwrap();
    assert_eq!(plain[0..2], [0x82, 1]);
    let (variant, payload) = choices::encode_api_poll_reply(Ok(value)).unwrap();
    assert_eq!(variant, "Reply");
    assert_eq!(payload, plain);
    let read_peer = env::args().nth(1).is_some();
    for name in ["ok", "app", "transport", "unknown", "missing", "malformed"] {
        let req = Req { a: name.into() };
        let (variant, payload) =
            encode_rp_decrypt_token_reply(Handler.decrypt_token(&(), req.clone())).unwrap();
        if name == "app" {
            assert_eq!(payload, b"\xa2\x64code\x07\x67message\x65limit");
        }
        let mut reply = RpcResponse::ok(variant, payload);
        match name {
            "transport" => {
                reply = RpcResponse::transport_error(Status::UnknownServiceOrOp, "unknown")
            }
            "unknown" => reply.variant = Some("Other".into()),
            "missing" => reply.variant = None,
            "malformed" => reply.payload = vec![0xff],
            _ => {}
        }
        let wire = reply.encode().unwrap();
        fs::write(format!("rust-{name}.cbor"), &wire).unwrap();
        if read_peer {
            let peer = fs::read(format!("go-{name}.cbor")).unwrap();
            assert_eq!(wire, peer, "{name} envelope");
            let received = RpClient::new(Carrier(peer)).decrypt_token(req);
            match (name, received) {
                ("ok", Ok(value)) => assert_eq!(value.b, "yes"),
                ("app", Err(ClientError::ApplicationRpError(e))) => {
                    assert_eq!(e.code, RpErrorCode::ApplicationUserLimit);
                    assert_eq!(e.message, "limit");
                }
                ("transport", Err(ClientError::Transport(message))) => {
                    assert!(message.contains("2"))
                }
                ("unknown" | "missing" | "malformed", Err(ClientError::Decode(_))) => {}
                outcome => panic!("unexpected {outcome:?}"),
            }
        }
    }
}
