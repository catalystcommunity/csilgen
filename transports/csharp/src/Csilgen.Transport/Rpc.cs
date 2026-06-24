// CSIL-RPC transport -- request/response/push envelopes -- see csil-rpc-transport.md.

namespace Csilgen.Transport;

// RpcRequest is a CSIL-RPC request (client -> server). Payload is the opaque CBOR(request type)
// bytes, wrapped in tag 24 on the wire.
public sealed record RpcRequest(string Service, string Op, byte[] Payload)
{
    public ulong? Id { get; init; }

    public string? Auth { get; init; }

    // New builds a request with no correlation id and no per-request auth.
    public static RpcRequest New(string service, string op, byte[] payload) =>
        new(service, op, payload);

    // WithId sets the correlation id (required on multiplexed carriers).
    public RpcRequest WithId(ulong id) => this with { Id = id };

    // WithAuth sets the per-request credential for caller-scoped operations.
    public RpcRequest WithAuth(string auth) => this with { Auth = auth };

    public byte[] Encode()
    {
        var entries = new List<CborEntry>
        {
            new(new CborText("v"), new CborUint(Conventions.Version)),
            new(new CborText("service"), new CborText(Service)),
            new(new CborText("op"), new CborText(Op)),
            new(new CborText("payload"), Conventions.Tag24(Payload)),
        };
        if (Id is ulong id)
        {
            entries.Add(new CborEntry(new CborText("id"), new CborUint(id)));
        }

        if (Auth is string auth)
        {
            entries.Add(new CborEntry(new CborText("auth"), new CborText(auth)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static RpcRequest Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        Conventions.CheckVersion(Conventions.GetUint(v, "v"));
        CborValue? p = Conventions.MapGet(v, "payload")
            ?? throw new MalformedException("missing 'payload'");
        return new RpcRequest(
            Conventions.GetText(v, "service"),
            Conventions.GetText(v, "op"),
            Conventions.Untag24(p))
        {
            Id = Conventions.GetUintOpt(v, "id"),
            Auth = Conventions.GetTextOpt(v, "auth"),
        };
    }
}

// RpcResponse is a CSIL-RPC response (server -> client). Variant names which declared output-choice
// arm Payload decodes to (the CSIL type name). Payload is the opaque CBOR(output type) bytes; it is
// a present-but-empty byte string when Status is non-zero.
public sealed record RpcResponse(Status Status, byte[] Payload)
{
    public ulong? Id { get; init; }

    public string? Variant { get; init; }

    public string? Error { get; init; }

    // Ok builds a successful (Status ok) typed reply.
    public static RpcResponse Ok(string variant, byte[] payload) =>
        new(Status.Ok, payload) { Variant = variant };

    // TransportError builds a transport-level failure (no typed payload).
    public static RpcResponse TransportError(Status status, string message) =>
        new(status, Array.Empty<byte>()) { Error = message };

    // WithId sets the echoed correlation id.
    public RpcResponse WithId(ulong? id) => this with { Id = id };

    public byte[] Encode()
    {
        var entries = new List<CborEntry>
        {
            new(new CborText("v"), new CborUint(Conventions.Version)),
            new(new CborText("status"), new CborInt(Status.Code)),
            new(new CborText("payload"), Conventions.Tag24(Payload)),
        };
        if (Id is ulong id)
        {
            entries.Add(new CborEntry(new CborText("id"), new CborUint(id)));
        }

        if (Variant is string variant)
        {
            entries.Add(new CborEntry(new CborText("variant"), new CborText(variant)));
        }

        if (Error is string error)
        {
            entries.Add(new CborEntry(new CborText("error"), new CborText(error)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static RpcResponse Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        Conventions.CheckVersion(Conventions.GetUint(v, "v"));

        // payload is present but may be an empty byte string on error.
        byte[] payload = Array.Empty<byte>();
        if (Conventions.MapGet(v, "payload") is CborValue p)
        {
            payload = Conventions.Untag24(p);
        }

        return new RpcResponse(Status.FromCode((int)Conventions.GetInt(v, "status")), payload)
        {
            Id = Conventions.GetUintOpt(v, "id"),
            Variant = Conventions.GetTextOpt(v, "variant"),
            Error = Conventions.GetTextOpt(v, "error"),
        };
    }

    // AsTransportError returns a StatusException for a non-ok response, or null when the response
    // carries a typed reply (status 0). Callers use this after decode to surface transport failures
    // distinctly from application errors.
    public StatusException? AsTransportError() =>
        Status.IsOk ? null : new StatusException(Status, Error);
}

// RpcPush is a CSIL-RPC server push (server -> client) for `<-` operations.
public sealed record RpcPush(string Service, string Event, byte[] Payload)
{
    public static RpcPush New(string service, string @event, byte[] payload) =>
        new(service, @event, payload);

    public byte[] Encode()
    {
        var entries = new List<CborEntry>
        {
            new(new CborText("v"), new CborUint(Conventions.Version)),
            new(new CborText("service"), new CborText(Service)),
            new(new CborText("event"), new CborText(Event)),
            new(new CborText("payload"), Conventions.Tag24(Payload)),
        };
        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static RpcPush Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        Conventions.CheckVersion(Conventions.GetUint(v, "v"));
        CborValue? p = Conventions.MapGet(v, "payload")
            ?? throw new MalformedException("missing 'payload'");
        return new RpcPush(
            Conventions.GetText(v, "service"),
            Conventions.GetText(v, "event"),
            Conventions.Untag24(p));
    }
}

// HandlerOutcome is what a server handler returns for one request: a typed reply (variant name +
// encoded payload) on success, or a transport status on failure.
public sealed record HandlerOutcome
{
    private HandlerOutcome()
    {
    }

    // IsReply distinguishes a typed reply from a transport-status outcome.
    public bool IsReply { get; private init; }

    public string Variant { get; private init; } = string.Empty;

    public byte[] Payload { get; private init; } = Array.Empty<byte>();

    public Status Status { get; private init; }

    public string Message { get; private init; } = string.Empty;

    // Reply builds a successful handler outcome.
    public static HandlerOutcome Reply(string variant, byte[] payload) =>
        new() { IsReply = true, Variant = variant, Payload = payload };

    // Transport builds a transport-status handler outcome.
    public static HandlerOutcome Transport(Status status, string message) =>
        new() { Status = status, Message = message };
}

// RpcClient is a CSIL-RPC client over a frame carrier. The carrier is injected (bring your own); the
// client owns the envelope and a per-connection monotonic correlation id.
public sealed class RpcClient
{
    private readonly IFrameCarrier _carrier;
    private readonly bool _multiplexed;
    private ulong _nextId = 1;

    // multiplexed true assigns a correlation id to every request (required on WS / pipelined
    // streams); false omits it (one-in-flight carriers such as HTTP).
    public RpcClient(IFrameCarrier carrier, bool multiplexed)
    {
        _carrier = carrier;
        _multiplexed = multiplexed;
    }

    public IFrameCarrier Carrier => _carrier;

    // Call invokes service/op with an encoded request payload, returning the decoded response. A
    // non-zero transport status is surfaced as a StatusException.
    public RpcResponse Call(string service, string op, byte[] payload, string? auth = null)
    {
        var req = RpcRequest.New(service, op, payload) with { Auth = auth };
        if (_multiplexed)
        {
            req = req with { Id = _nextId };
            _nextId++;
        }

        _carrier.SendFrame(req.Encode());
        byte[]? incoming = _carrier.RecvFrame()
            ?? throw new CarrierException("connection closed before response");
        RpcResponse resp = RpcResponse.Decode(incoming);
        StatusException? err = resp.AsTransportError();
        if (err is not null)
        {
            throw err;
        }

        return resp;
    }
}

// RpcServer is a CSIL-RPC server over a frame carrier. The host supplies a handler mapping
// (service, op, request-payload) to an outcome; the generated router is the natural implementation
// of that handler.
public sealed class RpcServer
{
    private readonly IFrameCarrier _carrier;

    public RpcServer(IFrameCarrier carrier) => _carrier = carrier;

    // ServeOne reads one request, dispatches it through handler, and writes the response. It returns
    // false at a clean end of stream.
    public bool ServeOne(Func<RpcRequest, HandlerOutcome> handler)
    {
        byte[]? frame = _carrier.RecvFrame();
        if (frame is null)
        {
            return false;
        }

        RpcResponse resp;
        try
        {
            RpcRequest req = RpcRequest.Decode(frame);
            HandlerOutcome outcome = handler(req);
            resp = outcome.IsReply
                ? RpcResponse.Ok(outcome.Variant, outcome.Payload).WithId(req.Id)
                : RpcResponse.TransportError(outcome.Status, outcome.Message).WithId(req.Id);
        }
        catch (TransportException ex)
        {
            resp = RpcResponse.TransportError(Status.MalformedEnvelope, ex.Message);
        }

        _carrier.SendFrame(resp.Encode());
        return true;
    }
}
