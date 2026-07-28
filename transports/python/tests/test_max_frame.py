"""The configurable max-frame guard (conventions doc section 5): a host sets the
limit up or down through the carrier's public API, the limit applies to reads and
writes alike, an oversized inbound length is rejected before allocation, and an
invalid limit fails at construction rather than on the first frame."""

from __future__ import annotations

import io
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from csilgen_transport.carrier import StreamCarrier  # noqa: E402
from csilgen_transport.conventions import (  # noqa: E402
    MAX_FRAME_DEFAULT,
    MAX_FRAME_LIMIT,
    FrameTooLargeError,
    InvalidMaxFrameError,
)


class Duplex:
    """An in-memory duplex: writes land in a buffer that subsequent reads drain.
    ``read_count`` lets a test prove the guard fires before a frame body is pulled."""

    def __init__(self, initial: bytes = b"") -> None:
        self._buf = bytearray(initial)
        self._pos = 0
        self.read_count = 0

    def read(self, n: int) -> bytes:
        chunk = bytes(self._buf[self._pos : self._pos + n])
        self._pos += len(chunk)
        self.read_count += len(chunk)
        return chunk

    def write(self, data: bytes) -> int:
        self._buf.extend(data)
        return len(data)

    def flush(self) -> None:
        pass

    @property
    def written(self) -> bytes:
        return bytes(self._buf)


class MaxFrameTests(unittest.TestCase):
    def test_default_limit_accepts_frame_below_it(self):
        carrier = StreamCarrier(Duplex())
        frame = b"\xab" * 1024
        carrier.send_frame(frame)
        self.assertEqual(carrier.recv_frame(), frame)

    def test_default_limit_rejects_frame_above_it(self):
        stream = Duplex()
        carrier = StreamCarrier(stream)
        with self.assertRaises(FrameTooLargeError) as ctx:
            carrier.send_frame(b"\x00" * (MAX_FRAME_DEFAULT + 1))
        self.assertEqual(ctx.exception.maximum, MAX_FRAME_DEFAULT)
        self.assertEqual(
            stream.written, b"", "a rejected frame must not put bytes on the wire"
        )

    def test_larger_custom_limit_accepts_what_default_rejects(self):
        carrier = StreamCarrier(Duplex(), max_frame=MAX_FRAME_DEFAULT + 4096)
        frame = b"\x00" * (MAX_FRAME_DEFAULT + 1)
        carrier.send_frame(frame)
        self.assertEqual(carrier.recv_frame(), frame)

    def test_smaller_custom_limit_rejects_what_default_accepts(self):
        carrier = StreamCarrier(Duplex(), max_frame=64)
        with self.assertRaises(FrameTooLargeError):
            carrier.send_frame(b"\xcd" * 1024)

    def test_oversized_incoming_length_rejected_before_allocation(self):
        # A prefix claiming ~4 GiB followed by no body: if the guard ran after the
        # read this would block or allocate; it must fail on the prefix alone.
        stream = Duplex(b"\xff\xff\xff\xff")
        carrier = StreamCarrier(stream, max_frame=4096)
        with self.assertRaises(FrameTooLargeError):
            carrier.recv_frame()
        self.assertEqual(
            stream.read_count, 4, "guard must fire on the 4-byte prefix alone"
        )

    def test_invalid_limits_rejected_at_construction(self):
        for limit in (0, -1, -4096, MAX_FRAME_LIMIT + 1, 1 << 40, True, "4096", 1.5):
            with self.subTest(limit=limit):
                with self.assertRaises(InvalidMaxFrameError):
                    StreamCarrier(Duplex(), max_frame=limit)

    def test_boundary_limits_accepted(self):
        for limit in (1, MAX_FRAME_DEFAULT, MAX_FRAME_LIMIT):
            with self.subTest(limit=limit):
                self.assertEqual(
                    StreamCarrier(Duplex(), max_frame=limit).max_frame, limit
                )

    def test_stream_carrier_works_over_a_real_bytesio(self):
        # The guard is not tied to the test double: a plain BytesIO behaves the same.
        buf = io.BytesIO()
        StreamCarrier(buf, max_frame=1024).send_frame(b"hello")
        buf.seek(0)
        self.assertEqual(StreamCarrier(buf, max_frame=1024).recv_frame(), b"hello")


if __name__ == "__main__":
    unittest.main()
