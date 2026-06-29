# Elixir interop harness -- mirrors the Rust reference (tests/interop/harness/rust).
#
# Runs as a server (binds a loopback TCP/UDP port, serves one transport) or a client
# (connects, runs the case battery for one transport, prints one JSON results object).
# The on-wire behavior and case names match the Rust reference so an Elixir client works
# against a Rust server and vice versa. See tests/interop/README.md for the contract.
#
# Stream transports (rpc, events) ride a loopback TCP socket framed with the canonical
# 4-byte big-endian length prefix; `:gen_tcp` with `packet: 4` produces exactly that
# framing. Datagrams ride a loopback UDP socket via `:gen_udp`.

alias Csilgen.Generated, as: G
alias Csilgen.Transport.{RPC, Events, Datagrams, Status}

defmodule Ctl do
  # Events control-plane event names (conventions doc); identical strings across langs.
  def hello, do: "$hello"
  def hello_ack, do: "$hello-ack"
  def ping, do: "$ping"
  def pong, do: "$pong"
  def close, do: "$close"
  def error, do: "$error"
end

defmodule Vectors do
  @moduledoc "Fixed language-neutral test vectors (see README 'Fixed test vectors')."

  def ts do
    {:ok, dt, _} = DateTime.from_iso8601("2026-06-29T12:34:56Z")
    dt
  end

  def scalars_ok do
    %G.Scalars{
      i: -42,
      u: 42,
      n: -7,
      f: 3.5,
      t: "héllo 世界",
      raw: <<0x01, 0x02, 0xF0, 0xFF>>,
      flag: true,
      when: ts(),
      amount: G.Decimal.from_string("123.45")
    }
  end

  def collections_ok do
    %G.Collections{
      names: ["a", "b"],
      at_least_one: [1, 2, 3],
      bounded: [10, 20],
      exact3: [7, 8, 9],
      scores: %{"x" => 1, "y" => 2},
      color: "green",
      prio: 2,
      who: 4242,
      pair: {"p", 5},
      triple: {"t", 9, true},
      # `any`-valued map: the value rides as the codec's own CBOR value tree.
      extra: %{"k" => {:text, "v"}}
    }
  end

  def constrained_ok do
    %G.Constrained{
      code: "PRD-AB12CD",
      qty: 10,
      rate: G.Decimal.from_string("0.25"),
      password: "hunter2hunter2",
      tags: ["one", "two"]
    }
  end

  def constrained_bad do
    %G.Constrained{
      code: "bad",
      qty: 0,
      rate: G.Decimal.from_string("9.9"),
      password: "x",
      tags: []
    }
  end

  def nested_ok do
    %G.Nested{inner: scalars_ok(), maybe: constrained_ok(), many: [scalars_ok()]}
  end
end

defmodule Cases do
  @moduledoc "Result accumulation + one-line JSON output."
  defstruct lang: "elixir", transport: nil, out: []

  def new(transport), do: %__MODULE__{transport: transport, out: []}

  def pass(c, name), do: %{c | out: c.out ++ [{name, true, ""}]}
  def fail(c, name, detail), do: %{c | out: c.out ++ [{name, false, to_string(detail)}]}

  def check(c, name, true, _detail), do: pass(c, name)
  def check(c, name, false, detail), do: fail(c, name, detail)

  def emit(c) do
    obj = %{
      "lang" => c.lang,
      "transport" => c.transport,
      "cases" =>
        Enum.map(c.out, fn {name, ok, detail} ->
          %{"name" => name, "ok" => ok, "detail" => detail}
        end)
    }

    IO.puts(JSON.encode!(obj))
  end
end

defmodule InteropCodec do
  @moduledoc "Codec seam for the generated channel router/encoders: per-type CBOR."
  @behaviour G.Codec
  @impl true
  def encode(%mod{} = value), do: mod.to_cbor(value)
  @impl true
  def decode(data, type), do: type.from_cbor(data)
end

defmodule Handlers do
  @moduledoc "Server handlers implementing the generated InteropServer behaviour."
  @behaviour G.InteropServer
  @impl true
  def echo_scalars(req, _ctx), do: {:ok, req}
  @impl true
  def echo_collections(req, _ctx), do: {:ok, req}
  @impl true
  def echo_nested(req, _ctx), do: {:ok, %G.EchoNestedResult{ok: true, echo: req}}
  @impl true
  def validate_constrained(req, _ctx) do
    case G.Validation.validate_constrained(req) do
      :ok -> {:ok, req}
      {:error, msg} -> {:error, %G.ServiceError{code: 422, message: msg}}
    end
  end

  @impl true
  def duplex(_msg, _ctx), do: :ok
end

# ---------------------------------------------------------------------------
# Stream framing over :gen_tcp (packet: 4 == 4-byte big-endian length prefix).
# ---------------------------------------------------------------------------

defmodule FrameSock do
  # `packet: 4` is the canonical 4-byte big-endian length prefix the lib StreamCarrier
  # expects; it is carrier-independent, so it stays identical across the socket switch.
  @opts [:binary, {:packet, 4}, {:active, false}, {:ip, {127, 0, 0, 1}}]

  def listen(port) do
    {:ok, lsock} = :gen_tcp.listen(port, @opts ++ [{:reuseaddr, true}])
    lsock
  end

  def accept(lsock) do
    {:ok, conn} = :gen_tcp.accept(lsock)
    conn
  end

  def connect_retry(port), do: connect_retry(port, 200)

  defp connect_retry(port, 0), do: do_connect!(port)

  defp connect_retry(port, n) do
    case :gen_tcp.connect({127, 0, 0, 1}, port, @opts) do
      {:ok, sock} ->
        sock

      {:error, _} ->
        Process.sleep(15)
        connect_retry(port, n - 1)
    end
  end

  defp do_connect!(port) do
    {:ok, sock} = :gen_tcp.connect({127, 0, 0, 1}, port, @opts)
    sock
  end

  def send_frame(sock, frame), do: :gen_tcp.send(sock, frame)

  # Returns the next frame, or nil on clean EOF / error.
  def recv_frame(sock, timeout \\ :infinity) do
    case :gen_tcp.recv(sock, 0, timeout) do
      {:ok, data} -> data
      {:error, _} -> nil
    end
  end
end

# ---------------------------------------------------------------------------
# RPC
# ---------------------------------------------------------------------------

# Implements the transport seam the generated InteropClient expects: `call/4` takes the
# already-encoded request bytes and returns the response payload bytes. A status-0 reply
# whose variant is `ServiceError` is a typed application error -- the carrier maps it to
# the language error path (a raised error); a non-ok transport status also raises.
defmodule RpcCarrier do
  defstruct [:sock]

  def call(%__MODULE__{sock: sock}, service, op, req) do
    frame = RPC.encode_request(RPC.new_request(service, op, req))
    :ok = FrameSock.send_frame(sock, frame)
    resp_frame = FrameSock.recv_frame(sock, 5000)
    if resp_frame == nil, do: raise("rpc: no response")
    {:ok, resp} = RPC.decode_response(resp_frame)

    cond do
      resp.variant == "ServiceError" ->
        se = G.ServiceError.from_cbor(resp.payload)
        raise "service error #{se.code}: #{se.message}"

      resp.status != Status.ok() ->
        raise "transport status #{resp.status}: #{resp.error || ""}"

      true ->
        resp.payload
    end
  end
end

defmodule Rpc do
  def dispatch(req) do
    case req.op do
      "EchoScalars" ->
        {:ok, r} = Handlers.echo_scalars(G.Scalars.from_cbor(req.payload), %{})
        RPC.new_response_ok("Scalars", G.Scalars.to_cbor(r))

      "EchoCollections" ->
        {:ok, r} = Handlers.echo_collections(G.Collections.from_cbor(req.payload), %{})
        RPC.new_response_ok("Collections", G.Collections.to_cbor(r))

      "EchoNested" ->
        {:ok, r} = Handlers.echo_nested(G.Nested.from_cbor(req.payload), %{})
        RPC.new_response_ok("EchoNestedResult", G.EchoNestedResult.to_cbor(r))

      "ValidateConstrained" ->
        # The typed error arm rides as a status-0 `ServiceError` variant.
        case Handlers.validate_constrained(G.Constrained.from_cbor(req.payload), %{}) do
          {:ok, r} -> RPC.new_response_ok("Constrained", G.Constrained.to_cbor(r))
          {:error, se} -> RPC.new_response_ok("ServiceError", G.ServiceError.to_cbor(se))
        end

      other ->
        RPC.new_response_transport_error(Status.unknown_service_or_op(), "unknown op #{other}")
    end
  rescue
    e -> RPC.new_response_transport_error(Status.malformed_envelope(), Exception.message(e))
  end

  def server(lsock) do
    conn = FrameSock.accept(lsock)
    serve_conn(conn)
    server(lsock)
  end

  defp serve_conn(conn) do
    case FrameSock.recv_frame(conn) do
      nil ->
        :gen_tcp.close(conn)

      frame ->
        case RPC.decode_request(frame) do
          {:ok, req} ->
            :ok = FrameSock.send_frame(conn, RPC.encode_response(dispatch(req)))
            serve_conn(conn)

          {:error, _} ->
            :gen_tcp.close(conn)
        end
    end
  end

  def client(sock) do
    transport = %RpcCarrier{sock: sock}
    client = G.InteropClient.new(transport)
    cases = Cases.new("rpc")

    cases =
      try do
        r = G.InteropClient.echo_scalars(client, Vectors.scalars_ok())
        Cases.check(cases, "echo-scalars/success", r == Vectors.scalars_ok(), "mismatch: #{inspect(r)}")
      rescue
        e -> Cases.fail(cases, "echo-scalars/success", Exception.message(e))
      end

    cases =
      try do
        r = G.InteropClient.echo_collections(client, Vectors.collections_ok())
        Cases.check(cases, "echo-collections/success", r == Vectors.collections_ok(), "mismatch: #{inspect(r)}")
      rescue
        e -> Cases.fail(cases, "echo-collections/success", Exception.message(e))
      end

    cases =
      try do
        r = G.InteropClient.echo_nested(client, Vectors.nested_ok())
        Cases.check(cases, "echo-nested/success", r.ok && r.echo == Vectors.nested_ok(), "mismatch: #{inspect(r)}")
      rescue
        e -> Cases.fail(cases, "echo-nested/success", Exception.message(e))
      end

    cases =
      try do
        r = G.InteropClient.validate_constrained(client, Vectors.constrained_ok())
        Cases.check(cases, "validate-constrained/success", r == Vectors.constrained_ok(), "mismatch: #{inspect(r)}")
      rescue
        e -> Cases.fail(cases, "validate-constrained/success", Exception.message(e))
      end

    cases =
      try do
        _ = G.InteropClient.validate_constrained(client, Vectors.constrained_bad())
        Cases.fail(cases, "validate-constrained/failure", "server accepted invalid input")
      rescue
        # The typed error arm surfaces as a raised error -- the failure-case path.
        _ -> Cases.pass(cases, "validate-constrained/failure")
      end

    cases
  end
end

# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------

defmodule Evt do
  @ticks 3
  @service "interop"

  defp send_event(sock, %Events.Event{} = ev) do
    FrameSock.send_frame(sock, Events.encode_event(ev, :verbose))
  end

  defp verbose(service, event, payload),
    do: %Events.Event{service: service, event: event, payload: payload}

  def server(lsock) do
    conn = FrameSock.accept(lsock)
    serve_conn(conn)
    server(lsock)
  end

  defp serve_conn(conn) do
    with frame when frame != nil <- FrameSock.recv_frame(conn),
         {:ok, ev} <- Events.decode_event(frame, :verbose),
         true <- ev.event == Ctl.hello() do
      {:ok, _hello} = {:ok, ev.payload}

      ack = Events.encode_hello_ack(%Events.HelloAck{v: 1, profile: "verbose", session: nil})
      send_event(conn, verbose(nil, Ctl.hello_ack(), ack))

      # Push N ticks (on-tick / server push), seq 0..N-1.
      Enum.each(0..(@ticks - 1), fn seq ->
        {method, bytes} = G.InteropServer.encode_on_tick(InteropCodec, %G.Tick{seq: seq})
        send_event(conn, verbose(@service, method, bytes))
      end)

      react(conn)
    else
      _ -> :ok
    end

    :gen_tcp.close(conn)
  end

  defp react(conn) do
    case FrameSock.recv_frame(conn) do
      nil ->
        :ok

      frame ->
        case Events.decode_event(frame, :verbose) do
          {:ok, ev} -> handle_event(conn, ev)
          {:error, _} -> :ok
        end
    end
  end

  defp handle_event(conn, ev) do
    cond do
      ev.event == Ctl.ping() ->
        send_event(conn, verbose(nil, Ctl.pong(), ev.payload))
        react(conn)

      ev.event == Ctl.close() ->
        :ok

      ev.event == "Duplex" ->
        case decode_scalars(ev.payload) do
          {:ok, s} ->
            Handlers.duplex(s, %{})
            {m, b} = G.InteropServer.encode_duplex(InteropCodec, s)
            send_event(conn, verbose(@service, m, b))

          :error ->
            :ok
        end

        react(conn)

      true ->
        # Exercise the generated router's unknown-method path, then signal $error.
        _ = G.InteropServer.route(Handlers, InteropCodec, ev.event, ev.payload, %{})
        send_event(conn, verbose(nil, Ctl.error(), <<>>))
        react(conn)
    end
  end

  def client(sock) do
    cases = Cases.new("events")

    # Handshake.
    hello =
      Events.encode_hello(%Events.Hello{
        versions: [1],
        profiles: ["verbose"],
        service: @service,
        auth: nil
      })

    send_event(sock, verbose(nil, Ctl.hello(), hello))

    ack_ok =
      case FrameSock.recv_frame(sock, 5000) do
        nil -> false
        frame -> match?({:ok, %Events.Event{event: "$hello-ack"}}, Events.decode_event(frame, :verbose))
      end

    cases = Cases.check(cases, "events/handshake", ack_ok, "no $hello-ack")

    # on-tick: expect TICKS pushes with seq 0..TICKS.
    {tick_ok, detail} = read_ticks(sock, 0, @ticks)
    cases = Cases.check(cases, "on-tick/success", tick_ok, detail)

    # duplex: send Scalars, expect echo.
    {m, b} = G.InteropServer.encode_duplex(InteropCodec, Vectors.scalars_ok())
    send_event(sock, verbose(@service, m, b))

    cases =
      case FrameSock.recv_frame(sock, 5000) do
        nil ->
          Cases.fail(cases, "duplex/success", "no echo")

        frame ->
          case Events.decode_event(frame, :verbose) do
            {:ok, %Events.Event{event: "Duplex", payload: p}} ->
              s = G.Scalars.from_cbor(p)
              Cases.check(cases, "duplex/success", s == Vectors.scalars_ok(), "echo mismatch")

            {:ok, ev} ->
              Cases.fail(cases, "duplex/success", "got #{inspect(ev.event)}")

            {:error, e} ->
              Cases.fail(cases, "duplex/success", inspect(e))
          end
      end

    # unknown-method: send a bogus channel method, expect $error.
    send_event(sock, verbose(@service, "Bogus", <<>>))

    cases =
      case FrameSock.recv_frame(sock, 5000) do
        nil ->
          Cases.fail(cases, "unknown-method/failure", "no $error")

        frame ->
          is_err = match?({:ok, %Events.Event{event: "$error"}}, Events.decode_event(frame, :verbose))
          Cases.check(cases, "unknown-method/failure", is_err, "expected $error")
      end

    send_event(sock, verbose(nil, Ctl.close(), <<>>))
    cases
  end

  defp decode_scalars(payload) do
    {:ok, G.Scalars.from_cbor(payload)}
  rescue
    _ -> :error
  end

  defp read_ticks(_sock, n, n), do: {true, ""}

  defp read_ticks(sock, expect, total) do
    case FrameSock.recv_frame(sock, 5000) do
      nil ->
        {false, "stream closed during ticks"}

      frame ->
        case Events.decode_event(frame, :verbose) do
          {:ok, %Events.Event{event: "OnTick", payload: p}} ->
            t = G.Tick.from_cbor(p)

            if t.seq == expect do
              read_ticks(sock, expect + 1, total)
            else
              {false, "tick seq #{t.seq} != #{expect}"}
            end

          {:ok, ev} ->
            {false, "expected OnTick got #{inspect(ev.event)}"}

          {:error, e} ->
            {false, "decode: #{inspect(e)}"}
        end
    end
  end
end

# ---------------------------------------------------------------------------
# Datagrams (loopback UDP via :gen_udp)
# ---------------------------------------------------------------------------

defmodule Dgram do
  @op_echo_scalars 0
  @op_echo_collections 1
  @op_error_sentinel 255
  @opts [:binary, {:active, false}, {:ip, {127, 0, 0, 1}}]

  def open_bind(port) do
    {:ok, sock} = :gen_udp.open(port, @opts)
    sock
  end

  def server(sock) do
    case :gen_udp.recv(sock, 0) do
      {:ok, {addr, srcport, data}} ->
        handle(sock, addr, srcport, data)
        server(sock)

      {:error, _} ->
        server(sock)
    end
  end

  defp handle(sock, addr, srcport, data) do
    case Datagrams.decode_datagram(data) do
      {:ok, dg} ->
        reply = reply_for(dg)
        :gen_udp.send(sock, addr, srcport, Datagrams.encode_datagram(reply))

      {:error, _} ->
        :ok
    end
  end

  defp reply_for(dg) do
    case dg.op_ord do
      @op_echo_scalars ->
        case safe(fn -> G.Scalars.from_cbor(dg.payload) end) do
          {:ok, s} -> Datagrams.new_datagram(@op_echo_scalars, dg.seq, G.Scalars.to_cbor(s))
          :error -> Datagrams.new_datagram(@op_error_sentinel, dg.seq, <<>>)
        end

      @op_echo_collections ->
        case safe(fn -> G.Collections.from_cbor(dg.payload) end) do
          {:ok, c} -> Datagrams.new_datagram(@op_echo_collections, dg.seq, G.Collections.to_cbor(c))
          :error -> Datagrams.new_datagram(@op_error_sentinel, dg.seq, <<>>)
        end

      _ ->
        Datagrams.new_datagram(@op_error_sentinel, dg.seq, <<>>)
    end
  end

  defp safe(fun) do
    {:ok, fun.()}
  rescue
    _ -> :error
  end

  def client(sock, server_port) do
    cases = Cases.new("datagrams")

    roundtrip = fn op, seq, payload ->
      dg = Datagrams.new_datagram(op, seq, payload)
      :ok = :gen_udp.send(sock, {127, 0, 0, 1}, server_port, Datagrams.encode_datagram(dg))

      case :gen_udp.recv(sock, 0, 3000) do
        {:ok, {_addr, _srcport, data}} -> Datagrams.decode_datagram(data)
        {:error, reason} -> {:error, reason}
      end
    end

    cases =
      case roundtrip.(@op_echo_scalars, 1, G.Scalars.to_cbor(Vectors.scalars_ok())) do
        {:ok, d} when d.op_ord == @op_echo_scalars ->
          s = G.Scalars.from_cbor(d.payload)
          Cases.check(cases, "echo-scalars/success", s == Vectors.scalars_ok(), "payload mismatch")

        {:ok, d} ->
          Cases.fail(cases, "echo-scalars/success", "op_ord #{d.op_ord}")

        {:error, e} ->
          Cases.fail(cases, "echo-scalars/success", inspect(e))
      end

    cases =
      case roundtrip.(@op_echo_collections, 2, G.Collections.to_cbor(Vectors.collections_ok())) do
        {:ok, d} when d.op_ord == @op_echo_collections ->
          c = G.Collections.from_cbor(d.payload)
          Cases.check(cases, "echo-collections/success", c == Vectors.collections_ok(), "payload mismatch")

        {:ok, d} ->
          Cases.fail(cases, "echo-collections/success", "op_ord #{d.op_ord}")

        {:error, e} ->
          Cases.fail(cases, "echo-collections/success", inspect(e))
      end

    cases =
      case roundtrip.(99, 3, <<>>) do
        {:ok, d} ->
          Cases.check(
            cases,
            "bad-op-ord/failure",
            d.op_ord == @op_error_sentinel,
            "expected error sentinel, got op_ord #{d.op_ord}"
          )

        {:error, e} ->
          Cases.fail(cases, "bad-op-ord/failure", inspect(e))
      end

    cases
  end
end

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

defmodule Main do
  def run([mode, transport, port_str]) do
    port = String.to_integer(port_str)

    case {mode, transport} do
      {"server", "datagrams"} ->
        sock = Dgram.open_bind(port)
        ready()
        Dgram.server(sock)

      {"server", t} when t in ["rpc", "events"] ->
        lsock = FrameSock.listen(port)
        ready()
        if t == "rpc", do: Rpc.server(lsock), else: Evt.server(lsock)

      {"client", "datagrams"} ->
        # Bind an ephemeral local UDP port; the server replies to this source.
        sock = Dgram.open_bind(0)

        try do
          Dgram.client(sock, port) |> Cases.emit()
        after
          :gen_udp.close(sock)
        end

      {"client", t} when t in ["rpc", "events"] ->
        sock = FrameSock.connect_retry(port)
        cases = if t == "rpc", do: Rpc.client(sock), else: Evt.client(sock)
        Cases.emit(cases)
        :gen_tcp.close(sock)

      _ ->
        IO.puts(:stderr, "bad mode/transport")
        System.halt(2)
    end
  end

  def run(_) do
    IO.puts(:stderr, "usage: main.exs <server|client> <rpc|events|datagrams> <port>")
    System.halt(2)
  end

  defp ready do
    IO.puts("READY")
    # Stdout is line-buffered to a pipe; nothing else is written before serving, but
    # flush defensively so the orchestrator observes READY immediately.
    :ok
  end
end

Main.run(System.argv())
