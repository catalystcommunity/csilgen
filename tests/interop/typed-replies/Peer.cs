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

class Carrier : ICsilTransport
{
    public byte[] Call(string service, string op, byte[] request)
    {
        var req = Codec.Decode<Req>(request);
        if (req.A == "unknown") throw new CsilClientException(2, "unknown");
        var reply = Codec.EncodeRpDecryptTokenReply(() => new Handler().DecryptToken(req));
        if (reply.Variant == "RpError" && reply.Payload[6] != 7) throw new System.Exception("wrong CBOR error code");
        return Codec.ResolveRpcReply(service, op, reply.Variant, reply.Payload);
    }
}

class Peer
{
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
    }
}
