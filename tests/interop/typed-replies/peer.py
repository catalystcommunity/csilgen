import json

from api.client import RpClient
from api.codec import RpErrorRefusal, encode_rp_decrypt_token_reply, resolve_rpc_reply
from api.types import Req, Resp, RpError, RpErrorCode

assert RpErrorCode.ACCOUNT_RATE_LIMITED.wire_number == 42
assert RpErrorCode.from_number(42) is RpErrorCode.ACCOUNT_RATE_LIMITED


def handle(req):
    if req.a == "limit":
        raise RpErrorRefusal(RpError(RpErrorCode.APPLICATION_USER_LIMIT, "limit"))
    return Resp("ok")


class Carrier:
    def call(self, service, op, payload):
        req = Req.from_cbor(payload)
        if req.a == "unknown":
            raise ConnectionError(2)
        variant, reply = encode_rp_decrypt_token_reply(handle, req)
        return resolve_rpc_reply(service, op, variant, reply)


value = RpError(RpErrorCode.ACCOUNT_RATE_LIMITED, "limit")
assert RpError.from_json(value.to_json()).code is RpErrorCode.ACCOUNT_RATE_LIMITED
assert json.dumps(value.code) == '"account_rate_limited"'
assert value.to_cbor() == bytes.fromhex("a264636f6465182a676d657373616765656c696d6974")
assert RpError.from_cbor(value.to_cbor()).code is RpErrorCode.ACCOUNT_RATE_LIMITED

client = RpClient(Carrier())
assert client.decrypt_token(Req("ok")).b == "ok"
try:
    client.decrypt_token(Req("limit"))
except RpErrorRefusal as exc:
    assert exc.value.code is RpErrorCode.APPLICATION_USER_LIMIT
else:
    raise AssertionError("missing refusal")
try:
    client.decrypt_token(Req("unknown"))
except ConnectionError as exc:
    assert exc.args == (2,)
else:
    raise AssertionError("missing transport error")
