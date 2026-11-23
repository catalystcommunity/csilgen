#nullable enable
using Csilgen.Transport;

class Handler : IRpService
{
    public Resp DecryptToken(Req req)
    {
        if (req.A == "limit")
            throw new Codec.RpErrorRefusal(new RpError { Code = RpErrorCode.ApplicationUserLimit, Message = "limit" });
        return new Resp { B = "ok" };
    }
}

// A fake carrier: it answers with the variant the generated server encoder picks, unless a
// variant is forced; the request "none" gets a reply with no variant.
class Carrier(string? forced = null) : ICsilTransport, ICsilAsyncTransport
{
    public byte[] Call(string service, string op, byte[] request) =>
        throw new System.Exception("a multi-arm op must use CallReply");

    public CsilWireReply CallReply(string service, string op, byte[] request)
    {
        if (service != "Rp" || op != "decrypt-token") throw new System.Exception("wrong wire names");
        var req = Codec.Decode<Req>(request);
        if (req.A == "unknown") throw new CsilClientException(2, "unknown");
        var reply = Codec.EncodeRpDecryptTokenReply(() => new Handler().DecryptToken(req));
        if (reply.Variant == "RpError" && reply.Payload[6] != 7) throw new System.Exception("wrong CBOR error code");
        if (req.A == "none") return new CsilWireReply(null, reply.Payload);
        return new CsilWireReply(forced ?? reply.Variant, reply.Payload);
    }

    System.Threading.Tasks.Task<byte[]> ICsilAsyncTransport.Call(string service, string op, byte[] request) =>
        System.Threading.Tasks.Task.FromResult(Call(service, op, request));

    System.Threading.Tasks.Task<CsilWireReply> ICsilAsyncTransport.CallReply(string service, string op, byte[] request) =>
        System.Threading.Tasks.Task.FromResult(CallReply(service, op, request));
}

class Peer
{
    static void Expect<T>(System.Action call, string what) where T : System.Exception
    {
        try { call(); }
        catch (T) { return; }
        throw new System.Exception(what);
    }

    static void Main()
    {
        long number = RpErrorCode.AccountRateLimited.ToNumber();
        if (number != 42 || RpErrorCodeNumbers.FromNumber(number) != RpErrorCode.AccountRateLimited)
            throw new System.Exception("incorrect enum number lookup");
        var client = new RpClient(new Carrier());
        if (client.DecryptToken(new Req { A = "ok" }).B != "ok") throw new System.Exception("wrong response");
        try {
            client.DecryptToken(new Req { A = "limit" });
            throw new System.Exception("missing refusal");
        } catch (Codec.RpErrorRefusal e) when (e.Value.Code == RpErrorCode.ApplicationUserLimit) { }
        try {
            client.DecryptToken(new Req { A = "unknown" });
            throw new System.Exception("missing transport error");
        } catch (CsilClientException e) when (e.Code == 2) { }
        Expect<CborException>(() => client.DecryptToken(new Req { A = "none" }), "missing variant accepted");
        Expect<CborException>(() => new RpClient(new Carrier("Other")).DecryptToken(new Req { A = "ok" }), "unknown variant accepted");

        var asyncClient = new RpAsyncClient(new Carrier());
        if (asyncClient.DecryptTokenAsync(new Req { A = "ok" }).GetAwaiter().GetResult().B != "ok")
            throw new System.Exception("wrong async response");
        Expect<Codec.RpErrorRefusal>(() => asyncClient.DecryptTokenAsync(new Req { A = "limit" }).GetAwaiter().GetResult(), "missing async refusal");
        Expect<CborException>(() => new RpAsyncClient(new Carrier("Other")).DecryptTokenAsync(new Req { A = "ok" }).GetAwaiter().GetResult(), "unknown async variant accepted");
        System.Console.WriteLine("csharp typed replies: ok");
    }
}
