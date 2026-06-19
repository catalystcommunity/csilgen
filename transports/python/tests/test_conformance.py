"""Verify the Python reference library against the checked-in conformance vectors.

This both guards the Python encoders/decoders against drift and demonstrates the
contract every language's reference library follows: reconstruct each vector's
envelope from its language-neutral ``input``, assert encode -> ``hex``, and assert
decode(``hex``) -> the same envelope. Ported from ``transports/rust/tests/conformance.rs``.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

# Make the package importable when run from anywhere via `unittest discover`.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from csilgen_transport.conventions import Status  # noqa: E402
from csilgen_transport.datagrams import CompactDatagram, Datagram  # noqa: E402
from csilgen_transport.events import (  # noqa: E402
    Close,
    Event,
    Hello,
    HelloAck,
    Heartbeat,
    Profile,
)
from csilgen_transport.rpc import RpcPush, RpcRequest, RpcResponse  # noqa: E402

_CONFORMANCE_DIR = (
    Path(__file__).resolve().parent.parent.parent / "conformance"
)


def _load(name: str) -> list[dict]:
    doc = json.loads((_CONFORMANCE_DIR / name).read_text())
    return doc["vectors"]


def _unhex(text: str) -> bytes:
    return bytes.fromhex(text)


class RpcVectorsTest(unittest.TestCase):
    def test_rpc_vectors(self) -> None:
        for vec in _load("rpc.json"):
            name = vec["name"]
            inp = vec["input"]
            expected_hex = vec["hex"]
            kind = inp["kind"]
            with self.subTest(vector=name):
                if kind == "request":
                    env = RpcRequest(
                        service=inp["service"],
                        op=inp["op"],
                        id=inp["id"],
                        payload=_unhex(inp["payload_hex"]),
                        auth=inp["auth"],
                    )
                    encoded = env.encode()
                    self.assertEqual(RpcRequest.decode(encoded), env)
                elif kind == "response":
                    env = RpcResponse(
                        id=inp["id"],
                        status=Status.from_code(inp["status"]),
                        variant=inp["variant"],
                        error=inp["error"],
                        payload=_unhex(inp["payload_hex"]),
                    )
                    encoded = env.encode()
                    self.assertEqual(RpcResponse.decode(encoded), env)
                elif kind == "push":
                    env = RpcPush(
                        service=inp["service"],
                        event=inp["event"],
                        payload=_unhex(inp["payload_hex"]),
                    )
                    encoded = env.encode()
                    self.assertEqual(RpcPush.decode(encoded), env)
                else:
                    self.fail(f"unknown rpc kind {kind}")
                self.assertEqual(encoded.hex(), expected_hex)


class EventsVectorsTest(unittest.TestCase):
    def test_events_vectors(self) -> None:
        for vec in _load("events.json"):
            name = vec["name"]
            inp = vec["input"]
            expected_hex = vec["hex"]
            with self.subTest(vector=name):
                control = inp.get("control")
                if control is not None:
                    if control == "hello":
                        encoded = Hello(
                            versions=list(inp["versions"]),
                            profiles=list(inp["profiles"]),
                            service=inp.get("service"),
                            auth=inp.get("auth"),
                        ).encode()
                    elif control == "hello_ack":
                        encoded = HelloAck(
                            v=inp["v"],
                            profile=inp["profile"],
                            session=inp.get("session"),
                        ).encode()
                    elif control == "ping":
                        encoded = Heartbeat(
                            nonce=inp["nonce"], at=inp.get("at")
                        ).encode()
                    elif control == "close":
                        encoded = Close(
                            status=Status.from_code(inp["status"]),
                            reason=inp.get("reason"),
                        ).encode()
                    else:
                        self.fail(f"unknown control {control}")
                    self.assertEqual(encoded.hex(), expected_hex)
                    continue

                profile = Profile(inp["profile"])
                payload = _unhex(inp["payload_hex"])
                if profile is Profile.VERBOSE:
                    env = Event.verbose(inp.get("service"), inp["event"], payload)
                    env.id = inp.get("id")
                else:
                    env = Event.compact(
                        inp["service_ord"], inp["op_ord"], payload
                    )
                    env.id = inp.get("id")
                encoded = env.encode(profile)
                self.assertEqual(encoded.hex(), expected_hex)
                self.assertEqual(Event.decode(encoded, profile), env)


class DatagramVectorsTest(unittest.TestCase):
    def test_datagram_vectors(self) -> None:
        for vec in _load("datagrams.json"):
            name = vec["name"]
            inp = vec["input"]
            expected_hex = vec["hex"]
            with self.subTest(vector=name):
                profile = inp["profile"]
                if profile == "cbor-array":
                    env = Datagram(
                        op_ord=inp["op_ord"],
                        seq=inp["seq"],
                        payload=_unhex(inp["payload_hex"]),
                    )
                    encoded = env.encode()
                    self.assertEqual(encoded.hex(), expected_hex)
                    self.assertEqual(Datagram.decode(encoded), env)
                elif profile == "compact-header":
                    env = CompactDatagram(
                        op_ord=inp["op_ord"],
                        seq=inp["seq"],
                        epoch=inp.get("epoch"),
                        body=_unhex(inp["body_hex"]),
                    )
                    encoded = env.encode()
                    self.assertEqual(encoded.hex(), expected_hex)
                    self.assertEqual(CompactDatagram.decode(encoded), env)
                else:
                    self.fail(f"unknown datagram profile {profile}")


if __name__ == "__main__":
    unittest.main()
