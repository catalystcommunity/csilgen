#include <assert.h>
#include "client.gen.h"
#include "server.gen.h"

static int handle(void *ctx, const Req *req, RpDecryptTokenReply *reply) {
    (void)ctx;
    if (strcmp(req->a, "unknown") == 0) return 2;
    if (strcmp(req->a, "limit") == 0) {
        reply->kind = RpDecryptTokenReply_as_rp_error;
        reply->value.as_rp_error = (RpError){ .code = RP_ERROR_CODE_APPLICATION_USER_LIMIT, .message = "limit" };
    } else {
        reply->kind = RpDecryptTokenReply_as_resp;
        reply->value.as_resp.b = "ok";
    }
    return 0;
}

static int call(void *ctx, const char *service, const char *op, const uint8_t *data, size_t len,
                uint8_t **payload, size_t *size, char **variant) {
    assert(strcmp(service, "Rp") == 0 && strcmp(op, "decrypt-token") == 0);
    RpHandlers handlers = { .decrypt_token = handle };
    Req req;
    CsilCodecArena *arena;
    assert(csil_decode_Req(data, len, &req, &arena) == 0);
    RpDecryptTokenReply reply;
    int status = handlers.decrypt_token(ctx, &req, &reply);
    if (!status) {
        const char *wire;
        assert(csil_encode_RpDecryptTokenReply(&reply, &wire, payload, size) == 0);
        if (strcmp(wire, "RpError") == 0) assert(*size > 6 && (*payload)[6] == 7);
        *variant = malloc(strlen(wire) + 1);
        assert(*variant);
        strcpy(*variant, wire);
    }
    csil_codec_arena_free(arena);
    return status;
}

int main(void) {
    CsilgenTransport transport = { .call_reply = call };
    RpDecryptTokenReply reply;
    CsilCodecArena *arena;
    assert(csil_rp_decrypt_token(&transport, &(Req){"ok"}, &reply, &arena) == 0);
    assert(reply.kind == RpDecryptTokenReply_as_resp && strcmp(reply.value.as_resp.b, "ok") == 0);
    csil_codec_arena_free(arena);
    assert(csil_rp_decrypt_token(&transport, &(Req){"limit"}, &reply, &arena) == 0);
    assert(reply.kind == RpDecryptTokenReply_as_rp_error && reply.value.as_rp_error.code == RP_ERROR_CODE_APPLICATION_USER_LIMIT);
    csil_codec_arena_free(arena);
    assert(csil_rp_decrypt_token(&transport, &(Req){"unknown"}, &reply, &arena) == 2);
}
