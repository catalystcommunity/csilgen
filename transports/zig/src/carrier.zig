//! Carrier seams — the bring-your-own-carrier boundary (conventions doc §7).
//!
//! The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
//! byte/datagram transport) is injected. A host supplies QUIC, WebRTC, a platform
//! media stack, or anything else by satisfying one of these seams — without
//! changing the library.
//!
//! Zig has no interfaces, so the seam is the standard fat-pointer shape (an
//! `*anyopaque` context plus a vtable of function pointers, exactly like
//! `std.mem.Allocator`). This is chosen over comptime generics because hosts inject
//! carriers at *runtime*; one non-generic server type, not a type per carrier.

const std = @import("std");
const conv = @import("conventions.zig");

/// A carrier-level transport error (a wrapped I/O failure or a protocol violation by
/// the carrier) or a frame that exceeds the max-frame guard. Allocation failures
/// join the set because `recv` allocates the frame into caller memory.
pub const CarrierError = error{
    Carrier,
    FrameTooLarge,
} || std.mem.Allocator.Error;

/// FrameCarrier sends and receives one *delimited message* at a time. Used by
/// CSIL-RPC and CSIL-Events. Built-in implementations frame with a 4-byte
/// big-endian length prefix; a host may implement this over WebSocket binary
/// frames, a WebTransport stream, etc.
pub const FrameCarrier = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        send_frame: *const fn (ctx: *anyopaque, frame: []const u8) CarrierError!void,
        /// Returns null at a clean end of stream. The frame is allocated from `alloc`,
        /// which the caller frees.
        recv_frame: *const fn (ctx: *anyopaque, alloc: std.mem.Allocator) CarrierError!?[]u8,
        close: *const fn (ctx: *anyopaque) void,
    };

    pub fn send(self: FrameCarrier, frame: []const u8) CarrierError!void {
        return self.vtable.send_frame(self.ptr, frame);
    }
    pub fn recv(self: FrameCarrier, alloc: std.mem.Allocator) CarrierError!?[]u8 {
        return self.vtable.recv_frame(self.ptr, alloc);
    }
    pub fn close(self: FrameCarrier) void {
        self.vtable.close(self.ptr);
    }
};

/// DatagramCarrier sends and receives one self-contained datagram (each within the
/// channel MTU), with no delivery or ordering guarantee. Used by CSIL-Datagrams.
pub const DatagramCarrier = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        send_datagram: *const fn (ctx: *anyopaque, datagram: []const u8) CarrierError!void,
        recv_datagram: *const fn (ctx: *anyopaque, alloc: std.mem.Allocator) CarrierError!?[]u8,
        close: *const fn (ctx: *anyopaque) void,
    };

    pub fn send(self: DatagramCarrier, datagram: []const u8) CarrierError!void {
        return self.vtable.send_datagram(self.ptr, datagram);
    }
    pub fn recv(self: DatagramCarrier, alloc: std.mem.Allocator) CarrierError!?[]u8 {
        return self.vtable.recv_datagram(self.ptr, alloc);
    }
    pub fn close(self: DatagramCarrier) void {
        self.vtable.close(self.ptr);
    }
};

/// write_length_prefixed writes a 4-byte big-endian length prefix followed by `frame`
/// (CSIL stream framing), enforcing the max-frame guard before writing.
pub fn write_length_prefixed(stream: std.net.Stream, frame: []const u8, max: usize) CarrierError!void {
    if (frame.len > max) return error.FrameTooLarge;
    var prefix: [4]u8 = undefined;
    std.mem.writeInt(u32, &prefix, @intCast(frame.len), .big);
    stream.writeAll(&prefix) catch return error.Carrier;
    stream.writeAll(frame) catch return error.Carrier;
}

/// read_length_prefixed reads one length-prefixed frame, enforcing the max-frame guard
/// before allocating. It returns null at a clean EOF before any byte of a frame. The
/// length is compared as an unsigned value before narrowing, so an oversized prefix
/// can never slip past the guard and over-allocate.
pub fn read_length_prefixed(stream: std.net.Stream, alloc: std.mem.Allocator, max: usize) CarrierError!?[]u8 {
    var prefix: [4]u8 = undefined;
    const got = read_full(stream, &prefix) catch return error.Carrier;
    if (got == 0) return null;
    if (got < prefix.len) return error.Carrier;
    const length: usize = @intCast(std.mem.readInt(u32, &prefix, .big));
    if (length > max) return error.FrameTooLarge;
    const buf = try alloc.alloc(u8, length);
    errdefer alloc.free(buf);
    const n = read_full(stream, buf) catch return error.Carrier;
    if (n < buf.len) return error.Carrier;
    return buf;
}

fn read_full(stream: std.net.Stream, buf: []u8) !usize {
    var off: usize = 0;
    while (off < buf.len) {
        const n = try stream.read(buf[off..]);
        if (n == 0) break;
        off += n;
    }
    return off;
}

/// StreamCarrier is a FrameCarrier over any byte stream (TCP, TLS, Unix socket),
/// using the canonical 4-byte length-prefix framing.
pub const StreamCarrier = struct {
    stream: std.net.Stream,
    max_frame: usize = conv.MAX_FRAME_DEFAULT,

    pub fn init(stream: std.net.Stream) StreamCarrier {
        return .{ .stream = stream };
    }

    pub fn carrier(self: *StreamCarrier) FrameCarrier {
        return .{ .ptr = self, .vtable = &vtable };
    }

    const vtable = FrameCarrier.VTable{ .send_frame = send, .recv_frame = recv, .close = close_fn };

    fn send(ctx: *anyopaque, frame: []const u8) CarrierError!void {
        const self: *StreamCarrier = @ptrCast(@alignCast(ctx));
        return write_length_prefixed(self.stream, frame, self.max_frame);
    }
    fn recv(ctx: *anyopaque, alloc: std.mem.Allocator) CarrierError!?[]u8 {
        const self: *StreamCarrier = @ptrCast(@alignCast(ctx));
        return read_length_prefixed(self.stream, alloc, self.max_frame);
    }
    fn close_fn(ctx: *anyopaque) void {
        const self: *StreamCarrier = @ptrCast(@alignCast(ctx));
        self.stream.close();
    }
};

/// LoopbackFrameCarrier is an in-memory FrameCarrier backed by queues of frames —
/// for tests and for driving the codec without a socket. It owns its queued bytes
/// and frees them on deinit.
pub const LoopbackFrameCarrier = struct {
    alloc: std.mem.Allocator,
    inbound: std.ArrayList([]u8),
    outbound: std.ArrayList([]u8),
    in_idx: usize = 0,

    pub fn init(alloc: std.mem.Allocator) LoopbackFrameCarrier {
        return .{
            .alloc = alloc,
            .inbound = std.ArrayList([]u8).init(alloc),
            .outbound = std.ArrayList([]u8).init(alloc),
        };
    }

    pub fn deinit(self: *LoopbackFrameCarrier) void {
        for (self.inbound.items) |f| self.alloc.free(f);
        for (self.outbound.items) |f| self.alloc.free(f);
        self.inbound.deinit();
        self.outbound.deinit();
    }

    /// push_inbound queues a frame that a subsequent recv will return.
    pub fn push_inbound(self: *LoopbackFrameCarrier, frame: []const u8) !void {
        try self.inbound.append(try self.alloc.dupe(u8, frame));
    }

    /// take_outbound returns the next frame that was sent (still owned by the carrier),
    /// or null if none.
    pub fn take_outbound(self: *LoopbackFrameCarrier, idx: usize) ?[]const u8 {
        if (idx >= self.outbound.items.len) return null;
        return self.outbound.items[idx];
    }

    pub fn carrier(self: *LoopbackFrameCarrier) FrameCarrier {
        return .{ .ptr = self, .vtable = &vtable };
    }

    const vtable = FrameCarrier.VTable{ .send_frame = send, .recv_frame = recv, .close = close_fn };

    fn send(ctx: *anyopaque, frame: []const u8) CarrierError!void {
        const self: *LoopbackFrameCarrier = @ptrCast(@alignCast(ctx));
        const cp = self.alloc.dupe(u8, frame) catch return error.Carrier;
        self.outbound.append(cp) catch return error.Carrier;
    }
    fn recv(ctx: *anyopaque, alloc: std.mem.Allocator) CarrierError!?[]u8 {
        const self: *LoopbackFrameCarrier = @ptrCast(@alignCast(ctx));
        if (self.in_idx >= self.inbound.items.len) return null;
        const f = self.inbound.items[self.in_idx];
        self.in_idx += 1;
        return try alloc.dupe(u8, f);
    }
    fn close_fn(_: *anyopaque) void {}
};

/// LoopbackDatagramCarrier is an in-memory DatagramCarrier — for tests and codec
/// drives. Same queue shape as the frame loopback.
pub const LoopbackDatagramCarrier = struct {
    alloc: std.mem.Allocator,
    inbound: std.ArrayList([]u8),
    outbound: std.ArrayList([]u8),
    in_idx: usize = 0,

    pub fn init(alloc: std.mem.Allocator) LoopbackDatagramCarrier {
        return .{
            .alloc = alloc,
            .inbound = std.ArrayList([]u8).init(alloc),
            .outbound = std.ArrayList([]u8).init(alloc),
        };
    }

    pub fn deinit(self: *LoopbackDatagramCarrier) void {
        for (self.inbound.items) |f| self.alloc.free(f);
        for (self.outbound.items) |f| self.alloc.free(f);
        self.inbound.deinit();
        self.outbound.deinit();
    }

    pub fn push_inbound(self: *LoopbackDatagramCarrier, datagram: []const u8) !void {
        try self.inbound.append(try self.alloc.dupe(u8, datagram));
    }

    pub fn take_outbound(self: *LoopbackDatagramCarrier, idx: usize) ?[]const u8 {
        if (idx >= self.outbound.items.len) return null;
        return self.outbound.items[idx];
    }

    pub fn carrier(self: *LoopbackDatagramCarrier) DatagramCarrier {
        return .{ .ptr = self, .vtable = &vtable };
    }

    const vtable = DatagramCarrier.VTable{ .send_datagram = send, .recv_datagram = recv, .close = close_fn };

    fn send(ctx: *anyopaque, datagram: []const u8) CarrierError!void {
        const self: *LoopbackDatagramCarrier = @ptrCast(@alignCast(ctx));
        const cp = self.alloc.dupe(u8, datagram) catch return error.Carrier;
        self.outbound.append(cp) catch return error.Carrier;
    }
    fn recv(ctx: *anyopaque, alloc: std.mem.Allocator) CarrierError!?[]u8 {
        const self: *LoopbackDatagramCarrier = @ptrCast(@alignCast(ctx));
        if (self.in_idx >= self.inbound.items.len) return null;
        const f = self.inbound.items[self.in_idx];
        self.in_idx += 1;
        return try alloc.dupe(u8, f);
    }
    fn close_fn(_: *anyopaque) void {}
};

test "loopback frame carrier moves bytes through the seam" {
    const a = std.testing.allocator;
    var lb = LoopbackFrameCarrier.init(a);
    defer lb.deinit();
    const c = lb.carrier();

    try c.send(&.{ 1, 2, 3 });
    try std.testing.expectEqualSlices(u8, &.{ 1, 2, 3 }, lb.take_outbound(0).?);

    try lb.push_inbound(&.{ 4, 5 });
    const got = (try c.recv(a)).?;
    defer a.free(got);
    try std.testing.expectEqualSlices(u8, &.{ 4, 5 }, got);
    try std.testing.expect((try c.recv(a)) == null);
}
