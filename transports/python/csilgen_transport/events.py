"""CSIL-Events transport -- typed bidirectional event streams -- see
``csil-events-transport.md``. Verbose (text-keyed) and compact (positional) profiles,
plus the control plane (service ordinal 0) lifecycle payloads.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import Optional

from .conventions import (
    VERSION,
    MalformedError,
    Status,
    canon_map,
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


class Profile(enum.Enum):
    """Which wire profile a connection uses for its lifetime, fixed by ``hello``."""

    VERBOSE = "verbose"
    COMPACT = "compact"

    def as_str(self) -> str:
        return self.value

    @classmethod
    def parse(cls, text: str) -> Optional["Profile"]:
        try:
            return cls(text)
        except ValueError:
            return None


@dataclass
class Event:
    """One typed event flowing in either direction. Identified by service+operation
    (verbose) or by their ordinals (compact); carries an optional correlation ``id``
    when it is a request expecting a reply, or that reply."""

    # CSIL service name (verbose). None on a single-service verbose connection.
    service: Optional[str] = None
    # Service ordinal (compact). Always present in compact frames.
    service_ord: Optional[int] = None
    # CSIL operation name (verbose).
    event: Optional[str] = None
    # Operation ordinal (compact).
    op_ord: Optional[int] = None
    id: Optional[int] = None
    # Opaque CBOR(event type) bytes.
    payload: bytes = b""

    @classmethod
    def verbose(
        cls, service: Optional[str], event: str, payload: bytes
    ) -> "Event":
        """A verbose event by name."""
        return cls(service=service, event=event, payload=payload)

    @classmethod
    def compact(cls, service_ord: int, op_ord: int, payload: bytes) -> "Event":
        """A compact event by ordinals."""
        return cls(service_ord=service_ord, op_ord=op_ord, payload=payload)

    def with_id(self, id: int) -> "Event":
        self.id = id
        return self

    def encode(self, profile: Profile) -> bytes:
        if profile is Profile.VERBOSE:
            return self._encode_verbose()
        return self._encode_compact()

    def _encode_verbose(self) -> bytes:
        if self.event is None:
            raise MalformedError("verbose event missing 'event' name")
        entries: list[tuple[str, object]] = [
            ("event", self.event),
            ("payload", tag24(self.payload)),
        ]
        if self.service is not None:
            entries.append(("service", self.service))
        if self.id is not None:
            entries.append(("id", self.id))
        return encode_value(canon_map(entries))

    def _encode_compact(self) -> bytes:
        if self.service_ord is None:
            raise MalformedError("compact event missing service ordinal")
        if self.op_ord is None:
            raise MalformedError("compact event missing op ordinal")
        arr: list[object] = [self.service_ord, self.op_ord]
        if self.id is not None:
            arr.append(self.id)
        arr.append(tag24(self.payload))
        return encode_value(arr)

    @classmethod
    def decode(cls, data: bytes, profile: Profile) -> "Event":
        if profile is Profile.VERBOSE:
            return cls._decode_verbose(data)
        return cls._decode_compact(data)

    @classmethod
    def _decode_verbose(cls, data: bytes) -> "Event":
        value = decode_value(data)
        payload_value = map_get(value, "payload")
        if payload_value is None:
            raise MalformedError("missing 'payload'")
        return cls(
            service=get_text_opt(value, "service"),
            event=get_text(value, "event"),
            id=get_uint_opt(value, "id"),
            payload=untag24(payload_value),
        )

    @classmethod
    def _decode_compact(cls, data: bytes) -> "Event":
        value = decode_value(data)
        if not isinstance(value, list):
            raise MalformedError("compact event is not an array")
        # 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
        if len(value) == 3:
            service_ord, op_ord, payload_value = value[0], value[1], value[2]
            id_value = None
        elif len(value) == 4:
            service_ord, op_ord, id_value, payload_value = value
        else:
            raise MalformedError(
                f"compact event array has {len(value)} elements, expected 3 or 4"
            )

        def as_uint(val: object) -> int:
            if isinstance(val, int) and not isinstance(val, bool) and val >= 0:
                return val
            raise MalformedError("ordinal is not a non-negative integer")

        return cls(
            service_ord=as_uint(service_ord),
            op_ord=as_uint(op_ord),
            id=as_uint(id_value) if id_value is not None else None,
            payload=untag24(payload_value),
        )


class control:
    """Control-plane operation ordinals (under service ordinal 0) and their
    ``$``-sigil verbose names."""

    HELLO = 0
    HELLO_ACK = 1
    PING = 2
    PONG = 3
    CLOSE = 4
    ERROR = 5

    HELLO_NAME = "$hello"
    HELLO_ACK_NAME = "$hello-ack"
    PING_NAME = "$ping"
    PONG_NAME = "$pong"
    CLOSE_NAME = "$close"
    ERROR_NAME = "$error"


@dataclass
class Hello:
    """The ``$hello`` payload offered by the connection initiator."""

    versions: list[int]
    profiles: list[str]
    service: Optional[str] = None
    auth: Optional[str] = None

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [
            ("versions", list(self.versions)),
            ("profiles", list(self.profiles)),
        ]
        if self.service is not None:
            entries.append(("service", self.service))
        if self.auth is not None:
            entries.append(("auth", self.auth))
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "Hello":
        value = decode_value(data)
        versions_value = map_get(value, "versions")
        if not isinstance(versions_value, list):
            raise MalformedError("hello missing 'versions'")
        versions = [
            v
            for v in versions_value
            if isinstance(v, int) and not isinstance(v, bool) and v >= 0
        ]
        profiles_value = map_get(value, "profiles")
        if not isinstance(profiles_value, list):
            raise MalformedError("hello missing 'profiles'")
        profiles = [p for p in profiles_value if isinstance(p, str)]
        return cls(
            versions=versions,
            profiles=profiles,
            service=get_text_opt(value, "service"),
            auth=get_text_opt(value, "auth"),
        )

    def negotiate(
        self, supported: list[Profile]
    ) -> Optional[tuple[int, Profile]]:
        """Select a profile from this hello's offers, honoring the peer's preference
        order and what the server supports. Returns the chosen ``(version, profile)``,
        or ``None`` if nothing is mutually supported."""
        if VERSION not in self.versions:
            return None
        for offered in self.profiles:
            parsed = Profile.parse(offered)
            if parsed is not None and parsed in supported:
                return (VERSION, parsed)
        return None


@dataclass
class HelloAck:
    """The ``$hello-ack`` payload returned by the peer."""

    v: int
    profile: str
    session: Optional[str] = None

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [
            ("v", self.v),
            ("profile", self.profile),
        ]
        if self.session is not None:
            entries.append(("session", self.session))
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "HelloAck":
        value = decode_value(data)
        return cls(
            v=get_uint(value, "v"),
            profile=get_text(value, "profile"),
            session=get_text_opt(value, "session"),
        )


@dataclass
class Heartbeat:
    """A ``$ping`` / ``$pong`` heartbeat payload."""

    nonce: int
    at: Optional[int] = None

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [("nonce", self.nonce)]
        if self.at is not None:
            entries.append(("at", self.at))
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "Heartbeat":
        value = decode_value(data)
        return cls(nonce=get_uint(value, "nonce"), at=get_uint_opt(value, "at"))


@dataclass
class Close:
    """A ``$close`` payload."""

    status: Status
    reason: Optional[str] = None

    def encode(self) -> bytes:
        entries: list[tuple[str, object]] = [("status", self.status.code)]
        if self.reason is not None:
            entries.append(("reason", self.reason))
        return encode_value(canon_map(entries))

    @classmethod
    def decode(cls, data: bytes) -> "Close":
        value = decode_value(data)
        return cls(
            status=Status.from_code(get_int(value, "status")),
            reason=get_text_opt(value, "reason"),
        )
