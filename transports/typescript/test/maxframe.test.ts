// The configurable max-frame guard (conventions doc §5): a host sets the limit up
// or down where the framing is configured, the limit applies to outbound frames and
// inbound length prefixes alike, an oversized prefix is rejected before its body is
// buffered, and an invalid limit fails where it is configured rather than on the
// first frame.
//
// TypeScript has no stateful stream carrier (a host wires its own WebSocket /
// WebTransport / TCP source), so the knob lives on `frameLengthPrefixed` for the
// write side and on the `LengthPrefixedDeframer` constructor for the read side.

import { test } from "node:test";
import assert from "node:assert/strict";

import { frameLengthPrefixed, LengthPrefixedDeframer } from "../src/carrier.ts";
import { MAX_FRAME_DEFAULT, MAX_FRAME_LIMIT, TransportError } from "../src/conventions.ts";

// A 4-byte big-endian prefix for `len`, used to feed the deframer a claim without a body.
function prefix(len: number): Uint8Array {
  const out = new Uint8Array(4);
  new DataView(out.buffer).setUint32(0, len, false);
  return out;
}

test("default limit accepts a frame below it", () => {
  const body = new Uint8Array(1024).fill(0xab);
  const framed = frameLengthPrefixed(body);
  assert.equal(framed.length, 4 + body.length);

  const deframer = new LengthPrefixedDeframer();
  deframer.push(framed);
  assert.deepEqual(deframer.next(), body);
});

test("default limit rejects a frame above it", () => {
  const body = new Uint8Array(MAX_FRAME_DEFAULT + 1);
  assert.throws(() => frameLengthPrefixed(body), (e: unknown) => {
    assert.ok(e instanceof TransportError);
    assert.equal(e.kind, "frame-too-large");
    return true;
  });
});

test("a larger custom limit accepts what the default rejects", () => {
  const body = new Uint8Array(MAX_FRAME_DEFAULT + 1);
  const framed = frameLengthPrefixed(body, MAX_FRAME_DEFAULT + 4096);
  assert.equal(framed.length, 4 + body.length);

  const deframer = new LengthPrefixedDeframer(MAX_FRAME_DEFAULT + 4096);
  deframer.push(framed);
  assert.equal(deframer.next()?.length, body.length);
});

test("a smaller custom limit rejects what the default accepts", () => {
  const body = new Uint8Array(1024).fill(0xcd);
  assert.throws(() => frameLengthPrefixed(body, 64), (e: unknown) => {
    assert.ok(e instanceof TransportError);
    assert.equal(e.kind, "frame-too-large");
    return true;
  });
});

test("an oversized incoming length is rejected before its body is buffered", () => {
  // Only the 4-byte prefix is pushed — no body at all. The guard must fire on the
  // claim itself rather than waiting for bytes that will never arrive.
  const deframer = new LengthPrefixedDeframer(4096);
  deframer.push(prefix(0xffffffff));
  assert.throws(() => deframer.next(), (e: unknown) => {
    assert.ok(e instanceof TransportError);
    assert.equal(e.kind, "frame-too-large");
    return true;
  });
});

test("invalid limits are rejected where they are configured", () => {
  const invalid = [0, -1, -4096, MAX_FRAME_LIMIT + 1, 1.5, NaN, Infinity];
  for (const limit of invalid) {
    assert.throws(
      () => new LengthPrefixedDeframer(limit),
      (e: unknown) => {
        assert.ok(e instanceof TransportError);
        assert.equal(e.kind, "invalid-max-frame");
        return true;
      },
      `deframer limit ${limit} must be rejected`,
    );
    assert.throws(
      () => frameLengthPrefixed(new Uint8Array(1), limit),
      (e: unknown) => {
        assert.ok(e instanceof TransportError);
        assert.equal(e.kind, "invalid-max-frame");
        return true;
      },
      `framing limit ${limit} must be rejected`,
    );
  }
});

test("boundary limits are accepted", () => {
  for (const limit of [1, MAX_FRAME_DEFAULT, MAX_FRAME_LIMIT]) {
    assert.equal(new LengthPrefixedDeframer(limit).maxFrame, limit);
  }
});
