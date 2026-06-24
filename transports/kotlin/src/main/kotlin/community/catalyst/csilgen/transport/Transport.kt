// Package entrypoint for the CSIL transport library.
//
// Everything lives in the single flat package `community.catalyst.csilgen.transport`
// (Kotlin favors top-level declarations over deep nesting), so a consumer imports the
// symbols it needs directly — there is no façade object to route through. This file holds
// only library-level metadata; the wire types live alongside in Cbor/Conventions/Carrier/
// Rpc/Events/Datagrams.
//
// Design invariants worth restating here because they are easy to "helpfully" break:
//   * NO coroutines, ever. Every carrier method is plain blocking I/O; the host owns the
//     loop and any threads. A future contributor must not suspend-ify the API.
//   * Zero third-party runtime dependencies. The codec is hand-rolled canonical CBOR; the
//     only runtime artifact is the Kotlin stdlib (the language runtime, not a library).
package community.catalyst.csilgen.transport

/** The library's own release version (distinct from the wire [VERSION]). */
const val LIBRARY_VERSION: String = "0.1.0"
