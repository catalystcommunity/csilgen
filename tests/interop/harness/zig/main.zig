// Zig interop harness — mirrors the Rust reference (tests/interop/harness/rust).
//
// Runs as a server (binds a loopback TCP/UDP port, serves one transport) or a client
// (connects, runs the case battery for one transport, prints one JSON results object).
// The on-wire behavior and case names match the Rust reference so a Zig client works
// against a Rust server and vice versa. See tests/interop/README.md.
//
// Stream transports (rpc/events) use AF_INET SOCK_STREAM (loopback TCP) via std.posix
// plus the transport lib's 4-byte length-prefix framing (StreamCarrier). Datagrams use
// AF_INET SOCK_DGRAM (loopback UDP) via std.posix (bind/sendto/recvfrom) — Zig has
// direct posix socket access, so no native addon is needed.

const std = @import("std");
const transport = @import("csilgen_transport");

const carrier = transport.carrier;
const rpc = transport.rpc;
const events = transport.events;
const dgram = transport.datagrams;
const Status = transport.Status;

const gen_types = @import("gen/types.gen.zig");
const gen_codec = @import("gen/codec.gen.zig");
const gen_client = @import("gen/client.gen.zig");
const gen_server = @import("gen/server.gen.zig");
const gen_valid = @import("gen/validation.gen.zig");

const Scalars = gen_types.Scalars;
const Collections = gen_types.Collections;
const Constrained = gen_types.Constrained;
const Nested = gen_types.Nested;
const EchoNestedResult = gen_types.EchoNestedResult;
const CsilValue = gen_codec.CsilValue;

const SERVICE = "interop";

fn streq(a: []const u8, b: []const u8) bool {
    return std.mem.eql(u8, a, b);
}

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see README "Fixed test vectors").
// ---------------------------------------------------------------------------

fn scalarsOk() Scalars {
    return .{
        .i = -42,
        .u = 42,
        .n = -7,
        .f = 3.5,
        .t = "héllo 世界",
        .raw = &[_]u8{ 0x01, 0x02, 0xf0, 0xff },
        .flag = true,
        .when = .{ .rfc3339 = "2026-06-29T12:34:56Z", .epoch_seconds = 0 },
        // 123.45 == 12345 * 10^-2.
        .amount = .{ .exponent = -2, .mantissa = 12345 },
    };
}

fn collectionsOk(a: std.mem.Allocator) !Collections {
    var scores = std.StringHashMapUnmanaged(i64){};
    try scores.put(a, "x", 1);
    try scores.put(a, "y", 2);
    var extra = std.StringHashMapUnmanaged(CsilValue){};
    try extra.put(a, "k", .{ .text = "v" });
    return .{
        .names = try a.dupe([]const u8, &[_][]const u8{ "a", "b" }),
        .at_least_one = try a.dupe(i64, &[_]i64{ 1, 2, 3 }),
        .bounded = try a.dupe(i64, &[_]i64{ 10, 20 }),
        .exact3 = try a.dupe(i64, &[_]i64{ 7, 8, 9 }),
        .scores = scores,
        .color = .green,
        .prio = .v2,
        .who = .{ .uint = 4242 },
        .pair = .{ "p", 5 },
        .triple = .{ "t", 9, true },
        .extra = extra,
    };
}

fn constrainedOk(a: std.mem.Allocator) !Constrained {
    return .{
        .code = "PRD-AB12CD",
        .qty = 10,
        // 0.25 == 25 * 10^-2.
        .rate = .{ .exponent = -2, .mantissa = 25 },
        .password = "hunter2hunter2",
        .tags = try a.dupe([]const u8, &[_][]const u8{ "one", "two" }),
    };
}

fn constrainedBad(a: std.mem.Allocator) !Constrained {
    return .{
        .code = "bad",
        .qty = 0,
        // 9.9 == 99 * 10^-1.
        .rate = .{ .exponent = -1, .mantissa = 99 },
        .password = "x",
        .tags = try a.dupe([]const u8, &[_][]const u8{}),
    };
}

fn nestedOk(a: std.mem.Allocator) !Nested {
    const many = try a.alloc(Scalars, 1);
    many[0] = scalarsOk();
    return .{ .inner = scalarsOk(), .maybe = try constrainedOk(a), .many = many };
}

// ---------------------------------------------------------------------------
// Structural equality across the decoded shapes.
// ---------------------------------------------------------------------------

fn sliceEqlStr(a: [][]const u8, b: [][]const u8) bool {
    if (a.len != b.len) return false;
    for (a, b) |x, y| if (!streq(x, y)) return false;
    return true;
}

fn sliceEqlI64(a: []i64, b: []i64) bool {
    if (a.len != b.len) return false;
    for (a, b) |x, y| if (x != y) return false;
    return true;
}

fn valueEql(a: CsilValue, b: CsilValue) bool {
    return switch (a) {
        .uint => |x| b == .uint and b.uint == x,
        .int => |x| b == .int and b.int == x,
        .float => |x| b == .float and b.float == x,
        .boolean => |x| b == .boolean and b.boolean == x,
        .null => b == .null,
        .bytes => |x| b == .bytes and std.mem.eql(u8, x, b.bytes),
        .text => |x| b == .text and streq(x, b.text),
        // Nested arrays/maps/tags are not exercised by the interop `any` vector.
        else => false,
    };
}

fn scalarsEql(a: Scalars, b: Scalars) bool {
    return a.i == b.i and a.u == b.u and a.n == b.n and a.f == b.f and
        streq(a.t, b.t) and std.mem.eql(u8, a.raw, b.raw) and a.flag == b.flag and
        streq(a.when.rfc3339, b.when.rfc3339) and
        a.amount.exponent == b.amount.exponent and a.amount.mantissa == b.amount.mantissa;
}

fn constrainedEql(a: Constrained, b: Constrained) bool {
    return streq(a.code, b.code) and a.qty == b.qty and
        a.rate.exponent == b.rate.exponent and a.rate.mantissa == b.rate.mantissa and
        streq(a.password, b.password) and sliceEqlStr(a.tags, b.tags);
}

fn nestedEql(a: Nested, b: Nested) bool {
    if (!scalarsEql(a.inner, b.inner)) return false;
    if ((a.maybe == null) != (b.maybe == null)) return false;
    if (a.maybe) |am| if (!constrainedEql(am, b.maybe.?)) return false;
    if (a.many.len != b.many.len) return false;
    for (a.many, b.many) |x, y| if (!scalarsEql(x, y)) return false;
    return true;
}

fn scoresEql(a: std.StringHashMapUnmanaged(i64), b: std.StringHashMapUnmanaged(i64)) bool {
    if (a.count() != b.count()) return false;
    var it = a.iterator();
    while (it.next()) |e| {
        const bv = b.get(e.key_ptr.*) orelse return false;
        if (bv != e.value_ptr.*) return false;
    }
    return true;
}

fn extraEql(a: std.StringHashMapUnmanaged(CsilValue), b: std.StringHashMapUnmanaged(CsilValue)) bool {
    if (a.count() != b.count()) return false;
    var it = a.iterator();
    while (it.next()) |e| {
        const bv = b.get(e.key_ptr.*) orelse return false;
        if (!valueEql(e.value_ptr.*, bv)) return false;
    }
    return true;
}

fn optI64Eql(a: ?i64, b: ?i64) bool {
    if ((a == null) != (b == null)) return false;
    if (a) |x| return x == b.?;
    return true;
}

fn optBoolEql(a: ?bool, b: ?bool) bool {
    if ((a == null) != (b == null)) return false;
    if (a) |x| return x == b.?;
    return true;
}

fn collectionsEql(a: Collections, b: Collections) bool {
    if (!sliceEqlStr(a.names, b.names)) return false;
    if (!sliceEqlI64(a.at_least_one, b.at_least_one)) return false;
    if (!sliceEqlI64(a.bounded, b.bounded)) return false;
    if (!sliceEqlI64(a.exact3, b.exact3)) return false;
    if (!scoresEql(a.scores, b.scores)) return false;
    if (a.color != b.color) return false;
    if (a.prio != b.prio) return false;
    // who: tagged union.
    switch (a.who) {
        .uint => |x| if (!(b.who == .uint and b.who.uint == x)) return false,
        .text => |x| if (!(b.who == .text and streq(x, b.who.text))) return false,
    }
    if (!streq(a.pair[0], b.pair[0]) or a.pair[1] != b.pair[1]) return false;
    if (!streq(a.triple[0], b.triple[0])) return false;
    if (!optI64Eql(a.triple[1], b.triple[1])) return false;
    if (!optBoolEql(a.triple[2], b.triple[2])) return false;
    if (!extraEql(a.extra, b.extra)) return false;
    return true;
}

// ---------------------------------------------------------------------------
// Result accumulation + JSON output.
// ---------------------------------------------------------------------------

const Cases = struct {
    transport_name: []const u8,
    out: std.ArrayList(u8),
    first: bool = true,

    fn init(a: std.mem.Allocator, transport_name: []const u8) Cases {
        return .{ .transport_name = transport_name, .out = std.ArrayList(u8).init(a) };
    }

    fn record(self: *Cases, name: []const u8, ok: bool, detail: []const u8) void {
        if (!self.first) self.out.append(',') catch {};
        self.first = false;
        self.out.appendSlice("{\"name\":\"") catch {};
        self.out.appendSlice(name) catch {};
        self.out.appendSlice("\",\"ok\":") catch {};
        self.out.appendSlice(if (ok) "true" else "false") catch {};
        self.out.appendSlice(",\"detail\":") catch {};
        appendJsonStr(&self.out, detail);
        self.out.append('}') catch {};
    }

    fn pass(self: *Cases, name: []const u8) void {
        self.record(name, true, "");
    }
    fn fail(self: *Cases, name: []const u8, detail: []const u8) void {
        self.record(name, false, detail);
    }
    fn check(self: *Cases, name: []const u8, cond: bool, detail: []const u8) void {
        self.record(name, cond, if (cond) "" else detail);
    }

    fn emit(self: *Cases) void {
        const stdout = std.io.getStdOut().writer();
        stdout.print("{{\"lang\":\"zig\",\"transport\":\"{s}\",\"cases\":[{s}]}}\n", .{ self.transport_name, self.out.items }) catch {};
    }
};

fn appendJsonStr(out: *std.ArrayList(u8), s: []const u8) void {
    out.append('"') catch {};
    for (s) |c| {
        switch (c) {
            '"' => out.appendSlice("\\\"") catch {},
            '\\' => out.appendSlice("\\\\") catch {},
            '\n' => out.appendSlice("\\n") catch {},
            '\t' => out.appendSlice("\\t") catch {},
            '\r' => out.appendSlice("\\r") catch {},
            else => out.append(c) catch {},
        }
    }
    out.append('"') catch {};
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

const ServerCtx = struct {
    arena: *std.heap.ArenaAllocator,
};

fn rpcHandle(ptr: *anyopaque, req: *const rpc.RpcRequest) rpc.HandlerOutcome {
    const self: *ServerCtx = @ptrCast(@alignCast(ptr));
    const a = self.arena.allocator();
    if (streq(req.op, "EchoScalars")) {
        var v: Scalars = undefined;
        gen_codec.decode_Scalars(a, req.payload, &v) catch
            return rpc.HandlerOutcome.make_transport(Status.malformed_envelope, "decode");
        const b = gen_codec.encode_Scalars(a, &v) catch
            return rpc.HandlerOutcome.make_transport(Status.internal, "encode");
        return rpc.HandlerOutcome.make_reply("Scalars", b);
    } else if (streq(req.op, "EchoCollections")) {
        var v: Collections = undefined;
        gen_codec.decode_Collections(a, req.payload, &v) catch
            return rpc.HandlerOutcome.make_transport(Status.malformed_envelope, "decode");
        const b = gen_codec.encode_Collections(a, &v) catch
            return rpc.HandlerOutcome.make_transport(Status.internal, "encode");
        return rpc.HandlerOutcome.make_reply("Collections", b);
    } else if (streq(req.op, "EchoNested")) {
        var v: Nested = undefined;
        gen_codec.decode_Nested(a, req.payload, &v) catch
            return rpc.HandlerOutcome.make_transport(Status.malformed_envelope, "decode");
        const r = EchoNestedResult{ .ok = true, .echo = v };
        const b = gen_codec.encode_EchoNestedResult(a, &r) catch
            return rpc.HandlerOutcome.make_transport(Status.internal, "encode");
        return rpc.HandlerOutcome.make_reply("EchoNestedResult", b);
    } else if (streq(req.op, "ValidateConstrained")) {
        var v: Constrained = undefined;
        gen_codec.decode_Constrained(a, req.payload, &v) catch
            return rpc.HandlerOutcome.make_transport(Status.malformed_envelope, "decode");
        if (gen_valid.validate_constrained(&v)) {
            const b = gen_codec.encode_Constrained(a, &v) catch
                return rpc.HandlerOutcome.make_transport(Status.internal, "encode");
            return rpc.HandlerOutcome.make_reply("Constrained", b);
        }
        // The typed error arm rides as a status-0 `ServiceError` variant.
        const se = gen_types.ServiceError{ .code = 422, .message = "constraint violation" };
        const b = gen_codec.encode_ServiceError(a, &se) catch
            return rpc.HandlerOutcome.make_transport(Status.internal, "encode");
        return rpc.HandlerOutcome.make_reply("ServiceError", b);
    }
    return rpc.HandlerOutcome.make_transport(Status.unknown_service_or_op, "unknown op");
}

fn rpcServeConn(base: std.mem.Allocator, stream: std.net.Stream) void {
    var sc = carrier.StreamCarrier.init(stream);
    var server = rpc.RpcServer.init(sc.carrier());
    var arena = std.heap.ArenaAllocator.init(base);
    defer arena.deinit();
    var ctx = ServerCtx{ .arena = &arena };
    const handler = rpc.Handler{ .ctx = &ctx, .func = rpcHandle };
    while (true) {
        const more = server.serve_one(base, handler) catch break;
        _ = arena.reset(.retain_capacity);
        if (!more) break;
    }
    sc.carrier().close();
}

const ClientCtx = struct {
    client: *rpc.RpcClient,
    base: std.mem.Allocator,
};

// The carrier maps a status-0 `ServiceError` variant to the language error path.
fn rpcClientCall(
    ptr: *anyopaque,
    alloc: std.mem.Allocator,
    service: []const u8,
    op: []const u8,
    req: []const u8,
) anyerror![]u8 {
    const self: *ClientCtx = @ptrCast(@alignCast(ptr));
    var arena = std.heap.ArenaAllocator.init(self.base);
    defer arena.deinit();
    const resp = try self.client.call(self.base, arena.allocator(), service, op, req, null);
    if (resp.variant) |variant| {
        if (streq(variant, "ServiceError")) return error.ServiceError;
    }
    return try alloc.dupe(u8, resp.payload);
}

fn rpcClient(a: std.mem.Allocator, stream: std.net.Stream) Cases {
    var cases = Cases.init(a, "rpc");
    var sc = carrier.StreamCarrier.init(stream);
    var client = rpc.RpcClient.init(sc.carrier(), false);
    var cctx = ClientCtx{ .client = &client, .base = a };
    const tr = gen_client.CsilgenTransport{ .ptr = &cctx, .call = rpcClientCall };
    const c = gen_client.InteropClient.init(tr);

    {
        var got: Scalars = undefined;
        const want = scalarsOk();
        if (c.echo_scalars(a, &want, &got)) {
            cases.check("echo-scalars/success", scalarsEql(got, want), "mismatch");
        } else |err| {
            cases.fail("echo-scalars/success", @errorName(err));
        }
    }
    {
        var got: Collections = undefined;
        const want = collectionsOk(a) catch unreachable;
        if (c.echo_collections(a, &want, &got)) {
            cases.check("echo-collections/success", collectionsEql(got, want), "mismatch");
        } else |err| {
            cases.fail("echo-collections/success", @errorName(err));
        }
    }
    {
        var got: EchoNestedResult = undefined;
        const want = nestedOk(a) catch unreachable;
        if (c.echo_nested(a, &want, &got)) {
            cases.check("echo-nested/success", got.ok and nestedEql(got.echo, want), "mismatch");
        } else |err| {
            cases.fail("echo-nested/success", @errorName(err));
        }
    }
    {
        var got: Constrained = undefined;
        const want = constrainedOk(a) catch unreachable;
        if (c.validate_constrained(a, &want, &got)) {
            cases.check("validate-constrained/success", constrainedEql(got, want), "mismatch");
        } else |err| {
            cases.fail("validate-constrained/success", @errorName(err));
        }
    }
    {
        var got: Constrained = undefined;
        const bad = constrainedBad(a) catch unreachable;
        if (c.validate_constrained(a, &bad, &got)) {
            cases.fail("validate-constrained/failure", "server accepted invalid input");
        } else |err| {
            // The typed error arm must surface as a service error.
            cases.check("validate-constrained/failure", err == error.ServiceError, @errorName(err));
        }
    }
    sc.carrier().close();
    return cases;
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

const TICKS: u64 = 3;

fn sendEvent(c: carrier.FrameCarrier, a: std.mem.Allocator, ev: events.Event) void {
    const frame = ev.encode(a, .verbose) catch return;
    c.send(frame) catch {};
}

// Channel-router glue: only `Duplex` (Scalars) is a registered channel method, and the
// harness drives it directly; the router is invoked solely to exercise its
// unknown-method path. The codec/handlers are required arguments either way.
fn chDecodeScalars(ptr: *anyopaque, data: []const u8, out: *anyopaque) anyerror!void {
    const a: *std.mem.Allocator = @ptrCast(@alignCast(ptr));
    const s: *Scalars = @ptrCast(@alignCast(out));
    return gen_codec.decode_Scalars(a.*, data, s);
}
fn chEncodeScalars(ptr: *anyopaque, value: *const anyopaque) anyerror![]u8 {
    const a: *std.mem.Allocator = @ptrCast(@alignCast(ptr));
    const s: *const Scalars = @ptrCast(@alignCast(value));
    return gen_codec.encode_Scalars(a.*, s);
}
fn chEchoScalars(_: *anyopaque, _: *const Scalars, _: *Scalars) anyerror!void {}
fn chEchoCollections(_: *anyopaque, _: *const Collections, _: *Collections) anyerror!void {}
fn chEchoNested(_: *anyopaque, _: *const Nested, _: *EchoNestedResult) anyerror!void {}
fn chValidateConstrained(_: *anyopaque, _: *const Constrained, _: *Constrained) anyerror!void {}
fn chDuplex(_: *anyopaque, _: *const Scalars) anyerror!void {}

fn eventsServeConn(base: std.mem.Allocator, stream: std.net.Stream) void {
    var sc = carrier.StreamCarrier.init(stream);
    const c = sc.carrier();
    defer c.close();
    var arena = std.heap.ArenaAllocator.init(base);
    defer arena.deinit();
    const a = arena.allocator();

    // Handshake: read $hello, reply $hello-ack (verbose).
    const hello_frame = (c.recv(a) catch return) orelse return;
    const hello_ev = events.decode_event(a, hello_frame, .verbose) catch return;
    if (hello_ev.event == null or !streq(hello_ev.event.?, events.HELLO_NAME)) return;
    _ = events.decode_hello(a, hello_ev.payload) catch {};
    const ack = events.HelloAck{ .v = 1, .profile = "verbose" };
    const ack_payload = ack.encode(a) catch return;
    sendEvent(c, a, events.Event.verbose(null, events.HELLO_ACK_NAME, ack_payload));

    // Push N ticks (on-tick / server push).
    var seq: u64 = 0;
    while (seq < TICKS) : (seq += 1) {
        const t = gen_types.Tick{ .seq = seq };
        const tb = gen_codec.encode_Tick(a, &t) catch return;
        sendEvent(c, a, events.Event.verbose(SERVICE, "OnTick", tb));
    }

    // React loop.
    const handlers = gen_server.InteropHandlers{
        .echo_scalars = chEchoScalars,
        .echo_collections = chEchoCollections,
        .echo_nested = chEchoNested,
        .validate_constrained = chValidateConstrained,
        .duplex = chDuplex,
    };
    var codec_alloc = a;
    const codec = gen_server.CsilgenCodec{ .ptr = &codec_alloc, .decode = chDecodeScalars, .encode = chEncodeScalars };
    var dummy: u8 = 0;
    while (true) {
        const frame = (c.recv(a) catch break) orelse break;
        const ev = events.decode_event(a, frame, .verbose) catch break;
        const method = ev.event orelse "";
        if (streq(method, events.PING_NAME)) {
            sendEvent(c, a, events.Event.verbose(null, events.PONG_NAME, ev.payload));
        } else if (streq(method, events.CLOSE_NAME)) {
            break;
        } else if (streq(method, "Duplex")) {
            var s: Scalars = undefined;
            if (gen_codec.decode_Scalars(a, ev.payload, &s)) {
                const sb = gen_codec.encode_Scalars(a, &s) catch continue;
                sendEvent(c, a, events.Event.verbose(SERVICE, "Duplex", sb));
            } else |_| {}
        } else {
            // Exercise the generated router's unknown-method path.
            gen_server.route_interop_channel(&handlers, &dummy, codec, method, ev.payload) catch {};
            sendEvent(c, a, events.Event.verbose(null, events.ERROR_NAME, ""));
        }
    }
}

fn eventsClient(a: std.mem.Allocator, stream: std.net.Stream) Cases {
    var cases = Cases.init(a, "events");
    var sc = carrier.StreamCarrier.init(stream);
    const c = sc.carrier();
    defer c.close();

    // Handshake.
    const versions = [_]u64{1};
    const profiles = [_][]const u8{"verbose"};
    const hello = events.Hello{ .versions = &versions, .profiles = &profiles, .service = SERVICE };
    const hello_payload = hello.encode(a) catch unreachable;
    sendEvent(c, a, events.Event.verbose(null, events.HELLO_NAME, hello_payload));

    var ack_ok = false;
    if (c.recv(a) catch null) |f| {
        if (events.decode_event(a, f, .verbose)) |ev| {
            ack_ok = ev.event != null and streq(ev.event.?, events.HELLO_ACK_NAME);
        } else |_| {}
    }
    cases.check("events/handshake", ack_ok, "no $hello-ack");

    // on-tick: expect TICKS pushes with seq 0..TICKS.
    var tick_ok = true;
    var detail: []const u8 = "";
    var expect: u64 = 0;
    while (expect < TICKS) : (expect += 1) {
        const f = (c.recv(a) catch null) orelse {
            tick_ok = false;
            detail = "stream closed during ticks";
            break;
        };
        const ev = events.decode_event(a, f, .verbose) catch {
            tick_ok = false;
            detail = "tick decode";
            break;
        };
        if (ev.event == null or !streq(ev.event.?, "OnTick")) {
            tick_ok = false;
            detail = "expected OnTick";
            break;
        }
        var t: gen_types.Tick = undefined;
        gen_codec.decode_Tick(a, ev.payload, &t) catch {
            tick_ok = false;
            detail = "tick payload decode";
            break;
        };
        if (t.seq != expect) {
            tick_ok = false;
            detail = "tick seq mismatch";
            break;
        }
    }
    cases.check("on-tick/success", tick_ok, detail);

    // duplex: send Scalars, expect echo.
    {
        const want = scalarsOk();
        const sb = gen_codec.encode_Scalars(a, &want) catch unreachable;
        sendEvent(c, a, events.Event.verbose(SERVICE, "Duplex", sb));
        if (c.recv(a) catch null) |f| {
            if (events.decode_event(a, f, .verbose)) |ev| {
                if (ev.event != null and streq(ev.event.?, "Duplex")) {
                    var s: Scalars = undefined;
                    if (gen_codec.decode_Scalars(a, ev.payload, &s)) {
                        cases.check("duplex/success", scalarsEql(s, want), "echo mismatch");
                    } else |err| {
                        cases.fail("duplex/success", @errorName(err));
                    }
                } else {
                    cases.fail("duplex/success", "unexpected event");
                }
            } else |err| {
                cases.fail("duplex/success", @errorName(err));
            }
        } else {
            cases.fail("duplex/success", "no echo");
        }
    }

    // unknown-method: send a bogus channel method, expect $error.
    {
        sendEvent(c, a, events.Event.verbose(SERVICE, "Bogus", ""));
        var is_err = false;
        if (c.recv(a) catch null) |f| {
            if (events.decode_event(a, f, .verbose)) |ev| {
                is_err = ev.event != null and streq(ev.event.?, events.ERROR_NAME);
            } else |_| {}
        }
        cases.check("unknown-method/failure", is_err, "expected $error");
    }

    sendEvent(c, a, events.Event.verbose(null, events.CLOSE_NAME, ""));
    return cases;
}

// ---------------------------------------------------------------------------
// Datagrams (AF_INET SOCK_DGRAM / loopback UDP via std.posix)
// ---------------------------------------------------------------------------

const OP_ECHO_SCALARS: u64 = 0;
const OP_ECHO_COLLECTIONS: u64 = 1;
const OP_ERROR_SENTINEL: u64 = 255;

fn datagramServer(base: std.mem.Allocator, fd: std.posix.socket_t) void {
    var buf: [65536]u8 = undefined;
    var arena = std.heap.ArenaAllocator.init(base);
    defer arena.deinit();
    while (true) {
        var src: std.posix.sockaddr.in = undefined;
        var srclen: std.posix.socklen_t = @sizeOf(std.posix.sockaddr.in);
        const n = std.posix.recvfrom(fd, &buf, 0, @ptrCast(&src), &srclen) catch continue;
        _ = arena.reset(.retain_capacity);
        const a = arena.allocator();
        const reply = datagramReply(a, buf[0..n]) catch continue;
        _ = std.posix.sendto(fd, reply, 0, @ptrCast(&src), srclen) catch {};
    }
}

fn datagramReply(a: std.mem.Allocator, data: []const u8) ![]u8 {
    const d = dgram.decode_datagram(a, data) catch {
        return (dgram.Datagram.init(OP_ERROR_SENTINEL, 0, "")).encode(a);
    };
    switch (d.op_ord) {
        OP_ECHO_SCALARS => {
            var s: Scalars = undefined;
            if (gen_codec.decode_Scalars(a, d.payload, &s)) {
                const sb = try gen_codec.encode_Scalars(a, &s);
                return (dgram.Datagram.init(OP_ECHO_SCALARS, d.seq, sb)).encode(a);
            } else |_| {
                return (dgram.Datagram.init(OP_ERROR_SENTINEL, d.seq, "")).encode(a);
            }
        },
        OP_ECHO_COLLECTIONS => {
            var col: Collections = undefined;
            if (gen_codec.decode_Collections(a, d.payload, &col)) {
                const cb = try gen_codec.encode_Collections(a, &col);
                return (dgram.Datagram.init(OP_ECHO_COLLECTIONS, d.seq, cb)).encode(a);
            } else |_| {
                return (dgram.Datagram.init(OP_ERROR_SENTINEL, d.seq, "")).encode(a);
            }
        },
        else => return (dgram.Datagram.init(OP_ERROR_SENTINEL, d.seq, "")).encode(a),
    }
}

fn datagramRoundtrip(
    a: std.mem.Allocator,
    fd: std.posix.socket_t,
    server_addr: std.net.Address,
    buf: []u8,
    op: u64,
    seq: u64,
    payload: []const u8,
) !dgram.Datagram {
    const out = try (dgram.Datagram.init(op, seq, payload)).encode(a);
    _ = try std.posix.sendto(fd, out, 0, &server_addr.any, server_addr.getOsSockLen());
    const n = try std.posix.recvfrom(fd, buf, 0, null, null);
    return dgram.decode_datagram(a, buf[0..n]);
}

fn datagramClient(a: std.mem.Allocator, fd: std.posix.socket_t, server_addr: std.net.Address) Cases {
    var cases = Cases.init(a, "datagrams");
    var buf: [65536]u8 = undefined;

    {
        const want = scalarsOk();
        const sb = gen_codec.encode_Scalars(a, &want) catch unreachable;
        if (datagramRoundtrip(a, fd, server_addr, &buf, OP_ECHO_SCALARS, 1, sb)) |d| {
            if (d.op_ord != OP_ECHO_SCALARS) {
                cases.fail("echo-scalars/success", "wrong op_ord");
            } else {
                var s: Scalars = undefined;
                if (gen_codec.decode_Scalars(a, d.payload, &s)) {
                    cases.check("echo-scalars/success", scalarsEql(s, want), "payload mismatch");
                } else |err| {
                    cases.fail("echo-scalars/success", @errorName(err));
                }
            }
        } else |err| {
            cases.fail("echo-scalars/success", @errorName(err));
        }
    }
    {
        const want = collectionsOk(a) catch unreachable;
        const cb = gen_codec.encode_Collections(a, &want) catch unreachable;
        if (datagramRoundtrip(a, fd, server_addr, &buf, OP_ECHO_COLLECTIONS, 2, cb)) |d| {
            if (d.op_ord != OP_ECHO_COLLECTIONS) {
                cases.fail("echo-collections/success", "wrong op_ord");
            } else {
                var col: Collections = undefined;
                if (gen_codec.decode_Collections(a, d.payload, &col)) {
                    cases.check("echo-collections/success", collectionsEql(col, want), "payload mismatch");
                } else |err| {
                    cases.fail("echo-collections/success", @errorName(err));
                }
            }
        } else |err| {
            cases.fail("echo-collections/success", @errorName(err));
        }
    }
    {
        if (datagramRoundtrip(a, fd, server_addr, &buf, 99, 3, "")) |d| {
            cases.check("bad-op-ord/failure", d.op_ord == OP_ERROR_SENTINEL, "expected error sentinel");
        } else |err| {
            cases.fail("bad-op-ord/failure", @errorName(err));
        }
    }
    return cases;
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn ready() void {
    const stdout = std.io.getStdOut();
    _ = stdout.write("READY\n") catch {};
}

// Bind the TCP listener with SO_REUSEADDR so a just-freed port from the previous
// server in the matrix is tolerated.
fn tcpListen(addr: std.net.Address) std.posix.socket_t {
    const fd = std.posix.socket(std.posix.AF.INET, std.posix.SOCK.STREAM, std.posix.IPPROTO.TCP) catch unreachable;
    const one: c_int = 1;
    std.posix.setsockopt(fd, std.posix.SOL.SOCKET, std.posix.SO.REUSEADDR, std.mem.asBytes(&one)) catch {};
    std.posix.bind(fd, &addr.any, addr.getOsSockLen()) catch unreachable;
    std.posix.listen(fd, 128) catch unreachable;
    return fd;
}

// Connect over loopback TCP, retrying briefly so a server still coming up in the
// matrix is tolerated.
fn connectRetry(addr: std.net.Address) std.net.Stream {
    var i: usize = 0;
    while (i < 200) : (i += 1) {
        const fd = std.posix.socket(std.posix.AF.INET, std.posix.SOCK.STREAM, std.posix.IPPROTO.TCP) catch {
            std.time.sleep(15 * std.time.ns_per_ms);
            continue;
        };
        if (std.posix.connect(fd, &addr.any, addr.getOsSockLen())) {
            return std.net.Stream{ .handle = fd };
        } else |_| {
            std.posix.close(fd);
            std.time.sleep(15 * std.time.ns_per_ms);
        }
    }
    const fd = std.posix.socket(std.posix.AF.INET, std.posix.SOCK.STREAM, std.posix.IPPROTO.TCP) catch unreachable;
    std.posix.connect(fd, &addr.any, addr.getOsSockLen()) catch unreachable;
    return std.net.Stream{ .handle = fd };
}

pub fn main() void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const base = gpa.allocator();

    const args = std.process.argsAlloc(base) catch unreachable;
    defer std.process.argsFree(base, args);
    if (args.len < 4) {
        std.debug.print("usage: csil-interop-zig <server|client> <rpc|events|datagrams> <port>\n", .{});
        std.process.exit(2);
    }
    const mode = args[1];
    const transport_name = args[2];
    const port = std.fmt.parseInt(u16, args[3], 10) catch {
        std.debug.print("bad port\n", .{});
        std.process.exit(2);
    };
    const addr = std.net.Address.parseIp4("127.0.0.1", port) catch unreachable;

    if (streq(mode, "server") and streq(transport_name, "datagrams")) {
        const fd = std.posix.socket(std.posix.AF.INET, std.posix.SOCK.DGRAM, std.posix.IPPROTO.UDP) catch unreachable;
        std.posix.bind(fd, &addr.any, addr.getOsSockLen()) catch unreachable;
        ready();
        datagramServer(base, fd);
        return;
    }
    if (streq(mode, "server")) {
        const listen_fd = tcpListen(addr);
        ready();
        while (true) {
            const conn_fd = std.posix.accept(listen_fd, null, null, 0) catch continue;
            const stream = std.net.Stream{ .handle = conn_fd };
            if (streq(transport_name, "rpc")) {
                rpcServeConn(base, stream);
            } else if (streq(transport_name, "events")) {
                eventsServeConn(base, stream);
            } else {
                std.debug.print("bad transport\n", .{});
                std.process.exit(2);
            }
        }
    }

    if (streq(mode, "client") and streq(transport_name, "datagrams")) {
        var arena = std.heap.ArenaAllocator.init(base);
        defer arena.deinit();
        const a = arena.allocator();
        // Bind an ephemeral local UDP port; the server replies to this source.
        const fd = std.posix.socket(std.posix.AF.INET, std.posix.SOCK.DGRAM, std.posix.IPPROTO.UDP) catch unreachable;
        const local = std.net.Address.parseIp4("127.0.0.1", 0) catch unreachable;
        std.posix.bind(fd, &local.any, local.getOsSockLen()) catch unreachable;
        // Bound a 3s receive timeout so a dropped reply fails the case instead of hanging.
        const tv = std.posix.timeval{ .sec = 3, .usec = 0 };
        std.posix.setsockopt(fd, std.posix.SOL.SOCKET, std.posix.SO.RCVTIMEO, std.mem.asBytes(&tv)) catch {};
        var cases = datagramClient(a, fd, addr);
        cases.emit();
        std.posix.close(fd);
        std.process.exit(0);
    }
    if (streq(mode, "client")) {
        var arena = std.heap.ArenaAllocator.init(base);
        defer arena.deinit();
        const a = arena.allocator();
        const stream = connectRetry(addr);
        var cases = if (streq(transport_name, "rpc"))
            rpcClient(a, stream)
        else if (streq(transport_name, "events"))
            eventsClient(a, stream)
        else {
            std.debug.print("bad transport\n", .{});
            std.process.exit(2);
        };
        cases.emit();
        std.process.exit(0);
    }

    std.debug.print("bad mode/transport\n", .{});
    std.process.exit(2);
}
