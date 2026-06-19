"""Conventions shared by every CSIL transport -- see ``csil-transport-conventions.md``.

This module owns the parts the three transports agree on: the CBOR rules
(deterministic encoding via :mod:`csilgen_transport.cbor`, tag-24 payloads), the
version constant, the transport status registry, and the max-frame guard. The
transport modules build their envelopes from the canonical-CBOR helpers here so the
bytes match the conformance vectors regardless of dataclass layout.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Optional

from . import cbor
from .cbor import Tag

# Current transport version. A new value is minted only for a breaking change to
# envelope layout or semantics.
VERSION: int = 1

# CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949 3.4.5.1).
TAG_ENCODED_CBOR: int = 24

# Reserved service ordinal for the transport control plane (Events lifecycle).
CONTROL_SERVICE_ORD: int = 0

# Default max encoded envelope size for stream/message carriers (16 MiB). A carrier
# rejects anything larger before allocating for it.
MAX_FRAME_DEFAULT: int = 16 * 1024 * 1024

_STATUS_NAMES = {
    0: "ok",
    1: "malformed-envelope",
    2: "unknown-service-or-op",
    3: "unauthenticated",
    4: "forbidden",
    5: "version-unsupported",
    6: "internal",
    7: "unavailable",
    8: "deadline-exceeded",
}


@dataclass(frozen=True)
class Status:
    """Transport-level status. Distinct from application errors, which ride inside
    the payload as a declared ``/ ErrorType`` arm. Codes 0..8 are the registry; any
    other code (host extensions at >= 64, or an unrecognized value) is carried as-is.
    """

    code: int

    @classmethod
    def from_code(cls, code: int) -> "Status":
        return cls(int(code))

    @property
    def name(self) -> str:
        return _STATUS_NAMES.get(self.code, "other")

    @property
    def is_ok(self) -> bool:
        return self.code == 0


# Registry constants (mirrors the Rust ``Status`` variants).
OK = Status(0)
MALFORMED_ENVELOPE = Status(1)
UNKNOWN_SERVICE_OR_OP = Status(2)
UNAUTHENTICATED = Status(3)
FORBIDDEN = Status(4)
VERSION_UNSUPPORTED = Status(5)
INTERNAL = Status(6)
UNAVAILABLE = Status(7)
DEADLINE_EXCEEDED = Status(8)


class TransportError(Exception):
    """Base for errors surfaced by the transport layer (the wire), distinct from a
    host's application errors."""


class EncodeError(TransportError):
    pass


class DecodeError(TransportError):
    pass


class MalformedError(TransportError):
    pass


class FrameTooLargeError(TransportError):
    def __init__(self, got: int, maximum: int) -> None:
        super().__init__(
            f"frame of {got} bytes exceeds max-frame guard of {maximum} bytes"
        )
        self.got = got
        self.maximum = maximum


class UnsupportedVersionError(TransportError):
    def __init__(self, version: int) -> None:
        super().__init__(f"unsupported transport version {version}")
        self.version = version


class StatusError(TransportError):
    """A non-zero transport status returned by the peer."""

    def __init__(self, status: Status, message: Optional[str] = None) -> None:
        text = f"transport status {status.name} ({status.code})"
        if message is not None:
            text += f": {message}"
        super().__init__(text)
        self.status = status
        self.code = status.code
        self.message = message


class CarrierError(TransportError):
    pass


def tag24(payload: bytes) -> Tag:
    """Wrap opaque payload bytes (themselves a CBOR item) in tag 24."""
    return Tag(TAG_ENCODED_CBOR, bytes(payload))


def untag24(value: Any) -> bytes:
    """Extract the opaque payload bytes from a tag-24 value."""
    if isinstance(value, Tag) and value.tag == TAG_ENCODED_CBOR:
        if isinstance(value.value, (bytes, bytearray)):
            return bytes(value.value)
        raise MalformedError("tag-24 payload is not a byte string")
    raise MalformedError("expected a tag-24 (encoded-cbor) payload")


def encode_value(value: Any) -> bytes:
    try:
        return cbor.encode(value)
    except cbor.CborError as exc:
        raise EncodeError(str(exc)) from exc


def decode_value(data: bytes) -> Any:
    try:
        return cbor.decode(data)
    except cbor.CborError as exc:
        raise DecodeError(str(exc)) from exc


def canon_map(entries: list[tuple[str, Any]]) -> dict[str, Any]:
    """Build a map from key/value pairs. The CBOR encoder sorts the entries into
    RFC 8949 deterministic order at encode time, so callers may append in any order.
    """
    return dict(entries)


def map_get(map_value: Any, key: str) -> Any:
    if isinstance(map_value, dict):
        return map_value.get(key)
    return None


def get_uint(map_value: Any, key: str) -> int:
    value = map_get(map_value, key)
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return value
    raise MalformedError(f"missing or non-integer field '{key}'")


def get_int(map_value: Any, key: str) -> int:
    value = map_get(map_value, key)
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    raise MalformedError(f"missing or non-integer field '{key}'")


def get_text(map_value: Any, key: str) -> str:
    value = map_get(map_value, key)
    if isinstance(value, str):
        return value
    raise MalformedError(f"missing or non-text field '{key}'")


def get_text_opt(map_value: Any, key: str) -> Optional[str]:
    value = map_get(map_value, key)
    return value if isinstance(value, str) else None


def get_uint_opt(map_value: Any, key: str) -> Optional[int]:
    value = map_get(map_value, key)
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return value
    return None


def check_version(version: int) -> None:
    """Verify a decoded envelope's version field, raising a clear error otherwise."""
    if version != VERSION:
        raise UnsupportedVersionError(version)


def hex_to_bytes(text: str) -> bytes:
    return bytes.fromhex(text)


def bytes_to_hex(data: bytes) -> str:
    return data.hex()
