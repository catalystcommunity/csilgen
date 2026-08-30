// Round-trip and behavioral unit tests, mirroring the Rust reference's
// `src/lib.rs` test module: encode/decode round-trips for every envelope, the
// status-0-application-error vs non-zero-transport-error distinction, the
// client/server loopback drive, version rejection, hello negotiation, sequence
// tracking, and length-prefix framing.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  Status,
  TransportError,
  VERSION,
  bytesToHex,
  canonMap,
  encodeValue,
  hexToBytes,
  intValue,
  tag24,
  textValue,
} from "../src/conventions.ts";
import { encode as cborEncode, decode as cborDecode, int, text, map, array, tag, bytes } from "../src/cbor.ts";
import {
  RpcRequest,
  RpcResponse,
  RpcPush,
  RpcClient,
  RpcServer,
  reply,
} from "../src/rpc.ts";
import { Event, Hello, HelloAck, Heartbeat, Close, control, type Profile } from "../src/events.ts";
import { Datagram, CompactDatagram, SeqTracker } from "../src/datagrams.ts";
import {
  LoopbackFrameCarrier,
  LoopbackDatagramCarrier,
  frameLengthPrefixed,
  LengthPrefixedDeframer,
} from "../src/carrier.ts";

const b = (...vals: number[]) => Uint8Array.from(vals);

test("cbor canonical map sorts keys by encoded bytes", () => {
  // "v" (0x6176) < "id" (0x626964) < "service" (0x67..) — the deterministic order.
  const m = canonMap([
    ["service", textValue("S")],
    ["id", intValue(7)],
    ["v", intValue(1)],
  ]);
  const decoded = cborDecode(encodeValue(m));
  assert.equal(decoded.t, "map");
  if (decoded.t === "map") {
    const keys = decoded.v.map(([k]) => (k.t === "text" ? k.v : "?"));
    assert.deepStrictEqual(keys, ["v", "id", "service"]);
  }
});

test("cbor decoder rejects hostile lengths, depth, and UTF-8", () => {
  const bad = [
    b(0x9b, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00),
    b(0xbb, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00),
    b(0x61, 0xff),
    b(...Array(65).fill(0xc0), 0x00),
  ];
  for (const payload of bad) assert.throws(() => cborDecode(payload));
  assert.doesNotThrow(() => cborDecode(b(...Array(64).fill(0xc0), 0x00)));
});

test("cbor primitives round-trip", () => {
  const values = [
    int(0),
    int(23),
    int(24),
    int(255),
    int(256),
    int(65535),
    int(65536),
    int(-1),
    int(-100),
    text("héllo"),
    bytes(b(0xde, 0xad, 0xbe, 0xef)),
    array([int(1), text("a"), tag(24, bytes(b(0xa0)))]),
    map([
      [text("k"), int(1)],
      [text("z"), int(2)],
    ]),
  ];
  for (const v of values) {
    assert.deepStrictEqual(cborDecode(cborEncode(v)), v);
  }
});

test("cbor decode rejects trailing bytes after the item", () => {
  // An envelope is a single self-contained CBOR item; a trailing byte after a
  // complete item is a framing error, not silently ignored.
  const dg = new Datagram(0, 5, b(0x60));
  const valid = dg.encode();
  const withTrailer = new Uint8Array(valid.length + 1);
  withTrailer.set(valid, 0);
  withTrailer[valid.length] = 0x00; // one extra byte past the item
  assert.throws(() => cborDecode(withTrailer), /trailing bytes after CBOR item/);
  // The clean bytes still decode fine.
  assert.doesNotThrow(() => cborDecode(valid));
});

test("rpc request round-trip", () => {
  const req = new RpcRequest("Attestation", "deposit-claim", b(0xa0)).withId(7);
  assert.deepStrictEqual(RpcRequest.decode(req.encode()), req);
});

test("rpc response success and error", () => {
  const ok = RpcResponse.ok("DepositClaimResponse", b(0xa0)).withId(7);
  const okDecoded = RpcResponse.decode(ok.encode());
  assert.equal(okDecoded.status, Status.Ok);
  assert.equal(okDecoded.variant, "DepositClaimResponse");
  assert.equal(okDecoded.intoTransportError(), okDecoded);

  const err = RpcResponse.transportError(Status.UnknownServiceOrOp, "no such op");
  const errDecoded = RpcResponse.decode(err.encode());
  assert.equal(errDecoded.status, Status.UnknownServiceOrOp);
  assert.throws(() => errDecoded.intoTransportError(), TransportError);
});

test("rpc application error is transport status ok", () => {
  // An application error rides as a typed variant with transport status 0.
  const appErr = RpcResponse.ok("ServiceError", b(0xa1, 0x00, 0x01));
  const decoded = RpcResponse.decode(appErr.encode());
  assert.equal(decoded.status, Status.Ok);
  assert.equal(decoded.variant, "ServiceError");
  // intoTransportError leaves it ok — the caller routes it by variant.
  assert.doesNotThrow(() => decoded.intoTransportError());
});

test("rpc push round-trip", () => {
  const push = new RpcPush("World", "room-delta", b(0xa0));
  assert.deepStrictEqual(RpcPush.decode(push.encode()), push);
});

test("rpc client/server over loopback", async () => {
  // Server replies to a request placed on the loopback's inbound queue, and the
  // response it sends decodes to the echoed payload.
  const req = new RpcRequest("Echo", "say", b(0x63, 0x68, 0x69)).withId(1);
  const carrier = new LoopbackFrameCarrier();
  carrier.pushInbound(req.encode());
  const server = new RpcServer(carrier);
  const served = await server.serveOne((r) => reply("SayResponse", r.payload));
  assert.equal(served, true);

  const respBytes = carrier.takeOutbound();
  assert.ok(respBytes);
  const resp = RpcResponse.decode(respBytes!);
  assert.equal(resp.id, 1);
  assert.equal(resp.variant, "SayResponse");
  assert.deepStrictEqual(resp.payload, req.payload);
});

test("rpc client surfaces non-zero status as transport error", async () => {
  const carrier = new LoopbackFrameCarrier();
  carrier.pushInbound(RpcResponse.transportError(Status.Unavailable, "busy").withId(1).encode());
  const client = new RpcClient(carrier, true);
  await assert.rejects(() => client.call("S", "o", b(0xa0)), (e: unknown) => {
    assert.ok(e instanceof TransportError);
    assert.equal(e.kind, "status");
    assert.equal(e.code, Status.Unavailable);
    return true;
  });
  // The request it sent carried a correlation id (multiplexed client).
  const sent = RpcRequest.decode(carrier.takeOutbound()!);
  assert.equal(sent.id, 1);
});

test("rpc version mismatch rejected", () => {
  // Hand-craft an envelope with v=2; decode must reject, not misparse.
  const bad = encodeValue(
    map([
      [text("v"), int(2)],
      [text("service"), text("S")],
      [text("op"), text("o")],
      [text("payload"), tag24(b(0xa0))],
    ]),
  );
  assert.throws(() => RpcRequest.decode(bad), TransportError);
});

test("events verbose round-trip", () => {
  const ev = Event.verbose("World", "chat", b(0x60)).withId(3);
  assert.deepStrictEqual(Event.decode(ev.encode("verbose"), "verbose"), ev);
});

test("events compact round-trip both shapes", () => {
  const fire = Event.compact(1, 0, b(0x60));
  assert.deepStrictEqual(Event.decode(fire.encode("compact"), "compact"), fire);

  const corr = Event.compact(1, 2, b(0x60)).withId(42);
  const decoded = Event.decode(corr.encode("compact"), "compact");
  assert.equal(decoded.id, 42);
  assert.deepStrictEqual(decoded, corr);
});

test("events wrong-arity compact array rejected", () => {
  const five = cborEncode(array([int(1), int(0), int(0), int(0), tag24(b(0x60))]));
  // A 5-element array is neither the 3- nor 4-element compact shape.
  assert.throws(() => Event.decode(five, "compact"), TransportError);
});

test("events hello negotiation", () => {
  const hello = new Hello([1], ["compact", "verbose"], "World");
  const round = Hello.decode(hello.encode());
  assert.deepStrictEqual(round, hello);
  const chosen = round.negotiate(["verbose", "compact"]);
  assert.deepStrictEqual(chosen, [1, "compact"]); // peer prefers compact, server supports it
});

test("events hello negotiation fails on version", () => {
  const hello = new Hello([99], ["verbose"]);
  assert.equal(hello.negotiate(["verbose"]), undefined);
});

test("events control payloads round-trip", () => {
  const ack = new HelloAck(VERSION, "compact", "s1");
  assert.deepStrictEqual(HelloAck.decode(ack.encode()), ack);
  const ping = new Heartbeat(9);
  assert.deepStrictEqual(Heartbeat.decode(ping.encode()), ping);
  const close = new Close(Status.VersionUnsupported, "bad v");
  assert.deepStrictEqual(Close.decode(close.encode()), close);
  // Control op ordinals and $-sigil names are as the spec assigns them.
  assert.equal(control.HELLO, 0);
  assert.equal(control.ERROR, 5);
  assert.equal(control.HELLO_ACK_NAME, "$hello-ack");
});

test("datagram cbor-array round-trip and arity guard", () => {
  const dg = new Datagram(0, 5, b(0x60));
  assert.deepStrictEqual(Datagram.decode(dg.encode()), dg);
  // A 3-element array is rejected.
  const three = cborEncode(array([int(1), int(0), tag24(b(0x60))]));
  assert.throws(() => Datagram.decode(three), TransportError);
});

test("datagram compact-header round-trip", () => {
  const dg = new CompactDatagram(1, 0x1234, b(1, 2, 3));
  const enc = dg.encode();
  assert.equal(enc[0] >> 4, 1); // version nibble
  assert.deepStrictEqual(CompactDatagram.decode(enc), dg);

  const withEpoch = new CompactDatagram(2, 7, b(9)).withEpoch(4);
  const decoded = CompactDatagram.decode(withEpoch.encode());
  assert.equal(decoded.epoch, 4);
  assert.deepStrictEqual(decoded, withEpoch);
});

test("datagram compact-header rejects unknown version", () => {
  const enc = new CompactDatagram(1, 1, b(0)).encode();
  enc[0] = (2 << 4) | (enc[0] & 0x0f); // bump version nibble to 2
  assert.throws(() => CompactDatagram.decode(enc), TransportError);
});

test("seq tracker detects loss reorder restart", () => {
  const t = new SeqTracker();
  assert.deepStrictEqual(t.observe(1, 0), { kind: "first" });
  assert.deepStrictEqual(t.observe(2, 0), { kind: "advanced", gap: 0 });
  assert.deepStrictEqual(t.observe(5, 0), { kind: "advanced", gap: 2 }); // lost 3,4
  assert.deepStrictEqual(t.observe(3, 0), { kind: "late-or-duplicate" });
  assert.deepStrictEqual(t.observe(1, 1), { kind: "restart" }); // epoch bumped
});

test("seq tracker treats unsequenced seq 0 as always advanced", () => {
  // seq 0 means "unsequenced": it carries no ordering, so it is never late or
  // duplicate. Repeated observe(0) all advance with a zero gap, and the running
  // sequence stays untouched alongside the sequenced stream.
  const t = new SeqTracker();
  assert.deepStrictEqual(t.observe(0, 0), { kind: "advanced", gap: 0 });
  assert.deepStrictEqual(t.observe(0, 0), { kind: "advanced", gap: 0 });
  assert.deepStrictEqual(t.observe(0, 0), { kind: "advanced", gap: 0 });

  // Mixed with sequenced traffic, seq 0 does not disturb the tracked sequence.
  const mixed = new SeqTracker();
  assert.deepStrictEqual(mixed.observe(1, 0), { kind: "first" });
  assert.deepStrictEqual(mixed.observe(0, 0), { kind: "advanced", gap: 0 });
  assert.deepStrictEqual(mixed.observe(2, 0), { kind: "advanced", gap: 0 });
});

test("length-prefix framing round-trips and guards size", () => {
  const framed = frameLengthPrefixed(b(1, 2, 3));
  assert.equal(bytesToHex(framed.subarray(0, 4)), "00000003");

  const deframer = new LengthPrefixedDeframer();
  // Feed the frame in two chunks to exercise the incremental buffering.
  deframer.push(framed.subarray(0, 2));
  assert.equal(deframer.next(), null);
  deframer.push(framed.subarray(2));
  assert.deepStrictEqual(deframer.next(), b(1, 2, 3));
  assert.equal(deframer.next(), null);

  // Over-limit length prefix is rejected before allocating its body.
  assert.throws(() => frameLengthPrefixed(b(0, 0), 1), TransportError);
  const hostile = new LengthPrefixedDeframer(8);
  hostile.push(b(0xff, 0xff, 0xff, 0xff)); // claims ~4 GiB
  assert.throws(() => hostile.next(), TransportError);
});

test("datagram loopback carrier round-trips a datagram", () => {
  const carrier = new LoopbackDatagramCarrier();
  const dg = new Datagram(1, 3, b(0xa0));
  carrier.sendDatagram(dg.encode());
  const sent = carrier.takeOutbound();
  assert.ok(sent);
  carrier.pushInbound(sent!);
  const received = carrier.recvDatagram();
  assert.ok(received);
  assert.deepStrictEqual(Datagram.decode(received!), dg);
});
