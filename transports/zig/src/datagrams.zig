//! CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
//! csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header
//! profiles. A datagram channel is single-service: the service is bound at channel
//! setup, so datagrams carry no service ordinal. Ported from
//! transports/go/datagrams.go.

const std = @import("std");
const cbor = @import("cbor.zig");
const conv = @import("conventions.zig");

const Error = conv.Error;

/// The conservative max datagram size (envelope + payload) safe across
/// UDP/WebRTC/QUIC.
pub const MAX_DATAGRAM_DEFAULT: usize = 1200;

/// A datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]. All
/// three numeric positions are full CBOR uints in this profile.
pub const Datagram = struct {
    op_ord: u64,
    /// Per-channel sequence; 0 means "unsequenced".
    seq: u64,
    /// Opaque CBOR(message type) bytes.
    payload: []const u8,

    pub fn init(op_ord: u64, seq: u64, payload: []const u8) Datagram {
        return .{ .op_ord = op_ord, .seq = seq, .payload = payload };
    }

    pub fn encode(self: Datagram, alloc: std.mem.Allocator) Error![]u8 {
        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        const arr = [_]cbor.Value{
            .{ .uint = conv.VERSION },
            .{ .uint = self.op_ord },
            .{ .uint = self.seq },
            try conv.tag24(a, self.payload),
        };
        return try cbor.encode(alloc, .{ .array = &arr });
    }

    pub fn eql(self: Datagram, other: Datagram) bool {
        return self.op_ord == other.op_ord and self.seq == other.seq and
            std.mem.eql(u8, self.payload, other.payload);
    }
};

pub fn decode_datagram(alloc: std.mem.Allocator, b: []const u8) Error!Datagram {
    const v = try cbor.decode_envelope(alloc, b);
    const arr = switch (v) {
        .array => |x| x,
        else => return error.Malformed,
    };
    if (arr.len != 4) return error.Malformed;
    try conv.check_version(conv.as_u64(arr[0]) orelse return error.Malformed);
    return .{
        .op_ord = conv.as_u64(arr[1]) orelse return error.Malformed,
        .seq = conv.as_u64(arr[2]) orelse return error.Malformed,
        .payload = try conv.untag24(arr[3]),
    };
}

const COMPACT_VER: u8 = 1;
const FLAG_EPOCH: u8 = 0b0001;

/// A datagram in the compact fixed-header profile. Header layout:
/// [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then the opaque body. The integer
/// widths are fixed by the spec (not CBOR): op_ord=u8, seq=u16, epoch=u8.
pub const CompactDatagram = struct {
    op_ord: u8,
    seq: u16,
    /// Present when the sender tracks restarts (sets the flags epoch bit).
    epoch: ?u8 = null,
    /// Opaque body bytes (tag-24 CBOR or a raw media frame, by channel agreement).
    body: []const u8,

    pub fn init(op_ord: u8, seq: u16, body: []const u8) CompactDatagram {
        return .{ .op_ord = op_ord, .seq = seq, .body = body };
    }

    /// with_epoch sets the epoch byte (and the flags bit that signals its presence).
    pub fn with_epoch(self: CompactDatagram, epoch: u8) CompactDatagram {
        var d = self;
        d.epoch = epoch;
        return d;
    }

    pub fn encode(self: CompactDatagram, alloc: std.mem.Allocator) std.mem.Allocator.Error![]u8 {
        var out = std.ArrayList(u8).init(alloc);
        errdefer out.deinit();
        const flags: u8 = if (self.epoch != null) FLAG_EPOCH else 0;
        try out.append((COMPACT_VER << 4) | (flags & 0x0f));
        try out.append(self.op_ord);
        try out.append(@intCast(self.seq >> 8));
        try out.append(@intCast(self.seq & 0xff));
        if (self.epoch) |e| try out.append(e);
        try out.appendSlice(self.body);
        return try out.toOwnedSlice();
    }

    pub fn eql(self: CompactDatagram, other: CompactDatagram) bool {
        const epoch_eql = if (self.epoch == null or other.epoch == null)
            self.epoch == null and other.epoch == null
        else
            self.epoch.? == other.epoch.?;
        return self.op_ord == other.op_ord and self.seq == other.seq and
            epoch_eql and std.mem.eql(u8, self.body, other.body);
    }
};

pub fn decode_compact_datagram(alloc: std.mem.Allocator, b: []const u8) Error!CompactDatagram {
    if (b.len < 4) return error.Malformed;
    const ver = b[0] >> 4;
    if (ver != COMPACT_VER) return error.UnsupportedVersion;
    const flags = b[0] & 0x0f;
    const op_ord = b[1];
    const seq: u16 = (@as(u16, b[2]) << 8) | @as(u16, b[3]);
    var epoch: ?u8 = null;
    var body_start: usize = 4;
    if ((flags & FLAG_EPOCH) != 0) {
        if (b.len < 5) return error.Malformed;
        epoch = b[4];
        body_start = 5;
    }
    return .{
        .op_ord = op_ord,
        .seq = seq,
        .epoch = epoch,
        .body = try alloc.dupe(u8, b[body_start..]),
    };
}

/// Classifies an incoming sequence number relative to what was last seen, for
/// loss/reorder/restart detection. The transport detects; the app decides.
pub const SeqEventKind = enum {
    /// The first datagram seen on the channel.
    first,
    /// Strictly newer than the last (possibly skipping some — a gap/loss).
    advanced,
    /// Not newer (a late or duplicate datagram).
    late_or_duplicate,
    /// The sender restarted (epoch changed); seq numbering reset.
    restart,
};

/// A sequence classification plus the gap count for `advanced`.
pub const SeqEvent = struct {
    kind: SeqEventKind,
    /// The number of skipped sequence numbers (meaningful only for `advanced`).
    gap: u64 = 0,
};

/// Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced
/// datagrams (seq 0) are reported as `advanced` with gap 0.
pub const SeqTracker = struct {
    last_seq: ?u64 = null,
    last_epoch: ?u8 = null,

    pub fn init() SeqTracker {
        return .{};
    }

    /// observe classifies an arriving (seq, epoch) and updates the tracker state. A
    /// restart fires only when a *prior* epoch existed and changed; going from
    /// no-epoch to the first epoch is not a restart.
    pub fn observe(self: *SeqTracker, seq: u64, epoch: ?u8) SeqEvent {
        if (!epoch_equal(epoch, self.last_epoch) and self.last_epoch != null) {
            self.last_epoch = epoch;
            self.last_seq = seq;
            return .{ .kind = .restart };
        }
        self.last_epoch = epoch;
        // seq 0 marks an unsequenced datagram: it carries no ordering information, so
        // it is never late or duplicate. Report a zero-gap advance and leave the
        // running sequence untouched so a mix of sequenced and unsequenced still
        // tracks the sequenced ones. Matches the Go/Rust reference SeqTracker.
        if (seq == 0) return .{ .kind = .advanced, .gap = 0 };
        if (self.last_seq == null) {
            self.last_seq = seq;
            return .{ .kind = .first };
        }
        const last = self.last_seq.?;
        if (seq > last) {
            const gap = seq - last - 1;
            self.last_seq = seq;
            return .{ .kind = .advanced, .gap = gap };
        }
        return .{ .kind = .late_or_duplicate };
    }
};

fn epoch_equal(a: ?u8, b: ?u8) bool {
    if (a == null or b == null) return a == null and b == null;
    return a.? == b.?;
}

test "cbor-array datagram round-trips" {
    const a = std.testing.allocator;
    const d = Datagram.init(0, 5, &.{0xa0});
    const out = try d.encode(a);
    defer a.free(out);
    try std.testing.expectEqualSlices(u8, &.{ 0x84, 0x01, 0x00, 0x05, 0xd8, 0x18, 0x41, 0xa0 }, out);

    var arena = std.heap.ArenaAllocator.init(a);
    defer arena.deinit();
    const dec = try decode_datagram(arena.allocator(), out);
    try std.testing.expect(d.eql(dec));
}

test "compact datagram with epoch round-trips" {
    const a = std.testing.allocator;
    const d = CompactDatagram.init(2, 7, &.{0x09}).with_epoch(4);
    const out = try d.encode(a);
    defer a.free(out);
    try std.testing.expectEqualSlices(u8, &.{ 0x11, 0x02, 0x00, 0x07, 0x04, 0x09 }, out);

    var arena = std.heap.ArenaAllocator.init(a);
    defer arena.deinit();
    const dec = try decode_compact_datagram(arena.allocator(), out);
    try std.testing.expect(d.eql(dec));
}

test "sequence tracker classifies first, advance, restart" {
    var t = SeqTracker.init();
    try std.testing.expectEqual(SeqEventKind.first, t.observe(1, null).kind);
    const adv = t.observe(4, null);
    try std.testing.expectEqual(SeqEventKind.advanced, adv.kind);
    try std.testing.expectEqual(@as(u64, 2), adv.gap);
    try std.testing.expectEqual(SeqEventKind.late_or_duplicate, t.observe(2, null).kind);

    // First epoch after a no-epoch history is not a restart.
    var e = SeqTracker.init();
    try std.testing.expectEqual(SeqEventKind.first, e.observe(1, 0).kind);
    try std.testing.expectEqual(SeqEventKind.restart, e.observe(1, 1).kind);
}
