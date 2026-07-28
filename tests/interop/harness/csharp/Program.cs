// C# interop harness — mirrors the Rust reference (tests/interop/harness/rust/src/main.rs).
//
// Runs as a server (binds a loopback TCP/UDP port and serves one transport) or a client
// (connects, runs the case battery for one transport, prints one JSON line of results). See
// tests/interop/README.md for the contract this implements. Synchronous and blocking
// throughout — no async, by house rule.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using Csilgen.Transport;
using interop_api;
// The generated `any` value tree and the transport library both expose a `CborValue`; alias
// the generated one so the unqualified name is never ambiguous.
using GenCbor = interop_api.CborValue;

internal static class Harness
{
    private const string Service = "Interop";

    // -----------------------------------------------------------------------
    // Fixed language-neutral test vectors (see README "Fixed test vectors").
    // -----------------------------------------------------------------------

    private static DateTimeOffset Ts() => new(2026, 6, 29, 12, 34, 56, TimeSpan.Zero);

    private static Scalars ScalarsOk() => new()
    {
        I = -42,
        U = 42,
        N = -7,
        F = 3.5,
        T = "héllo 世界",
        Raw = new byte[] { 0x01, 0x02, 0xf0, 0xff },
        Flag = true,
        When = Ts(),
        Amount = CsilDecimal.Parse("123.45"),
        // mixed-union coverage: a value equal to a literal arm must wire as
        // [1,"pending"]; a value with no literal match must wire as [0,...].
        StatusLiteral = new MixedStatusVariant2("pending"),
        StatusFree = new MixedStatusVariant1("unlisted"),
        // inline mixed choice (hoisted; literal arm match, ScalarsNoteVariant2 == "info").
        Note = new ScalarsNoteVariant2("info"),
        // inline all-literal choice (hoisted to an enum).
        Size = ScalarsSize.Medium,
        // named enum with a trailing `.default`-constrained last arm.
        Level = Level.High,
        // named enum assembled via base rule + `/=` extension.
        Season = Season.Autumn,
        // named all-literal enum with mixed literal kinds (text + int arms);
        // renders as a real C# enum.
        ShipText = ShipMode.Ground,
        ShipInt = ShipMode.Value2,
    };

    private static Collections CollectionsOk() => new()
    {
        Names = new List<string> { "a", "b" },
        AtLeastOne = new List<long> { 1, 2, 3 },
        Bounded = new List<long> { 10, 20 },
        Exact3 = new List<long> { 7, 8, 9 },
        Scores = new Dictionary<string, long> { ["x"] = 1, ["y"] = 2 },
        Color = Color.Green,
        Tone = Color.Blue,
        Prio = Priority.Value2,
        Who = new IdOrNameVariant1(4242),
        Pair = ("p", 5),
        Triple = ("t", 9, true),
        Extra = new Dictionary<string, GenCbor> { ["k"] = new GenCbor.Text("v") },
    };

    private static Constrained ConstrainedOk() => new()
    {
        Code = "PRD-AB12CD",
        Qty = 10,
        Rate = CsilDecimal.Parse("0.25"),
        Password = "hunter2hunter2",
        Tags = new List<string> { "one", "two" },
    };

    private static Constrained ConstrainedBad() => new()
    {
        Code = "bad",
        Qty = 0,
        Rate = CsilDecimal.Parse("9.9"),
        Password = "x",
        Tags = new List<string>(),
    };

    private static Nested NestedOk() => new()
    {
        Inner = ScalarsOk(),
        Maybe = ConstrainedOk(),
        Many = new List<Scalars> { ScalarsOk() },
    };

    // -----------------------------------------------------------------------
    // Structural equality (C# record `==` is reference-equal for List/Dictionary fields,
    // so the exchanged vectors are compared field-by-field).
    // -----------------------------------------------------------------------

    private static bool ScalarsEq(Scalars a, Scalars b) =>
        a.I == b.I && a.U == b.U && a.N == b.N && a.F.Equals(b.F) && a.T == b.T
        && a.Raw.SequenceEqual(b.Raw) && a.Flag == b.Flag && a.When == b.When
        && a.Amount.CompareTo(b.Amount) == 0
        && a.StatusLiteral.Equals(b.StatusLiteral) && a.StatusFree.Equals(b.StatusFree)
        && a.Note.Equals(b.Note) && a.Size == b.Size && a.Level == b.Level && a.Season == b.Season
        && a.ShipText == b.ShipText && a.ShipInt == b.ShipInt;

    private static bool ConstrainedEq(Constrained a, Constrained b) =>
        a.Code == b.Code && a.Qty == b.Qty && a.Rate.CompareTo(b.Rate) == 0
        && a.Password == b.Password && a.Tags.SequenceEqual(b.Tags);

    private static bool CborEq(GenCbor a, GenCbor b) => a switch
    {
        GenCbor.Bytes x when b is GenCbor.Bytes y => x.Value.SequenceEqual(y.Value),
        _ => a.Equals(b),
    };

    private static bool CollectionsEq(Collections a, Collections b) =>
        a.Names.SequenceEqual(b.Names) && a.AtLeastOne.SequenceEqual(b.AtLeastOne)
        && a.Bounded.SequenceEqual(b.Bounded) && a.Exact3.SequenceEqual(b.Exact3)
        && a.Scores.Count == b.Scores.Count
        && a.Scores.All(kv => b.Scores.TryGetValue(kv.Key, out var v) && v == kv.Value)
        && a.Color == b.Color && a.Tone == b.Tone && a.Prio == b.Prio && a.Who == b.Who
        && a.Pair == b.Pair && a.Triple == b.Triple
        && a.Extra.Count == b.Extra.Count
        && a.Extra.All(kv => b.Extra.TryGetValue(kv.Key, out var v) && CborEq(kv.Value, v));

    private static bool NestedEq(Nested a, Nested b) =>
        ScalarsEq(a.Inner, b.Inner)
        && (a.Maybe is null ? b.Maybe is null : b.Maybe is not null && ConstrainedEq(a.Maybe, b.Maybe))
        && a.Many.Count == b.Many.Count
        && a.Many.Zip(b.Many, ScalarsEq).All(x => x);

    // -----------------------------------------------------------------------
    // Result accumulation + JSON output (no third-party serializer).
    // -----------------------------------------------------------------------

    private sealed class Cases
    {
        private readonly string _transport;
        private readonly List<(string Name, bool Ok, string Detail)> _out = new();

        public Cases(string transport) => _transport = transport;

        public void Pass(string name) => _out.Add((name, true, string.Empty));

        public void Fail(string name, string detail) => _out.Add((name, false, detail));

        public void Check(string name, bool cond, string detail)
        {
            if (cond)
            {
                Pass(name);
            }
            else
            {
                Fail(name, detail);
            }
        }

        public void Emit()
        {
            var sb = new StringBuilder();
            sb.Append("{\"lang\":\"csharp\",\"transport\":\"").Append(_transport).Append("\",\"cases\":[");
            for (int i = 0; i < _out.Count; i++)
            {
                if (i > 0)
                {
                    sb.Append(',');
                }

                var (name, ok, detail) = _out[i];
                sb.Append("{\"name\":\"").Append(name).Append("\",\"ok\":")
                    .Append(ok ? "true" : "false").Append(",\"detail\":").Append(JsonStr(detail)).Append('}');
            }

            sb.Append("]}");
            Console.WriteLine(sb.ToString());
        }
    }

    private static string JsonStr(string s)
    {
        var sb = new StringBuilder("\"");
        foreach (char c in s)
        {
            switch (c)
            {
                case '"': sb.Append("\\\""); break;
                case '\\': sb.Append("\\\\"); break;
                case '\n': sb.Append("\\n"); break;
                case '\t': sb.Append("\\t"); break;
                case '\r': sb.Append("\\r"); break;
                default:
                    if (c < 0x20)
                    {
                        sb.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                    }
                    else
                    {
                        sb.Append(c);
                    }

                    break;
            }
        }

        sb.Append('"');
        return sb.ToString();
    }

    // -----------------------------------------------------------------------
    // Server-side handlers + the channel codec the generated router needs.
    // -----------------------------------------------------------------------

    private sealed class Handlers : IInteropService
    {
        public Scalars EchoScalars(Scalars scalars) => scalars;

        public Collections EchoCollections(Collections collections) => collections;

        public EchoNestedResult EchoNested(Nested nested) => new() { Ok = true, Echo = nested };

        public OptBytes EchoOptBytes(OptBytes optBytes) => optBytes;

        // Throws System.ArgumentException on invalid input; the RPC dispatch maps that to the
        // typed `ServiceError` arm (the failure-case path).
        public Constrained ValidateConstrained(Constrained constrained)
        {
            constrained.Validate();
            return constrained;
        }

        public void Duplex(Scalars scalars)
        {
        }
    }

    // The codec the generated channel router is wired against: canonical CBOR via the
    // generated typed codec.
    private sealed class CborCodec : ICsilCodec
    {
        public byte[] Encode(object value) => Codec.Encode(value);

        public object Decode(byte[] data, Type targetType)
        {
            if (targetType == typeof(Scalars))
            {
                return Codec.Decode<Scalars>(data);
            }

            if (targetType == typeof(Tick))
            {
                return Codec.Decode<Tick>(data);
            }

            throw new ArgumentException($"no codec for {targetType}");
        }
    }

    // -----------------------------------------------------------------------
    // RPC
    // -----------------------------------------------------------------------

    // The carrier owns the convention that a status-0 reply whose variant is the
    // `ServiceError` arm is a typed application error: it surfaces as a client exception so
    // the typed client's error path fires (mirrors the Rust RpcTransport).
    private sealed class RpcTransport : ICsilTransport
    {
        private readonly RpcClient _client;

        public RpcTransport(RpcClient client) => _client = client;

        public byte[] Call(string service, string op, byte[] request)
        {
            RpcResponse resp = _client.Call(service, op, request);
            if (resp.Variant == "ServiceError")
            {
                ServiceError se = Codec.Decode<ServiceError>(resp.Payload);
                throw new CsilClientException(se.Code, se.Message);
            }

            return resp.Payload;
        }
    }

    private static void RpcServer(Socket listener)
    {
        var handlers = new Handlers();
        while (true)
        {
            Socket conn;
            try
            {
                conn = listener.Accept();
            }
            catch (SocketException)
            {
                continue;
            }

            using var stream = new NetworkStream(conn, ownsSocket: true);
            var server = new Csilgen.Transport.RpcServer(new StreamCarrier(stream));

            HandlerOutcome Dispatch(RpcRequest req)
            {
                switch (req.Op)
                {
                    case "echo-scalars":
                        try
                        {
                            var v = Codec.Decode<Scalars>(req.Payload);
                            return HandlerOutcome.Reply("Scalars", Codec.Encode(handlers.EchoScalars(v)));
                        }
                        catch (Exception e)
                        {
                            return HandlerOutcome.Transport(Status.MalformedEnvelope, e.Message);
                        }

                    case "echo-collections":
                        try
                        {
                            var v = Codec.Decode<Collections>(req.Payload);
                            return HandlerOutcome.Reply("Collections", Codec.Encode(handlers.EchoCollections(v)));
                        }
                        catch (Exception e)
                        {
                            return HandlerOutcome.Transport(Status.MalformedEnvelope, e.Message);
                        }

                    case "echo-nested":
                        try
                        {
                            var v = Codec.Decode<Nested>(req.Payload);
                            return HandlerOutcome.Reply("EchoNestedResult", Codec.Encode(handlers.EchoNested(v)));
                        }
                        catch (Exception e)
                        {
                            return HandlerOutcome.Transport(Status.MalformedEnvelope, e.Message);
                        }

                    case "echo-opt-bytes":
                        try
                        {
                            var v = Codec.Decode<OptBytes>(req.Payload);
                            return HandlerOutcome.Reply("OptBytes", Codec.Encode(handlers.EchoOptBytes(v)));
                        }
                        catch (Exception e)
                        {
                            return HandlerOutcome.Transport(Status.MalformedEnvelope, e.Message);
                        }

                    case "validate-constrained":
                        Constrained input;
                        try
                        {
                            input = Codec.Decode<Constrained>(req.Payload);
                        }
                        catch (Exception e)
                        {
                            return HandlerOutcome.Transport(Status.MalformedEnvelope, e.Message);
                        }

                        try
                        {
                            return HandlerOutcome.Reply("Constrained", Codec.Encode(handlers.ValidateConstrained(input)));
                        }
                        catch (ArgumentException e)
                        {
                            // The typed error arm rides as a status-0 `ServiceError` variant.
                            var se = new ServiceError { Code = 422, Message = e.Message };
                            return HandlerOutcome.Reply("ServiceError", Codec.Encode(se));
                        }

                    default:
                        return HandlerOutcome.Transport(Status.UnknownServiceOrOp, $"unknown op {req.Op}");
                }
            }

            try
            {
                while (server.ServeOne(Dispatch))
                {
                }
            }
            catch (TransportException)
            {
                // Client hung up mid-frame; move on to the next connection.
            }
        }
    }

    private static Cases RpcClient(Socket sock)
    {
        var cases = new Cases("rpc");
        using var stream = new NetworkStream(sock, ownsSocket: true);
        var transport = new RpcTransport(new RpcClient(new StreamCarrier(stream), multiplexed: false));
        var client = new InteropClient(transport);

        try
        {
            var r = client.EchoScalars(ScalarsOk());
            cases.Check("echo-scalars/success", ScalarsEq(r, ScalarsOk()), "mismatch");
        }
        catch (Exception e)
        {
            cases.Fail("echo-scalars/success", e.Message);
        }

        try
        {
            var r = client.EchoCollections(CollectionsOk());
            cases.Check("echo-collections/success", CollectionsEq(r, CollectionsOk()), "mismatch");
        }
        catch (Exception e)
        {
            cases.Fail("echo-collections/success", e.Message);
        }

        try
        {
            var r = client.EchoNested(NestedOk());
            cases.Check("echo-nested/success", r.Ok && NestedEq(r.Echo, NestedOk()), "mismatch");
        }
        catch (Exception e)
        {
            cases.Fail("echo-nested/success", e.Message);
        }

        try
        {
            var r = client.ValidateConstrained(ConstrainedOk());
            cases.Check("validate-constrained/success", ConstrainedEq(r, ConstrainedOk()), "mismatch");
        }
        catch (Exception e)
        {
            cases.Fail("validate-constrained/success", e.Message);
        }

        try
        {
            client.ValidateConstrained(ConstrainedBad());
            cases.Fail("validate-constrained/failure", "server accepted invalid input");
        }
        catch (CsilClientException)
        {
            // The typed error arm must surface as a service error, not a transport error.
            cases.Pass("validate-constrained/failure");
        }
        catch (Exception e)
        {
            cases.Fail("validate-constrained/failure", $"expected service error, got {e.Message}");
        }

        // Optional-bytes presence: absent, present-empty, and present-non-empty are three
        // distinct states that must each survive the round trip unchanged. `null` is the
        // absent marker -- a zero-length byte[] is present and must not decode to null.
        try
        {
            var r = client.EchoOptBytes(new OptBytes { Tag = "absent", Payload = null });
            cases.Check("opt-bytes/absent", r.Payload is null, $"expected absent, got {r.Payload?.Length}");
        }
        catch (Exception e)
        {
            cases.Fail("opt-bytes/absent", e.Message);
        }

        try
        {
            var r = client.EchoOptBytes(new OptBytes { Tag = "empty", Payload = Array.Empty<byte>() });
            cases.Check(
                "opt-bytes/present-empty",
                r.Payload is not null && r.Payload.Length == 0,
                $"expected present-and-empty, got {(r.Payload is null ? "null" : r.Payload.Length.ToString())}");
        }
        catch (Exception e)
        {
            cases.Fail("opt-bytes/present-empty", e.Message);
        }

        try
        {
            byte[] sent = { 0x01, 0x02, 0xf0, 0xff };
            var r = client.EchoOptBytes(new OptBytes { Tag = "full", Payload = sent });
            cases.Check(
                "opt-bytes/present-non-empty",
                r.Payload is not null && r.Payload.AsSpan().SequenceEqual(sent),
                "expected 0102f0ff");
        }
        catch (Exception e)
        {
            cases.Fail("opt-bytes/present-non-empty", e.Message);
        }

        return cases;
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    private const ulong Ticks = 3;

    private static void SendEvent(StreamCarrier carrier, Event ev) =>
        carrier.SendFrame(ev.Encode(Profile.Verbose));

    private static void EventsServer(Socket listener)
    {
        var handlers = new Handlers();
        var codec = new CborCodec();
        while (true)
        {
            Socket conn;
            try
            {
                conn = listener.Accept();
            }
            catch (SocketException)
            {
                continue;
            }

            using var stream = new NetworkStream(conn, ownsSocket: true);
            var carrier = new StreamCarrier(stream);

            // Handshake: read $hello, reply $hello-ack (verbose).
            byte[]? helloFrame = carrier.RecvFrame();
            if (helloFrame is null)
            {
                continue;
            }

            Event helloEv = Event.Decode(helloFrame, Profile.Verbose);
            if (helloEv.EventName != Control.HelloName)
            {
                continue;
            }

            _ = Hello.Decode(helloEv.Payload);
            var ack = new HelloAck(1, "verbose");
            SendEvent(carrier, Event.Verbose(null, Control.HelloAckName, ack.Encode()));

            // Push N ticks (on-tick / server push).
            for (ulong seq = 0; seq < Ticks; seq++)
            {
                var (method, bytes) = InteropRouter.EncodeOnTick(codec, new Tick { Seq = seq });
                SendEvent(carrier, Event.Verbose(Service, method, bytes));
            }

            // React loop.
            while (true)
            {
                byte[]? frame = carrier.RecvFrame();
                if (frame is null)
                {
                    break;
                }

                Event ev;
                try
                {
                    ev = Event.Decode(frame, Profile.Verbose);
                }
                catch (TransportException)
                {
                    break;
                }

                string method = ev.EventName ?? string.Empty;
                if (method == Control.PingName)
                {
                    SendEvent(carrier, Event.Verbose(null, Control.PongName, ev.Payload));
                }
                else if (method == Control.CloseName)
                {
                    break;
                }
                else if (method == "duplex")
                {
                    try
                    {
                        var s = Codec.Decode<Scalars>(ev.Payload);
                        handlers.Duplex(s);
                        var (m, b) = InteropRouter.EncodeDuplex(codec, s);
                        SendEvent(carrier, Event.Verbose(Service, m, b));
                    }
                    catch (Exception)
                    {
                        // A malformed Duplex payload is dropped, as in the reference.
                    }
                }
                else
                {
                    // Exercise the generated router's unknown-method path, then signal $error.
                    try
                    {
                        InteropRouter.RouteChannel(handlers, codec, method, ev.Payload);
                    }
                    catch (ArgumentException)
                    {
                        // Expected for an unknown channel method.
                    }

                    SendEvent(carrier, Event.Verbose(null, Control.ErrorName, Array.Empty<byte>()));
                }
            }
        }
    }

    private static Cases EventsClient(Socket sock)
    {
        var cases = new Cases("events");
        using var stream = new NetworkStream(sock, ownsSocket: true);
        var carrier = new StreamCarrier(stream);
        var codec = new CborCodec();

        // Handshake.
        var hello = new Hello(new ulong[] { 1 }, new[] { "verbose" }) { Service = Service };
        SendEvent(carrier, Event.Verbose(null, Control.HelloName, hello.Encode()));
        byte[]? ackFrame = carrier.RecvFrame();
        bool ackOk = ackFrame is not null
            && Event.Decode(ackFrame, Profile.Verbose).EventName == Control.HelloAckName;
        cases.Check("events/handshake", ackOk, "no $hello-ack");

        // on-tick: expect TICKS pushes with seq 0..TICKS.
        bool tickOk = true;
        string detail = string.Empty;
        for (ulong expect = 0; expect < Ticks; expect++)
        {
            byte[]? frame = carrier.RecvFrame();
            if (frame is null)
            {
                tickOk = false;
                detail = "stream closed during ticks";
                break;
            }

            Event ev = Event.Decode(frame, Profile.Verbose);
            if (ev.EventName != "on-tick")
            {
                tickOk = false;
                detail = $"expected on-tick got {ev.EventName}";
                break;
            }

            Tick t = Codec.Decode<Tick>(ev.Payload);
            if (t.Seq != expect)
            {
                tickOk = false;
                detail = $"tick seq {t.Seq} != {expect}";
                break;
            }
        }

        cases.Check("on-tick/success", tickOk, detail);

        // duplex: send Scalars, expect echo.
        var (m, b) = InteropRouter.EncodeDuplex(codec, ScalarsOk());
        SendEvent(carrier, Event.Verbose(Service, m, b));
        byte[]? duplexFrame = carrier.RecvFrame();
        if (duplexFrame is null)
        {
            cases.Fail("duplex/success", "no echo");
        }
        else
        {
            Event ev = Event.Decode(duplexFrame, Profile.Verbose);
            if (ev.EventName == "duplex")
            {
                var s = Codec.Decode<Scalars>(ev.Payload);
                cases.Check("duplex/success", ScalarsEq(s, ScalarsOk()), "echo mismatch");
            }
            else
            {
                cases.Fail("duplex/success", $"got {ev.EventName}");
            }
        }

        // unknown-method: send a bogus channel method, expect $error.
        SendEvent(carrier, Event.Verbose(Service, "Bogus", Array.Empty<byte>()));
        byte[]? errFrame = carrier.RecvFrame();
        bool isErr = errFrame is not null
            && Event.Decode(errFrame, Profile.Verbose).EventName == Control.ErrorName;
        cases.Check("unknown-method/failure", isErr, "expected $error");

        SendEvent(carrier, Event.Verbose(null, Control.CloseName, Array.Empty<byte>()));
        return cases;
    }

    // -----------------------------------------------------------------------
    // Datagrams
    // -----------------------------------------------------------------------

    private const ulong OpEchoScalars = 0;
    private const ulong OpEchoCollections = 1;
    private const ulong OpErrorSentinel = 255;

    private static void DatagramServer(Socket sock)
    {
        var buf = new byte[65536];
        while (true)
        {
            EndPoint remote = new IPEndPoint(IPAddress.Any, 0);
            int n;
            try
            {
                n = sock.ReceiveFrom(buf, ref remote);
            }
            catch (SocketException)
            {
                continue;
            }

            Datagram dg;
            try
            {
                dg = Datagram.Decode(buf.AsSpan(0, n).ToArray());
            }
            catch (TransportException)
            {
                continue;
            }

            Datagram reply;
            switch (dg.OpOrd)
            {
                case OpEchoScalars:
                    try
                    {
                        var s = Codec.Decode<Scalars>(dg.Payload);
                        reply = Datagram.New(OpEchoScalars, dg.Seq, Codec.Encode(s));
                    }
                    catch (Exception)
                    {
                        reply = Datagram.New(OpErrorSentinel, dg.Seq, Array.Empty<byte>());
                    }

                    break;
                case OpEchoCollections:
                    try
                    {
                        var c = Codec.Decode<Collections>(dg.Payload);
                        reply = Datagram.New(OpEchoCollections, dg.Seq, Codec.Encode(c));
                    }
                    catch (Exception)
                    {
                        reply = Datagram.New(OpErrorSentinel, dg.Seq, Array.Empty<byte>());
                    }

                    break;
                default:
                    reply = Datagram.New(OpErrorSentinel, dg.Seq, Array.Empty<byte>());
                    break;
            }

            try
            {
                sock.SendTo(reply.Encode(), remote);
            }
            catch (SocketException)
            {
                // Client gone; ignore.
            }
        }
    }

    private static Cases DatagramClient(Socket sock)
    {
        var cases = new Cases("datagrams");
        sock.ReceiveTimeout = 3000;
        var buf = new byte[65536];

        Datagram Roundtrip(ulong op, ulong seq, byte[] payload)
        {
            sock.Send(Datagram.New(op, seq, payload).Encode());
            int n = sock.Receive(buf);
            return Datagram.Decode(buf.AsSpan(0, n).ToArray());
        }

        try
        {
            Datagram d = Roundtrip(OpEchoScalars, 1, Codec.Encode(ScalarsOk()));
            if (d.OpOrd == OpEchoScalars)
            {
                var s = Codec.Decode<Scalars>(d.Payload);
                cases.Check("echo-scalars/success", ScalarsEq(s, ScalarsOk()), "payload mismatch");
            }
            else
            {
                cases.Fail("echo-scalars/success", $"op_ord {d.OpOrd}");
            }
        }
        catch (Exception e)
        {
            cases.Fail("echo-scalars/success", e.Message);
        }

        try
        {
            Datagram d = Roundtrip(OpEchoCollections, 2, Codec.Encode(CollectionsOk()));
            if (d.OpOrd == OpEchoCollections)
            {
                var c = Codec.Decode<Collections>(d.Payload);
                cases.Check("echo-collections/success", CollectionsEq(c, CollectionsOk()), "payload mismatch");
            }
            else
            {
                cases.Fail("echo-collections/success", $"op_ord {d.OpOrd}");
            }
        }
        catch (Exception e)
        {
            cases.Fail("echo-collections/success", e.Message);
        }

        try
        {
            Datagram d = Roundtrip(99, 3, Array.Empty<byte>());
            cases.Check(
                "bad-op-ord/failure",
                d.OpOrd == OpErrorSentinel,
                $"expected error sentinel, got op_ord {d.OpOrd}");
        }
        catch (Exception e)
        {
            cases.Fail("bad-op-ord/failure", e.Message);
        }

        return cases;
    }

    // -----------------------------------------------------------------------
    // main
    // -----------------------------------------------------------------------

    private static int Main(string[] args)
    {
        if (args.Length < 3)
        {
            Console.Error.WriteLine("usage: csil-interop <server|client> <rpc|events|datagrams> <port>");
            return 2;
        }

        string mode = args[0];
        string transport = args[1];
        if (!int.TryParse(args[2], NumberStyles.Integer, CultureInfo.InvariantCulture, out int port))
        {
            Console.Error.WriteLine($"bad port {args[2]}");
            return 2;
        }

        var addr = new IPEndPoint(IPAddress.Loopback, port);

        switch (mode, transport)
        {
            case ("server", "datagrams"):
            {
                var sock = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
                sock.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.ReuseAddress, true);
                sock.Bind(addr);
                Ready();
                DatagramServer(sock);
                return 0;
            }

            case ("server", "rpc"):
            case ("server", "events"):
            {
                Socket listener = BindRetry(addr);
                Ready();
                if (transport == "rpc")
                {
                    RpcServer(listener);
                }
                else
                {
                    EventsServer(listener);
                }

                return 0;
            }

            case ("client", "datagrams"):
            {
                // Ephemeral local UDP port; the server replies to this source endpoint.
                var sock = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
                sock.Bind(new IPEndPoint(IPAddress.Loopback, 0));
                sock.Connect(addr);
                Cases cases = DatagramClient(sock);
                cases.Emit();
                sock.Dispose();
                return 0;
            }

            case ("client", "rpc"):
            case ("client", "events"):
            {
                Socket sock = ConnectRetry(addr);
                Cases cases = transport == "rpc" ? RpcClient(sock) : EventsClient(sock);
                cases.Emit();
                return 0;
            }

            default:
                Console.Error.WriteLine("bad mode/transport");
                return 2;
        }
    }

    private static void Ready()
    {
        Console.WriteLine("READY");
        Console.Out.Flush();
    }

    // Bind the TCP listener, retrying briefly so a just-freed port from the previous server in
    // the matrix is tolerated even with SO_REUSEADDR set (TIME_WAIT on rapid sequential rebind).
    private static Socket BindRetry(IPEndPoint ep)
    {
        for (int i = 0; i < 200; i++)
        {
            var listener = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
            listener.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.ReuseAddress, true);
            try
            {
                listener.Bind(ep);
                listener.Listen(128);
                return listener;
            }
            catch (SocketException)
            {
                listener.Dispose();
                Thread.Sleep(15);
            }
        }

        var last = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
        last.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.ReuseAddress, true);
        last.Bind(ep);
        last.Listen(128);
        return last;
    }

    private static Socket ConnectRetry(IPEndPoint ep)
    {
        for (int i = 0; i < 200; i++)
        {
            var sock = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
            try
            {
                sock.Connect(ep);
                return sock;
            }
            catch (SocketException)
            {
                sock.Dispose();
                Thread.Sleep(15);
            }
        }

        var last = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
        last.Connect(ep);
        return last;
    }
}
