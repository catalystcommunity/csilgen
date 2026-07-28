// TypeScript interop harness — mirrors the Rust reference (tests/interop/harness/rust).
//
// Runs as a server (binds a loopback TCP/UDP port, serves one transport) or a client
// (connects, runs the case battery for one transport, prints one JSON results object).
// The on-wire behavior and case names match the Rust reference so a TypeScript client
// works against a Rust server and vice versa. See tests/interop/README.md.
//
// Stream transports (rpc/events) use loopback TCP via `node:net` plus the transport
// lib's length-prefix framing. Datagrams use loopback UDP via `node:dgram` — both are
// stdlib, so the harness needs no native addon.

import * as net from "node:net";
import * as dgram from "node:dgram";
import * as fs from "node:fs";

import {
  Datagram,
  Event,
  Hello,
  HelloAck,
  type FrameCarrier,
  type HandlerOutcome,
  type Profile,
  type RpcRequest,
  RpcClient,
  RpcServer,
  Status,
  control,
  frameLengthPrefixed,
  reply as okReply,
  transport as failOutcome,
} from "../../../../transports/typescript/src/index.ts";

import {
  CsilDecimal,
  type Collections,
  type Constrained,
  type Nested,
  type Scalars,
  InteropAsyncClient,
  type AsyncServiceTransport,
  fromCollectionsCbor,
  fromConstrainedCbor,
  fromNestedCbor,
  fromOptBytesCbor,
  fromScalarsCbor,
  fromServiceErrorCbor,
  fromTickCbor,
  toCollectionsCbor,
  toConstrainedCbor,
  toEchoNestedResultCbor,
  toOptBytesCbor,
  toScalarsCbor,
  toServiceErrorCbor,
  toTickCbor,
  validateConstrained,
} from "./gen/dist/index.js";

// Wire names are the verbatim CSIL names (cbor-wire-contract.md "RPC call naming"):
// service as written in interop.csil, ops as written (kebab-case).
const SERVICE = "Interop";
const VERBOSE: Profile = "verbose";
const HOST = "127.0.0.1";

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see README "Fixed test vectors").
// ---------------------------------------------------------------------------

function scalarsOk(): Scalars {
  return {
    i: -42,
    u: 42,
    n: -7,
    f: 3.5,
    t: "héllo 世界",
    raw: new Uint8Array([0x01, 0x02, 0xf0, 0xff]),
    flag: true,
    when: new Date("2026-06-29T12:34:56Z"),
    amount: CsilDecimal.fromString("123.45"),
    // mixed-union coverage: a value equal to a literal arm must wire as
    // [1,"pending"]; a value with no literal match must wire as [0,...].
    statusLiteral: "pending",
    statusFree: "unlisted",
    // inline mixed choice (hoisted; literal arm match).
    note: "info",
    // inline all-literal choice (not hoisted).
    size: "medium",
    // named enum with a trailing `.default`-constrained last arm.
    level: "high",
    // named enum assembled via base rule + `/=` extension.
    season: "autumn",
    // named all-literal enum with mixed literal kinds (text + int arms);
    // renders as a plain union-of-literal-types alias, bare value.
    shipText: "ground",
    shipInt: 2,
  };
}

function collectionsOk(): Collections {
  return {
    names: ["a", "b"],
    atLeastOne: [1, 2, 3],
    bounded: [10, 20],
    exact3: [7, 8, 9],
    scores: { x: 1, y: 2 },
    color: "green",
    tone: "blue",
    prio: 2,
    who: 4242,
    pair: ["p", 5],
    triple: ["t", 9, true],
    extra: { k: "v" },
  };
}

function constrainedOk(): Constrained {
  return {
    code: "PRD-AB12CD",
    qty: 10,
    rate: CsilDecimal.fromString("0.25"),
    password: "hunter2hunter2",
    tags: ["one", "two"],
  };
}

function constrainedBad(): Constrained {
  return {
    code: "bad",
    qty: 0,
    rate: CsilDecimal.fromString("9.9"),
    password: "x",
    tags: [],
  };
}

function nestedOk(): Nested {
  return { inner: scalarsOk(), maybe: constrainedOk(), many: [scalarsOk()] };
}

// Deep structural equality across the decoded shapes: primitives, Uint8Array, Date,
// CsilDecimal (exact compare), arrays/tuples, and plain records.
function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a instanceof CsilDecimal && b instanceof CsilDecimal) return a.compare(b) === 0;
  if (a instanceof Date && b instanceof Date) return a.getTime() === b.getTime();
  if (a instanceof Uint8Array && b instanceof Uint8Array) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
    return true;
  }
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false;
    return a.every((x, i) => deepEqual(x, b[i]));
  }
  if (a && b && typeof a === "object" && typeof b === "object") {
    const ak = Object.keys(a as object);
    const bk = Object.keys(b as object);
    if (ak.length !== bk.length) return false;
    return ak.every((k) =>
      deepEqual((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]),
    );
  }
  return false;
}

// ---------------------------------------------------------------------------
// Result accumulation + JSON output.
// ---------------------------------------------------------------------------

class Cases {
  readonly lang = "typescript";
  private readonly out: Array<{ name: string; ok: boolean; detail: string }> = [];
  private readonly transport: string;
  constructor(transport: string) {
    this.transport = transport;
  }
  pass(name: string): void {
    this.out.push({ name, ok: true, detail: "" });
  }
  fail(name: string, detail: string): void {
    this.out.push({ name, ok: false, detail });
  }
  check(name: string, cond: boolean, detail: string): void {
    if (cond) this.pass(name);
    else this.fail(name, detail);
  }
  emit(): void {
    process.stdout.write(
      `${JSON.stringify({ lang: this.lang, transport: this.transport, cases: this.out })}\n`,
    );
  }
}

// ---------------------------------------------------------------------------
// Stream frame carrier — a TCP stream + the lib's 4-byte length framing.
// ---------------------------------------------------------------------------

class StreamFrameCarrier implements FrameCarrier {
  private readonly frames: Uint8Array[] = [];
  private readonly waiters: Array<(f: Uint8Array | null) => void> = [];
  private buf: Uint8Array = new Uint8Array(0);
  private closed = false;
  private readonly sock: net.Socket;

  constructor(sock: net.Socket) {
    this.sock = sock;
    sock.on("data", (chunk: Buffer) => this.onData(new Uint8Array(chunk)));
    sock.on("end", () => this.finish());
    sock.on("close", () => this.finish());
    sock.on("error", () => this.finish());
  }

  private onData(chunk: Uint8Array): void {
    const merged = new Uint8Array(this.buf.length + chunk.length);
    merged.set(this.buf, 0);
    merged.set(chunk, this.buf.length);
    this.buf = merged;
    // Drain every complete 4-byte-length-prefixed frame that has arrived.
    for (;;) {
      if (this.buf.length < 4) break;
      const len = new DataView(this.buf.buffer, this.buf.byteOffset, 4).getUint32(0, false);
      if (this.buf.length < 4 + len) break;
      const frame = this.buf.slice(4, 4 + len);
      this.buf = this.buf.slice(4 + len);
      const w = this.waiters.shift();
      if (w) w(frame);
      else this.frames.push(frame);
    }
  }

  private finish(): void {
    if (this.closed) return;
    this.closed = true;
    let w: ((f: Uint8Array | null) => void) | undefined;
    while ((w = this.waiters.shift())) w(null);
  }

  sendFrame(bytes: Uint8Array): Promise<void> {
    return new Promise((res, rej) => {
      this.sock.write(frameLengthPrefixed(bytes), (e) => (e ? rej(e) : res()));
    });
  }

  recvFrame(): Promise<Uint8Array | null> {
    const f = this.frames.shift();
    if (f !== undefined) return Promise.resolve(f);
    if (this.closed) return Promise.resolve(null);
    return new Promise((res) => this.waiters.push(res));
  }
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

// The server-side dispatch: decode the request, run the handler, re-encode. The
// typed error arm rides as a status-0 `ServiceError` variant (the failure path).
function rpcDispatch(req: RpcRequest): HandlerOutcome {
  try {
    switch (req.op) {
      case "echo-scalars":
        return okReply("Scalars", toScalarsCbor(fromScalarsCbor(req.payload)));
      case "echo-collections":
        return okReply("Collections", toCollectionsCbor(fromCollectionsCbor(req.payload)));
      case "echo-nested":
        return okReply(
          "EchoNestedResult",
          toEchoNestedResultCbor({ ok: true, echo: fromNestedCbor(req.payload) }),
        );
      case "echo-opt-bytes":
        return okReply("OptBytes", toOptBytesCbor(fromOptBytesCbor(req.payload)));
      case "validate-constrained": {
        const v = fromConstrainedCbor(req.payload);
        const errs = validateConstrained(v);
        if (errs.length > 0) {
          return okReply("ServiceError", toServiceErrorCbor({ code: 422, message: errs.join("; ") }));
        }
        return okReply("Constrained", toConstrainedCbor(v));
      }
      default:
        return failOutcome(Status.UnknownServiceOrOp, `unknown op ${req.op}`);
    }
  } catch (e) {
    return failOutcome(Status.MalformedEnvelope, e instanceof Error ? e.message : String(e));
  }
}

async function rpcServer(server: net.Server): Promise<void> {
  server.on("connection", async (sock) => {
    const rpc = new RpcServer(new StreamFrameCarrier(sock));
    try {
      while (await rpc.serveOne(rpcDispatch)) {
        // serve until clean EOF
      }
    } catch {
      // a broken connection just ends this loop
    }
  });
}

async function rpcClient(sock: net.Socket): Promise<Cases> {
  const cases = new Cases("rpc");
  const rpc = new RpcClient(new StreamFrameCarrier(sock), false);
  // The carrier maps a status-0 `ServiceError` variant to the language error path.
  const transport: AsyncServiceTransport = {
    async call(service, op, reqBytes) {
      const resp = await rpc.call(service, op, reqBytes);
      if (resp.variant === "ServiceError") {
        const se = fromServiceErrorCbor(resp.payload);
        throw new Error(`service error ${se.code}: ${se.message}`);
      }
      return resp.payload;
    },
  };
  const client = new InteropAsyncClient(transport);

  try {
    const r = await client.echoScalars(scalarsOk());
    cases.check("echo-scalars/success", deepEqual(r, scalarsOk()), `mismatch: ${JSON.stringify(r)}`);
  } catch (e) {
    cases.fail("echo-scalars/success", String(e));
  }
  try {
    const r = await client.echoCollections(collectionsOk());
    cases.check(
      "echo-collections/success",
      deepEqual(r, collectionsOk()),
      `mismatch: ${JSON.stringify(r)}`,
    );
  } catch (e) {
    cases.fail("echo-collections/success", String(e));
  }
  try {
    const r = await client.echoNested(nestedOk());
    cases.check(
      "echo-nested/success",
      r.ok && deepEqual(r.echo, nestedOk()),
      `mismatch: ${JSON.stringify(r)}`,
    );
  } catch (e) {
    cases.fail("echo-nested/success", String(e));
  }
  try {
    const r = await client.validateConstrained(constrainedOk());
    cases.check(
      "validate-constrained/success",
      deepEqual(r, constrainedOk()),
      `mismatch: ${JSON.stringify(r)}`,
    );
  } catch (e) {
    cases.fail("validate-constrained/success", String(e));
  }
  try {
    await client.validateConstrained(constrainedBad());
    cases.fail("validate-constrained/failure", "server accepted invalid input");
  } catch {
    // The typed error arm surfaces as a thrown service error.
    cases.pass("validate-constrained/failure");
  }

  // Optional-bytes presence: absent, present-empty, and present-non-empty are three
  // distinct states that must each survive the round trip unchanged. `undefined` is the
  // absent marker -- a zero-length Uint8Array is present, and must not decode to undefined.
  try {
    const r = await client.echoOptBytes({ tag: "absent" });
    cases.check("opt-bytes/absent", r.payload === undefined, `expected absent, got ${r.payload}`);
  } catch (e) {
    cases.fail("opt-bytes/absent", String(e));
  }
  try {
    const r = await client.echoOptBytes({ tag: "empty", payload: new Uint8Array(0) });
    cases.check(
      "opt-bytes/present-empty",
      r.payload !== undefined && r.payload.length === 0,
      `expected present-and-empty, got ${r.payload}`,
    );
  } catch (e) {
    cases.fail("opt-bytes/present-empty", String(e));
  }
  try {
    const sent = new Uint8Array([0x01, 0x02, 0xf0, 0xff]);
    const r = await client.echoOptBytes({ tag: "full", payload: sent });
    cases.check(
      "opt-bytes/present-non-empty",
      r.payload !== undefined && Buffer.compare(Buffer.from(r.payload), Buffer.from(sent)) === 0,
      `expected 0102f0ff, got ${r.payload}`,
    );
  } catch (e) {
    cases.fail("opt-bytes/present-non-empty", String(e));
  }
  return cases;
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

const TICKS = 3;

async function eventsServer(server: net.Server): Promise<void> {
  server.on("connection", async (sock) => {
    const carrier = new StreamFrameCarrier(sock);
    const send = (ev: Event) => carrier.sendFrame(ev.encode(VERBOSE));
    try {
      // Handshake: read $hello, reply $hello-ack (verbose).
      const hello = await carrier.recvFrame();
      if (hello === null) return;
      const helloEv = Event.decode(hello, VERBOSE);
      if (helloEv.event !== control.HELLO_NAME) return;
      Hello.decode(helloEv.payload);
      await send(Event.verbose(undefined, control.HELLO_ACK_NAME, new HelloAck(1, "verbose").encode()));

      // Push N ticks (on-tick / server push).
      for (let seq = 0; seq < TICKS; seq++) {
        await send(Event.verbose(SERVICE, "on-tick", toTickCbor({ seq })));
      }

      // React loop.
      for (;;) {
        const frame = await carrier.recvFrame();
        if (frame === null) break;
        let ev: Event;
        try {
          ev = Event.decode(frame, VERBOSE);
        } catch {
          break;
        }
        const method = ev.event ?? "";
        if (method === control.PING_NAME) {
          await send(Event.verbose(undefined, control.PONG_NAME, ev.payload));
        } else if (method === control.CLOSE_NAME) {
          break;
        } else if (method === "duplex") {
          let s: Scalars;
          try {
            s = fromScalarsCbor(ev.payload);
          } catch {
            continue;
          }
          await send(Event.verbose(SERVICE, "duplex", toScalarsCbor(s)));
        } else {
          await send(Event.verbose(undefined, control.ERROR_NAME, new Uint8Array(0)));
        }
      }
    } catch {
      // a broken connection just ends this handler
    }
  });
}

async function eventsClient(sock: net.Socket): Promise<Cases> {
  const cases = new Cases("events");
  const carrier = new StreamFrameCarrier(sock);
  const send = (ev: Event) => carrier.sendFrame(ev.encode(VERBOSE));

  // Handshake.
  await send(
    Event.verbose(undefined, control.HELLO_NAME, new Hello([1], ["verbose"], SERVICE).encode()),
  );
  const ackFrame = await carrier.recvFrame();
  let ackOk = false;
  if (ackFrame !== null) {
    try {
      ackOk = Event.decode(ackFrame, VERBOSE).event === control.HELLO_ACK_NAME;
    } catch {
      ackOk = false;
    }
  }
  cases.check("events/handshake", ackOk, "no $hello-ack");

  // on-tick: expect TICKS pushes with seq 0..TICKS.
  let tickOk = true;
  let detail = "";
  for (let expect = 0; expect < TICKS; expect++) {
    const frame = await carrier.recvFrame();
    if (frame === null) {
      tickOk = false;
      detail = "stream closed during ticks";
      break;
    }
    try {
      const ev = Event.decode(frame, VERBOSE);
      if (ev.event !== "on-tick") {
        tickOk = false;
        detail = `expected on-tick got ${ev.event}`;
        break;
      }
      const t = fromTickCbor(ev.payload);
      if (t.seq !== expect) {
        tickOk = false;
        detail = `tick seq ${t.seq} != ${expect}`;
        break;
      }
    } catch (e) {
      tickOk = false;
      detail = `decode: ${e}`;
      break;
    }
  }
  cases.check("on-tick/success", tickOk, detail);

  // duplex: send Scalars, expect echo.
  await send(Event.verbose(SERVICE, "duplex", toScalarsCbor(scalarsOk())));
  const dupFrame = await carrier.recvFrame();
  if (dupFrame === null) {
    cases.fail("duplex/success", "no echo");
  } else {
    try {
      const ev = Event.decode(dupFrame, VERBOSE);
      if (ev.event === "duplex") {
        const s = fromScalarsCbor(ev.payload);
        cases.check("duplex/success", deepEqual(s, scalarsOk()), "echo mismatch");
      } else {
        cases.fail("duplex/success", `got ${ev.event}`);
      }
    } catch (e) {
      cases.fail("duplex/success", String(e));
    }
  }

  // unknown-method: send a bogus channel method, expect $error.
  await send(Event.verbose(SERVICE, "Bogus", new Uint8Array(0)));
  const errFrame = await carrier.recvFrame();
  if (errFrame === null) {
    cases.fail("unknown-method/failure", "no $error");
  } else {
    let isErr = false;
    try {
      isErr = Event.decode(errFrame, VERBOSE).event === control.ERROR_NAME;
    } catch {
      isErr = false;
    }
    cases.check("unknown-method/failure", isErr, "expected $error");
  }

  await send(Event.verbose(undefined, control.CLOSE_NAME, new Uint8Array(0)));
  return cases;
}

// ---------------------------------------------------------------------------
// Datagrams
// ---------------------------------------------------------------------------

const OP_ECHO_SCALARS = 0;
const OP_ECHO_COLLECTIONS = 1;
const OP_ERROR_SENTINEL = 255;

// UDP server: one socket, reply to each datagram's source (rinfo). The decode/encode
// path is identical to the AF_UNIX version — only the socket primitive changed.
function datagramServer(sock: dgram.Socket): void {
  sock.on("message", (msg: Buffer, rinfo: dgram.RemoteInfo) => {
    let reply: Datagram;
    try {
      const dg = Datagram.decode(new Uint8Array(msg));
      if (dg.op_ord === OP_ECHO_SCALARS) {
        const s = fromScalarsCbor(dg.payload);
        reply = new Datagram(OP_ECHO_SCALARS, dg.seq, toScalarsCbor(s));
      } else if (dg.op_ord === OP_ECHO_COLLECTIONS) {
        const c = fromCollectionsCbor(dg.payload);
        reply = new Datagram(OP_ECHO_COLLECTIONS, dg.seq, toCollectionsCbor(c));
      } else {
        reply = new Datagram(OP_ERROR_SENTINEL, dg.seq, new Uint8Array(0));
      }
    } catch {
      reply = new Datagram(OP_ERROR_SENTINEL, 0, new Uint8Array(0));
    }
    sock.send(reply.encode(), rinfo.port, rinfo.address, () => {
      // drop on send error
    });
  });
}

async function datagramClient(sock: dgram.Socket, port: number): Promise<Cases> {
  const cases = new Cases("datagrams");
  // Serialized request/reply: send one datagram, await the next inbound message
  // (or a 3s timeout). The reply rides back to this socket's ephemeral source port.
  const roundtrip = (op: number, seq: number, payload: Uint8Array): Promise<Datagram> =>
    new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        sock.removeListener("message", onMessage);
        reject(new Error("recv timed out"));
      }, 3000);
      const onMessage = (msg: Buffer) => {
        clearTimeout(timer);
        try {
          resolve(Datagram.decode(new Uint8Array(msg)));
        } catch (e) {
          reject(e);
        }
      };
      sock.once("message", onMessage);
      sock.send(new Datagram(op, seq, payload).encode(), port, HOST, (err) => {
        if (err) {
          clearTimeout(timer);
          sock.removeListener("message", onMessage);
          reject(err);
        }
      });
    });

  try {
    const d = await roundtrip(OP_ECHO_SCALARS, 1, toScalarsCbor(scalarsOk()));
    if (d.op_ord !== OP_ECHO_SCALARS) cases.fail("echo-scalars/success", `op_ord ${d.op_ord}`);
    else cases.check("echo-scalars/success", deepEqual(fromScalarsCbor(d.payload), scalarsOk()), "payload mismatch");
  } catch (e) {
    cases.fail("echo-scalars/success", String(e));
  }
  try {
    const d = await roundtrip(OP_ECHO_COLLECTIONS, 2, toCollectionsCbor(collectionsOk()));
    if (d.op_ord !== OP_ECHO_COLLECTIONS) cases.fail("echo-collections/success", `op_ord ${d.op_ord}`);
    else
      cases.check(
        "echo-collections/success",
        deepEqual(fromCollectionsCbor(d.payload), collectionsOk()),
        "payload mismatch",
      );
  } catch (e) {
    cases.fail("echo-collections/success", String(e));
  }
  try {
    const d = await roundtrip(99, 3, new Uint8Array(0));
    cases.check(
      "bad-op-ord/failure",
      d.op_ord === OP_ERROR_SENTINEL,
      `expected error sentinel, got op_ord ${d.op_ord}`,
    );
  } catch (e) {
    cases.fail("bad-op-ord/failure", String(e));
  }
  return cases;
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

function ready(): void {
  fs.writeSync(1, "READY\n");
}

function connectRetry(port: number): Promise<net.Socket> {
  return new Promise((resolve, reject) => {
    let attempts = 0;
    const tryConnect = () => {
      const sock = net.connect(port, HOST);
      sock.once("connect", () => resolve(sock));
      sock.once("error", () => {
        sock.destroy();
        if (++attempts >= 200) reject(new Error(`connect failed: ${HOST}:${port}`));
        else setTimeout(tryConnect, 15);
      });
    };
    tryConnect();
  });
}

// Listen the TCP server, retrying briefly so a just-freed port from the previous
// server in the matrix is tolerated without a SO_REUSEADDR dependency.
function listenRetry(server: net.Server, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    let attempts = 0;
    const tryListen = () => {
      const onError = (err: NodeJS.ErrnoException) => {
        if (err.code === "EADDRINUSE" && ++attempts < 200) {
          setTimeout(tryListen, 15);
        } else {
          reject(err);
        }
      };
      server.once("error", onError);
      server.listen(port, HOST, () => {
        server.removeListener("error", onError);
        resolve();
      });
    };
    tryListen();
  });
}

async function main(): Promise<void> {
  const [mode, transport, portArg] = process.argv.slice(2);
  if (!mode || !transport || !portArg) {
    process.stderr.write("usage: main.ts <server|client> <rpc|events|datagrams> <port>\n");
    process.exit(2);
  }
  const port = parseInt(portArg, 10);

  if (mode === "server" && transport === "datagrams") {
    const sock = dgram.createSocket("udp4");
    datagramServer(sock);
    sock.on("listening", () => ready());
    sock.bind(port, HOST);
    return; // serve until killed
  }
  if (mode === "server") {
    const server = net.createServer();
    if (transport === "rpc") await rpcServer(server);
    else if (transport === "events") await eventsServer(server);
    else {
      process.stderr.write("bad transport\n");
      process.exit(2);
    }
    await listenRetry(server, port);
    ready();
    return; // serve until killed
  }
  if (mode === "client" && transport === "datagrams") {
    // Bind an ephemeral local UDP port; the server replies to this source.
    const sock = dgram.createSocket("udp4");
    try {
      const cases = await datagramClient(sock, port);
      cases.emit();
    } finally {
      sock.close();
    }
    process.exit(0);
  }
  if (mode === "client") {
    const sock = await connectRetry(port);
    const cases =
      transport === "rpc"
        ? await rpcClient(sock)
        : transport === "events"
          ? await eventsClient(sock)
          : null;
    if (cases === null) {
      process.stderr.write("bad transport\n");
      process.exit(2);
    }
    cases.emit();
    sock.destroy();
    process.exit(0);
  }

  process.stderr.write("bad mode/transport\n");
  process.exit(2);
}

main().catch((e) => {
  process.stderr.write(`fatal: ${e instanceof Error ? e.stack : String(e)}\n`);
  process.exit(1);
});
