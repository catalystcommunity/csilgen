const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // The public package module: downstreams consume this as "csilgen_transport".
    const mod = b.addModule("csilgen_transport", .{
        .root_source_file = b.path("src/root.zig"),
        .target = target,
        .optimize = optimize,
    });

    const test_step = b.step("test", "Run unit + conformance tests");

    // Unit tests: the colocated `test {}` blocks in every src file, pulled in by
    // root.zig's refAllDeclsRecursive.
    const lib_tests = b.addTest(.{ .root_module = mod });
    test_step.dependOn(&b.addRunArtifact(lib_tests).step);

    // Conformance tests run only when the shared vectors are reachable — i.e. when
    // building inside this monorepo. A downstream that fetches just this package does
    // not have transports/conformance, and its `b.dependency(...)` evaluates this
    // build() too; reading the vectors unconditionally would panic that build. So the
    // conformance step is wired only if all three vector files are present.
    if (readVectors(b, "../conformance/rpc.json")) |rpc_json| {
        const events_json = readVectors(b, "../conformance/events.json") orelse return;
        const datagrams_json = readVectors(b, "../conformance/datagrams.json") orelse return;

        // A separate module so the conformance-only vector injection is not a
        // dependency of the shipped library. The vectors live outside this Zig package
        // and are injected as compile-time strings, so the test does not depend on the
        // run step's working directory or on @embedFile escaping the package root.
        const conf_mod = b.createModule(.{
            .root_source_file = b.path("src/conformance_test.zig"),
            .target = target,
            .optimize = optimize,
        });
        conf_mod.addImport("csilgen_transport", mod);

        const vec_opts = b.addOptions();
        vec_opts.addOption([]const u8, "rpc_json", rpc_json);
        vec_opts.addOption([]const u8, "events_json", events_json);
        vec_opts.addOption([]const u8, "datagrams_json", datagrams_json);
        conf_mod.addOptions("conformance_vectors", vec_opts);

        const conf_tests = b.addTest(.{ .root_module = conf_mod });
        test_step.dependOn(&b.addRunArtifact(conf_tests).step);
    }
}

/// Reads a conformance vector file at configure time relative to the build root, or
/// returns null if it is not present (a fetched-package build without the monorepo's
/// conformance directory).
fn readVectors(b: *std.Build, rel_path: []const u8) ?[]const u8 {
    return b.build_root.handle.readFileAlloc(b.allocator, rel_path, 4 * 1024 * 1024) catch null;
}
