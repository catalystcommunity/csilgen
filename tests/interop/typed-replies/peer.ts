import { RpErrorCode, RpErrorCodeNumbers } from "./types.gen.ts";
import { RpClient } from "./client.gen.ts";
import { fromReqCbor, toReqCbor, resolveRpcReply, RpErrorRefusal } from "./codec.gen.ts";
import { dispatch } from "./server.gen.ts";

const number = RpErrorCodeNumbers.numberFromEnum(RpErrorCode.ACCOUNT_RATE_LIMITED);
if (number !== 42n || RpErrorCodeNumbers.enumFromNumber(number) !== RpErrorCode.ACCOUNT_RATE_LIMITED)
  throw new Error("incorrect enum number lookup");

async function test() {
  const handlers = { rp: { async decryptToken(req: { a: string }) {
    if (req.a === "limit") throw new RpErrorRefusal({ code: RpErrorCode.APPLICATION_USER_LIMIT, message: "limit" });
    return { b: "ok" };
  } } };
  for (const a of ["ok", "limit", "unknown"]) {
    const reply = await dispatch(handlers, {
      decode<T>(bytes: Uint8Array) { return fromReqCbor(bytes) as T; },
      encode() { throw Error("unexpected generic reply encoder"); },
    }, "Rp", "decrypt-token", toReqCbor({ a }), {});
    if (reply instanceof Uint8Array) throw Error("missing variant");
    if (reply.variant === "RpError" && reply.payload[6] !== 7) throw Error("wrong CBOR error code");
    const client = new RpClient({ call(service, op) {
      if (a === "unknown") throw new RangeError("status 2");
      return resolveRpcReply(service, op, reply.variant, reply.payload);
    } });
    try {
      const success = client.decryptToken({ a });
      if (a !== "ok" || success.b !== "ok") throw Error("wrong success");
    } catch (err) {
      if (a === "limit" && err instanceof RpErrorRefusal && err.value.code === RpErrorCode.APPLICATION_USER_LIMIT) continue;
      if (a === "unknown" && err instanceof RangeError && err.message === "status 2") continue;
      throw err;
    }
  }
}
test().catch(err => { throw err; });
