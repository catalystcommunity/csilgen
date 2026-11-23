package csilgen.generated;

import community.catalyst.csilgen.transport.Status;
import community.catalyst.csilgen.transport.StatusException;

import static csilgen.generated.CsilCbor.*;

public class Peer {
    // A fake carrier: it answers every call with the given variant, or with the variant the
    // generated server encoder picks when that is null.
    private static RpClient client(Rp handler, String forced) {
        return new RpClient(new Transport() {
            @Override
            public byte[] call(String service, String op, byte[] bytes) {
                throw new AssertionError("a multi-arm op must use callReply");
            }

            @Override
            public WireReply callReply(String service, String op, byte[] bytes) {
                assert "Rp".equals(service) && "decrypt-token".equals(op);
                Req req = decodeReq(bytes);
                if ("unknown".equals(req.a())) throw new StatusException(Status.UNKNOWN_SERVICE_OR_OP, "unknown");
                RpcReply reply = encodeRpDecryptTokenReply(() -> handler.decryptToken(req));
                if ("RpError".equals(reply.variant())) assert reply.payload()[6] == 7;
                if ("none".equals(req.a())) return new WireReply(null, reply.payload());
                return new WireReply(forced == null ? reply.variant() : forced, reply.payload());
            }
        });
    }

    public static void main(String[] args) throws Exception {
        assert RpErrorCode.ACCOUNT_RATE_LIMITED.toNumber() == 42;
        assert RpErrorCode.fromNumber(42) == RpErrorCode.ACCOUNT_RATE_LIMITED;
        Rp handler = req -> {
            if ("limit".equals(req.a())) {
                throw new RpErrorRefusal(new RpError(RpErrorCode.APPLICATION_USER_LIMIT, "limit"));
            }
            return new Resp("ok");
        };
        RpClient client = client(handler, null);
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
        try {
            client.decryptToken(new Req("none"));
            throw new AssertionError("missing variant accepted");
        } catch (CsilCborException e) {
            // A reply without a variant cannot select an arm.
        }
        try {
            client(handler, "Other").decryptToken(new Req("ok"));
            throw new AssertionError("unknown variant accepted");
        } catch (CsilCborException e) {
            // An undeclared variant is a decode error.
        }
        System.out.println("java typed replies: ok");
    }
}
