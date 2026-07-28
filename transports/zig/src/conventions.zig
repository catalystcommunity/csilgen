//! Conventions shared by every CSIL transport — see csil-transport-conventions.md.
//!
//! This file owns the parts the three transports agree on: the version constant,
//! the transport status registry, tag-24 payload wrap/unwrap, the max-frame guard,
//! and the canonical-CBOR field accessors the envelopes build on so their bytes
//! match the conformance vectors regardless of struct layout. Ported from
//! transports/go/conventions.go.

const std = @import("std");
const cbor = @import("cbor.zig");

/// The current transport version. A new value is minted only for a breaking change
/// to envelope layout or semantics.
pub const VERSION: u64 = 1;

/// The CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949
/// §3.4.5.1).
pub const TAG_ENCODED_CBOR: u64 = 24;

/// The reserved service ordinal for the transport control plane (Events lifecycle).
pub const CONTROL_SERVICE_ORD: u64 = 0;

/// The default max encoded envelope size for stream/message carriers (16 MiB). A
/// carrier rejects anything larger before allocating for it.
pub const MAX_FRAME_DEFAULT: usize = 16 * 1024 * 1024;

/// The largest max-frame limit a host may configure (2 GiB - 1). The 4-byte length
/// prefix can express more, but the limit lives in a signed 32-bit integer in
/// several supported languages, so this is the portable ceiling every CSIL
/// transport agrees on. Anything above it would also disable the receive guard
/// outright, since no wire length could exceed it.
pub const MAX_FRAME_LIMIT: usize = 2147483647;

/// Raised when a host configures a max-frame limit outside the valid range.
pub const InvalidMaxFrameError = error{InvalidMaxFrame};

/// Check a host-supplied max-frame limit against the conventions doc range
/// (1..=MAX_FRAME_LIMIT), so an unusable carrier fails at construction rather than
/// on its first frame.
pub fn validate_max_frame(max_frame: usize) InvalidMaxFrameError!usize {
    if (max_frame < 1 or max_frame > MAX_FRAME_LIMIT) return error.InvalidMaxFrame;
    return max_frame;
}

/// Errors raised across the envelope layer. Joins the codec errors with the
/// transport-level distinctions a host needs to handle.
pub const Error = cbor.Error || error{
    UnsupportedVersion,
    FrameTooLarge,
};

/// A transport-level status. It is distinct from application errors, which ride
/// inside the payload as a declared `/ ErrorType` arm. Equality is by the
/// underlying code, so host-defined extension codes (>= 64) and unknown codes
/// compare correctly. The code is signed because the registry models it as a
/// signed CBOR integer even though defined codes are non-negative.
pub const Status = struct {
    code: i64,

    pub const ok = Status{ .code = 0 };
    pub const malformed_envelope = Status{ .code = 1 };
    pub const unknown_service_or_op = Status{ .code = 2 };
    pub const unauthenticated = Status{ .code = 3 };
    pub const forbidden = Status{ .code = 4 };
    pub const version_unsupported = Status{ .code = 5 };
    pub const internal = Status{ .code = 6 };
    pub const unavailable = Status{ .code = 7 };
    pub const deadline_exceeded = Status{ .code = 8 };

    /// from_code maps a wire code onto a Status, preserving host-defined and
    /// unknown codes verbatim.
    pub fn from_code(code: i64) Status {
        return .{ .code = code };
    }

    /// is_ok reports whether the status indicates a typed reply is present.
    pub fn is_ok(self: Status) bool {
        return self.code == 0;
    }

    /// name returns the registry name for the status, or "other" for codes outside it.
    pub fn name(self: Status) []const u8 {
        return switch (self.code) {
            0 => "ok",
            1 => "malformed-envelope",
            2 => "unknown-service-or-op",
            3 => "unauthenticated",
            4 => "forbidden",
            5 => "version-unsupported",
            6 => "internal",
            7 => "unavailable",
            8 => "deadline-exceeded",
            else => "other",
        };
    }
};

/// check_version verifies a decoded envelope's version field, so an unknown version
/// is never silently misparsed.
pub fn check_version(v: u64) Error!void {
    if (v != VERSION) return error.UnsupportedVersion;
}

/// tag24 wraps opaque payload bytes (themselves a CBOR item) in tag 24. The content
/// pointer is allocated from `alloc` (use the encode arena); `payload` is referenced,
/// not copied, so it must outlive the encode call.
pub fn tag24(alloc: std.mem.Allocator, payload: []const u8) std.mem.Allocator.Error!cbor.Value {
    const content = try alloc.create(cbor.Value);
    content.* = .{ .bytes = payload };
    return .{ .tag = .{ .num = TAG_ENCODED_CBOR, .content = content } };
}

/// untag24 extracts the opaque payload bytes from a tag-24 value. The returned slice
/// aliases the decoded tree's storage (the decode arena), so it is valid for as long
/// as that arena.
pub fn untag24(v: cbor.Value) Error![]const u8 {
    switch (v) {
        .tag => |t| {
            if (t.num != TAG_ENCODED_CBOR) return error.Malformed;
            return switch (t.content.*) {
                .bytes => |b| b,
                else => error.Malformed,
            };
        },
        else => return error.Malformed,
    }
}

/// map_get looks up a text key in a decoded CBOR map value.
pub fn map_get(v: cbor.Value, key: []const u8) ?cbor.Value {
    switch (v) {
        .map => |m| {
            for (m) |e| {
                switch (e.key) {
                    .text => |t| if (std.mem.eql(u8, t, key)) return e.val,
                    else => {},
                }
            }
            return null;
        },
        else => return null,
    }
}

/// as_u64 reads a non-negative integer from a decoded CBOR integer value.
pub fn as_u64(v: cbor.Value) ?u64 {
    return switch (v) {
        .uint => |x| x,
        .int => |x| if (x >= 0) @intCast(x) else null,
        else => null,
    };
}

/// as_i64 reads a signed integer from a decoded CBOR integer value.
pub fn as_i64(v: cbor.Value) ?i64 {
    return switch (v) {
        .uint => |x| @intCast(x),
        .int => |x| x,
        else => null,
    };
}

pub fn get_uint(m: cbor.Value, key: []const u8) Error!u64 {
    const v = map_get(m, key) orelse return error.Malformed;
    return as_u64(v) orelse error.Malformed;
}

pub fn get_int(m: cbor.Value, key: []const u8) Error!i64 {
    const v = map_get(m, key) orelse return error.Malformed;
    return as_i64(v) orelse error.Malformed;
}

pub fn get_text(m: cbor.Value, key: []const u8) Error![]const u8 {
    const v = map_get(m, key) orelse return error.Malformed;
    return switch (v) {
        .text => |t| t,
        else => error.Malformed,
    };
}

pub fn get_text_opt(m: cbor.Value, key: []const u8) ?[]const u8 {
    const v = map_get(m, key) orelse return null;
    return switch (v) {
        .text => |t| t,
        else => null,
    };
}

pub fn get_uint_opt(m: cbor.Value, key: []const u8) ?u64 {
    const v = map_get(m, key) orelse return null;
    return as_u64(v);
}

test "status registry names and ok-ness" {
    try std.testing.expect(Status.ok.is_ok());
    try std.testing.expect(!Status.unknown_service_or_op.is_ok());
    try std.testing.expectEqualStrings("unknown-service-or-op", Status.from_code(2).name());
    try std.testing.expectEqualStrings("other", Status.from_code(64).name());
}

test "tag24 round-trips opaque payload" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    const payload = [_]u8{0xa0};
    const wrapped = try tag24(a, &payload);
    const out = try cbor.encode(a, wrapped);
    // tag 24, byte string len 1, 0xa0.
    try std.testing.expectEqualSlices(u8, &.{ 0xd8, 0x18, 0x41, 0xa0 }, out);
    const decoded = try cbor.decode_envelope(a, out);
    try std.testing.expectEqualSlices(u8, &payload, try untag24(decoded));
}
