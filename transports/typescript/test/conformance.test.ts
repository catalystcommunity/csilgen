// Verify the TypeScript reference library against the checked-in conformance
// vectors — the normative, language-neutral byte-exact contract. This is a
// faithful port of `transports/rust/tests/conformance.rs`: for every vector,
// reconstruct the envelope from its `input`, assert encode → `hex`, and assert
// decode(`hex`) → an equal envelope. The vectors win; if our bytes differ, our
// code is wrong.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { bytesToHex, hexToBytes, Status } from "../src/conventions.ts";
import { RpcRequest, RpcResponse, RpcPush } from "../src/rpc.ts";
import { Event, Hello, HelloAck, Heartbeat, Close, type Profile } from "../src/events.ts";
import { Datagram, CompactDatagram } from "../src/datagrams.ts";

interface Vector {
  name: string;
  description: string;
  hex: string;
  // language-neutral fields; shape varies per transport, hence `any`.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  input: Record<string, any>;
}

function load(name: string): Vector[] {
  const path = join(import.meta.dirname, "..", "..", "conformance", name);
  const doc = JSON.parse(readFileSync(path, "utf8")) as { vectors: Vector[] };
  return doc.vectors;
}

// null → undefined, so a missing optional field is the absence of a value rather
// than a present null. Mirrors how the Rust `opt_*` helpers map JSON null.
function opt<T>(v: T | null | undefined): T | undefined {
  return v === null || v === undefined ? undefined : v;
}

test("rpc vectors", () => {
  for (const vec of load("rpc.json")) {
    const { name, input } = vec;
    const payload = () => hexToBytes(input.payload_hex as string);
    let actual: Uint8Array;
    switch (input.kind) {
      case "request": {
        const r = new RpcRequest(input.service as string, input.op as string, payload());
        r.id = opt<number>(input.id);
        r.auth = opt<string>(input.auth);
        actual = r.encode();
        assert.deepStrictEqual(RpcRequest.decode(actual), r, `decode ${name}`);
        break;
      }
      case "response": {
        const r = new RpcResponse(input.status as number, payload());
        r.id = opt<number>(input.id);
        r.variant = opt<string>(input.variant);
        r.error = opt<string>(input.error);
        actual = r.encode();
        assert.deepStrictEqual(RpcResponse.decode(actual), r, `decode ${name}`);
        break;
      }
      case "push": {
        const p = new RpcPush(input.service as string, input.event as string, payload());
        actual = p.encode();
        assert.deepStrictEqual(RpcPush.decode(actual), p, `decode ${name}`);
        break;
      }
      default:
        throw new Error(`unknown rpc kind ${input.kind}`);
    }
    assert.equal(bytesToHex(actual), vec.hex, `encode ${name}`);
  }
});

test("events vectors", () => {
  for (const vec of load("events.json")) {
    const { name, input } = vec;

    // Control payloads are keyed by "control"; application events by "profile".
    if (typeof input.control === "string") {
      let bytes: Uint8Array;
      switch (input.control) {
        case "hello": {
          const h = new Hello(
            (input.versions as number[]).slice(),
            (input.profiles as string[]).slice(),
            opt<string>(input.service),
            opt<string>(input.auth),
          );
          bytes = h.encode();
          assert.deepStrictEqual(Hello.decode(bytes), h, `decode ${name}`);
          break;
        }
        case "hello_ack": {
          const h = new HelloAck(input.v as number, input.profile as string, opt<string>(input.session));
          bytes = h.encode();
          assert.deepStrictEqual(HelloAck.decode(bytes), h, `decode ${name}`);
          break;
        }
        case "ping": {
          const h = new Heartbeat(input.nonce as number, opt<number>(input.at));
          bytes = h.encode();
          assert.deepStrictEqual(Heartbeat.decode(bytes), h, `decode ${name}`);
          break;
        }
        case "close": {
          const c = new Close(input.status as number, opt<string>(input.reason));
          bytes = c.encode();
          assert.deepStrictEqual(Close.decode(bytes), c, `decode ${name}`);
          break;
        }
        default:
          throw new Error(`unknown control ${input.control}`);
      }
      assert.equal(bytesToHex(bytes), vec.hex, `encode ${name}`);
      continue;
    }

    const profile = input.profile as Profile;
    const payload = hexToBytes(input.payload_hex as string);
    let ev: Event;
    if (profile === "verbose") {
      ev = Event.verbose(opt<string>(input.service), input.event as string, payload);
      ev.id = opt<number>(input.id);
    } else {
      ev = Event.compact(input.service_ord as number, input.op_ord as number, payload);
      const id = opt<number>(input.id);
      if (id !== undefined) ev.id = id;
    }
    const bytes = ev.encode(profile);
    assert.equal(bytesToHex(bytes), vec.hex, `encode ${name}`);
    assert.deepStrictEqual(Event.decode(bytes, profile), ev, `decode ${name}`);
  }
});

test("datagram vectors", () => {
  for (const vec of load("datagrams.json")) {
    const { name, input } = vec;
    switch (input.profile) {
      case "cbor-array": {
        const d = new Datagram(
          input.op_ord as number,
          input.seq as number,
          hexToBytes(input.payload_hex as string),
        );
        const bytes = d.encode();
        assert.equal(bytesToHex(bytes), vec.hex, `encode ${name}`);
        assert.deepStrictEqual(Datagram.decode(bytes), d, `decode ${name}`);
        break;
      }
      case "compact-header": {
        const d = new CompactDatagram(
          input.op_ord as number,
          input.seq as number,
          hexToBytes(input.body_hex as string),
        );
        const epoch = opt<number>(input.epoch);
        if (epoch !== undefined) d.withEpoch(epoch);
        const bytes = d.encode();
        assert.equal(bytesToHex(bytes), vec.hex, `encode ${name}`);
        assert.deepStrictEqual(CompactDatagram.decode(bytes), d, `decode ${name}`);
        break;
      }
      default:
        throw new Error(`unknown datagram profile ${input.profile}`);
    }
  }
});

// Guard rails the registry naming as a sanity check that the codes line up.
test("status registry codes", () => {
  assert.equal(Status.Ok, 0);
  assert.equal(Status.VersionUnsupported, 5);
  assert.equal(Status.DeadlineExceeded, 8);
});
