"""Carrier seams -- the bring-your-own-carrier boundary (conventions doc section 7).

The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
byte/datagram transport) is injected. A host supplies QUIC, WebRTC, a platform media
stack, or anything else by subclassing one of these abstract bases -- without
changing the library.
"""

from __future__ import annotations

import abc
import socket as _socket
from collections import deque
from typing import Optional, Protocol

from .conventions import (
    MAX_FRAME_DEFAULT,
    CarrierError,
    FrameTooLargeError,
)


class FrameCarrier(abc.ABC):
    """A stream/frame carrier: sends and receives one *delimited message* at a time.

    Used by CSIL-RPC and CSIL-Events. Built-in implementations frame with a 4-byte
    big-endian length prefix; a host may implement this over WebSocket binary frames,
    a WebTransport stream, etc.
    """

    @abc.abstractmethod
    def send_frame(self, frame: bytes) -> None: ...

    @abc.abstractmethod
    def recv_frame(self) -> Optional[bytes]:
        """Receive the next frame, or ``None`` at a clean end of stream."""


class DatagramCarrier(abc.ABC):
    """A datagram carrier: sends and receives one self-contained datagram (each within
    the channel MTU), with no delivery or ordering guarantee. Used by CSIL-Datagrams.
    Built-in over UDP; a host plugs WebRTC unreliable channels, QUIC datagrams, etc.
    """

    @abc.abstractmethod
    def send_datagram(self, datagram: bytes) -> None: ...

    @abc.abstractmethod
    def recv_datagram(self) -> Optional[bytes]:
        """Receive the next datagram, or ``None`` if the carrier is closed."""


class _Reader(Protocol):
    def read(self, n: int) -> bytes: ...


class _Writer(Protocol):
    def write(self, data: bytes) -> object: ...

    def flush(self) -> object: ...


def _read_exact(reader: _Reader, n: int) -> Optional[bytes]:
    buf = bytearray()
    while len(buf) < n:
        chunk = reader.read(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


def write_length_prefixed(
    writer: _Writer, frame: bytes, maximum: int = MAX_FRAME_DEFAULT
) -> None:
    """Write a 4-byte big-endian length prefix followed by ``frame`` (CSIL stream
    framing). Rejects an over-limit frame before writing anything."""
    if len(frame) > maximum:
        raise FrameTooLargeError(len(frame), maximum)
    writer.write(len(frame).to_bytes(4, "big"))
    writer.write(frame)
    writer.flush()


def read_length_prefixed(
    reader: _Reader, maximum: int = MAX_FRAME_DEFAULT
) -> Optional[bytes]:
    """Read one length-prefixed frame, enforcing the max-frame guard before
    allocating. Returns ``None`` at a clean EOF before any byte of a frame."""
    prefix = _read_exact(reader, 4)
    if prefix is None:
        return None
    length = int.from_bytes(prefix, "big")
    if length > maximum:
        raise FrameTooLargeError(length, maximum)
    body = _read_exact(reader, length)
    if body is None:
        raise CarrierError("connection closed mid-frame")
    return body


class StreamCarrier(FrameCarrier):
    """A :class:`FrameCarrier` over any read/write/flush byte stream (a socket's
    ``makefile("rwb")``, a TLS-wrapped file object, an in-memory ``BytesIO`` pair),
    using the canonical 4-byte length-prefix framing."""

    def __init__(self, stream, max_frame: int = MAX_FRAME_DEFAULT) -> None:
        self._stream = stream
        self._max_frame = max_frame

    def send_frame(self, frame: bytes) -> None:
        write_length_prefixed(self._stream, frame, self._max_frame)

    def recv_frame(self) -> Optional[bytes]:
        return read_length_prefixed(self._stream, self._max_frame)

    @property
    def stream(self):
        return self._stream


class LoopbackFrameCarrier(FrameCarrier):
    """An in-memory :class:`FrameCarrier` backed by frame queues -- for tests and for
    driving the codec without a socket."""

    def __init__(self) -> None:
        self.outbound: deque[bytes] = deque()
        self.inbound: deque[bytes] = deque()

    def push_inbound(self, frame: bytes) -> None:
        """Queue a frame that a subsequent ``recv_frame`` will return."""
        self.inbound.append(bytes(frame))

    def take_outbound(self) -> Optional[bytes]:
        """Take the next frame that was sent via ``send_frame``."""
        return self.outbound.popleft() if self.outbound else None

    def send_frame(self, frame: bytes) -> None:
        self.outbound.append(bytes(frame))

    def recv_frame(self) -> Optional[bytes]:
        return self.inbound.popleft() if self.inbound else None


class LoopbackDatagramCarrier(DatagramCarrier):
    """An in-memory :class:`DatagramCarrier` -- for tests and codec drives."""

    def __init__(self) -> None:
        self.outbound: deque[bytes] = deque()
        self.inbound: deque[bytes] = deque()

    def push_inbound(self, datagram: bytes) -> None:
        self.inbound.append(bytes(datagram))

    def take_outbound(self) -> Optional[bytes]:
        return self.outbound.popleft() if self.outbound else None

    def send_datagram(self, datagram: bytes) -> None:
        self.outbound.append(bytes(datagram))

    def recv_datagram(self) -> Optional[bytes]:
        return self.inbound.popleft() if self.inbound else None


class UdpDatagramCarrier(DatagramCarrier):
    """A UDP-backed :class:`DatagramCarrier`. Built-in convenience for the native
    datagram path; the browser path (WebRTC unreliable / WebTransport) implements the
    same abstract base in the host."""

    def __init__(self, sock: _socket.socket, recv_size: int = 1200) -> None:
        self._socket = sock
        self._recv_size = recv_size

    def send_datagram(self, datagram: bytes) -> None:
        try:
            self._socket.send(datagram)
        except OSError as exc:
            raise CarrierError(str(exc)) from exc

    def recv_datagram(self) -> Optional[bytes]:
        try:
            data = self._socket.recv(self._recv_size)
        except OSError as exc:
            raise CarrierError(str(exc)) from exc
        return data
