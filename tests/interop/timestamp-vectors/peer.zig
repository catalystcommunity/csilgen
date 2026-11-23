// Decode every timestamp vector as a Stamp, then re-encode the rows that must
// decode. The Zig CsilTimestamp keeps canonical text, so it holds nine digits.
const std = @import("std");
const types = @import("types.gen.zig");
const codec = @import("codec.gen.zig");

const digits = 9;

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const alloc = gpa.allocator();
    const args = try std.process.argsAlloc(alloc);
    defer std.process.argsFree(alloc, args);
    if (args.len != 2) return error.Usage;
    const text = try std.fs.cwd().readFileAlloc(alloc, args[1], 1 << 20);
    defer alloc.free(text);
    const stdout = std.io.getStdOut().writer();
    var failures: usize = 0;
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |raw| {
        const line = std.mem.trimRight(u8, raw, "\r");
        if (line.len == 0 or line[0] == '#') continue;
        var cols = std.mem.splitScalar(u8, line, '\t');
        const name = cols.next().?;
        const expect = cols.next().?;
        const precision = try std.fmt.parseInt(u8, cols.next().?, 10);
        const input_hex = cols.next().?;
        const canonical = cols.next().?;
        const want_ok = std.mem.eql(u8, expect, "ok") and precision <= digits;

        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        const input = try a.alloc(u8, input_hex.len / 2);
        _ = try std.fmt.hexToBytes(input, input_hex);
        var stamp: types.Stamp = undefined;
        const decoded = if (codec.decode_Stamp(a, input, &stamp)) |_| true else |_| false;
        if (want_ok and !decoded) {
            try stdout.print("{s}: decode failed\n", .{name});
            failures += 1;
        } else if (want_ok) {
            const out = codec.encode_Stamp(a, &stamp) catch |err| {
                try stdout.print("{s}: encode failed: {s}\n", .{ name, @errorName(err) });
                failures += 1;
                continue;
            };
            const got = try std.fmt.allocPrint(a, "{s}", .{std.fmt.fmtSliceHexLower(out)});
            if (!std.mem.eql(u8, got, canonical)) {
                try stdout.print("{s}: encoded {s}\n", .{ name, got });
                failures += 1;
            }
        } else if (decoded) {
            try stdout.print("{s}: decode accepted {s}\n", .{ name, stamp.at.rfc3339 });
            failures += 1;
        }
    }
    if (failures != 0) std.process.exit(1);
    try stdout.print("zig: all timestamp vectors pass\n", .{});
}
