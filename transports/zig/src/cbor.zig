//! Minimal canonical CBOR codec (RFC 8949), hand-written and dependency-free so
//! the transport library stays offline-testable. It supports exactly what the CSIL
//! envelopes need — unsigned ints, negative ints, text strings, byte strings,
//! arrays, maps, and tags — and nothing else. Maps are encoded with core
//! deterministic encoding: entries sorted by the bytewise lexicographic order of
//! their encoded keys, matching the Go/Rust references so bytes are byte-identical
//! to the conformance vectors. Ported near-line-for-line from transports/go/cbor.go.

const std = @import("std");

/// Errors the codec can raise. Allocation failures join the set because decode
/// owns the decoded tree's storage and encode owns the output buffer.
pub const Error = error{
    /// A structurally invalid item: an unsupported major type, an indefinite or
    /// reserved additional-info code, or a negative integer below the i64 floor.
    Malformed,
    /// The input ended in the middle of an item (truncated head or content).
    UnexpectedEof,
    /// An envelope decoded to one item but extra bytes remained after it.
    TrailingBytes,
} || std.mem.Allocator.Error;

/// One CBOR map entry. The map itself is `Value{ .map = []const Entry }`.
pub const Entry = struct { key: Value, val: Value };

/// A tagged item. Content is heap-indirected because a tagged union may not embed
/// itself by value.
pub const Tag = struct { num: u64, content: *const Value };

/// The in-memory model of the CBOR items the envelopes use. Decoding produces
/// these and encoding consumes them; transports build envelopes from these
/// canonical values so byte layout is independent of Zig struct layout.
///
/// Integers split two ways on purpose: `.uint` is always major type 0, while
/// `.int` encodes major type 0 when non-negative and major type 1 when negative —
/// mirroring Go's `cUint`/`cInt`, so a signed field like `status` round-trips.
pub const Value = union(enum) {
    uint: u64,
    int: i64,
    bytes: []const u8,
    text: []const u8,
    array: []const Value,
    map: []const Entry,
    tag: Tag,
};

/// write_head emits the initial byte (major type in the high three bits) plus the
/// shortest-form argument bytes for `n`, per deterministic encoding.
pub fn write_head(out: *std.ArrayList(u8), major: u8, n: u64) std.mem.Allocator.Error!void {
    const mt: u8 = major << 5;
    if (n < 24) {
        try out.append(mt | @as(u8, @intCast(n)));
    } else if (n < (1 << 8)) {
        try out.append(mt | 24);
        try out.append(@intCast(n));
    } else if (n < (1 << 16)) {
        try out.append(mt | 25);
        var buf: [2]u8 = undefined;
        std.mem.writeInt(u16, &buf, @intCast(n), .big);
        try out.appendSlice(&buf);
    } else if (n < (1 << 32)) {
        try out.append(mt | 26);
        var buf: [4]u8 = undefined;
        std.mem.writeInt(u32, &buf, @intCast(n), .big);
        try out.appendSlice(&buf);
    } else {
        try out.append(mt | 27);
        var buf: [8]u8 = undefined;
        std.mem.writeInt(u64, &buf, n, .big);
        try out.appendSlice(&buf);
    }
}

fn encode_into(out: *std.ArrayList(u8), v: Value) std.mem.Allocator.Error!void {
    switch (v) {
        .uint => |x| try write_head(out, 0, x),
        .int => |x| {
            if (x >= 0) {
                try write_head(out, 0, @intCast(x));
            } else {
                // CBOR negative ints encode -1-n; the argument is the magnitude
                // minus one. Computing -(x+1) in i64 first avoids overflowing at
                // the i64 floor before the widen to u64.
                const mag: u64 = @intCast(-(x + 1));
                try write_head(out, 1, mag);
            }
        },
        .text => |s| {
            try write_head(out, 3, s.len);
            try out.appendSlice(s);
        },
        .bytes => |s| {
            try write_head(out, 2, s.len);
            try out.appendSlice(s);
        },
        .array => |arr| {
            try write_head(out, 4, arr.len);
            for (arr) |e| try encode_into(out, e);
        },
        .map => |m| {
            try write_head(out, 5, m.len);
            for (m) |e| {
                try encode_into(out, e.key);
                try encode_into(out, e.val);
            }
        },
        .tag => |t| {
            try write_head(out, 6, t.num);
            try encode_into(out, t.content.*);
        },
    }
}

/// encode serializes a value to canonical CBOR bytes. The returned slice is owned
/// by the caller, who frees it with `alloc.free`.
pub fn encode(alloc: std.mem.Allocator, v: Value) std.mem.Allocator.Error![]u8 {
    var out = std.ArrayList(u8).init(alloc);
    errdefer out.deinit();
    try encode_into(&out, v);
    return try out.toOwnedSlice();
}

/// canon_map returns a deterministically-keyed copy of `entries`: sorted by the
/// bytewise lexicographic order of their *encoded* keys (RFC 8949 §4.2.1), so the
/// same logical envelope always yields the same bytes. The returned slice and the
/// scratch key encodings are allocated from `alloc` (use an arena that the caller
/// frees in one shot).
pub fn canon_map(alloc: std.mem.Allocator, entries: []const Entry) std.mem.Allocator.Error![]Entry {
    const Keyed = struct { enc: []u8, entry: Entry };
    const keyed = try alloc.alloc(Keyed, entries.len);
    for (entries, 0..) |e, i| {
        keyed[i] = .{ .enc = try encode(alloc, e.key), .entry = e };
    }
    std.mem.sort(Keyed, keyed, {}, struct {
        fn lessThan(_: void, a: Keyed, b: Keyed) bool {
            return std.mem.order(u8, a.enc, b.enc) == .lt;
        }
    }.lessThan);
    const out = try alloc.alloc(Entry, entries.len);
    for (keyed, 0..) |k, i| out[i] = k.entry;
    return out;
}

/// The result of decoding one item: the value plus how many bytes it consumed.
pub const Decoded = struct { value: Value, consumed: usize };

/// decode_envelope decodes a complete envelope: one self-contained CBOR item with no
/// trailing bytes. The conventions doc says an envelope is a single CBOR item, so
/// leftover bytes are a malformed frame and rejected — matching the references
/// rather than silently ignoring them. All decoded storage is owned by `alloc`
/// (pass an arena); the caller frees the whole tree at once.
pub fn decode_envelope(alloc: std.mem.Allocator, b: []const u8) Error!Value {
    const d = try decode_value_depth(alloc, b, 0);
    if (d.consumed != b.len) return error.TrailingBytes;
    return d.value;
}

/// decode_value parses one CBOR item from the front of `b`, returning it and the
/// number of bytes consumed. It may leave trailing bytes (it is the recursive
/// workhorse used for nested items); envelope decoders use decode_envelope.
pub fn decode_value(alloc: std.mem.Allocator, b: []const u8) Error!Decoded {
    return decode_value_depth(alloc, b, 0);
}

fn decode_value_depth(alloc: std.mem.Allocator, b: []const u8, depth: usize) Error!Decoded {
    if (depth > 64) return error.Malformed;
    if (b.len == 0) return error.UnexpectedEof;
    const ib = b[0];
    const major: u8 = ib >> 5;
    const low: u8 = ib & 0x1f;
    const head = try read_arg(b, low);
    const arg = head.arg;
    const n = head.head;
    switch (major) {
        0 => return .{ .value = .{ .uint = arg }, .consumed = n },
        1 => {
            // CBOR negative ints encode -1-arg; an arg above maxInt(i64) names a
            // value below minInt(i64), which would silently wrap. Reject it, as Go
            // does, so behavior and bytes stay aligned.
            if (arg > std.math.maxInt(i64)) return error.Malformed;
            const val: i64 = -1 - @as(i64, @intCast(arg));
            return .{ .value = .{ .int = val }, .consumed = n };
        },
        2, 3 => {
            const end = try content_end(b, n, arg);
            if (major == 3 and !std.unicode.utf8ValidateSlice(b[n..end])) return error.Malformed;
            const slice = try alloc.dupe(u8, b[n..end]);
            const value: Value = if (major == 2) .{ .bytes = slice } else .{ .text = slice };
            return .{ .value = value, .consumed = end };
        },
        4 => {
            if (arg > b.len - n) return error.UnexpectedEof;
            const count: usize = @intCast(arg);
            const items = try alloc.alloc(Value, count);
            var off = n;
            var i: usize = 0;
            while (i < count) : (i += 1) {
                const d = try decode_value_depth(alloc, b[off..], depth + 1);
                items[i] = d.value;
                off += d.consumed;
            }
            return .{ .value = .{ .array = items }, .consumed = off };
        },
        5 => {
            if (arg > b.len - n) return error.UnexpectedEof;
            const count: usize = @intCast(arg);
            const entries = try alloc.alloc(Entry, count);
            var off = n;
            var i: usize = 0;
            while (i < count) : (i += 1) {
                const k = try decode_value_depth(alloc, b[off..], depth + 1);
                off += k.consumed;
                const v = try decode_value_depth(alloc, b[off..], depth + 1);
                off += v.consumed;
                entries[i] = .{ .key = k.value, .val = v.value };
            }
            return .{ .value = .{ .map = entries }, .consumed = off };
        },
        6 => {
            const inner = try decode_value_depth(alloc, b[n..], depth + 1);
            const content = try alloc.create(Value);
            content.* = inner.value;
            return .{ .value = .{ .tag = .{ .num = arg, .content = content } }, .consumed = n + inner.consumed };
        },
        else => return error.Malformed,
    }
}

/// content_end computes the end offset of a byte/text string body, bounds-checking
/// without overflowing: a length larger than the remaining input is a truncation.
fn content_end(b: []const u8, head: usize, arg: u64) Error!usize {
    const remaining: u64 = @intCast(b.len - head);
    if (arg > remaining) return error.UnexpectedEof;
    return head + @as(usize, @intCast(arg));
}

const Arg = struct { arg: u64, head: usize };

/// read_arg reads the additional-information argument for a head byte whose low five
/// bits are `low`, returning the argument value and the total head length.
fn read_arg(b: []const u8, low: u8) Error!Arg {
    if (low < 24) return .{ .arg = low, .head = 1 };
    switch (low) {
        24 => {
            if (b.len < 2) return error.UnexpectedEof;
            return .{ .arg = b[1], .head = 2 };
        },
        25 => {
            if (b.len < 3) return error.UnexpectedEof;
            return .{ .arg = std.mem.readInt(u16, b[1..][0..2], .big), .head = 3 };
        },
        26 => {
            if (b.len < 5) return error.UnexpectedEof;
            return .{ .arg = std.mem.readInt(u32, b[1..][0..4], .big), .head = 5 };
        },
        27 => {
            if (b.len < 9) return error.UnexpectedEof;
            return .{ .arg = std.mem.readInt(u64, b[1..][0..8], .big), .head = 9 };
        },
        // 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes.
        else => return error.Malformed,
    }
}

test "encode shortest-form heads" {
    const a = std.testing.allocator;
    {
        const out = try encode(a, .{ .uint = 0 });
        defer a.free(out);
        try std.testing.expectEqualSlices(u8, &.{0x00}, out);
    }
    {
        const out = try encode(a, .{ .uint = 23 });
        defer a.free(out);
        try std.testing.expectEqualSlices(u8, &.{0x17}, out);
    }
    {
        const out = try encode(a, .{ .uint = 24 });
        defer a.free(out);
        try std.testing.expectEqualSlices(u8, &.{ 0x18, 0x18 }, out);
    }
    {
        const out = try encode(a, .{ .uint = 0x1234 });
        defer a.free(out);
        try std.testing.expectEqualSlices(u8, &.{ 0x19, 0x12, 0x34 }, out);
    }
}

test "negative int decode guard rejects below i64 floor" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    // major type 1 with an 8-byte argument of all 0xff names -(2^64), far below i64.
    const bad = [_]u8{ 0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff };
    try std.testing.expectError(error.Malformed, decode_envelope(arena.allocator(), &bad));
}

test "decode rejects trailing bytes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const trailing = [_]u8{ 0x00, 0x00 };
    try std.testing.expectError(error.TrailingBytes, decode_envelope(arena.allocator(), &trailing));
}

test "canonical map sorts by encoded key bytes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const aa = arena.allocator();
    // Keys "b" and "aa": "b" (0x61 62) sorts before "aa" (0x62 61 61) because the
    // shorter encoded key wins on the first differing byte (0x61 < 0x62).
    const entries = [_]Entry{
        .{ .key = .{ .text = "aa" }, .val = .{ .uint = 2 } },
        .{ .key = .{ .text = "b" }, .val = .{ .uint = 1 } },
    };
    const sorted = try canon_map(aa, &entries);
    const out = try encode(aa, .{ .map = sorted });
    // map(2): "b"->1 then "aa"->2.
    try std.testing.expectEqualSlices(u8, &.{ 0xa2, 0x61, 0x62, 0x01, 0x62, 0x61, 0x61, 0x02 }, out);
}
