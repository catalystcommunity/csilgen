"""Round-trip unit tests mirroring the Rust ``lib.rs`` tests."""

from __future__ import annotations

import io
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from csilgen_transport import cbor  # noqa: E402
from csilgen_transport.carrier import (  # noqa: E402
    LoopbackFrameCarrier,
    StreamCarrier,
)
from csilgen_transport.conventions import (  # noqa: E402
    FrameTooLargeError,
    Status,
    StatusError,
    UnsupportedVersionError,
    canon_map,
    encode_value,
    tag24,
)
from csilgen_transport.datagrams import (  # noqa: E402
    CompactDatagram,
    Datagram,
    SeqTracker,
    advanced,
    first,
    late_or_duplicate,
    restart,
)
from csilgen_transport.events import (  # noqa: E402
    Close,
    Event,
    Hello,
    HelloAck,
    Heartbeat,
    Profile,
)
from csilgen_transport.rpc import (  # noqa: E402
    Reply,
    RpcClient,
    RpcRequest,
    RpcResponse,
    RpcServer,
    TransportOutcome,
)


class RpcTest(unittest.TestCase):
    def test_request_round_trip(self) -> None:
        req = RpcRequest("Attestation", "deposit-claim", payload=bytes([0xA0])).with_id(7)
        self.assertEqual(RpcRequest.decode(req.encode()), req)

    def test_response_success_and_error(self) -> None:
        ok = RpcResponse.ok("DepositClaimResponse", bytes([0xA0])).with_id(7)
        decoded = RpcResponse.decode(ok.encode())
        self.assertEqual(decoded.status, Status(0))
        self.assertEqual(decoded.variant, "DepositClaimResponse")
        self.assertIs(decoded.into_transport_error(), decoded)

        err = RpcResponse.transport_error(Status(2), "no such op")
        decoded = RpcResponse.decode(err.encode())
        self.assertEqual(decoded.status, Status(2))
        with self.assertRaises(StatusError):
            decoded.into_transport_error()

    def test_application_error_is_status_ok(self) -> None:
        # An application error rides as a typed variant with transport status 0.
        app_err = RpcResponse.ok("ServiceError", bytes([0xA1, 0x00, 0x01]))
        decoded = RpcResponse.decode(app_err.encode())
        self.assertEqual(decoded.status, Status(0))
        self.assertEqual(decoded.variant, "ServiceError")
        # into_transport_error leaves it ok -- the caller routes it by variant.
        self.assertIs(decoded.into_transport_error(), decoded)

    def test_client_server_over_loopback(self) -> None:
        req = RpcRequest("Echo", "say", payload=bytes([0x63, ord("h"), ord("i")])).with_id(1)
        carrier = LoopbackFrameCarrier()
        carrier.push_inbound(req.encode())
        server = RpcServer(carrier)
        served = server.serve_one(
            lambda r: Reply(variant="SayResponse", payload=r.payload)
        )
        self.assertTrue(served)
        # The framed response was placed on the carrier's outbound queue.
        out = carrier.take_outbound()
        self.assertIsNotNone(out)
        resp = RpcResponse.decode(out)
        self.assertEqual(resp.variant, "SayResponse")
        self.assertEqual(resp.payload, req.payload)
        self.assertEqual(resp.id, 1)

    def test_client_call_round_trip(self) -> None:
        # Pre-load a response the client will read after sending its request.
        carrier = LoopbackFrameCarrier()
        carrier.push_inbound(
            RpcResponse.ok("R", bytes([0xA0])).with_id(1).encode()
        )
        client = RpcClient(carrier, multiplexed=True)
        resp = client.call("S", "op", bytes([0xA0]))
        self.assertEqual(resp.variant, "R")
        # The outbound request carried the assigned correlation id.
        sent = RpcRequest.decode(carrier.take_outbound())
        self.assertEqual(sent.id, 1)

    def test_client_call_surfaces_transport_status(self) -> None:
        carrier = LoopbackFrameCarrier()
        carrier.push_inbound(
            RpcResponse.transport_error(Status(2), "no such op").with_id(1).encode()
        )
        client = RpcClient(carrier, multiplexed=True)
        with self.assertRaises(StatusError) as ctx:
            client.call("S", "op", bytes([0xA0]))
        self.assertEqual(ctx.exception.code, 2)

    def test_version_mismatch_rejected(self) -> None:
        bad = canon_map(
            [
                ("v", 2),
                ("service", "S"),
                ("op", "o"),
                ("payload", tag24(bytes([0xA0]))),
            ]
        )
        with self.assertRaises(UnsupportedVersionError):
            RpcRequest.decode(encode_value(bad))


class EventsTest(unittest.TestCase):
    def test_verbose_round_trip(self) -> None:
        ev = Event.verbose("World", "chat", bytes([0x60])).with_id(3)
        self.assertEqual(Event.decode(ev.encode(Profile.VERBOSE), Profile.VERBOSE), ev)

    def test_compact_round_trip_both_shapes(self) -> None:
        fire = Event.compact(1, 0, bytes([0x60]))
        self.assertEqual(
            Event.decode(fire.encode(Profile.COMPACT), Profile.COMPACT), fire
        )

        corr = Event.compact(1, 2, bytes([0x60])).with_id(42)
        decoded = Event.decode(corr.encode(Profile.COMPACT), Profile.COMPACT)
        self.assertEqual(decoded.id, 42)
        self.assertEqual(decoded, corr)

    def test_hello_negotiation(self) -> None:
        hello = Hello(
            versions=[1],
            profiles=["compact", "verbose"],
            service="World",
            auth=None,
        )
        round_tripped = Hello.decode(hello.encode())
        self.assertEqual(round_tripped, hello)
        chosen = round_tripped.negotiate([Profile.VERBOSE, Profile.COMPACT])
        self.assertEqual(chosen, (1, Profile.COMPACT))

    def test_hello_negotiation_fails_on_version(self) -> None:
        hello = Hello(versions=[99], profiles=["verbose"], service=None, auth=None)
        self.assertIsNone(hello.negotiate([Profile.VERBOSE]))

    def test_control_payload_round_trips(self) -> None:
        ack = HelloAck(v=1, profile="compact", session="s1")
        self.assertEqual(HelloAck.decode(ack.encode()), ack)
        beat = Heartbeat(nonce=9)
        self.assertEqual(Heartbeat.decode(beat.encode()), beat)
        close = Close(status=Status(5), reason="bad v")
        self.assertEqual(Close.decode(close.encode()), close)


class DatagramsTest(unittest.TestCase):
    def test_cbor_array_round_trip(self) -> None:
        dg = Datagram(0, 5, bytes([0x60]))
        self.assertEqual(Datagram.decode(dg.encode()), dg)
        # Wrong arity rejected.
        three = encode_value([1, 0, tag24(bytes([0x60]))])
        with self.assertRaises(Exception):
            Datagram.decode(three)

    def test_compact_header_round_trip(self) -> None:
        dg = CompactDatagram(1, 0x1234, body=bytes([1, 2, 3]))
        encoded = dg.encode()
        self.assertEqual(encoded[0] >> 4, 1)
        self.assertEqual(CompactDatagram.decode(encoded), dg)

        with_epoch = CompactDatagram(2, 7, body=bytes([9])).with_epoch(4)
        decoded = CompactDatagram.decode(with_epoch.encode())
        self.assertEqual(decoded.epoch, 4)
        self.assertEqual(decoded, with_epoch)

    def test_seq_tracker_detects_loss_reorder_restart(self) -> None:
        t = SeqTracker()
        self.assertEqual(t.observe(1, 0), first())
        self.assertEqual(t.observe(2, 0), advanced(0))
        self.assertEqual(t.observe(5, 0), advanced(2))  # lost 3, 4
        self.assertEqual(t.observe(3, 0), late_or_duplicate())
        self.assertEqual(t.observe(1, 1), restart())  # epoch bumped

    def test_seq_tracker_treats_unsequenced_as_advanced(self) -> None:
        # An all-unsequenced (seq 0) channel must never report late/duplicate.
        t = SeqTracker()
        self.assertEqual(t.observe(0, None), advanced(0))
        self.assertEqual(t.observe(0, None), advanced(0))
        self.assertEqual(t.observe(0, None), advanced(0))


class FramingTest(unittest.TestCase):
    def test_length_prefix_round_trips_over_loopback(self) -> None:
        c = LoopbackFrameCarrier()
        c.send_frame(bytes([1, 2, 3]))
        framed = c.take_outbound()
        c.push_inbound(framed)
        self.assertEqual(c.recv_frame(), bytes([1, 2, 3]))

    def test_stream_carrier_round_trip(self) -> None:
        # A BytesIO acts as the socket-like read/write/flush stream.
        buf = io.BytesIO()
        sender = StreamCarrier(buf)
        sender.send_frame(bytes([9, 8, 7]))
        buf.seek(0)
        receiver = StreamCarrier(buf)
        self.assertEqual(receiver.recv_frame(), bytes([9, 8, 7]))
        # Clean EOF returns None.
        self.assertIsNone(receiver.recv_frame())

    def test_stream_carrier_rejects_oversize_on_send(self) -> None:
        carrier = StreamCarrier(io.BytesIO(), max_frame=4)
        with self.assertRaises(FrameTooLargeError):
            carrier.send_frame(bytes(5))

    def test_stream_carrier_rejects_oversize_length_prefix(self) -> None:
        # A hostile 0xffffffff length prefix is rejected before allocation.
        buf = io.BytesIO((0xFFFFFFFF).to_bytes(4, "big"))
        carrier = StreamCarrier(buf, max_frame=16)
        with self.assertRaises(FrameTooLargeError):
            carrier.recv_frame()


class CborTest(unittest.TestCase):
    def test_canonical_map_key_ordering(self) -> None:
        # Keys must come out in deterministic order regardless of insertion order.
        a = cbor.encode({"service": "x", "v": 1, "op": "y"})
        b = cbor.encode({"v": 1, "op": "y", "service": "x"})
        self.assertEqual(a, b)
        self.assertEqual(cbor.decode(a), {"v": 1, "op": "y", "service": "x"})

    def test_negative_integers(self) -> None:
        for n in (-1, -24, -256, -65536, -(2**32)):
            self.assertEqual(cbor.decode(cbor.encode(n)), n)

    def test_tag24_round_trip(self) -> None:
        wrapped = tag24(bytes([0xA0]))
        decoded = cbor.decode(cbor.encode(wrapped))
        self.assertEqual(decoded.tag, 24)
        self.assertEqual(decoded.value, bytes([0xA0]))

    def test_trailing_bytes_rejected(self) -> None:
        with self.assertRaises(cbor.CborError):
            cbor.decode(cbor.encode(1) + b"\x00")

    def test_hostile_lengths_depth_and_text_rejected(self) -> None:
        bad = [
            bytes.fromhex("9b0000010000000000"),
            bytes.fromhex("bb0000010000000000"),
            b"\x61\xff",
            b"\xc0" * 65 + b"\x00",
        ]
        for payload in bad:
            with self.assertRaises((cbor.CborError, UnicodeDecodeError)):
                cbor.decode(payload)
        self.assertIsNotNone(cbor.decode(b"\xc0" * 64 + b"\x00"))


if __name__ == "__main__":
    unittest.main()
