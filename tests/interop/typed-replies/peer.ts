import { RpErrorCode, RpErrorCodeNumbers } from "./types.gen.ts";
import { RpClient } from "./client.gen.ts";
import { RpAsyncClient } from "./client.async.gen.ts";
import { fromReqCbor, toReqCbor, toRespCbor, RpErrorRefusal } from "./codec.gen.ts";
import { dispatch } from "./server.gen.ts";

const number = RpErrorCodeNumbers.numberFromEnum(RpErrorCode.ACCOUNT_RATE_LIMITED);
if (number !== 42n || RpErrorCodeNumbers.enumFromNumber(number) !== RpErrorCode.ACCOUNT_RATE_LIMITED)
  throw new Error("incorrect enum number lookup");

type WireReply = { variant?: string | null; payload: Uint8Array };

// A fake carrier: the reply variant reaches the client only through `callReply`.
function carrier(answer: () => WireReply) {
  return {
    call(): Uint8Array { throw Error("a multi-arm op must not use call"); },
    callReply(service: string, op: string): WireReply {
      if (service !== "Rp" || op !== "decrypt-token") throw Error(`wrong route ${service}/${op}`);
      return answer();
    },
  };
}

function asyncCarrier(answer: () => WireReply) {
  const sync = carrier(answer);
  return {
    async call(): Promise<Uint8Array> { return sync.call(); },
    async callReply(service: string, op: string): Promise<WireReply> { return sync.callReply(service, op); },
  };
}

async function outcome(answer: () => WireReply): Promise<string[]> {
  const results: string[] = [];
  const record = (run: () => { b: string }) => {
    try {
      results.push(`ok:${run().b}`);
    } catch (err) {
      if (err instanceof RpErrorRefusal) results.push(`refusal:${err.value.code}`);
      else if (err instanceof RangeError) results.push(`transport:${err.message}`);
      else results.push("error");
    }
  };
  record(() => new RpClient(carrier(answer)).decryptToken({ a: "x" }));
  try {
    results.push(`ok:${(await new RpAsyncClient(asyncCarrier(answer)).decryptToken({ a: "x" })).b}`);
  } catch (err) {
    if (err instanceof RpErrorRefusal) results.push(`refusal:${err.value.code}`);
    else if (err instanceof RangeError) results.push(`transport:${err.message}`);
    else results.push("error");
  }
  return results;
}

function expectBoth(got: string[], want: string, label: string) {
  if (got.length !== 2 || got[0] !== want || got[1] !== want) {
    throw Error(`${label}: got ${JSON.stringify(got)}, want ${want} from the sync and async clients`);
  }
}

async function test() {
  const handlers = { rp: { async decryptToken(req: { a: string }) {
    if (req.a === "limit") throw new RpErrorRefusal({ code: RpErrorCode.APPLICATION_USER_LIMIT, message: "limit" });
    return { b: "ok" };
  } } };
  const codec = {
    decode<T>(bytes: Uint8Array) { return fromReqCbor(bytes) as T; },
    encode(): Uint8Array { throw Error("unexpected generic reply encoder"); },
  };
  for (const [a, want] of [["ok", "ok:ok"], ["limit", `refusal:${RpErrorCode.APPLICATION_USER_LIMIT}`]]) {
    const reply = await dispatch(handlers, codec, "Rp", "decrypt-token", toReqCbor({ a }), {});
    if (reply instanceof Uint8Array) throw Error("missing variant");
    if (reply.variant === "RpError" && reply.payload[6] !== 7) throw Error("wrong CBOR error code");
    expectBoth(await outcome(() => reply), want, `server reply for ${a}`);
  }
  const resp = toRespCbor({ b: "ok" });
  expectBoth(await outcome(() => ({ variant: "Resp", payload: resp })), "ok:ok", "variant Resp");
  expectBoth(await outcome(() => ({ variant: "Other", payload: resp })), "error", "variant Other");
  expectBoth(await outcome(() => ({ payload: resp })), "error", "no variant");
  expectBoth(await outcome(() => ({ variant: null, payload: resp })), "error", "null variant");
  expectBoth(
    await outcome(() => { throw new RangeError("status 2"); }),
    "transport:status 2",
    "transport error",
  );
}
test().catch(err => { throw err; });
