//! Verify the Zig library against the checked-in conformance vectors.
//!
//! This both guards the Zig encoders/decoders against drift and demonstrates the
//! contract every reference library follows: reconstruct each vector's envelope from
//! its language-neutral `input`, assert encode → `hex`, and assert decode(`hex`) →
//! the same envelope. Mirrors transports/go/conformance_test.go.
//!
//! The vector JSON is injected at build time as a string (see build.zig) rather than
//! read from disk, so the test does not depend on the run step's working directory.

const std = @import("std");
const json = std.json;
const csil = @import("csilgen_transport");
const vectors = @import("conformance_vectors");

const cbor = csil.cbor;
const conv = csil.conventions;
const rpc = csil.rpc;
const events = csil.events;
const datagrams = csil.datagrams;

fn opt_str(obj: json.Value, key: []const u8) ?[]const u8 {
    const v = obj.object.get(key) orelse return null;
    return switch (v) {
        .string => |s| s,
        else => null,
    };
}

fn opt_u64(obj: json.Value, key: []const u8) ?u64 {
    const v = obj.object.get(key) orelse return null;
    return switch (v) {
        .integer => |i| @intCast(i),
        else => null,
    };
}

fn req_str(obj: json.Value, key: []const u8) []const u8 {
    return opt_str(obj, key).?;
}

fn req_u64(obj: json.Value, key: []const u8) u64 {
    return opt_u64(obj, key).?;
}

fn req_i64(obj: json.Value, key: []const u8) i64 {
    return switch (obj.object.get(key).?) {
        .integer => |i| i,
        else => unreachable,
    };
}

fn json_u64_array(a: std.mem.Allocator, obj: json.Value, key: []const u8) ![]u64 {
    const arr = obj.object.get(key).?.array;
    const out = try a.alloc(u64, arr.items.len);
    for (arr.items, 0..) |v, i| out[i] = @intCast(v.integer);
    return out;
}

fn json_str_array(a: std.mem.Allocator, obj: json.Value, key: []const u8) ![][]const u8 {
    const arr = obj.object.get(key).?.array;
    const out = try a.alloc([]const u8, arr.items.len);
    for (arr.items, 0..) |v, i| out[i] = v.string;
    return out;
}

/// unhex decodes a hex string into freshly allocated bytes. An empty string yields
/// an empty (non-null) slice, matching the conformance "" payloads.
fn unhex(a: std.mem.Allocator, s: []const u8) ![]u8 {
    const out = try a.alloc(u8, s.len / 2);
    return std.fmt.hexToBytes(out, s);
}

fn to_hex(a: std.mem.Allocator, bytes: []const u8) ![]u8 {
    const digits = "0123456789abcdef";
    const out = try a.alloc(u8, bytes.len * 2);
    for (bytes, 0..) |b, i| {
        out[i * 2] = digits[b >> 4];
        out[i * 2 + 1] = digits[b & 0x0f];
    }
    return out;
}

const VectorIter = struct {
    parsed: json.Parsed(json.Value),
    items: []json.Value,
    idx: usize = 0,

    fn open(a: std.mem.Allocator, source: []const u8) !VectorIter {
        const parsed = try json.parseFromSlice(json.Value, a, source, .{});
        return .{ .parsed = parsed, .items = parsed.value.object.get("vectors").?.array.items };
    }
    fn deinit(self: *VectorIter) void {
        self.parsed.deinit();
    }
    fn next(self: *VectorIter) ?json.Value {
        if (self.idx >= self.items.len) return null;
        const v = self.items[self.idx];
        self.idx += 1;
        return v;
    }
};

test "rpc conformance vectors" {
    var it = try VectorIter.open(std.testing.allocator, vectors.rpc_json);
    defer it.deinit();
    while (it.next()) |vec| {
        var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
        defer arena.deinit();
        const a = arena.allocator();
        const name = req_str(vec, "name");
        const want_hex = req_str(vec, "hex");
        const in = vec.object.get("input").?;
        const kind = req_str(in, "kind");

        var out: []u8 = undefined;
        if (std.mem.eql(u8, kind, "request")) {
            var req = rpc.RpcRequest.init(req_str(in, "service"), req_str(in, "op"), try unhex(a, req_str(in, "payload_hex")));
            req.id = opt_u64(in, "id");
            req.auth = opt_str(in, "auth");
            out = try req.encode(a);
            const dec = try rpc.decode_rpc_request(a, out);
            try std.testing.expect(req.eql(dec));
        } else if (std.mem.eql(u8, kind, "response")) {
            const resp = rpc.RpcResponse{
                .id = opt_u64(in, "id"),
                .status = conv.Status.from_code(req_i64(in, "status")),
                .variant = opt_str(in, "variant"),
                .err = opt_str(in, "error"),
                .payload = try unhex(a, req_str(in, "payload_hex")),
            };
            out = try resp.encode(a);
            const dec = try rpc.decode_rpc_response(a, out);
            try std.testing.expect(resp.eql(dec));
        } else if (std.mem.eql(u8, kind, "push")) {
            const push = rpc.RpcPush.init(req_str(in, "service"), req_str(in, "event"), try unhex(a, req_str(in, "payload_hex")));
            out = try push.encode(a);
            const dec = try rpc.decode_rpc_push(a, out);
            try std.testing.expect(push.eql(dec));
        } else {
            std.debug.print("unknown rpc kind {s}\n", .{kind});
            return error.UnknownKind;
        }
        const got = try to_hex(a, out);
        std.testing.expectEqualStrings(want_hex, got) catch |e| {
            std.debug.print("rpc vector {s} hex mismatch\n", .{name});
            return e;
        };
    }
}

test "events conformance vectors" {
    var it = try VectorIter.open(std.testing.allocator, vectors.events_json);
    defer it.deinit();
    while (it.next()) |vec| {
        var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
        defer arena.deinit();
        const a = arena.allocator();
        const name = req_str(vec, "name");
        const want_hex = req_str(vec, "hex");
        const in = vec.object.get("input").?;

        var out: []u8 = undefined;
        if (opt_str(in, "control")) |control| {
            if (std.mem.eql(u8, control, "hello")) {
                const h = events.Hello{
                    .versions = try json_u64_array(a, in, "versions"),
                    .profiles = try json_str_array(a, in, "profiles"),
                    .service = opt_str(in, "service"),
                    .auth = opt_str(in, "auth"),
                };
                out = try h.encode(a);
            } else if (std.mem.eql(u8, control, "hello_ack")) {
                const h = events.HelloAck{ .v = req_u64(in, "v"), .profile = req_str(in, "profile"), .session = opt_str(in, "session") };
                out = try h.encode(a);
            } else if (std.mem.eql(u8, control, "ping")) {
                const h = events.Heartbeat{ .nonce = req_u64(in, "nonce"), .at = opt_u64(in, "at") };
                out = try h.encode(a);
            } else if (std.mem.eql(u8, control, "close")) {
                const h = events.Close{ .status = conv.Status.from_code(req_i64(in, "status")), .reason = opt_str(in, "reason") };
                out = try h.encode(a);
            } else {
                std.debug.print("unknown control {s}\n", .{control});
                return error.UnknownControl;
            }
            const got = try to_hex(a, out);
            std.testing.expectEqualStrings(want_hex, got) catch |e| {
                std.debug.print("events vector {s} hex mismatch\n", .{name});
                return e;
            };
            continue;
        }

        const profile = events.Profile.parse(req_str(in, "profile")).?;
        const payload = try unhex(a, req_str(in, "payload_hex"));
        var ev = switch (profile) {
            .verbose => events.Event.verbose(opt_str(in, "service"), req_str(in, "event"), payload),
            .compact => events.Event.compact(req_u64(in, "service_ord"), req_u64(in, "op_ord"), payload),
        };
        ev.id = opt_u64(in, "id");
        out = try ev.encode(a, profile);
        const got = try to_hex(a, out);
        std.testing.expectEqualStrings(want_hex, got) catch |e| {
            std.debug.print("events vector {s} hex mismatch\n", .{name});
            return e;
        };
        const dec = try events.decode_event(a, out, profile);
        try std.testing.expect(ev.eql(dec));
    }
}

test "datagrams conformance vectors" {
    var it = try VectorIter.open(std.testing.allocator, vectors.datagrams_json);
    defer it.deinit();
    while (it.next()) |vec| {
        var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
        defer arena.deinit();
        const a = arena.allocator();
        const name = req_str(vec, "name");
        const want_hex = req_str(vec, "hex");
        const in = vec.object.get("input").?;
        const profile = req_str(in, "profile");

        var out: []u8 = undefined;
        if (std.mem.eql(u8, profile, "cbor-array")) {
            const d = datagrams.Datagram.init(req_u64(in, "op_ord"), req_u64(in, "seq"), try unhex(a, req_str(in, "payload_hex")));
            out = try d.encode(a);
            const dec = try datagrams.decode_datagram(a, out);
            try std.testing.expect(d.eql(dec));
        } else if (std.mem.eql(u8, profile, "compact-header")) {
            var d = datagrams.CompactDatagram.init(@intCast(req_u64(in, "op_ord")), @intCast(req_u64(in, "seq")), try unhex(a, req_str(in, "body_hex")));
            if (opt_u64(in, "epoch")) |e| d = d.with_epoch(@intCast(e));
            out = try d.encode(a);
            const dec = try datagrams.decode_compact_datagram(a, out);
            try std.testing.expect(d.eql(dec));
        } else {
            std.debug.print("unknown datagram profile {s}\n", .{profile});
            return error.UnknownProfile;
        }
        const got = try to_hex(a, out);
        std.testing.expectEqualStrings(want_hex, got) catch |e| {
            std.debug.print("datagrams vector {s} hex mismatch\n", .{name});
            return e;
        };
    }
}
