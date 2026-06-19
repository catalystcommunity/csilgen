"""CSIL-Datagrams transport -- unreliable, unordered, message-oriented -- see
``csil-datagrams-transport.md``. CBOR-array (default), compact fixed-header, and
payload-only profiles. A datagram channel is single-service: the service is bound at
channel setup, so datagrams carry no service ordinal.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

from .conventions import (
    VERSION,
    MalformedError,
    UnsupportedVersionError,
    check_version,
    decode_value,
    encode_value,
    tag24,
    untag24,
)

# Conservative max datagram size (envelope + payload) safe across UDP/WebRTC/QUIC.
MAX_DATAGRAM_DEFAULT: int = 1200

_COMPACT_VER: int = 1
_FLAG_EPOCH: int = 0b0001


@dataclass
class Datagram:
    """A datagram in the CBOR-array (default) profile: ``[v, op_ord, seq, payload]``."""

    op_ord: int
    # Per-channel sequence; 0 means "unsequenced".
    seq: int
    # Opaque CBOR(message type) bytes.
    payload: bytes = b""

    def encode(self) -> bytes:
        return encode_value([VERSION, self.op_ord, self.seq, tag24(self.payload)])

    @classmethod
    def decode(cls, data: bytes) -> "Datagram":
        value = decode_value(data)
        if not isinstance(value, list):
            raise MalformedError("datagram is not an array")
        if len(value) != 4:
            raise MalformedError(
                f"datagram array has {len(value)} elements, expected 4"
            )

        def as_uint(val: object) -> int:
            if isinstance(val, int) and not isinstance(val, bool) and val >= 0:
                return val
            raise MalformedError("datagram field is not a non-negative integer")

        check_version(as_uint(value[0]))
        return cls(
            op_ord=as_uint(value[1]),
            seq=as_uint(value[2]),
            payload=untag24(value[3]),
        )


@dataclass
class CompactDatagram:
    """A datagram in the compact fixed-header profile. Header layout:
    ``[ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8])`` then the opaque body."""

    op_ord: int
    seq: int
    # Present when the sender tracks restarts (sets the flags epoch bit).
    epoch: Optional[int] = None
    # Opaque body bytes (tag-24 CBOR or a raw media frame, by channel agreement).
    body: bytes = b""

    def with_epoch(self, epoch: int) -> "CompactDatagram":
        self.epoch = epoch
        return self

    def encode(self) -> bytes:
        flags = _FLAG_EPOCH if self.epoch is not None else 0
        out = bytearray()
        out.append((_COMPACT_VER << 4) | (flags & 0x0F))
        out.append(self.op_ord & 0xFF)
        out.extend((self.seq & 0xFFFF).to_bytes(2, "big"))
        if self.epoch is not None:
            out.append(self.epoch & 0xFF)
        out.extend(self.body)
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> "CompactDatagram":
        if len(data) < 4:
            raise MalformedError("compact datagram shorter than the 4-byte header")
        ver = data[0] >> 4
        if ver != _COMPACT_VER:
            raise UnsupportedVersionError(ver)
        flags = data[0] & 0x0F
        op_ord = data[1]
        seq = int.from_bytes(data[2:4], "big")
        if flags & _FLAG_EPOCH:
            if len(data) < 5:
                raise MalformedError(
                    "compact datagram flags claim an epoch byte that is absent"
                )
            epoch: Optional[int] = data[4]
            body_start = 5
        else:
            epoch = None
            body_start = 4
        return cls(op_ord=op_ord, seq=seq, epoch=epoch, body=bytes(data[body_start:]))


@dataclass(frozen=True)
class SeqEvent:
    """Classification of an incoming sequence number relative to what was last seen,
    for loss/reorder/restart detection. The transport detects; the app decides."""

    kind: str
    # Only meaningful for ADVANCED: how many sequence numbers were skipped (lost).
    gap: int = 0


# Singleton-style kinds, mirroring the Rust ``SeqEvent`` variants.
def first() -> SeqEvent:
    return SeqEvent("first")


def advanced(gap: int) -> SeqEvent:
    return SeqEvent("advanced", gap)


def late_or_duplicate() -> SeqEvent:
    return SeqEvent("late_or_duplicate")


def restart() -> SeqEvent:
    return SeqEvent("restart")


class SeqTracker:
    """Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced
    datagrams (seq 0) are reported as ``advanced(gap=0)`` once a first is seen."""

    def __init__(self) -> None:
        self._last_seq: Optional[int] = None
        self._last_epoch: Optional[int] = None

    def observe(self, seq: int, epoch: Optional[int] = None) -> SeqEvent:
        if epoch != self._last_epoch and self._last_epoch is not None:
            self._last_epoch = epoch
            self._last_seq = seq
            return restart()
        self._last_epoch = epoch
        # seq 0 marks an unsequenced datagram: no ordering information, so it is
        # never late or duplicate. Report a zero-gap advance and leave the running
        # sequence untouched.
        if seq == 0:
            return advanced(0)
        if self._last_seq is None:
            self._last_seq = seq
            return first()
        if seq > self._last_seq:
            gap = seq - self._last_seq - 1
            self._last_seq = seq
            return advanced(gap)
        return late_or_duplicate()
