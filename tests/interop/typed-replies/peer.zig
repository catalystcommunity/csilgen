const std = @import("std");
const api = @import("client.gen.zig");
const server = @import("server.gen.zig");
const types = @import("types.gen.zig");
const codec = @import("codec.gen.zig");

fn handle(_: *anyopaque, req: *const types.Req, out: *types.RpDecryptTokenReply) anyerror!void {
    if (std.mem.eql(u8, req.a, "unknown")) return error.UnknownServiceOrOp;
    out.* = if (std.mem.eql(u8, req.a, "limit"))
        .{ .rp_error = .{ .code = .application_user_limit, .message = "limit" } }
    else
        .{ .resp = .{ .b = "ok" } };
}

fn callReply(ctx: *anyopaque, alloc: std.mem.Allocator, _: []const u8, _: []const u8, data: []const u8) anyerror!api.WireReply {
    const handlers = server.RpHandlers{ .decrypt_token = handle };
    var req: types.Req = undefined;
    try codec.decode_Req(alloc, data, &req);
    var result: types.RpDecryptTokenReply = undefined;
    try handlers.decrypt_token(ctx, &req, &result);
    const reply = try codec.encode_RpDecryptTokenReply(alloc, result);
    if (std.mem.eql(u8, reply.variant, "RpError")) try std.testing.expectEqual(@as(u8, 7), reply.payload[6]);
    return .{ .variant = try alloc.dupe(u8, reply.variant), .payload = reply.payload };
}

fn plain(_: *anyopaque, _: std.mem.Allocator, _: []const u8, _: []const u8, _: []const u8) anyerror![]u8 {
    return error.UnexpectedByteCall;
}

test "typed refusal and transport failure" {
    try std.testing.expectEqual(@as(i64, 42), types.RpErrorCode.account_rate_limited.to_number());
    try std.testing.expectEqual(types.RpErrorCode.account_rate_limited, types.RpErrorCode.from_number(42).?);
    try std.testing.expect(types.RpErrorCode.from_number(1) == null);
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const alloc = arena.allocator();
    var context: u8 = 0;
    const client = api.RpClient.init(.{ .ptr = &context, .call = plain, .call_reply = callReply });
    var out: types.RpDecryptTokenReply = undefined;
    try client.decrypt_token(alloc, &.{ .a = "ok" }, &out);
    try std.testing.expectEqualStrings("ok", out.resp.b);
    try client.decrypt_token(alloc, &.{ .a = "limit" }, &out);
    try std.testing.expectEqual(types.RpErrorCode.application_user_limit, out.rp_error.code);
    try std.testing.expectError(error.UnknownServiceOrOp, client.decrypt_token(alloc, &.{ .a = "unknown" }, &out));
}
