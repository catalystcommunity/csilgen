//! CSIL-RPC transport — request/response/push envelopes — see csil-rpc-transport.md.
//! Ported from transports/go/rpc.go.

const std = @import("std");
const cbor = @import("cbor.zig");
const conv = @import("conventions.zig");
const carrier = @import("carrier.zig");

const Error = conv.Error;

/// A CSIL-RPC request (client → server). String/byte fields alias caller storage on
/// build and the decode arena on decode; keep that storage alive while using them.
pub const RpcRequest = struct {
    service: []const u8,
    op: []const u8,
    id: ?u64 = null,
    /// Opaque CBOR(request type) bytes (wrapped in tag 24 on the wire).
    payload: []const u8,
    auth: ?[]const u8 = null,

    pub fn init(service: []const u8, op: []const u8, payload: []const u8) RpcRequest {
        return .{ .service = service, .op = op, .payload = payload };
    }

    pub fn encode(self: RpcRequest, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "v" }, .val = .{ .uint = conv.VERSION } });
        try entries.append(.{ .key = .{ .text = "service" }, .val = .{ .text = self.service } });
        try entries.append(.{ .key = .{ .text = "op" }, .val = .{ .text = self.op } });
        try entries.append(.{ .key = .{ .text = "payload" }, .val = try conv.tag24(a, self.payload) });
        if (self.id) |id| try entries.append(.{ .key = .{ .text = "id" }, .val = .{ .uint = id } });
        if (self.auth) |auth| try entries.append(.{ .key = .{ .text = "auth" }, .val = .{ .text = auth } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }

    pub fn eql(self: RpcRequest, other: RpcRequest) bool {
        return std.mem.eql(u8, self.service, other.service) and
            std.mem.eql(u8, self.op, other.op) and
            opt_u64_eql(self.id, other.id) and
            std.mem.eql(u8, self.payload, other.payload) and
            opt_bytes_eql(self.auth, other.auth);
    }
};

pub fn decode_rpc_request(alloc: std.mem.Allocator, b: []const u8) Error!RpcRequest {
    const v = try cbor.decode_envelope(alloc, b);
    try conv.check_version(try conv.get_uint(v, "v"));
    const p = conv.map_get(v, "payload") orelse return error.Malformed;
    return .{
        .service = try conv.get_text(v, "service"),
        .op = try conv.get_text(v, "op"),
        .id = conv.get_uint_opt(v, "id"),
        .payload = try conv.untag24(p),
        .auth = conv.get_text_opt(v, "auth"),
    };
}

/// A CSIL-RPC response (server → client).
pub const RpcResponse = struct {
    id: ?u64 = null,
    status: conv.Status,
    /// Names which declared output-choice arm `payload` decodes to (the CSIL type name).
    variant: ?[]const u8 = null,
    err: ?[]const u8 = null,
    /// Opaque CBOR(output type) bytes; empty when `status` is non-zero, but still a
    /// present tag-24 wrapping empty bytes on the wire.
    payload: []const u8,

    /// ok builds a successful (status ok) typed reply.
    pub fn ok(variant: []const u8, payload: []const u8) RpcResponse {
        return .{ .status = conv.Status.ok, .variant = variant, .payload = payload };
    }

    /// transport_error builds a transport-level failure (no typed payload).
    pub fn transport_error(status: conv.Status, message: []const u8) RpcResponse {
        return .{ .status = status, .err = message, .payload = "" };
    }

    pub fn with_id(self: RpcResponse, id: ?u64) RpcResponse {
        var r = self;
        r.id = id;
        return r;
    }

    pub fn encode(self: RpcResponse, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "v" }, .val = .{ .uint = conv.VERSION } });
        try entries.append(.{ .key = .{ .text = "status" }, .val = .{ .int = self.status.code } });
        try entries.append(.{ .key = .{ .text = "payload" }, .val = try conv.tag24(a, self.payload) });
        if (self.id) |id| try entries.append(.{ .key = .{ .text = "id" }, .val = .{ .uint = id } });
        if (self.variant) |variant| try entries.append(.{ .key = .{ .text = "variant" }, .val = .{ .text = variant } });
        if (self.err) |e| try entries.append(.{ .key = .{ .text = "error" }, .val = .{ .text = e } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }

    /// as_transport_error surfaces a non-ok response as a transport error, or returns
    /// without error when the response carries a typed reply (status 0). Callers use
    /// this after decode to distinguish transport failures from application errors.
    pub fn as_transport_error(self: RpcResponse) error{Status}!void {
        if (!self.status.is_ok()) return error.Status;
    }

    pub fn eql(self: RpcResponse, other: RpcResponse) bool {
        return opt_u64_eql(self.id, other.id) and
            self.status.code == other.status.code and
            opt_bytes_eql(self.variant, other.variant) and
            opt_bytes_eql(self.err, other.err) and
            std.mem.eql(u8, self.payload, other.payload);
    }
};

pub fn decode_rpc_response(alloc: std.mem.Allocator, b: []const u8) Error!RpcResponse {
    const v = try cbor.decode_envelope(alloc, b);
    try conv.check_version(try conv.get_uint(v, "v"));
    // payload is present but may be an empty byte string on error.
    var payload: []const u8 = "";
    if (conv.map_get(v, "payload")) |p| {
        payload = try conv.untag24(p);
    }
    return .{
        .id = conv.get_uint_opt(v, "id"),
        .status = conv.Status.from_code(try conv.get_int(v, "status")),
        .variant = conv.get_text_opt(v, "variant"),
        .err = conv.get_text_opt(v, "error"),
        .payload = payload,
    };
}

/// A CSIL-RPC server push (server → client) for `<-` operations.
pub const RpcPush = struct {
    service: []const u8,
    event: []const u8,
    payload: []const u8,

    pub fn init(service: []const u8, event: []const u8, payload: []const u8) RpcPush {
        return .{ .service = service, .event = event, .payload = payload };
    }

    pub fn encode(self: RpcPush, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "v" }, .val = .{ .uint = conv.VERSION } });
        try entries.append(.{ .key = .{ .text = "service" }, .val = .{ .text = self.service } });
        try entries.append(.{ .key = .{ .text = "event" }, .val = .{ .text = self.event } });
        try entries.append(.{ .key = .{ .text = "payload" }, .val = try conv.tag24(a, self.payload) });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }

    pub fn eql(self: RpcPush, other: RpcPush) bool {
        return std.mem.eql(u8, self.service, other.service) and
            std.mem.eql(u8, self.event, other.event) and
            std.mem.eql(u8, self.payload, other.payload);
    }
};

pub fn decode_rpc_push(alloc: std.mem.Allocator, b: []const u8) Error!RpcPush {
    const v = try cbor.decode_envelope(alloc, b);
    try conv.check_version(try conv.get_uint(v, "v"));
    const p = conv.map_get(v, "payload") orelse return error.Malformed;
    return .{
        .service = try conv.get_text(v, "service"),
        .event = try conv.get_text(v, "event"),
        .payload = try conv.untag24(p),
    };
}

/// What a server handler returns for one request: a typed reply (variant name +
/// encoded payload) on success, or a transport status on failure. A tagged union
/// keeps the two outcomes from being conflated.
pub const HandlerOutcome = union(enum) {
    reply: struct { variant: []const u8, payload: []const u8 },
    transport: struct { status: conv.Status, message: []const u8 },

    pub fn make_reply(variant: []const u8, payload: []const u8) HandlerOutcome {
        return .{ .reply = .{ .variant = variant, .payload = payload } };
    }

    pub fn make_transport(status: conv.Status, message: []const u8) HandlerOutcome {
        return .{ .transport = .{ .status = status, .message = message } };
    }
};

/// A request handler the server dispatches to: a runtime context pointer plus a
/// function, the same fat-pointer seam the carrier uses. A non-generic shape keeps
/// `RpcServer` a single concrete type (and keeps the library's test reflection from
/// tripping over generic functions).
pub const Handler = struct {
    ctx: *anyopaque,
    func: *const fn (ctx: *anyopaque, req: *const RpcRequest) HandlerOutcome,

    pub fn dispatch(self: Handler, req: *const RpcRequest) HandlerOutcome {
        return self.func(self.ctx, req);
    }
};

/// A CSIL-RPC client over a frame carrier. The carrier is injected (bring your own);
/// the client owns the envelope and a per-connection monotonic correlation id.
pub const RpcClient = struct {
    carrier: carrier.FrameCarrier,
    next_id: u64 = 1,
    multiplexed: bool,

    /// init creates a client. `multiplexed` true assigns a correlation id to every
    /// request (required on WS / pipelined streams); false omits it (one-in-flight
    /// carriers such as HTTP).
    pub fn init(c: carrier.FrameCarrier, multiplexed: bool) RpcClient {
        return .{ .carrier = c, .multiplexed = multiplexed };
    }

    /// call invokes service/op with an encoded request payload. It decodes the
    /// response into `arena` (whose storage the returned value aliases) and surfaces
    /// a non-zero transport status as error.Status.
    pub fn call(
        self: *RpcClient,
        alloc: std.mem.Allocator,
        arena: std.mem.Allocator,
        service: []const u8,
        op: []const u8,
        payload: []const u8,
        auth: ?[]const u8,
    ) !RpcResponse {
        var req = RpcRequest.init(service, op, payload);
        req.auth = auth;
        if (self.multiplexed) {
            req.id = self.next_id;
            self.next_id += 1;
        }
        const frame = try req.encode(alloc);
        defer alloc.free(frame);
        try self.carrier.send(frame);
        const in = try self.carrier.recv(alloc) orelse return error.Carrier;
        defer alloc.free(in);
        const resp = try decode_rpc_response(arena, in);
        try resp.as_transport_error();
        return resp;
    }
};

/// A CSIL-RPC server over a frame carrier. The host supplies a handler mapping a
/// decoded request to an outcome; the generated router is the natural handler.
pub const RpcServer = struct {
    carrier: carrier.FrameCarrier,

    pub fn init(c: carrier.FrameCarrier) RpcServer {
        return .{ .carrier = c };
    }

    /// serve_one reads one request, dispatches it through `handler`, and writes the
    /// response. It returns false at a clean end of stream. The decode arena is
    /// freed only after the response is encoded, because a handler's reply payload
    /// may alias the decoded request's storage.
    pub fn serve_one(self: *RpcServer, alloc: std.mem.Allocator, handler: Handler) !bool {
        const frame = try self.carrier.recv(alloc) orelse return false;
        defer alloc.free(frame);

        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();

        var resp: RpcResponse = undefined;
        if (decode_rpc_request(a, frame)) |req| {
            const outcome = handler.dispatch(&req);
            resp = switch (outcome) {
                .reply => |r| RpcResponse.ok(r.variant, r.payload).with_id(req.id),
                .transport => |t| RpcResponse.transport_error(t.status, t.message).with_id(req.id),
            };
        } else |_| {
            resp = RpcResponse.transport_error(conv.Status.malformed_envelope, "malformed request envelope");
        }
        const out = try resp.encode(alloc);
        defer alloc.free(out);
        try self.carrier.send(out);
        return true;
    }
};

fn opt_u64_eql(a: ?u64, b: ?u64) bool {
    if (a == null or b == null) return a == null and b == null;
    return a.? == b.?;
}

fn opt_bytes_eql(a: ?[]const u8, b: ?[]const u8) bool {
    if (a == null or b == null) return a == null and b == null;
    return std.mem.eql(u8, a.?, b.?);
}

test "rpc request round-trips through a loopback carrier" {
    const a = std.testing.allocator;
    var lb = carrier.LoopbackFrameCarrier.init(a);
    defer lb.deinit();

    var req = RpcRequest.init("Attestation", "deposit-claim", &.{0xa0});
    req.id = 7;
    const frame = try req.encode(a);
    defer a.free(frame);

    var arena = std.heap.ArenaAllocator.init(a);
    defer arena.deinit();
    const decoded = try decode_rpc_request(arena.allocator(), frame);
    try std.testing.expect(req.eql(decoded));
}

test "rpc server dispatches a request and replies" {
    const a = std.testing.allocator;
    var srv_carrier = carrier.LoopbackFrameCarrier.init(a);
    defer srv_carrier.deinit();

    var req = RpcRequest.init("Attestation", "deposit-claim", &.{0xa0});
    req.id = 7;
    const req_frame = try req.encode(a);
    defer a.free(req_frame);
    try srv_carrier.push_inbound(req_frame);

    var server = RpcServer.init(srv_carrier.carrier());
    const H = struct {
        fn handle(_: *anyopaque, r: *const RpcRequest) HandlerOutcome {
            _ = r;
            return HandlerOutcome.make_reply("DepositClaimResponse", &.{0xa0});
        }
    };
    var dummy: u8 = 0;
    const served = try server.serve_one(a, .{ .ctx = &dummy, .func = H.handle });
    try std.testing.expect(served);

    const out = srv_carrier.take_outbound(0).?;
    var arena = std.heap.ArenaAllocator.init(a);
    defer arena.deinit();
    const resp = try decode_rpc_response(arena.allocator(), out);
    try std.testing.expect(resp.status.is_ok());
    try std.testing.expectEqualStrings("DepositClaimResponse", resp.variant.?);
    try std.testing.expectEqual(@as(u64, 7), resp.id.?);
}
