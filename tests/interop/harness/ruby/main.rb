# frozen_string_literal: true

# Ruby interop harness -- mirrors the Rust reference (tests/interop/harness/rust).
#
# Runs as a server (binds a loopback TCP/UDP port, serves one transport) or a client (connects,
# runs the case battery for one transport, prints one JSON results object). The on-wire
# behavior and case names match the Rust reference so a Ruby client works against a Rust
# server and vice versa. See tests/interop/README.md for the contract.

require "json"
require "socket"
require "time"
require "bigdecimal"

require "interop_api"
require "csilgen/transport"

T = Csilgen::Transport
SERVICE = "interop"

# ---------------------------------------------------------------------------
# Codec adapter -- the generated router/encoders are codec-agnostic; wire them
# to the per-type CBOR codecs bound onto the generated value classes.
# ---------------------------------------------------------------------------

class CborCodec
  def encode(value)
    value.to_cbor
  end

  def decode(data, target_type)
    target_type.from_cbor(data)
  end
end

CODEC = CborCodec.new

# ---------------------------------------------------------------------------
# Fixed language-neutral test vectors (see README "Fixed test vectors").
# ---------------------------------------------------------------------------

def ts
  Time.utc(2026, 6, 29, 12, 34, 56)
end

def scalars_ok
  Scalars.new(
    i: -42,
    u: 42,
    n: -7,
    f: 3.5,
    t: "héllo 世界",
    raw: "\x01\x02\xf0\xff".b,
    flag: true,
    when: ts,
    amount: BigDecimal("123.45")
  )
end

def collections_ok
  Collections.new(
    names: ["a", "b"],
    at_least_one: [1, 2, 3],
    bounded: [10, 20],
    exact3: [7, 8, 9],
    scores: { "x" => 1, "y" => 2 },
    color: "green",
    tone: "blue",
    prio: 2,
    who: 4242,
    pair: ["p", 5],
    triple: ["t", 9, true],
    extra: { "k" => "v" }
  )
end

def constrained_ok
  Constrained.new(
    code: "PRD-AB12CD",
    qty: 10,
    rate: BigDecimal("0.25"),
    password: "hunter2hunter2",
    tags: ["one", "two"]
  )
end

def constrained_bad
  # The generated Data class does not validate at construction; build the
  # intentionally-invalid payload directly so the server's validate() surfaces it.
  Constrained.new(
    code: "bad",
    qty: 0,
    rate: BigDecimal("9.9"),
    password: "x",
    tags: []
  )
end

def nested_ok
  Nested.new(inner: scalars_ok, maybe: constrained_ok, many: [scalars_ok])
end

# ---------------------------------------------------------------------------
# Result accumulation + JSON output.
# ---------------------------------------------------------------------------

class Cases
  def initialize(transport)
    @lang = "ruby"
    @transport = transport
    @out = []
  end

  def pass(name)
    @out << [name, true, ""]
  end

  def fail(name, detail)
    @out << [name, false, detail.to_s]
  end

  def check(name, cond, detail)
    cond ? pass(name) : fail(name, detail)
  end

  def emit
    obj = {
      "lang" => @lang,
      "transport" => @transport,
      "cases" => @out.map { |name, ok, detail| { "name" => name, "ok" => ok, "detail" => detail } }
    }
    puts JSON.generate(obj)
  end
end

# ---------------------------------------------------------------------------
# Server-side handlers implementing the generated trait.
# ---------------------------------------------------------------------------

class Handlers < InteropHandlers
  def echo_scalars(req)
    req
  end

  def echo_collections(req)
    req
  end

  def echo_nested(req)
    EchoNestedResult.new(ok: true, echo: req)
  end

  def validate_constrained(req)
    req.validate
    req
  end

  def duplex(msg)
    nil
  end
end

# ---------------------------------------------------------------------------
# RPC
# ---------------------------------------------------------------------------

# Implements the transport seam the generated InteropClient expects:
# `call(service, op, req_bytes) -> resp_bytes`. A non-zero transport status raises
# (the lib's into_transport_error); a status-0 reply whose variant is `ServiceError`
# is a typed application error -- the carrier maps it to the language error path.
class RpcTransport
  def initialize(client)
    @client = client
  end

  def call(service, op, req)
    resp = @client.call(service, op, req)
    if resp.variant == "ServiceError"
      se = ServiceError.from_cbor(resp.payload)
      raise "service error #{se.code}: #{se.message}"
    end
    resp.payload
  end
end

def rpc_dispatch(handlers, req)
  case req.op
  when "EchoScalars"
    v = Scalars.from_cbor(req.payload)
    T::RPC::Reply.new("Scalars", handlers.echo_scalars(v).to_cbor)
  when "EchoCollections"
    v = Collections.from_cbor(req.payload)
    T::RPC::Reply.new("Collections", handlers.echo_collections(v).to_cbor)
  when "EchoNested"
    v = Nested.from_cbor(req.payload)
    T::RPC::Reply.new("EchoNestedResult", handlers.echo_nested(v).to_cbor)
  when "ValidateConstrained"
    v = Constrained.from_cbor(req.payload)
    begin
      # The typed error arm rides as a status-0 `ServiceError` variant.
      T::RPC::Reply.new("Constrained", handlers.validate_constrained(v).to_cbor)
    rescue ArgumentError => e
      T::RPC::Reply.new("ServiceError", ServiceError.new(code: 422, message: e.message).to_cbor)
    end
  else
    T::RPC::TransportOutcome.new(T::UNKNOWN_SERVICE_OR_OP, "unknown op #{req.op}")
  end
rescue T::Error => e
  T::RPC::TransportOutcome.new(T::MALFORMED_ENVELOPE, e.message)
end

def rpc_server(listener)
  handlers = Handlers.new
  loop do
    conn = listener.accept
    server = T::RPC::Server.new(T::StreamCarrier.new(conn))
    handler = ->(req) { rpc_dispatch(handlers, req) }
    begin
      loop { break unless server.serve_one(handler) }
    rescue T::Error, SystemCallError, IOError
      # connection ended
    ensure
      conn.close
    end
  end
end

def rpc_client(conn)
  cases = Cases.new("rpc")
  transport = RpcTransport.new(T::RPC::Client.new(T::StreamCarrier.new(conn), multiplexed: false))
  client = InteropClient.new(transport)

  begin
    r = client.echo_scalars(scalars_ok)
    cases.check("echo-scalars/success", r == scalars_ok, "mismatch: #{r.inspect}")
  rescue StandardError => e
    cases.fail("echo-scalars/success", e.message)
  end

  begin
    r = client.echo_collections(collections_ok)
    cases.check("echo-collections/success", r == collections_ok, "mismatch: #{r.inspect}")
  rescue StandardError => e
    cases.fail("echo-collections/success", e.message)
  end

  begin
    r = client.echo_nested(nested_ok)
    cases.check("echo-nested/success", r.ok && r.echo == nested_ok, "mismatch: #{r.inspect}")
  rescue StandardError => e
    cases.fail("echo-nested/success", e.message)
  end

  begin
    r = client.validate_constrained(constrained_ok)
    cases.check("validate-constrained/success", r == constrained_ok, "mismatch: #{r.inspect}")
  rescue StandardError => e
    cases.fail("validate-constrained/success", e.message)
  end

  begin
    client.validate_constrained(constrained_bad)
    cases.fail("validate-constrained/failure", "server accepted invalid input")
  rescue StandardError
    # The typed error arm surfaces as a raised error -- the failure-case path.
    cases.pass("validate-constrained/failure")
  end

  cases
end

# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------

TICKS = 3

def send_event(carrier, ev)
  carrier.send_frame(ev.encode(T::Events::Profile::VERBOSE))
end

def events_server(listener)
  handlers = Handlers.new
  ctrl = T::Events::Control
  loop do
    conn = listener.accept
    carrier = T::StreamCarrier.new(conn)
    begin
      # Handshake: read $hello, reply $hello-ack (verbose).
      frame = carrier.recv_frame
      next if frame.nil?

      ev = T::Events::Event.decode(frame, T::Events::Profile::VERBOSE)
      next if ev.event != ctrl::HELLO_NAME

      T::Events::Hello.decode(ev.payload)
      ack = T::Events::HelloAck.new(v: 1, profile: "verbose", session: nil)
      send_event(carrier, T::Events::Event.verbose(nil, ctrl::HELLO_ACK_NAME, ack.encode))

      # Push N ticks (on-tick / server push).
      TICKS.times do |seq|
        method, bytes = InteropRouter.encode_on_tick(CODEC, Tick.new(seq: seq))
        send_event(carrier, T::Events::Event.verbose(SERVICE, method, bytes))
      end

      # React loop.
      loop do
        frame = carrier.recv_frame
        break if frame.nil?

        begin
          ev = T::Events::Event.decode(frame, T::Events::Profile::VERBOSE)
        rescue T::Error
          break
        end
        method = ev.event || ""
        case method
        when ctrl::PING_NAME
          send_event(carrier, T::Events::Event.verbose(nil, ctrl::PONG_NAME, ev.payload))
        when ctrl::CLOSE_NAME
          break
        when "Duplex"
          begin
            s = Scalars.from_cbor(ev.payload)
          rescue T::Error
            next
          end
          handlers.duplex(s)
          m, b = InteropRouter.encode_duplex(CODEC, s)
          send_event(carrier, T::Events::Event.verbose(SERVICE, m, b))
        else
          # Exercise the generated router's unknown-method path.
          begin
            InteropRouter.route_channel(handlers, CODEC, method, ev.payload)
          rescue StandardError
            # unknown channel method
          end
          send_event(carrier, T::Events::Event.verbose(nil, ctrl::ERROR_NAME, "".b))
        end
      end
    rescue T::Error, SystemCallError, IOError
      # connection ended
    ensure
      conn.close
    end
  end
end

def events_client(conn)
  cases = Cases.new("events")
  carrier = T::StreamCarrier.new(conn)
  ctrl = T::Events::Control
  verbose = T::Events::Profile::VERBOSE

  # Handshake.
  hello = T::Events::Hello.new(versions: [1], profiles: ["verbose"], service: SERVICE, auth: nil)
  send_event(carrier, T::Events::Event.verbose(nil, ctrl::HELLO_NAME, hello.encode))
  ack_ok = false
  frame = carrier.recv_frame
  unless frame.nil?
    begin
      ack_ok = T::Events::Event.decode(frame, verbose).event == ctrl::HELLO_ACK_NAME
    rescue T::Error
      ack_ok = false
    end
  end
  cases.check("events/handshake", ack_ok, "no $hello-ack")

  # on-tick: expect TICKS pushes with seq 0..TICKS.
  tick_ok = true
  detail = ""
  TICKS.times do |expect|
    frame = carrier.recv_frame
    if frame.nil?
      tick_ok = false
      detail = "stream closed during ticks"
      break
    end
    begin
      ev = T::Events::Event.decode(frame, verbose)
      if ev.event != "OnTick"
        tick_ok = false
        detail = "expected OnTick got #{ev.event.inspect}"
        break
      end
      t = Tick.from_cbor(ev.payload)
      if t.seq != expect
        tick_ok = false
        detail = "tick seq #{t.seq} != #{expect}"
        break
      end
    rescue StandardError => e
      tick_ok = false
      detail = "decode: #{e.message}"
      break
    end
  end
  cases.check("on-tick/success", tick_ok, detail)

  # duplex: send Scalars, expect echo.
  m, b = InteropRouter.encode_duplex(CODEC, scalars_ok)
  send_event(carrier, T::Events::Event.verbose(SERVICE, m, b))
  frame = carrier.recv_frame
  if frame.nil?
    cases.fail("duplex/success", "no echo")
  else
    begin
      ev = T::Events::Event.decode(frame, verbose)
      if ev.event == "Duplex"
        s = Scalars.from_cbor(ev.payload)
        cases.check("duplex/success", s == scalars_ok, "echo mismatch")
      else
        cases.fail("duplex/success", "got #{ev.event.inspect}")
      end
    rescue StandardError => e
      cases.fail("duplex/success", e.message)
    end
  end

  # unknown-method: send a bogus channel method, expect $error.
  send_event(carrier, T::Events::Event.verbose(SERVICE, "Bogus", "".b))
  frame = carrier.recv_frame
  if frame.nil?
    cases.fail("unknown-method/failure", "no $error")
  else
    is_err = false
    begin
      is_err = T::Events::Event.decode(frame, verbose).event == ctrl::ERROR_NAME
    rescue T::Error
      is_err = false
    end
    cases.check("unknown-method/failure", is_err, "expected $error")
  end

  send_event(carrier, T::Events::Event.verbose(nil, ctrl::CLOSE_NAME, "".b))
  cases
end

# ---------------------------------------------------------------------------
# Datagrams
# ---------------------------------------------------------------------------

OP_ECHO_SCALARS = 0
OP_ECHO_COLLECTIONS = 1
OP_ERROR_SENTINEL = 255

def datagram_server(sock)
  loop do
    begin
      data, addr = sock.recvfrom(65_536)
    rescue SystemCallError
      next
    end
    next if data.nil? || data.empty?

    begin
      dg = T::Datagrams::Datagram.decode(data)
    rescue T::Error
      next
    end
    reply =
      case dg.op_ord
      when OP_ECHO_SCALARS
        begin
          s = Scalars.from_cbor(dg.payload)
          T::Datagrams::Datagram.new(op_ord: OP_ECHO_SCALARS, seq: dg.seq, payload: s.to_cbor)
        rescue T::Error
          T::Datagrams::Datagram.new(op_ord: OP_ERROR_SENTINEL, seq: dg.seq)
        end
      when OP_ECHO_COLLECTIONS
        begin
          c = Collections.from_cbor(dg.payload)
          T::Datagrams::Datagram.new(op_ord: OP_ECHO_COLLECTIONS, seq: dg.seq, payload: c.to_cbor)
        rescue T::Error
          T::Datagrams::Datagram.new(op_ord: OP_ERROR_SENTINEL, seq: dg.seq)
        end
      else
        T::Datagrams::Datagram.new(op_ord: OP_ERROR_SENTINEL, seq: dg.seq)
      end
    begin
      sock.send(reply.encode, 0, addr[3], addr[1])
    rescue SystemCallError
      next
    end
  end
end

def datagram_client(sock)
  cases = Cases.new("datagrams")

  roundtrip = lambda do |op, seq, payload|
    sock.send(T::Datagrams::Datagram.new(op_ord: op, seq: seq, payload: payload).encode, 0)
    raise "recv timeout" if IO.select([sock], nil, nil, 3).nil?

    data = sock.recv(65_536)
    T::Datagrams::Datagram.decode(data)
  end

  begin
    d = roundtrip.call(OP_ECHO_SCALARS, 1, scalars_ok.to_cbor)
    if d.op_ord != OP_ECHO_SCALARS
      cases.fail("echo-scalars/success", "op_ord #{d.op_ord}")
    else
      s = Scalars.from_cbor(d.payload)
      cases.check("echo-scalars/success", s == scalars_ok, "payload mismatch")
    end
  rescue StandardError => e
    cases.fail("echo-scalars/success", e.message)
  end

  begin
    d = roundtrip.call(OP_ECHO_COLLECTIONS, 2, collections_ok.to_cbor)
    if d.op_ord != OP_ECHO_COLLECTIONS
      cases.fail("echo-collections/success", "op_ord #{d.op_ord}")
    else
      c = Collections.from_cbor(d.payload)
      cases.check("echo-collections/success", c == collections_ok, "payload mismatch")
    end
  rescue StandardError => e
    cases.fail("echo-collections/success", e.message)
  end

  begin
    d = roundtrip.call(99, 3, "".b)
    cases.check(
      "bad-op-ord/failure",
      d.op_ord == OP_ERROR_SENTINEL,
      "expected error sentinel, got op_ord #{d.op_ord}"
    )
  rescue StandardError => e
    cases.fail("bad-op-ord/failure", e.message)
  end

  cases
end

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

# Bind the TCP listener, retrying briefly so a just-freed port from the previous
# server in the matrix is tolerated.
def bind_retry(port)
  200.times do
    begin
      listener = TCPServer.new("127.0.0.1", port)
      listener.setsockopt(Socket::SOL_SOCKET, Socket::SO_REUSEADDR, true)
      return listener
    rescue SystemCallError
      sleep 0.015
    end
  end
  TCPServer.new("127.0.0.1", port)
end

def connect_retry(port)
  200.times do
    begin
      return TCPSocket.new("127.0.0.1", port)
    rescue SystemCallError
      sleep 0.015
    end
  end
  TCPSocket.new("127.0.0.1", port)
end

def main
  if ARGV.length < 3
    warn "usage: main.rb <server|client> <rpc|events|datagrams> <port>"
    exit 2
  end
  mode, transport = ARGV[0], ARGV[1]
  port = Integer(ARGV[2])

  case [mode, transport]
  in ["server", "datagrams"]
    sock = UDPSocket.new
    sock.setsockopt(Socket::SOL_SOCKET, Socket::SO_REUSEADDR, true)
    sock.bind("127.0.0.1", port)
    $stdout.puts "READY"
    $stdout.flush
    datagram_server(sock)
  in ["server", _]
    listener = bind_retry(port)
    $stdout.puts "READY"
    $stdout.flush
    case transport
    when "rpc" then rpc_server(listener)
    when "events" then events_server(listener)
    else
      warn "bad transport"
      exit 2
    end
  in ["client", "datagrams"]
    # Bind an ephemeral local UDP port; the server replies to this source.
    sock = UDPSocket.new
    sock.connect("127.0.0.1", port)
    begin
      datagram_client(sock).emit
    ensure
      sock.close
    end
  in ["client", _]
    conn = connect_retry(port)
    cases =
      case transport
      when "rpc" then rpc_client(conn)
      when "events" then events_client(conn)
      else
        warn "bad transport"
        exit 2
      end
    cases.emit
    conn.close
  else
    warn "bad mode/transport"
    exit 2
  end
end

main
