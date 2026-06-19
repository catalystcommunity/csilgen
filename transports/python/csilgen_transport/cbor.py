"""A minimal, hand-rolled canonical CBOR codec (RFC 8949), standard library only.

Only the data items the CSIL transports actually put on the wire are supported:
unsigned and negative integers, byte strings, text strings, arrays, maps, and the
tag-24 (encoded-cbor) wrapper. Maps are emitted with RFC 8949 core deterministic
encoding -- keys sorted by the bytewise lexicographic order of their *encoded* key
bytes -- so the output matches the Rust reference's ``canon_map`` byte for byte and
reproduces the conformance vectors.

A third-party CBOR library is deliberately avoided: the vectors are the normative
contract, and owning the encoder is the only way to guarantee determinism without
trusting an external dependency's option flags.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

# Major types (the high three bits of the initial byte).
_MAJOR_UINT = 0
_MAJOR_NINT = 1
_MAJOR_BYTES = 2
_MAJOR_TEXT = 3
_MAJOR_ARRAY = 4
_MAJOR_MAP = 5
_MAJOR_TAG = 6


class CborError(ValueError):
    """A malformed CBOR item or an unsupported construct."""


@dataclass
class Tag:
    """A CBOR semantic tag wrapping a single data item (RFC 8949 section 3.4)."""

    tag: int
    value: Any


def _encode_head(out: bytearray, major: int, arg: int) -> None:
    mt = major << 5
    if arg < 24:
        out.append(mt | arg)
    elif arg < 0x100:
        out.append(mt | 24)
        out.append(arg)
    elif arg < 0x1_0000:
        out.append(mt | 25)
        out.extend(arg.to_bytes(2, "big"))
    elif arg < 0x1_0000_0000:
        out.append(mt | 26)
        out.extend(arg.to_bytes(4, "big"))
    else:
        out.append(mt | 27)
        out.extend(arg.to_bytes(8, "big"))


def _encode_into(value: Any, out: bytearray) -> None:
    # bool is a subclass of int; guard so a stray flag never silently becomes 0/1.
    if isinstance(value, bool):
        raise CborError("booleans are not part of any CSIL transport envelope")
    if isinstance(value, Tag):
        _encode_head(out, _MAJOR_TAG, value.tag)
        _encode_into(value.value, out)
    elif isinstance(value, int):
        if value >= 0:
            _encode_head(out, _MAJOR_UINT, value)
        else:
            _encode_head(out, _MAJOR_NINT, -1 - value)
    elif isinstance(value, (bytes, bytearray)):
        _encode_head(out, _MAJOR_BYTES, len(value))
        out.extend(value)
    elif isinstance(value, str):
        encoded = value.encode("utf-8")
        _encode_head(out, _MAJOR_TEXT, len(encoded))
        out.extend(encoded)
    elif isinstance(value, (list, tuple)):
        _encode_head(out, _MAJOR_ARRAY, len(value))
        for item in value:
            _encode_into(item, out)
    elif isinstance(value, dict):
        # Deterministic map: sort entries by their encoded-key bytes. Because the
        # key head byte carries the length, shorter keys sort first -- the same
        # length-then-bytewise order the conventions doc mandates.
        items = []
        for key, val in value.items():
            key_bytes = encode(key)
            items.append((key_bytes, val))
        items.sort(key=lambda kv: kv[0])
        _encode_head(out, _MAJOR_MAP, len(items))
        for key_bytes, val in items:
            out.extend(key_bytes)
            _encode_into(val, out)
    else:
        raise CborError(f"cannot encode value of type {type(value).__name__}")


def encode(value: Any) -> bytes:
    """Encode a single value to canonical CBOR bytes."""
    out = bytearray()
    _encode_into(value, out)
    return bytes(out)


def _read_argument(data: bytes, pos: int, info: int) -> tuple[int, int]:
    if info < 24:
        return info, pos
    if info == 24:
        _need(data, pos, 1)
        return data[pos], pos + 1
    if info == 25:
        _need(data, pos, 2)
        return int.from_bytes(data[pos : pos + 2], "big"), pos + 2
    if info == 26:
        _need(data, pos, 4)
        return int.from_bytes(data[pos : pos + 4], "big"), pos + 4
    if info == 27:
        _need(data, pos, 8)
        return int.from_bytes(data[pos : pos + 8], "big"), pos + 8
    # 28..30 are reserved; 31 is indefinite length, forbidden in an envelope.
    raise CborError(f"unsupported additional-info value {info}")


def _need(data: bytes, pos: int, n: int) -> None:
    if pos + n > len(data):
        raise CborError("unexpected end of CBOR input")


def _decode_at(data: bytes, pos: int) -> tuple[Any, int]:
    _need(data, pos, 1)
    initial = data[pos]
    pos += 1
    major = initial >> 5
    info = initial & 0x1F

    if major == _MAJOR_UINT:
        arg, pos = _read_argument(data, pos, info)
        return arg, pos
    if major == _MAJOR_NINT:
        arg, pos = _read_argument(data, pos, info)
        return -1 - arg, pos
    if major == _MAJOR_BYTES:
        length, pos = _read_argument(data, pos, info)
        _need(data, pos, length)
        return bytes(data[pos : pos + length]), pos + length
    if major == _MAJOR_TEXT:
        length, pos = _read_argument(data, pos, info)
        _need(data, pos, length)
        return data[pos : pos + length].decode("utf-8"), pos + length
    if major == _MAJOR_ARRAY:
        length, pos = _read_argument(data, pos, info)
        items = []
        for _ in range(length):
            item, pos = _decode_at(data, pos)
            items.append(item)
        return items, pos
    if major == _MAJOR_MAP:
        length, pos = _read_argument(data, pos, info)
        result: dict[Any, Any] = {}
        for _ in range(length):
            key, pos = _decode_at(data, pos)
            val, pos = _decode_at(data, pos)
            result[key] = val
        return result, pos
    if major == _MAJOR_TAG:
        tag, pos = _read_argument(data, pos, info)
        inner, pos = _decode_at(data, pos)
        return Tag(tag, inner), pos
    raise CborError(f"unsupported CBOR major type {major}")


def decode(data: bytes) -> Any:
    """Decode exactly one CBOR item; reject trailing bytes (an envelope is one item)."""
    value, pos = _decode_at(data, 0)
    if pos != len(data):
        raise CborError("trailing bytes after CBOR item")
    return value
