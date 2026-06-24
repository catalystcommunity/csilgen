//! csilgen-transport — the Zig CSIL transport library.
//!
//! Package root: downstreams `@import("csilgen_transport")` and reach the submodules
//! through these re-exports. The library is a synchronous, dependency-free, hand-rolled
//! canonical-CBOR codec plus the three CSIL transports (RPC, Events, Datagrams) over a
//! bring-your-own-carrier seam. No async — the host owns the I/O loop; concurrency, if
//! a host wants it, is `std.Thread`.

const std = @import("std");

pub const cbor = @import("cbor.zig");
pub const conventions = @import("conventions.zig");
pub const carrier = @import("carrier.zig");
pub const rpc = @import("rpc.zig");
pub const events = @import("events.zig");
pub const datagrams = @import("datagrams.zig");

// Convenience re-exports of the most-used conventions surface.
pub const VERSION = conventions.VERSION;
pub const Status = conventions.Status;
pub const Error = conventions.Error;

test {
    // Pull every submodule's colocated tests into the root test binary.
    std.testing.refAllDeclsRecursive(@This());
}
