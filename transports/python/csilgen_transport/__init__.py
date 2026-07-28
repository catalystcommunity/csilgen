"""csilgen_transport -- reference implementation of the CSIL transport family.

CSIL-RPC, CSIL-Events, and CSIL-Datagrams for Python. It owns the envelope codecs,
framing, and connection lifecycle; the byte/datagram **carrier** is injected
(bring-your-own-carrier), so a host plugs HTTP, WebSocket, QUIC, WebRTC, or a
platform media stack without changing this library.

See the spec documents at the repo root: ``csil-transport-conventions.md``,
``csil-rpc-transport.md``, ``csil-events-transport.md``,
``csil-datagrams-transport.md``. The byte layout is pinned by the conformance
vectors in ``transports/conformance/``.
"""

from __future__ import annotations

from . import carrier, cbor, conventions, datagrams, events, rpc
from .conventions import (
    MAX_FRAME_DEFAULT,
    MAX_FRAME_LIMIT,
    VERSION,
    CarrierError,
    DecodeError,
    EncodeError,
    FrameTooLargeError,
    InvalidMaxFrameError,
    MalformedError,
    Status,
    StatusError,
    TransportError,
    UnsupportedVersionError,
    validate_max_frame,
)

__all__ = [
    "carrier",
    "cbor",
    "conventions",
    "datagrams",
    "events",
    "rpc",
    "VERSION",
    "MAX_FRAME_DEFAULT",
    "MAX_FRAME_LIMIT",
    "validate_max_frame",
    "InvalidMaxFrameError",
    "Status",
    "TransportError",
    "EncodeError",
    "DecodeError",
    "MalformedError",
    "FrameTooLargeError",
    "UnsupportedVersionError",
    "StatusError",
    "CarrierError",
]
