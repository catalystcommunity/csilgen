const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // The Zig CSIL transport library, consumed straight from the monorepo source.
    const transport_mod = b.createModule(.{
        .root_source_file = b.path("../../../../transports/zig/src/root.zig"),
        .target = target,
        .optimize = optimize,
    });

    const exe = b.addExecutable(.{
        .name = "csil-interop-zig",
        .root_module = b.createModule(.{
            .root_source_file = b.path("main.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    exe.root_module.addImport("csilgen_transport", transport_mod);
    b.installArtifact(exe);
}
