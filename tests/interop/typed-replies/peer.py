import asyncio
import json

from api.client import RpClient
from api.client_async import RpAsyncClient
from api.codec import RpErrorRefusal, encode_rp_decrypt_token_reply
from api.types import Req, Resp, RpError, RpErrorCode

assert RpErrorCode.ACCOUNT_RATE_LIMITED.wire_number == 42
assert RpErrorCode.from_number(42) is RpErrorCode.ACCOUNT_RATE_LIMITED


def handle(req):
    if req.a == "limit":
        raise RpErrorRefusal(RpError(RpErrorCode.APPLICATION_USER_LIMIT, "limit"))
    return Resp("ok")


class Carrier:
    """A fake carrier: the reply variant reaches the client only through call_reply."""

    def __init__(self, answer):
        self._answer = answer

    def call(self, service, op, payload):
        raise AssertionError("a multi-arm op must not use call")

    def call_reply(self, service, op, payload):
        assert (service, op) == ("Rp", "decrypt-token"), (service, op)
        return self._answer(Req.from_cbor(payload))


class AsyncCarrier:
    def __init__(self, answer):
        self._sync = Carrier(answer)

    async def call(self, service, op, payload):
        return self._sync.call(service, op, payload)

    async def call_reply(self, service, op, payload):
        return self._sync.call_reply(service, op, payload)


def outcomes(answer, a="x"):
    """The result of one call through the sync client and one through the async client."""

    def classify(run):
        try:
            return ("ok", run().b)
        except RpErrorRefusal as exc:
            return ("refusal", exc.value.code)
        except ConnectionError as exc:
            return ("transport", exc.args)
        except ValueError:
            return ("error", None)

    sync = classify(lambda: RpClient(Carrier(answer)).decrypt_token(Req(a)))
    loop_client = RpAsyncClient(AsyncCarrier(answer))
    run_async = classify(lambda: asyncio.run(loop_client.decrypt_token(Req(a))))
    return [sync, run_async]


def expect(got, want, label):
    assert got == [want, want], f"{label}: got {got}, want {want} from both clients"


value = RpError(RpErrorCode.ACCOUNT_RATE_LIMITED, "limit")
assert RpError.from_json(value.to_json()).code is RpErrorCode.ACCOUNT_RATE_LIMITED
assert json.dumps(value.code) == '"account_rate_limited"'
assert value.to_cbor() == bytes.fromhex("a264636f6465182a676d657373616765656c696d6974")
assert RpError.from_cbor(value.to_cbor()).code is RpErrorCode.ACCOUNT_RATE_LIMITED


def served(req):
    return encode_rp_decrypt_token_reply(handle, req)


expect(outcomes(served, "ok"), ("ok", "ok"), "server reply for ok")
expect(
    outcomes(served, "limit"),
    ("refusal", RpErrorCode.APPLICATION_USER_LIMIT),
    "server reply for limit",
)
resp = Resp("ok").to_cbor()
expect(outcomes(lambda _: ("Resp", resp)), ("ok", "ok"), "variant Resp")
expect(outcomes(lambda _: ("Other", resp)), ("error", None), "variant Other")
expect(outcomes(lambda _: (None, resp)), ("error", None), "no variant")


def refuse(_):
    raise ConnectionError(2)


expect(outcomes(refuse), ("transport", (2,)), "transport error")
print("ok")
