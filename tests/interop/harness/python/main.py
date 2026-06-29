#!/usr/bin/env python3
"""Python interop harness -- mirrors the Rust reference (tests/interop/harness/rust).

Runs as a server (binds a loopback TCP/UDP port, serves one transport) or a client
(connects, runs the case battery for one transport, prints one JSON results object).
RPC + Events ride TCP; datagrams ride UDP. The on-wire behavior and case names match
the Rust reference so a Python client works against a Rust server and vice versa.
See tests/interop/README.md for the contract.
"""

from __future__ import annotations

import json
import socket
import sys
import time
from datetime import datetime, timezone
from decimal import Decimal

from csilgen_transport.carrier import StreamCarrier
from csilgen_transport.conventions import Status
from csilgen_transport.datagrams import Datagram
from csilgen_transport.events import Event, Hello, HelloAck, Profile, control
from csilgen_transport.rpc import Reply, RpcClient, RpcServer, TransportOutcome

from interop_api import (
    Collections,
    Constrained,
    EchoNestedResult,
    InteropClient,
    InteropHandlers,
    Nested,
    Scalars,
    ServiceError,
    Tick,
    encode_interop_duplex,
    encode_interop_on_tick,
    route_interop_channel,
)

SERVICE = "interop"


# ---------------------------------------------------------------------------
# Codec adapter -- the generated router/encoders are codec-agnostic; wire them
# to the per-type CBOR codecs bound onto the generated types.
# ---------------------------------------------------------------------------


class CborCodec:
    def encode(self, value):
        return value.to_cbor()

    def decode(self, data, target_type):
        return target_type.from_cbor(data)


CODEC = CborCodec()


# ---------------------------------------------------------------------------
# Fixed language-neutral test vectors (see README "Fixed test vectors").
# ---------------------------------------------------------------------------


def ts() -> datetime:
    return datetime(2026, 6, 29, 12, 34, 56, tzinfo=timezone.utc)


def scalars_ok() -> Scalars:
    return Scalars(
        i=-42,
        u=42,
        n=-7,
        f=3.5,
        t="héllo 世界",
        raw=bytes([0x01, 0x02, 0xF0, 0xFF]),
        flag=True,
        when=ts(),
        amount=Decimal("123.45"),
    )


def collections_ok() -> Collections:
    return Collections(
        names=["a", "b"],
        at_least_one=[1, 2, 3],
        bounded=[10, 20],
        exact_3=[7, 8, 9],
        scores={"x": 1, "y": 2},
        color="green",
        prio=2,
        who=4242,
        pair=("p", 5),
        triple=("t", 9, True),
        extra={"k": "v"},
    )


def constrained_ok() -> Constrained:
    return Constrained(
        code="PRD-AB12CD",
        qty=10,
        rate=Decimal("0.25"),
        password="hunter2hunter2",
        tags=["one", "two"],
    )


def constrained_bad() -> Constrained:
    # The generated dataclass validates in __post_init__ and would reject these
    # values; bypass construction-time validation so the client can send an
    # intentionally-invalid payload (the server's decode then surfaces the error).
    bad = Constrained.__new__(Constrained)
    bad.code = "bad"
    bad.qty = 0
    bad.rate = Decimal("9.9")
    bad.password = "x"
    bad.tags = []
    return bad


def nested_ok() -> Nested:
    return Nested(inner=scalars_ok(), maybe=constrained_ok(), many=[scalars_ok()])


# ---------------------------------------------------------------------------
# Result accumulation + JSON output.
# ---------------------------------------------------------------------------


class Cases:
    def __init__(self, transport: str) -> None:
        self.lang = "python"
        self.transport = transport
        self.out: list[tuple[str, bool, str]] = []

    def pass_(self, name: str) -> None:
        self.out.append((name, True, ""))

    def fail(self, name: str, detail: str) -> None:
        self.out.append((name, False, detail))

    def check(self, name: str, cond: bool, detail: str) -> None:
        if cond:
            self.pass_(name)
        else:
            self.fail(name, detail)

    def emit(self) -> None:
        obj = {
            "lang": self.lang,
            "transport": self.transport,
            "cases": [
                {"name": name, "ok": ok, "detail": detail}
                for (name, ok, detail) in self.out
            ],
        }
        print(json.dumps(obj))


# ---------------------------------------------------------------------------
# Server-side handler implementing the generated trait.
# ---------------------------------------------------------------------------


class Handlers(InteropHandlers):
    def echo_scalars(self, req: Scalars, ctx: dict) -> Scalars:
        return req

    def echo_collections(self, req: Collections, ctx: dict) -> Collections:
        return req

    def echo_nested(self, req: Nested, ctx: dict) -> EchoNestedResult:
        return EchoNestedResult(ok=True, echo=req)

    def validate_constrained(self, req: Constrained, ctx: dict) -> Constrained:
        # Decode already ran validation (__post_init__); reaching here means valid.
        return req

    def duplex(self, msg: Scalars, ctx: dict) -> None:
        return None


# ---------------------------------------------------------------------------
# RPC
# ---------------------------------------------------------------------------


class RpcTransport:
    """Implements the generated Transport protocol over the lib's RpcClient."""

    def __init__(self, client: RpcClient) -> None:
        self._client = client

    def call(self, service: str, method: str, req: bytes) -> bytes:
        # RpcClient.call raises StatusError on a non-ok transport status. A status-0
        # reply whose variant is the ServiceError arm is a typed application error;
        # surface it by raising so the typed client method raises (the carrier owns
        # this mapping). Otherwise hand back the success payload.
        resp = self._client.call(service, method, req)
        if resp.variant == "ServiceError":
            se = ServiceError.from_cbor(resp.payload)
            raise RuntimeError(f"service error {se.code}: {se.message}")
        return resp.payload


def rpc_server(listener: socket.socket) -> None:
    handlers = Handlers()

    def dispatch(req):
        op = req.op
        try:
            if op == "EchoScalars":
                v = Scalars.from_cbor(req.payload)
                return Reply("Scalars", handlers.echo_scalars(v, {}).to_cbor())
            if op == "EchoCollections":
                v = Collections.from_cbor(req.payload)
                return Reply(
                    "Collections", handlers.echo_collections(v, {}).to_cbor()
                )
            if op == "EchoNested":
                v = Nested.from_cbor(req.payload)
                return Reply(
                    "EchoNestedResult", handlers.echo_nested(v, {}).to_cbor()
                )
            if op == "ValidateConstrained":
                # Decode validates; an invalid payload raises and is returned as the
                # typed ServiceError arm (status-0 variant), the failure-case path.
                try:
                    v = Constrained.from_cbor(req.payload)
                except Exception as exc:
                    se = ServiceError(code=422, message=str(exc))
                    return Reply("ServiceError", se.to_cbor())
                return Reply(
                    "Constrained", handlers.validate_constrained(v, {}).to_cbor()
                )
            return TransportOutcome(Status(2), f"unknown op {op}")
        except Exception as exc:
            return TransportOutcome(Status(6), str(exc))

    while True:
        conn, _ = listener.accept()
        stream = conn.makefile("rwb")
        server = RpcServer(StreamCarrier(stream))
        try:
            while server.serve_one(dispatch):
                pass
        except Exception:
            pass
        finally:
            stream.close()
            conn.close()


def rpc_client(stream) -> Cases:
    cases = Cases("rpc")
    transport = RpcTransport(RpcClient(StreamCarrier(stream), False))
    client = InteropClient(transport)

    try:
        r = client.echo_scalars(scalars_ok())
        cases.check("echo-scalars/success", r == scalars_ok(), f"mismatch: {r!r}")
    except Exception as exc:
        cases.fail("echo-scalars/success", str(exc))

    try:
        r = client.echo_collections(collections_ok())
        cases.check(
            "echo-collections/success", r == collections_ok(), f"mismatch: {r!r}"
        )
    except Exception as exc:
        cases.fail("echo-collections/success", str(exc))

    try:
        r = client.echo_nested(nested_ok())
        cases.check(
            "echo-nested/success",
            r.ok and r.echo == nested_ok(),
            f"mismatch: {r!r}",
        )
    except Exception as exc:
        cases.fail("echo-nested/success", str(exc))

    try:
        r = client.validate_constrained(constrained_ok())
        cases.check(
            "validate-constrained/success", r == constrained_ok(), f"mismatch: {r!r}"
        )
    except Exception as exc:
        cases.fail("validate-constrained/success", str(exc))

    try:
        client.validate_constrained(constrained_bad())
        cases.fail("validate-constrained/failure", "server accepted invalid input")
    except Exception:
        cases.pass_("validate-constrained/failure")

    return cases


# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------

TICKS = 3


def send_event(carrier: StreamCarrier, ev: Event) -> None:
    carrier.send_frame(ev.encode(Profile.VERBOSE))


def events_server(listener: socket.socket) -> None:
    handlers = Handlers()
    while True:
        conn, _ = listener.accept()
        stream = conn.makefile("rwb")
        carrier = StreamCarrier(stream)
        try:
            # Handshake: read $hello, reply $hello-ack (verbose).
            frame = carrier.recv_frame()
            if frame is None:
                continue
            ev = Event.decode(frame, Profile.VERBOSE)
            if ev.event != control.HELLO_NAME:
                continue
            Hello.decode(ev.payload)
            ack = HelloAck(v=1, profile="verbose", session=None)
            send_event(
                carrier, Event.verbose(None, control.HELLO_ACK_NAME, ack.encode())
            )

            # Push N ticks (on-tick / server push).
            for seq in range(TICKS):
                method, payload = encode_interop_on_tick(CODEC, Tick(seq=seq))
                send_event(carrier, Event.verbose(SERVICE, method, payload))

            # React loop.
            while True:
                frame = carrier.recv_frame()
                if frame is None:
                    break
                try:
                    ev = Event.decode(frame, Profile.VERBOSE)
                except Exception:
                    break
                method = ev.event or ""
                if method == control.PING_NAME:
                    send_event(
                        carrier, Event.verbose(None, control.PONG_NAME, ev.payload)
                    )
                elif method == control.CLOSE_NAME:
                    break
                elif method == "Duplex":
                    try:
                        s = Scalars.from_cbor(ev.payload)
                    except Exception:
                        continue
                    handlers.duplex(s, {})
                    m, b = encode_interop_duplex(CODEC, s)
                    send_event(carrier, Event.verbose(SERVICE, m, b))
                else:
                    # Exercise the generated router's unknown-method path.
                    try:
                        route_interop_channel(handlers, CODEC, method, ev.payload, {})
                    except Exception:
                        pass
                    send_event(
                        carrier, Event.verbose(None, control.ERROR_NAME, b"")
                    )
        except Exception:
            pass
        finally:
            stream.close()
            conn.close()


def events_client(stream) -> Cases:
    cases = Cases("events")
    carrier = StreamCarrier(stream)

    # Handshake.
    hello = Hello(versions=[1], profiles=["verbose"], service=SERVICE, auth=None)
    send_event(carrier, Event.verbose(None, control.HELLO_NAME, hello.encode()))
    ack_ok = False
    frame = carrier.recv_frame()
    if frame is not None:
        try:
            ack_ok = Event.decode(frame, Profile.VERBOSE).event == control.HELLO_ACK_NAME
        except Exception:
            ack_ok = False
    cases.check("events/handshake", ack_ok, "no $hello-ack")

    # on-tick: expect TICKS pushes with seq 0..TICKS.
    tick_ok = True
    detail = ""
    for expect in range(TICKS):
        frame = carrier.recv_frame()
        if frame is None:
            tick_ok = False
            detail = "stream closed during ticks"
            break
        try:
            ev = Event.decode(frame, Profile.VERBOSE)
            if ev.event != "OnTick":
                tick_ok = False
                detail = f"expected OnTick got {ev.event!r}"
                break
            t = Tick.from_cbor(ev.payload)
            if t.seq != expect:
                tick_ok = False
                detail = f"tick seq {t.seq} != {expect}"
                break
        except Exception as exc:
            tick_ok = False
            detail = f"decode: {exc}"
            break
    cases.check("on-tick/success", tick_ok, detail)

    # duplex: send Scalars, expect echo.
    m, b = encode_interop_duplex(CODEC, scalars_ok())
    send_event(carrier, Event.verbose(SERVICE, m, b))
    frame = carrier.recv_frame()
    if frame is None:
        cases.fail("duplex/success", "no echo")
    else:
        try:
            ev = Event.decode(frame, Profile.VERBOSE)
            if ev.event == "Duplex":
                s = Scalars.from_cbor(ev.payload)
                cases.check("duplex/success", s == scalars_ok(), "echo mismatch")
            else:
                cases.fail("duplex/success", f"got {ev.event!r}")
        except Exception as exc:
            cases.fail("duplex/success", str(exc))

    # unknown-method: send a bogus channel method, expect $error.
    send_event(carrier, Event.verbose(SERVICE, "Bogus", b""))
    frame = carrier.recv_frame()
    if frame is None:
        cases.fail("unknown-method/failure", "no $error")
    else:
        is_err = False
        try:
            is_err = Event.decode(frame, Profile.VERBOSE).event == control.ERROR_NAME
        except Exception:
            is_err = False
        cases.check("unknown-method/failure", is_err, "expected $error")

    send_event(carrier, Event.verbose(None, control.CLOSE_NAME, b""))
    return cases


# ---------------------------------------------------------------------------
# Datagrams
# ---------------------------------------------------------------------------

OP_ECHO_SCALARS = 0
OP_ECHO_COLLECTIONS = 1
OP_ERROR_SENTINEL = 255


def datagram_server(sock: socket.socket) -> None:
    while True:
        try:
            data, addr = sock.recvfrom(65536)
        except OSError:
            continue
        if not addr:
            continue
        try:
            dg = Datagram.decode(data)
        except Exception:
            continue
        if dg.op_ord == OP_ECHO_SCALARS:
            try:
                s = Scalars.from_cbor(dg.payload)
                reply = Datagram(OP_ECHO_SCALARS, dg.seq, s.to_cbor())
            except Exception:
                reply = Datagram(OP_ERROR_SENTINEL, dg.seq, b"")
        elif dg.op_ord == OP_ECHO_COLLECTIONS:
            try:
                c = Collections.from_cbor(dg.payload)
                reply = Datagram(OP_ECHO_COLLECTIONS, dg.seq, c.to_cbor())
            except Exception:
                reply = Datagram(OP_ERROR_SENTINEL, dg.seq, b"")
        else:
            reply = Datagram(OP_ERROR_SENTINEL, dg.seq, b"")
        try:
            sock.sendto(reply.encode(), addr)
        except OSError:
            continue


def datagram_client(sock: socket.socket) -> Cases:
    cases = Cases("datagrams")
    sock.settimeout(3.0)

    def roundtrip(op: int, seq: int, payload: bytes) -> Datagram:
        sock.send(Datagram(op, seq, payload).encode())
        return Datagram.decode(sock.recv(65536))

    try:
        d = roundtrip(OP_ECHO_SCALARS, 1, scalars_ok().to_cbor())
        if d.op_ord != OP_ECHO_SCALARS:
            cases.fail("echo-scalars/success", f"op_ord {d.op_ord}")
        else:
            s = Scalars.from_cbor(d.payload)
            cases.check(
                "echo-scalars/success", s == scalars_ok(), "payload mismatch"
            )
    except Exception as exc:
        cases.fail("echo-scalars/success", str(exc))

    try:
        d = roundtrip(OP_ECHO_COLLECTIONS, 2, collections_ok().to_cbor())
        if d.op_ord != OP_ECHO_COLLECTIONS:
            cases.fail("echo-collections/success", f"op_ord {d.op_ord}")
        else:
            c = Collections.from_cbor(d.payload)
            cases.check(
                "echo-collections/success", c == collections_ok(), "payload mismatch"
            )
    except Exception as exc:
        cases.fail("echo-collections/success", str(exc))

    try:
        d = roundtrip(99, 3, b"")
        cases.check(
            "bad-op-ord/failure",
            d.op_ord == OP_ERROR_SENTINEL,
            f"expected error sentinel, got op_ord {d.op_ord}",
        )
    except Exception as exc:
        cases.fail("bad-op-ord/failure", str(exc))

    return cases


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def connect_retry(port: int) -> socket.socket:
    last_exc: Exception = OSError("connect failed")
    for _ in range(200):
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            sock.connect(("127.0.0.1", port))
            return sock
        except OSError as exc:
            last_exc = exc
            sock.close()
            time.sleep(0.015)
    raise last_exc


def main() -> None:
    args = sys.argv
    if len(args) < 4:
        print(
            "usage: main.py <server|client> <rpc|events|datagrams> <port>",
            file=sys.stderr,
        )
        sys.exit(2)
    mode, transport, port = args[1], args[2], int(args[3])

    if mode == "server" and transport == "datagrams":
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("127.0.0.1", port))
        sys.stdout.write("READY\n")
        sys.stdout.flush()
        datagram_server(sock)
    elif mode == "server":
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        # Rapid rebind across the sequential per-transport runs would otherwise hit
        # TCP TIME_WAIT on the just-freed port.
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", port))
        listener.listen(128)
        sys.stdout.write("READY\n")
        sys.stdout.flush()
        if transport == "rpc":
            rpc_server(listener)
        elif transport == "events":
            events_server(listener)
        else:
            print("bad transport", file=sys.stderr)
            sys.exit(2)
    elif mode == "client" and transport == "datagrams":
        # Bind an ephemeral local UDP port; the server replies to this source.
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("127.0.0.1", 0))
        sock.connect(("127.0.0.1", port))
        try:
            datagram_client(sock).emit()
        finally:
            sock.close()
    elif mode == "client":
        sock = connect_retry(port)
        stream = sock.makefile("rwb")
        if transport == "rpc":
            cases = rpc_client(stream)
        elif transport == "events":
            cases = events_client(stream)
        else:
            print("bad transport", file=sys.stderr)
            sys.exit(2)
        cases.emit()
        stream.close()
        sock.close()
    else:
        print("bad mode/transport", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
