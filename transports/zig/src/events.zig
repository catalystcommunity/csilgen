//! CSIL-Events transport — typed bidirectional event streams — see
//! csil-events-transport.md. Verbose (text-keyed) and compact (positional)
//! profiles, plus the control plane (service ordinal 0) lifecycle events. Ported
//! from transports/go/events.go.

const std = @import("std");
const cbor = @import("cbor.zig");
const conv = @import("conventions.zig");

const Error = conv.Error;

/// Which wire profile a connection uses for its lifetime, fixed by hello.
pub const Profile = enum {
    verbose,
    compact,

    pub fn to_string(self: Profile) []const u8 {
        return switch (self) {
            .verbose => "verbose",
            .compact => "compact",
        };
    }

    pub fn parse(s: []const u8) ?Profile {
        if (std.mem.eql(u8, s, "verbose")) return .verbose;
        if (std.mem.eql(u8, s, "compact")) return .compact;
        return null;
    }
};

/// One typed event flowing in either direction. It is identified by service+operation
/// (verbose) or by their ordinals (compact), and carries an optional correlation id
/// when it is a request expecting a reply, or that reply.
pub const Event = struct {
    /// CSIL service name (verbose); null on a single-service verbose connection.
    service: ?[]const u8 = null,
    /// Service ordinal (compact); always present in compact frames.
    service_ord: ?u64 = null,
    /// CSIL operation name (verbose).
    event: ?[]const u8 = null,
    /// Operation ordinal (compact).
    op_ord: ?u64 = null,
    id: ?u64 = null,
    payload: []const u8,

    pub fn verbose(service: ?[]const u8, event: []const u8, payload: []const u8) Event {
        return .{ .service = service, .event = event, .payload = payload };
    }

    pub fn compact(service_ord: u64, op_ord: u64, payload: []const u8) Event {
        return .{ .service_ord = service_ord, .op_ord = op_ord, .payload = payload };
    }

    pub fn encode(self: Event, alloc: std.mem.Allocator, profile: Profile) Error![]u8 {
        return switch (profile) {
            .verbose => self.encode_verbose(alloc),
            .compact => self.encode_compact(alloc),
        };
    }

    fn encode_verbose(self: Event, alloc: std.mem.Allocator) Error![]u8 {
        const event = self.event orelse return error.Malformed;
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "event" }, .val = .{ .text = event } });
        try entries.append(.{ .key = .{ .text = "payload" }, .val = try conv.tag24(a, self.payload) });
        if (self.service) |service| try entries.append(.{ .key = .{ .text = "service" }, .val = .{ .text = service } });
        if (self.id) |id| try entries.append(.{ .key = .{ .text = "id" }, .val = .{ .uint = id } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }

    fn encode_compact(self: Event, alloc: std.mem.Allocator) Error![]u8 {
        const service_ord = self.service_ord orelse return error.Malformed;
        const op_ord = self.op_ord orelse return error.Malformed;
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var arr = std.ArrayList(cbor.Value).init(a);
        try arr.append(.{ .uint = service_ord });
        try arr.append(.{ .uint = op_ord });
        if (self.id) |id| try arr.append(.{ .uint = id });
        try arr.append(try conv.tag24(a, self.payload));
        return try cbor.encode(alloc, .{ .array = arr.items });
    }

    pub fn eql(self: Event, other: Event) bool {
        return opt_bytes_eql(self.service, other.service) and
            opt_u64_eql(self.service_ord, other.service_ord) and
            opt_bytes_eql(self.event, other.event) and
            opt_u64_eql(self.op_ord, other.op_ord) and
            opt_u64_eql(self.id, other.id) and
            std.mem.eql(u8, self.payload, other.payload);
    }
};

pub fn decode_event(alloc: std.mem.Allocator, b: []const u8, profile: Profile) Error!Event {
    return switch (profile) {
        .verbose => decode_verbose_event(alloc, b),
        .compact => decode_compact_event(alloc, b),
    };
}

fn decode_verbose_event(alloc: std.mem.Allocator, b: []const u8) Error!Event {
    const v = try cbor.decode_envelope(alloc, b);
    const p = conv.map_get(v, "payload") orelse return error.Malformed;
    return .{
        .service = conv.get_text_opt(v, "service"),
        .event = try conv.get_text(v, "event"),
        .id = conv.get_uint_opt(v, "id"),
        .payload = try conv.untag24(p),
    };
}

fn decode_compact_event(alloc: std.mem.Allocator, b: []const u8) Error!Event {
    const v = try cbor.decode_envelope(alloc, b);
    const arr = switch (v) {
        .array => |x| x,
        else => return error.Malformed,
    };
    // 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
    var id_v: ?cbor.Value = null;
    var service_ord_v: cbor.Value = undefined;
    var op_ord_v: cbor.Value = undefined;
    var payload_v: cbor.Value = undefined;
    switch (arr.len) {
        3 => {
            service_ord_v = arr[0];
            op_ord_v = arr[1];
            payload_v = arr[2];
        },
        4 => {
            service_ord_v = arr[0];
            op_ord_v = arr[1];
            id_v = arr[2];
            payload_v = arr[3];
        },
        else => return error.Malformed,
    }
    var ev = Event{
        .service_ord = conv.as_u64(service_ord_v) orelse return error.Malformed,
        .op_ord = conv.as_u64(op_ord_v) orelse return error.Malformed,
        .payload = try conv.untag24(payload_v),
    };
    if (id_v) |idv| ev.id = conv.as_u64(idv) orelse return error.Malformed;
    return ev;
}

/// Control-plane operation ordinals (under service ordinal 0).
pub const control = struct {
    pub const hello: u64 = 0;
    pub const hello_ack: u64 = 1;
    pub const ping: u64 = 2;
    pub const pong: u64 = 3;
    pub const close: u64 = 4;
    pub const err: u64 = 5;
};

/// Verbose control-event names (the `$`-sigil names).
pub const HELLO_NAME = "$hello";
pub const HELLO_ACK_NAME = "$hello-ack";
pub const PING_NAME = "$ping";
pub const PONG_NAME = "$pong";
pub const CLOSE_NAME = "$close";
pub const ERROR_NAME = "$error";

/// The `$hello` payload offered by the connection initiator.
pub const Hello = struct {
    versions: []const u64,
    profiles: []const []const u8,
    service: ?[]const u8 = null,
    auth: ?[]const u8 = null,

    pub fn encode(self: Hello, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();

        const versions = try a.alloc(cbor.Value, self.versions.len);
        for (self.versions, 0..) |x, i| versions[i] = .{ .uint = x };
        const profiles = try a.alloc(cbor.Value, self.profiles.len);
        for (self.profiles, 0..) |x, i| profiles[i] = .{ .text = x };

        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "versions" }, .val = .{ .array = versions } });
        try entries.append(.{ .key = .{ .text = "profiles" }, .val = .{ .array = profiles } });
        if (self.service) |service| try entries.append(.{ .key = .{ .text = "service" }, .val = .{ .text = service } });
        if (self.auth) |auth| try entries.append(.{ .key = .{ .text = "auth" }, .val = .{ .text = auth } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }

    /// negotiate selects a profile from this hello's offers, honoring the peer's
    /// preference order and what the receiver supports. It returns null if nothing is
    /// mutually supported.
    pub fn negotiate(self: Hello, supported: []const Profile) ?struct { version: u64, profile: Profile } {
        var version: ?u64 = null;
        for (self.versions) |v| {
            if (v == conv.VERSION) {
                version = v;
                break;
            }
        }
        const ver = version orelse return null;
        for (self.profiles) |offered| {
            const p = Profile.parse(offered) orelse continue;
            for (supported) |s| {
                if (s == p) return .{ .version = ver, .profile = p };
            }
        }
        return null;
    }
};

pub fn decode_hello(alloc: std.mem.Allocator, b: []const u8) Error!Hello {
    const v = try cbor.decode_envelope(alloc, b);
    const vers_raw = conv.map_get(v, "versions") orelse return error.Malformed;
    const vers_arr = switch (vers_raw) {
        .array => |x| x,
        else => return error.Malformed,
    };
    var versions = std.ArrayList(u64).init(alloc);
    for (vers_arr) |x| {
        if (conv.as_u64(x)) |n| try versions.append(n);
    }
    const prof_raw = conv.map_get(v, "profiles") orelse return error.Malformed;
    const prof_arr = switch (prof_raw) {
        .array => |x| x,
        else => return error.Malformed,
    };
    var profiles = std.ArrayList([]const u8).init(alloc);
    for (prof_arr) |x| {
        switch (x) {
            .text => |t| try profiles.append(t),
            else => {},
        }
    }
    return .{
        .versions = try versions.toOwnedSlice(),
        .profiles = try profiles.toOwnedSlice(),
        .service = conv.get_text_opt(v, "service"),
        .auth = conv.get_text_opt(v, "auth"),
    };
}

/// The `$hello-ack` payload returned by the peer.
pub const HelloAck = struct {
    v: u64,
    profile: []const u8,
    session: ?[]const u8 = null,

    pub fn encode(self: HelloAck, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "v" }, .val = .{ .uint = self.v } });
        try entries.append(.{ .key = .{ .text = "profile" }, .val = .{ .text = self.profile } });
        if (self.session) |session| try entries.append(.{ .key = .{ .text = "session" }, .val = .{ .text = session } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }
};

pub fn decode_hello_ack(alloc: std.mem.Allocator, b: []const u8) Error!HelloAck {
    const v = try cbor.decode_envelope(alloc, b);
    return .{
        .v = try conv.get_uint(v, "v"),
        .profile = try conv.get_text(v, "profile"),
        .session = conv.get_text_opt(v, "session"),
    };
}

/// A `$ping`/`$pong` heartbeat payload.
pub const Heartbeat = struct {
    nonce: u64,
    at: ?u64 = null,

    pub fn encode(self: Heartbeat, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "nonce" }, .val = .{ .uint = self.nonce } });
        if (self.at) |at| try entries.append(.{ .key = .{ .text = "at" }, .val = .{ .uint = at } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }
};

pub fn decode_heartbeat(alloc: std.mem.Allocator, b: []const u8) Error!Heartbeat {
    const v = try cbor.decode_envelope(alloc, b);
    return .{
        .nonce = try conv.get_uint(v, "nonce"),
        .at = conv.get_uint_opt(v, "at"),
    };
}

/// A `$close` payload.
pub const Close = struct {
    status: conv.Status,
    reason: ?[]const u8 = null,

    pub fn encode(self: Close, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        var entries = std.ArrayList(cbor.Entry).init(a);
        try entries.append(.{ .key = .{ .text = "status" }, .val = .{ .int = self.status.code } });
        if (self.reason) |reason| try entries.append(.{ .key = .{ .text = "reason" }, .val = .{ .text = reason } });
        const sorted = try cbor.canon_map(a, entries.items);
        return try cbor.encode(alloc, .{ .map = sorted });
    }
};

pub fn decode_close(alloc: std.mem.Allocator, b: []const u8) Error!Close {
    const v = try cbor.decode_envelope(alloc, b);
    return .{
        .status = conv.Status.from_code(try conv.get_int(v, "status")),
        .reason = conv.get_text_opt(v, "reason"),
    };
}

fn opt_u64_eql(a: ?u64, b: ?u64) bool {
    if (a == null or b == null) return a == null and b == null;
    return a.? == b.?;
}

fn opt_bytes_eql(a: ?[]const u8, b: ?[]const u8) bool {
    if (a == null or b == null) return a == null and b == null;
    return std.mem.eql(u8, a.?, b.?);
}

test "verbose event round-trips" {
    const a = std.testing.allocator;
    const ev = Event.verbose("World", "room-state", &.{0xa0});
    var with_id = ev;
    with_id.id = 42;
    const out = try with_id.encode(a, .verbose);
    defer a.free(out);

    var arena = std.heap.ArenaAllocator.init(a);
    defer arena.deinit();
    const dec = try decode_event(arena.allocator(), out, .verbose);
    try std.testing.expect(with_id.eql(dec));
}

test "hello negotiates the first mutually supported profile" {
    const versions = [_]u64{1};
    const profiles = [_][]const u8{ "compact", "verbose" };
    const h = Hello{ .versions = &versions, .profiles = &profiles, .service = "World" };
    const chosen = h.negotiate(&.{ .verbose, .compact }) orelse return error.TestExpectedNegotiation;
    try std.testing.expectEqual(Profile.compact, chosen.profile);
}
