"""CSIL-RPC transport -- request/response/push envelopes -- see ``csil-rpc-transport.md``."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Optional, Union

from .carrier import FrameCarrier
from .conventions import (
    VERSION,
    CarrierError,
    MalformedError,
    Status,
    StatusError,
    canon_map,
    check_version,
    decode_value,
    encode_value,
    get_int,
    get_text,
    get_text_opt,
    get_uint,
    get_uint_opt,
    map_get,
    tag24,
    untag24,
)


@dataclass
class RpcRequest:
    """A CSIL-RPC request (client -> server)."""

    service: str
    op: str
    id: Optional[int] = None
    # Opaque CBOR(request type) bytes (wrapped in tag 24 on the wire).
    payload: bytes = b""
    auth: Optional[str] = None

    def with_id(self, id: int) -> "RpcRequest":
        self.id = id
        return self

    def with_auth(self, auth: str) -> "RpcRequest":
        self.auth = auth
        return self

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [
            ("v", VERSION),
            ("service", self.service),
            ("op", self.op),
            ("payload", tag24(self.payload)),
        ]
        if self.id is not None:
            entries.append(("id", self.id))
        if self.auth is not None:
            entries.append(("auth", self.auth))
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "RpcRequest":
        value = decode_value(data)
        check_version(get_uint(value, "v"))
        payload_value = map_get(value, "payload")
        if payload_value is None:
            raise MalformedError("missing 'payload'")
        return cls(
            service=get_text(value, "service"),
            op=get_text(value, "op"),
            id=get_uint_opt(value, "id"),
            payload=untag24(payload_value),
            auth=get_text_opt(value, "auth"),
        )


@dataclass
class RpcResponse:
    """A CSIL-RPC response (server -> client)."""

    id: Optional[int]
    status: Status
    # Which declared output-choice arm `payload` decodes to (the CSIL type name).
    variant: Optional[str]
    error: Optional[str]
    # Opaque CBOR(output type) bytes; empty when `status` is non-zero.
    payload: bytes = b""

    @classmethod
    def ok(cls, variant: str, payload: bytes) -> "RpcResponse":
        """A successful (status Ok) typed reply."""
        return cls(id=None, status=Status(0), variant=variant, error=None, payload=payload)

    @classmethod
    def transport_error(cls, status: Status, message: str) -> "RpcResponse":
        """A transport-level failure (no typed payload)."""
        return cls(id=None, status=status, variant=None, error=message, payload=b"")

    def with_id(self, id: Optional[int]) -> "RpcResponse":
        self.id = id
        return self

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [
            ("v", VERSION),
            ("status", self.status.code),
            ("payload", tag24(self.payload)),
        ]
        if self.id is not None:
            entries.append(("id", self.id))
        if self.variant is not None:
            entries.append(("variant", self.variant))
        if self.error is not None:
            entries.append(("error", self.error))
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "RpcResponse":
        value = decode_value(data)
        check_version(get_uint(value, "v"))
        # payload is present but may be an empty byte string on error.
        payload_value = map_get(value, "payload")
        payload = untag24(payload_value) if payload_value is not None else b""
        return cls(
            id=get_uint_opt(value, "id"),
            status=Status.from_code(get_int(value, "status")),
            variant=get_text_opt(value, "variant"),
            error=get_text_opt(value, "error"),
            payload=payload,
        )

    def into_transport_error(self) -> "RpcResponse":
        """Return self when the status is ok; otherwise raise :class:`StatusError`.
        Callers use this after ``decode`` to surface transport failures distinctly
        from application errors (which ride as a status-0 typed variant)."""
        if self.status.is_ok:
            return self
        raise StatusError(self.status, self.error)


@dataclass
class RpcPush:
    """A CSIL-RPC server push (server -> client) for ``<-`` operations."""

    service: str
    event: str
    payload: bytes = b""

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [
            ("v", VERSION),
            ("service", self.service),
            ("event", self.event),
            ("payload", tag24(self.payload)),
        ]
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "RpcPush":
        value = decode_value(data)
        check_version(get_uint(value, "v"))
        payload_value = map_get(value, "payload")
        if payload_value is None:
            raise MalformedError("missing 'payload'")
        return cls(
            service=get_text(value, "service"),
            event=get_text(value, "event"),
            payload=untag24(payload_value),
        )


@dataclass
class Reply:
    """A successful handler outcome: the chosen variant name and encoded payload."""

    variant: str
    payload: bytes


@dataclass
class TransportOutcome:
    """A transport-level handler outcome (no typed payload)."""

    status: Status
    message: str


# What a server handler returns for one request.
HandlerOutcome = Union[Reply, TransportOutcome]


class RpcClient:
    """A CSIL-RPC client over a frame carrier. The carrier is injected (bring your
    own); the client owns the envelope and a per-connection monotonic correlation id.
    """

    def __init__(self, carrier: FrameCarrier, multiplexed: bool) -> None:
        # `multiplexed` true assigns a correlation id to every request (required on
        # WS / pipelined streams); false omits it (one-in-flight carriers).
        self._carrier = carrier
        self._next_id = 1
        self._multiplexed = multiplexed

    def call(
        self,
        service: str,
        op: str,
        payload: bytes,
        auth: Optional[str] = None,
    ) -> RpcResponse:
        """Invoke ``service/op`` with an encoded request payload, returning the
        decoded response. A non-zero transport status is raised as
        :class:`StatusError`."""
        req = RpcRequest(service, op, payload=payload, auth=auth)
        if self._multiplexed:
            req.id = self._next_id
            self._next_id += 1
        self._carrier.send_frame(req.encode())
        frame = self._carrier.recv_frame()
        if frame is None:
            raise CarrierError("connection closed before response")
        return RpcResponse.decode(frame).into_transport_error()

    @property
    def carrier(self) -> FrameCarrier:
        return self._carrier


class RpcServer:
    """A CSIL-RPC server over a frame carrier. The host supplies a handler mapping a
    :class:`RpcRequest` to an outcome; the generated router is the natural
    implementation of that handler."""

    def __init__(self, carrier: FrameCarrier) -> None:
        self._carrier = carrier

    def serve_one(self, handler: Callable[[RpcRequest], HandlerOutcome]) -> bool:
        """Read one request, dispatch it through ``handler``, and write the response.
        Returns ``False`` at a clean end of stream."""
        frame = self._carrier.recv_frame()
        if frame is None:
            return False
        try:
            req = RpcRequest.decode(frame)
        except Exception as exc:  # malformed envelope -> transport status 1
            resp = RpcResponse.transport_error(Status(1), str(exc))
        else:
            outcome = handler(req)
            if isinstance(outcome, Reply):
                resp = RpcResponse.ok(outcome.variant, outcome.payload).with_id(req.id)
            else:
                resp = RpcResponse.transport_error(
                    outcome.status, outcome.message
                ).with_id(req.id)
        self._carrier.send_frame(resp.encode())
        return True
