#!/usr/bin/env python3
"""A minimal CSIL-RPC HTTP echo server for verifying generated-package Quickstart
carriers end to end, without any third-party dependency.

It speaks the envelope-in-body HTTP profile from docs/csil-rpc-transport.md: it
accepts a `CsilRpcRequest` CBOR map at `POST /csil/v1/rpc`, pulls the tag-24
(`#6.24(bstr)`) inner request payload back out, and returns a `status: 0`
`CsilRpcResponse` whose payload re-wraps those same inner bytes. Because the
verification spec pairs request/response records of identical wire shape
(`Ping{msg} -> Pong{msg}`), echoing the inner bytes is a valid typed reply, so any
conformant carrier round-trips its request fields back as the decoded response.

Op `Fail` instead returns the typed `ServiceError` arm (`variant: "ServiceError"`,
`status: 0`) so a carrier's error path can be exercised too; its inner payload is a
`{code, message}` record (the canonical ServiceError shape).

Binds 127.0.0.1 on an OS-assigned port and prints `http://127.0.0.1:<port>` as the
first stdout line so a test harness can read the base URL, then serves until killed.
Only the tiny, fixed envelope grammar is implemented by hand; the inner payload is
treated as opaque bytes.
"""

import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

TAG_ENCODED_CBOR = 24


# --- minimal CBOR (only what the envelope grammar needs) ---------------------

def _read_arg(buf, pos, info):
    if info < 24:
        return info, pos
    if info == 24:
        return buf[pos], pos + 1
    if info == 25:
        return int.from_bytes(buf[pos:pos + 2], "big"), pos + 2
    if info == 26:
        return int.from_bytes(buf[pos:pos + 4], "big"), pos + 4
    if info == 27:
        return int.from_bytes(buf[pos:pos + 8], "big"), pos + 8
    raise ValueError(f"unsupported additional info {info}")


def _decode(buf, pos):
    """Return (value, next_pos). Tags become ('tag', n, inner); bstr -> bytes;
    tstr -> str; map -> dict; uint -> int."""
    initial = buf[pos]
    pos += 1
    major, info = initial >> 5, initial & 0x1F
    if major == 0:  # unsigned int
        return _read_arg(buf, pos, info)
    if major == 2:  # byte string
        n, pos = _read_arg(buf, pos, info)
        return bytes(buf[pos:pos + n]), pos + n
    if major == 3:  # text string
        n, pos = _read_arg(buf, pos, info)
        return buf[pos:pos + n].decode("utf-8"), pos + n
    if major == 4:  # array
        n, pos = _read_arg(buf, pos, info)
        out = []
        for _ in range(n):
            v, pos = _decode(buf, pos)
            out.append(v)
        return out, pos
    if major == 5:  # map
        n, pos = _read_arg(buf, pos, info)
        out = {}
        for _ in range(n):
            k, pos = _decode(buf, pos)
            v, pos = _decode(buf, pos)
            out[k] = v
        return out, pos
    if major == 6:  # tag
        n, pos = _read_arg(buf, pos, info)
        inner, pos = _decode(buf, pos)
        return ("tag", n, inner), pos
    raise ValueError(f"unsupported major type {major}")


def _head(major, n):
    mt = major << 5
    if n < 24:
        return bytes([mt | n])
    if n < 0x100:
        return bytes([mt | 24, n])
    if n < 0x10000:
        return bytes([mt | 25]) + n.to_bytes(2, "big")
    if n < 0x100000000:
        return bytes([mt | 26]) + n.to_bytes(4, "big")
    return bytes([mt | 27]) + n.to_bytes(8, "big")


def _text(s):
    b = s.encode("utf-8")
    return _head(3, len(b)) + b


def _bytes(b):
    return _head(2, len(b)) + b


def _uint(n):
    return _head(0, n)


def _tag24(inner_bytes):
    return _head(6, TAG_ENCODED_CBOR) + _bytes(inner_bytes)


def _service_error_payload(code, message):
    # ServiceError record: {code, message}. Field keys are the CSIL names verbatim.
    return _head(5, 2) + _text("code") + _uint(code) + _text("message") + _text(message)


# --- HTTP handler ------------------------------------------------------------

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # keep stdout clean for the port handshake
        pass

    def do_POST(self):
        if self.path.rstrip("/") != "/csil/v1/rpc":
            self.send_response(404)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        try:
            env, _ = _decode(body, 0)
            op = env["op"]
            payload = env["payload"]
            assert isinstance(payload, tuple) and payload[0] == "tag" and payload[1] == TAG_ENCODED_CBOR
            inner = payload[2]
            assert isinstance(inner, (bytes, bytearray))
        except Exception as exc:  # malformed request -> transport status 1
            resp = _head(5, 3) + _text("v") + _uint(1) + _text("status") + _uint(1) \
                + _text("error") + _text(f"bad envelope: {exc}")
            self._send(resp)
            return

        if op == "Fail":
            # Typed ServiceError arm: status 0, variant names the chosen arm.
            inner_err = _service_error_payload(7, "mock service error")
            resp = (
                _head(5, 4)
                + _text("v") + _uint(1)
                + _text("status") + _uint(0)
                + _text("variant") + _text("ServiceError")
                + _text("payload") + _tag24(inner_err)
            )
            self._send(resp)
            return

        # Success: echo the inner request bytes back as the typed reply.
        resp = (
            _head(5, 3)
            + _text("v") + _uint(1)
            + _text("status") + _uint(0)
            + _text("payload") + _tag24(bytes(inner))
        )
        self._send(resp)

    def _send(self, body):
        self.send_response(200)
        self.send_header("Content-Type", "application/cbor")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    server = HTTPServer(("127.0.0.1", port), Handler)
    print(f"http://127.0.0.1:{server.server_address[1]}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
