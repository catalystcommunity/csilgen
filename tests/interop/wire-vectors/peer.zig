// Decode every wire vector as the shape its kind names, then re-encode the rows
// that must decode and compare with the canonical bytes.
const std = @import("std");
const types = @import("types.gen.zig");
const codec = @import("codec.gen.zig");

fn Shape(comptime T: type, comptime decode: anytype, comptime encode: anytype) type {
    return struct {
        // Returns the canonical re-encoding, or null when the input does not decode.
        fn run(a: std.mem.Allocator, input: []const u8) !?[]u8 {
            var value: T = undefined;
            decode(a, input, &value) catch return null;
            return try encode(a, &value);
        }
    };
}

const Holder = Shape(types.Holder, codec.decode_Holder, codec.encode_Holder);
const Contact = Shape(types.Contact, codec.decode_Contact, codec.encode_Contact);
const Output = Shape(types.LookupReadCountsOutput, codec.decode_LookupReadCountsOutput, codec.encode_LookupReadCountsOutput);

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
        const kind = cols.next().?;
        const expect = cols.next().?;
        const input_hex = cols.next().?;
        const canonical = cols.next().?;

        var arena = std.heap.ArenaAllocator.init(alloc);
        defer arena.deinit();
        const a = arena.allocator();
        const input = try a.alloc(u8, input_hex.len / 2);
        _ = try std.fmt.hexToBytes(input, input_hex);
        const encoded = if (std.mem.eql(u8, kind, "holder"))
            try Holder.run(a, input)
        else if (std.mem.eql(u8, kind, "contact"))
            try Contact.run(a, input)
        else if (std.mem.eql(u8, kind, "output"))
            try Output.run(a, input)
        else {
            try stdout.print("FAIL {s}: unknown kind {s}\n", .{ name, kind });
            failures += 1;
            continue;
        };
        if (std.mem.eql(u8, expect, "ok")) {
            const out = encoded orelse {
                try stdout.print("FAIL {s}: decode failed\n", .{name});
                failures += 1;
                continue;
            };
            const got = try std.fmt.allocPrint(a, "{s}", .{std.fmt.fmtSliceHexLower(out)});
            if (!std.mem.eql(u8, got, canonical)) {
                try stdout.print("FAIL {s}: encoded {s}, want {s}\n", .{ name, got, canonical });
                failures += 1;
            }
        } else if (encoded != null) {
            try stdout.print("FAIL {s}: decode accepted invalid input\n", .{name});
            failures += 1;
        }
    }
    if (failures != 0) std.process.exit(1);
    try stdout.print("all wire vectors pass\n", .{});
}
