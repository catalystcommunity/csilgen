# Serving CSIL-RPC over both HTTP and TCP

A CSIL-RPC **envelope** (`CsilRpcRequest` / `CsilRpcResponse`, the CBOR maps defined
in `csil-rpc-transport.md`) is **carrier-independent** — the bytes are identical no
matter what moves them. A *carrier* only decides how one envelope is **delimited** on
a given wire:

| carrier | delimiting | who frames it | typical peer |
| --- | --- | --- | --- |
| **HTTP** (envelope-in-body) | the whole POST body is one request envelope; the whole response body is one response envelope | HTTP `Content-Length` | browsers, fetch/XHR, curl, any HTTP client |
| **TCP / TLS / Unix stream** | a 4-byte big-endian length prefix + the envelope | the transport library (`StreamCarrier`) | native services, server↔server |

Because the envelope is the same, **one server can serve both at once**: bind an HTTP
socket *and* a TCP socket, and route both to **one shared dispatch**. This document
shows that pattern and gives the per-language transport-library entry points.

> No async. Per the project rule, the two listeners run on **threads**, not an async
> runtime. Each accepted connection is handled on its own thread (or the carrier's
> own server loop).

---

## 1. The shared dispatch (carrier-independent)

The generators emit, per service, a typed handler trait/interface plus the per-type
codec (`encode_<t>` / `decode_<t>`, or `to_cbor` / `from_cbor`). They do **not** emit
the request→response loop — that is the host's, and it is exactly what both carriers
share. It maps one decoded `RpcRequest` to a `HandlerOutcome`:

- match `req.op` (the PascalCase operation id, e.g. `"EchoScalars"`),
- `decode_*` the request payload, call your handler method, `encode_*` the result,
- return `Reply { variant, payload }` on success (variant = the chosen output-arm
  type name, e.g. `"Scalars"`, or `"ServiceError"` for a declared error arm — see
  `csil-rpc-transport.md` §1), or `Transport(status, message)` for a transport-level
  failure (unknown op, malformed envelope).

This `RpcRequest → HandlerOutcome` function is written **once** and used by every
carrier. The TCP carrier's `serve_one` calls it for you; the HTTP carrier calls it
inside your HTTP handler.

A second tiny helper — **`handle_envelope(bytes) -> bytes`** — decodes a request
envelope, runs the dispatch, maps the outcome onto an `RpcResponse`, and encodes it.
The TCP path gets this for free from `serve_one` (which adds framing); the HTTP path
calls it directly on the POST body.

---

## 2. TCP / Unix stream listener

The library owns the framing. Accept a connection, wrap it in a `StreamCarrier`, and
loop `serve_one(handler)`:

```
accept conn
  carrier = StreamCarrier(conn)            // 4-byte length prefix framing
  loop { serve_one(carrier, handler) }     // decode → dispatch → encode → send, per frame
```

`serve_one` reads one length-prefixed frame, runs your `RpcRequest → HandlerOutcome`
dispatch, and writes the length-prefixed response. This is exactly what the interop
test suite exercises (`tests/interop/harness/<lang>/`).

## 3. HTTP listener (envelope-in-body, `POST /csil/v1/rpc`)

The POST body **is** the request envelope; the response body **is** the response
envelope. There is no length prefix — HTTP's `Content-Length` delimits. The HTTP
status is `200` whenever an envelope comes back (even one carrying a non-zero
transport `status`); reserve non-200 for carrier failures (wrong mount → 404, over
the size guard → 413).

```
POST /csil/v1/rpc, Content-Type: application/cbor
  body  = read request body
  reply = handle_envelope(body)            // RpcRequest.decode → dispatch → RpcResponse.encode
  respond 200, Content-Type: application/cbor, body = reply
```

The request is self-routing (`service`/`op` live inside the envelope), so the path is
not semantic — `/csil/v1/rpc` is the canonical default mount, but any path works.

> **Path-routed profile (optional).** For REST-shaped hosts, `service`/`op` move into
> the path (`POST /csil/{service}/{op}`) and the body is the **payload only** (tag-24
> CBOR of the request type), with the transport `status` on the HTTP status line plus
> an `X-Csil-Status` header and `variant` in `X-Csil-Variant`. The envelope-in-body
> profile above is preferred for new hosts and is what a browser hitting one endpoint
> will use.

## 4. One process, both sockets

Build the dispatch once, then start both listeners — the HTTP server on its own
thread and the TCP accept loop on another (or vice-versa). Both close over the same
handler, so a browser POST and a native length-prefixed peer hit identical logic and
identical typed handlers.

---

## 5. Worked examples

The three reference harnesses in `tests/interop/harness/` already implement the TCP
side over a Unix `StreamCarrier`; the snippets below add the HTTP side from the same
dispatch. `Handlers` is your implementation of the generated service trait.

### Rust (`transports/rust`)

```rust
use csilgen_transport::carrier::StreamCarrier;
use csilgen_transport::rpc::{HandlerOutcome, RpcRequest, RpcResponse, RpcServer};
use csilgen_transport::Status;
use interop_api::*; // generated codec + handler trait

// (1) shared dispatch
fn dispatch(h: &Handlers, req: &RpcRequest) -> HandlerOutcome {
    match req.op.as_str() {
        "EchoScalars" => match decode_scalars(&req.payload) {
            Ok(v) => match h.echo_scalars(&(), v) {
                Ok(r) => HandlerOutcome::Reply { variant: "Scalars".into(), payload: encode_scalars(&r) },
                Err(e) => HandlerOutcome::Transport(Status::Internal, e.message),
            },
            Err(e) => HandlerOutcome::Transport(Status::MalformedEnvelope, e.to_string()),
        },
        // ...one arm per op...
        other => HandlerOutcome::Transport(Status::UnknownServiceOrOp, format!("unknown op {other}")),
    }
}

// (2) one envelope in, one envelope out — used by the HTTP carrier
fn handle_envelope(h: &Handlers, frame: &[u8]) -> Vec<u8> {
    let resp = match RpcRequest::decode(frame) {
        Ok(req) => match dispatch(h, &req) {
            HandlerOutcome::Reply { variant, payload } => RpcResponse::ok(variant, payload).with_id(req.id),
            HandlerOutcome::Transport(s, m) => RpcResponse::transport_error(s, m).with_id(req.id),
        },
        Err(e) => RpcResponse::transport_error(Status::MalformedEnvelope, e.to_string()),
    };
    resp.encode().unwrap_or_default()
}

// (3a) TCP listener: the library frames for you
fn serve_tcp(h: std::sync::Arc<Handlers>, l: std::net::TcpListener) {
    for conn in l.incoming().flatten() {
        let h = h.clone();
        std::thread::spawn(move || {
            let mut server = RpcServer::new(StreamCarrier::new(conn));
            let mut handler = |req: &RpcRequest| dispatch(&h, req);
            while let Ok(true) = server.serve_one(&mut handler) {}
        });
    }
}

// (3b) HTTP listener: the body IS the envelope (tiny_http or any HTTP server)
fn serve_http(h: std::sync::Arc<Handlers>, server: tiny_http::Server) {
    for mut req in server.incoming_requests() {
        let mut body = Vec::new();
        req.as_reader().read_to_end(&mut body).ok();
        let out = handle_envelope(&h, &body);
        let resp = tiny_http::Response::from_data(out)
            .with_header("Content-Type: application/cbor".parse::<tiny_http::Header>().unwrap());
        let _ = req.respond(resp);
    }
}

fn main() {
    let h = std::sync::Arc::new(Handlers);
    let h2 = h.clone();
    std::thread::spawn(move || serve_tcp(h2, std::net::TcpListener::bind("0.0.0.0:5081").unwrap()));
    serve_http(h, tiny_http::Server::http("0.0.0.0:5080").unwrap());
}
```

### Go (`transports/go`) — both carriers on stdlib `net`/`net/http`

```go
// shared: one envelope in, one envelope out
func handleEnvelope(h ChatHandlers, body []byte) []byte {
    req, err := transport.DecodeRpcRequest(body)
    if err != nil {
        out, _ := transport.NewRpcResponseTransportError(transport.StatusMalformedEnvelope, err.Error()).Encode()
        return out
    }
    outcome := dispatch(h, &req) // your RpcRequest -> HandlerOutcome (matches req.Op, codec, handler)
    var resp transport.RpcResponse
    if outcome.IsReply {
        resp = transport.NewRpcResponseOk(outcome.Variant, outcome.Payload).WithID(req.ID)
    } else {
        resp = transport.NewRpcResponseTransportError(outcome.Status, outcome.Message).WithID(req.ID)
    }
    out, _ := resp.Encode()
    return out
}

func main() {
    h := handlers{}
    // HTTP carrier (browser-facing)
    go func() {
        mux := http.NewServeMux()
        mux.HandleFunc("/csil/v1/rpc", func(w http.ResponseWriter, r *http.Request) {
            body, _ := io.ReadAll(r.Body)
            w.Header().Set("Content-Type", "application/cbor")
            w.Write(handleEnvelope(h, body))
        })
        http.ListenAndServe(":5080", mux)
    }()
    // TCP carrier (length-prefixed): the library frames
    ln, _ := net.Listen("tcp", ":5081")
    for {
        conn, _ := ln.Accept()
        go func(c net.Conn) {
            server := transport.NewRpcServer(transport.NewStreamCarrier(c))
            for {
                if served, err := server.ServeOne(func(req *transport.RpcRequest) transport.HandlerOutcome { return dispatch(h, req) }); err != nil || !served {
                    return
                }
            }
        }(conn)
    }
}
```

### Python (`transports/python`) — `http.server` + a socket thread

```python
from csilgen_transport.rpc import RpcRequest, RpcResponse, RpcServer, Reply, TransportOutcome
from csilgen_transport.carrier import StreamCarrier
from csilgen_transport.conventions import Status

def handle_envelope(h, body: bytes) -> bytes:
    try:
        req = RpcRequest.decode(body)
    except Exception as e:
        return RpcResponse.transport_error(Status(1), str(e)).encode()
    outcome = dispatch(h, req)             # your RpcRequest -> Reply | TransportOutcome
    if isinstance(outcome, Reply):
        return RpcResponse.ok(outcome.variant, outcome.payload).with_id(req.id).encode()
    return RpcResponse.transport_error(outcome.status, outcome.message).with_id(req.id).encode()

# HTTP carrier
class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        out = handle_envelope(HANDLERS, body)
        self.send_response(200); self.send_header("Content-Type", "application/cbor")
        self.send_header("Content-Length", str(len(out))); self.end_headers(); self.wfile.write(out)

threading.Thread(target=lambda: HTTPServer(("", 5080), Handler).serve_forever(), daemon=True).start()

# TCP carrier (length-prefixed): the library frames
srv = socket.socket(); srv.bind(("", 5081)); srv.listen()
while True:
    conn, _ = srv.accept()
    carrier = StreamCarrier(conn.makefile("rwb"))
    server = RpcServer(carrier)
    threading.Thread(target=lambda: [None for _ in iter(lambda: server.serve_one(lambda req: dispatch(HANDLERS, req)), False)], daemon=True).start()
```

---

## 6. Per-language reference

Every transport library exposes the same shape; only the method casing and the HTTP
server primitive differ. For each language: the envelope decode/encode entry points,
the stream server loop, and the idiomatic HTTP server to mount `POST /csil/v1/rpc`.

| language | envelope (decode / encode) | stream server | idiomatic HTTP server for the body |
| --- | --- | --- | --- |
| **rust** | `RpcRequest::decode` / `RpcResponse::{ok,transport_error}.encode` | `RpcServer::serve_one` over `StreamCarrier` | `tiny_http` / `hyper` / any (stdlib has none) |
| **go** | `DecodeRpcRequest` / `RpcResponse.Encode` | `RpcServer.ServeOne` over `StreamCarrier` | `net/http` (stdlib) |
| **python** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serve_one` over `StreamCarrier` | `http.server` (stdlib) |
| **typescript** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serveOne` over `StreamCarrier` | `node:http` (stdlib) |
| **java** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serveOne` over `StreamCarrier` | `com.sun.net.httpserver.HttpServer` (JDK) + `ServerSocket` |
| **csharp** | `RpcRequest.Decode` / `RpcResponse.Encode` | `RpcServer.ServeOne` over `StreamCarrier` | `System.Net.HttpListener` + `TcpListener` |
| **kotlin** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serveOne` over `StreamCarrier` | `com.sun.net.httpserver.HttpServer` (JDK) + `ServerSocket` |
| **dart** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serveOne` over `StreamCarrier` | `dart:io` `HttpServer` + `ServerSocket` |
| **ruby** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serve_one` over `StreamCarrier` | `webrick` or a `TCPServer` + minimal HTTP parse |
| **zig** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serveOne` over `StreamCarrier` | `std.http.Server` + `std.net` (0.14) |
| **c** | `csil_rpc_request_decode` / `csil_rpc_response_encode` | `csil_rpc_server` `serve_one` over the stream vtable | no stdlib HTTP — front with `libmicrohttpd`/nginx, or hand-roll HTTP/1.1 over a socket and pass the body |
| **elixir** | `RpcRequest.decode` / `RpcResponse.encode` | `RpcServer.serve_one` over `StreamCarrier` | `:gen_tcp` (in `:kernel`) + a minimal HTTP read, or Plug/Bandit; **avoid `:inets`/`:httpd` under releases** (it is pruned unless declared in `extra_applications`) |

In every case the recipe is identical: **decode the request bytes into an
`RpcRequest`, run the one shared dispatch, encode the `RpcResponse` back to bytes.**
The stream carrier wraps that with 4-byte length-prefix framing via `serve_one`; the
HTTP carrier hands the POST body in and writes the encoded response out with HTTP
`200` and `Content-Type: application/cbor`. Mount both listeners in one process on
separate threads and they share the same typed handlers.

### Notes for the awkward HTTP cases

- **C / OCaml / Elixir** have no batteries-included production HTTP server in the
  standard distribution. The CSIL part is still just *decode → dispatch → encode on
  the body*; the practical pattern is to front the service with a real HTTP server or
  reverse proxy (nginx, Caddy, a language HTTP framework) that forwards the request
  body to your `handle_envelope`, while the TCP/stream carrier serves native peers
  directly via the transport library.
- A browser only ever needs the **HTTP envelope-in-body** path; native peers and
  server↔server links should prefer the **stream** carrier (less per-call overhead,
  no HTTP parsing).
