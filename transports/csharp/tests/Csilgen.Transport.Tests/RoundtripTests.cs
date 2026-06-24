// Round-trip unit tests mirroring the Go/Rust reference tests: codecs over the loopback carrier,
// events negotiation, sequence tracking, version rejection, and stream framing.

using Csilgen.Transport;
using Xunit;

namespace Csilgen.Transport.Tests;

public sealed class RoundtripTests
{
    [Fact]
    public void RpcRequestRoundTrip()
    {
        var req = RpcRequest.New("Attestation", "deposit-claim", new byte[] { 0xa0 }).WithId(7);
        RpcRequest dec = RpcRequest.Decode(req.Encode());
        Assert.Equal(req.Service, dec.Service);
        Assert.Equal(req.Op, dec.Op);
        Assert.Equal((ulong?)7, dec.Id);
        Assert.True(req.Payload.AsSpan().SequenceEqual(dec.Payload));
    }

    [Fact]
    public void RpcResponseSuccessAndError()
    {
        RpcResponse ok = RpcResponse.Ok("DepositClaimResponse", new byte[] { 0xa0 }).WithId(7);
        RpcResponse decOk = RpcResponse.Decode(ok.Encode());
        Assert.Equal(Status.Ok, decOk.Status);
        Assert.Equal("DepositClaimResponse", decOk.Variant);
        Assert.Null(decOk.AsTransportError());

        RpcResponse err = RpcResponse.TransportError(Status.UnknownServiceOrOp, "no such op");
        RpcResponse decErr = RpcResponse.Decode(err.Encode());
        Assert.Equal(Status.UnknownServiceOrOp, decErr.Status);
        Assert.NotNull(decErr.AsTransportError());
    }

    [Fact]
    public void RpcApplicationErrorIsStatusOk()
    {
        // An application error rides as a typed variant with transport status 0.
        RpcResponse appErr = RpcResponse.Ok("ServiceError", new byte[] { 0xa1, 0x00, 0x01 });
        RpcResponse dec = RpcResponse.Decode(appErr.Encode());
        Assert.Equal(Status.Ok, dec.Status);
        Assert.Equal("ServiceError", dec.Variant);
        Assert.Null(dec.AsTransportError());
    }

    [Fact]
    public void RpcClientServerOverLoopback()
    {
        var req = RpcRequest.New("Echo", "say", new byte[] { 0x63, (byte)'h', (byte)'i' }).WithId(1);
        var serverCarrier = new LoopbackFrameCarrier();
        serverCarrier.PushInbound(req.Encode());
        var server = new RpcServer(serverCarrier);
        bool served = server.ServeOne(r => HandlerOutcome.Reply("SayResponse", r.Payload));
        Assert.True(served);

        byte[] resp = serverCarrier.TakeOutbound()!;
        var clientCarrier = new LoopbackFrameCarrier();
        clientCarrier.PushInbound(resp);
        var client = new RpcClient(clientCarrier, multiplexed: true);
        RpcResponse got = client.Call("Echo", "say", new byte[] { 0x63, (byte)'h', (byte)'i' });
        Assert.Equal("SayResponse", got.Variant);
        Assert.True(new byte[] { 0x63, (byte)'h', (byte)'i' }.AsSpan().SequenceEqual(got.Payload));
    }

    [Fact]
    public void RpcVersionMismatchRejected()
    {
        // Hand-craft an envelope with v=2; decode must reject, not misparse.
        var bad = Cbor.CanonMap(new[]
        {
            new CborEntry(new CborText("v"), new CborUint(2)),
            new CborEntry(new CborText("service"), new CborText("S")),
            new CborEntry(new CborText("op"), new CborText("o")),
            new CborEntry(new CborText("payload"), Conventions.Tag24(new byte[] { 0xa0 })),
        });
        byte[] bytes = Cbor.Encode(bad);
        Assert.Throws<UnsupportedVersionException>(() => RpcRequest.Decode(bytes));
    }

    [Fact]
    public void EventsVerboseRoundTrip()
    {
        Event ev = Event.Verbose("World", "chat", new byte[] { 0x60 }).WithId(3);
        Event dec = Event.Decode(ev.Encode(Profile.Verbose), Profile.Verbose);
        Assert.Equal("World", dec.Service);
        Assert.Equal("chat", dec.EventName);
        Assert.Equal((ulong?)3, dec.Id);
    }

    [Fact]
    public void EventsCompactRoundTripBothShapes()
    {
        Event fire = Event.Compact(1, 0, new byte[] { 0x60 });
        Event decFire = Event.Decode(fire.Encode(Profile.Compact), Profile.Compact);
        Assert.Equal((ulong?)1, decFire.ServiceOrd);
        Assert.Equal((ulong?)0, decFire.OpOrd);
        Assert.Null(decFire.Id);

        Event corr = Event.Compact(1, 2, new byte[] { 0x60 }).WithId(42);
        Event decCorr = Event.Decode(corr.Encode(Profile.Compact), Profile.Compact);
        Assert.Equal((ulong?)42, decCorr.Id);
    }

    [Fact]
    public void EventsHelloNegotiation()
    {
        var hello = new Hello(new ulong[] { 1 }, new[] { "compact", "verbose" })
        {
            Service = "World",
        };
        Hello round = Hello.Decode(hello.Encode());
        var negotiated = round.Negotiate(new[] { Profile.Verbose, Profile.Compact });
        Assert.NotNull(negotiated);
        Assert.Equal((ulong)1, negotiated!.Value.Version);
        Assert.Equal(Profile.Compact, negotiated.Value.Profile);
    }

    [Fact]
    public void EventsHelloNegotiationFailsOnVersion()
    {
        var hello = new Hello(new ulong[] { 99 }, new[] { "verbose" });
        Assert.Null(hello.Negotiate(new[] { Profile.Verbose }));
    }

    [Fact]
    public void EventsControlRoundTrips()
    {
        var ack = new HelloAck(1, "compact") { Session = "s1" };
        HelloAck decAck = HelloAck.Decode(ack.Encode());
        Assert.Equal("compact", decAck.Profile);
        Assert.Equal("s1", decAck.Session);

        var ping = new Heartbeat(9);
        Heartbeat decPing = Heartbeat.Decode(ping.Encode());
        Assert.Equal((ulong)9, decPing.Nonce);
        Assert.Null(decPing.At);

        var close = new Close(Status.VersionUnsupported) { Reason = "bad v" };
        Close decClose = Close.Decode(close.Encode());
        Assert.Equal(Status.VersionUnsupported, decClose.Status);
        Assert.Equal("bad v", decClose.Reason);
    }

    [Fact]
    public void DatagramCborArrayRoundTrip()
    {
        var dg = Datagram.New(0, 5, new byte[] { 0x60 });
        Datagram dec = Datagram.Decode(dg.Encode());
        Assert.Equal((ulong)0, dec.OpOrd);
        Assert.Equal((ulong)5, dec.Seq);

        // Wrong arity rejected.
        byte[] three = Cbor.Encode(new CborArray(new CborValue[]
        {
            new CborUint(1), new CborUint(0), Conventions.Tag24(new byte[] { 0x60 }),
        }));
        Assert.Throws<MalformedException>(() => Datagram.Decode(three));
    }

    [Fact]
    public void DatagramCompactHeaderRoundTrip()
    {
        var dg = CompactDatagram.New(1, 0x1234, new byte[] { 1, 2, 3 });
        byte[] bytes = dg.Encode();
        Assert.Equal(1, bytes[0] >> 4);
        CompactDatagram dec = CompactDatagram.Decode(bytes);
        Assert.Equal((byte)1, dec.OpOrd);
        Assert.Equal((ushort)0x1234, dec.Seq);
        Assert.Null(dec.Epoch);

        CompactDatagram withEpoch = CompactDatagram.New(2, 7, new byte[] { 9 }).WithEpoch(4);
        CompactDatagram decEpoch = CompactDatagram.Decode(withEpoch.Encode());
        Assert.Equal((byte?)4, decEpoch.Epoch);
    }

    [Fact]
    public void SeqTrackerDetectsLossReorderRestart()
    {
        var tr = new SeqTracker();
        Assert.Equal(SeqEventKind.First, tr.Observe(1, 0).Kind);
        SeqEvent adv = tr.Observe(2, 0);
        Assert.Equal(SeqEventKind.Advanced, adv.Kind);
        Assert.Equal((ulong)0, adv.Gap);
        SeqEvent gap = tr.Observe(5, 0);
        Assert.Equal(SeqEventKind.Advanced, gap.Kind);
        Assert.Equal((ulong)2, gap.Gap); // lost 3,4
        Assert.Equal(SeqEventKind.LateOrDuplicate, tr.Observe(3, 0).Kind);
        Assert.Equal(SeqEventKind.Restart, tr.Observe(1, 1).Kind); // epoch bumped
    }

    [Fact]
    public void SeqTrackerUnsequencedNeverLateOrDuplicate()
    {
        // seq 0 is unsequenced and must never be classified as late/duplicate, even repeatedly.
        var tr = new SeqTracker();
        for (int i = 0; i < 3; i++)
        {
            SeqEvent got = tr.Observe(0, null);
            Assert.Equal(SeqEventKind.Advanced, got.Kind);
            Assert.Equal((ulong)0, got.Gap);
        }

        // First epoch arriving where there was no prior epoch is not a restart.
        Assert.Equal(SeqEventKind.First, tr.Observe(1, null).Kind);
        Assert.Equal(SeqEventKind.Advanced, tr.Observe(0, null).Kind);
        Assert.Equal(SeqEventKind.Advanced, tr.Observe(2, null).Kind);
    }

    [Fact]
    public void LengthPrefixFramingOverStream()
    {
        using var buf = new MemoryStream();
        Framing.WriteLengthPrefixed(buf, new byte[] { 1, 2, 3 }, Conventions.MaxFrameDefault);
        buf.Position = 0;
        byte[]? got = Framing.ReadLengthPrefixed(buf, Conventions.MaxFrameDefault);
        Assert.NotNull(got);
        Assert.True(new byte[] { 1, 2, 3 }.AsSpan().SequenceEqual(got));
        // A clean EOF before any frame byte returns no frame.
        Assert.Null(Framing.ReadLengthPrefixed(buf, Conventions.MaxFrameDefault));
    }

    [Fact]
    public void LengthPrefixOverLimitRejected()
    {
        // An over-limit length prefix is rejected before allocating.
        using var buf = new MemoryStream(new byte[] { 0x00, 0x00, 0x10, 0x00 }); // claims 4096
        Assert.Throws<FrameTooLargeException>(() => Framing.ReadLengthPrefixed(buf, 16));
    }

    [Fact]
    public void LengthPrefixHugeLengthRejected()
    {
        // A length with the high bit set would become a negative int and slip past a signed guard;
        // the unsigned comparison must reject it instead.
        using var buf = new MemoryStream(new byte[] { 0xFF, 0xFF, 0xFF, 0xFF }); // ~4 GiB
        Assert.Throws<FrameTooLargeException>(() => Framing.ReadLengthPrefixed(buf, 16));
    }

    [Fact]
    public void DecodeRejectsTrailingBytes()
    {
        var dg = Datagram.New(0, 5, new byte[] { 0x60 });
        byte[] withTrailing = dg.Encode().Concat(new byte[] { 0x00 }).ToArray();
        Assert.Throws<MalformedException>(() => Datagram.Decode(withTrailing));
    }

    [Fact]
    public void NegativeIntOutOfInt64RangeRejected()
    {
        // Major type 1 with an 8-byte argument of all-ones names -2^64, far below the int64 floor;
        // it must be rejected rather than silently wrapping.
        byte[] b = { 0x3B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF };
        Assert.Throws<MalformedException>(() => Cbor.DecodeEnvelope(b));
    }

    [Fact]
    public void LoopbackFrameCarrierRoundTrip()
    {
        using var c = new LoopbackFrameCarrier();
        c.SendFrame(new byte[] { 1, 2, 3 });
        byte[] framed = c.TakeOutbound()!;
        c.PushInbound(framed);
        byte[]? got = c.RecvFrame();
        Assert.NotNull(got);
        Assert.True(new byte[] { 1, 2, 3 }.AsSpan().SequenceEqual(got));
    }
}
