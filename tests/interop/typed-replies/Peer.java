package csilgen.generated;

import community.catalyst.csilgen.transport.Status;
import community.catalyst.csilgen.transport.StatusException;

import static csilgen.generated.CsilCbor.*;

public class Peer {
    public static void main(String[] args) throws Exception {
        assert RpErrorCode.ACCOUNT_RATE_LIMITED.toNumber() == 42;
        assert RpErrorCode.fromNumber(42) == RpErrorCode.ACCOUNT_RATE_LIMITED;
        Rp handler = req -> {
            if ("limit".equals(req.a())) {
                throw new RpErrorRefusal(new RpError(RpErrorCode.APPLICATION_USER_LIMIT, "limit"));
            }
            return new Resp("ok");
        };
        RpClient client = new RpClient((service, op, bytes) -> {
            Req req = decodeReq(bytes);
            if ("unknown".equals(req.a())) throw new StatusException(Status.UNKNOWN_SERVICE_OR_OP, "unknown");
            RpcReply reply = encodeRpDecryptTokenReply(() -> handler.decryptToken(req));
            if ("RpError".equals(reply.variant())) assert reply.payload()[6] == 7;
            return resolveRpcReply(service, op, reply.variant(), reply.payload());
        });
        assert "ok".equals(client.decryptToken(new Req("ok")).b());
        try {
            client.decryptToken(new Req("limit"));
            throw new AssertionError("missing refusal");
        } catch (RpErrorRefusal e) {
            assert e.value.code() == RpErrorCode.APPLICATION_USER_LIMIT;
        }
        try {
            client.decryptToken(new Req("unknown"));
            throw new AssertionError("missing transport error");
        } catch (StatusException e) {
            assert e.status.code() == 2;
        }
    }
}
