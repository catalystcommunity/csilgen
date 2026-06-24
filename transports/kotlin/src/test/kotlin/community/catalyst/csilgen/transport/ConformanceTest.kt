// Verify the Kotlin transport library against the checked-in conformance vectors.
//
// This both guards the encoders/decoders against drift and demonstrates the contract every
// reference library follows: reconstruct each vector's envelope from its language-neutral
// `input`, assert encode → `hex`, and assert decode(`hex`) → the same envelope. Ported from
// transports/go/conformance_test.go and transports/rust/tests/conformance.rs.
//
// `data class`/ByteArray identity equality is the headline Kotlin gotcha: the envelope
// classes override equals/hashCode with contentEquals, and the byte-level encode check uses
// assertContentEquals — assertEquals on raw ByteArray would compare references and silently
// pass-or-fail by identity.
package community.catalyst.csilgen.transport

import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail

private fun loadVectors(name: String): List<Map<String, Any?>> {
    val path = File("../conformance/$name")
    @Suppress("UNCHECKED_CAST")
    val doc = Json.parse(path.readText()) as Map<String, Any?>
    @Suppress("UNCHECKED_CAST")
    return doc["vectors"] as List<Map<String, Any?>>
}

private fun optStr(m: Map<String, Any?>, k: String): String? = m[k] as String?

private fun optU64(m: Map<String, Any?>, k: String): ULong? =
    (m[k] as Double?)?.toLong()?.toULong()

private fun reqStr(m: Map<String, Any?>, k: String): String =
    m[k] as String? ?: fail("missing required string field '$k'")

private fun reqU64(m: Map<String, Any?>, k: String): ULong =
    (m[k] as Double? ?: fail("missing required integer field '$k'")).toLong().toULong()

private fun reqI64(m: Map<String, Any?>, k: String): Long =
    (m[k] as Double? ?: fail("missing required integer field '$k'")).toLong()

/** Assert the encoded bytes match the vector hex, comparing as hex strings so a mismatch
 *  prints a readable diff rather than two byte arrays. */
private fun assertHex(name: String, expected: String, actual: ByteArray) {
    assertEquals(expected, toHex(actual), "$name encode mismatch")
}

class ConformanceTest {
    @Test
    fun rpcVectors() {
        for (vec in loadVectors("rpc.json")) {
            val name = vec["name"] as String
            val hex = vec["hex"] as String
            val input = @Suppress("UNCHECKED_CAST") (vec["input"] as Map<String, Any?>)
            when (val kind = reqStr(input, "kind")) {
                "request" -> {
                    val r = RpcRequest(
                        service = reqStr(input, "service"),
                        op = reqStr(input, "op"),
                        payload = unhex(reqStr(input, "payload_hex")),
                        id = optU64(input, "id"),
                        auth = optStr(input, "auth"),
                    )
                    assertHex(name, hex, r.encode())
                    assertEquals(r, RpcRequest.decode(r.encode()), "$name decode mismatch")
                }
                "response" -> {
                    val r = RpcResponse(
                        status = Status.fromCode(reqI64(input, "status").toInt()),
                        payload = unhex(reqStr(input, "payload_hex")),
                        id = optU64(input, "id"),
                        variant = optStr(input, "variant"),
                        error = optStr(input, "error"),
                    )
                    assertHex(name, hex, r.encode())
                    assertEquals(r, RpcResponse.decode(r.encode()), "$name decode mismatch")
                }
                "push" -> {
                    val p = RpcPush(
                        service = reqStr(input, "service"),
                        event = reqStr(input, "event"),
                        payload = unhex(reqStr(input, "payload_hex")),
                    )
                    assertHex(name, hex, p.encode())
                    assertEquals(p, RpcPush.decode(p.encode()), "$name decode mismatch")
                }
                else -> fail("unknown rpc kind '$kind'")
            }
        }
    }

    @Test
    fun eventsVectors() {
        for (vec in loadVectors("events.json")) {
            val name = vec["name"] as String
            val hex = vec["hex"] as String
            val input = @Suppress("UNCHECKED_CAST") (vec["input"] as Map<String, Any?>)

            val control = optStr(input, "control")
            if (control != null) {
                val bytes = when (control) {
                    "hello" -> Hello(
                        versions = (input["versions"] as List<*>).map { (it as Double).toLong().toULong() },
                        profiles = (input["profiles"] as List<*>).map { it as String },
                        service = optStr(input, "service"),
                        auth = optStr(input, "auth"),
                    ).encode()
                    "hello_ack" -> HelloAck(
                        v = reqU64(input, "v"),
                        profile = reqStr(input, "profile"),
                        session = optStr(input, "session"),
                    ).encode()
                    "ping" -> Heartbeat(
                        nonce = reqU64(input, "nonce"),
                        at = optU64(input, "at"),
                    ).encode()
                    "close" -> Close(
                        status = Status.fromCode(reqI64(input, "status").toInt()),
                        reason = optStr(input, "reason"),
                    ).encode()
                    else -> fail("unknown control '$control'")
                }
                assertHex(name, hex, bytes)
                continue
            }

            val profile = Profile.parse(reqStr(input, "profile")) ?: fail("unknown profile")
            val payload = unhex(reqStr(input, "payload_hex"))
            val ev = when (profile) {
                Profile.VERBOSE ->
                    Event.verbose(optStr(input, "service"), reqStr(input, "event"), payload)
                        .let { if (optU64(input, "id") != null) it.withId(optU64(input, "id")!!) else it }
                Profile.COMPACT ->
                    Event.compact(reqU64(input, "service_ord"), reqU64(input, "op_ord"), payload)
                        .let { if (optU64(input, "id") != null) it.withId(optU64(input, "id")!!) else it }
            }
            assertHex(name, hex, ev.encode(profile))
            assertEquals(ev, Event.decode(ev.encode(profile), profile), "$name decode mismatch")
        }
    }

    @Test
    fun datagramVectors() {
        for (vec in loadVectors("datagrams.json")) {
            val name = vec["name"] as String
            val hex = vec["hex"] as String
            val input = @Suppress("UNCHECKED_CAST") (vec["input"] as Map<String, Any?>)
            when (val profile = reqStr(input, "profile")) {
                "cbor-array" -> {
                    val d = Datagram(
                        opOrd = reqU64(input, "op_ord"),
                        seq = reqU64(input, "seq"),
                        payload = unhex(reqStr(input, "payload_hex")),
                    )
                    assertHex(name, hex, d.encode())
                    assertEquals(d, Datagram.decode(d.encode()), "$name decode mismatch")
                }
                "compact-header" -> {
                    var d = CompactDatagram(
                        opOrd = reqU64(input, "op_ord").toUByte(),
                        seq = reqU64(input, "seq").toUShort(),
                        body = unhex(reqStr(input, "body_hex")),
                    )
                    optU64(input, "epoch")?.let { d = d.withEpoch(it.toUByte()) }
                    assertHex(name, hex, d.encode())
                    assertEquals(d, CompactDatagram.decode(d.encode()), "$name decode mismatch")
                }
                else -> fail("unknown datagram profile '$profile'")
            }
        }
    }

    @Test
    fun everyVectorFileIsNonEmpty() {
        assertTrue(loadVectors("rpc.json").isNotEmpty())
        assertTrue(loadVectors("events.json").isNotEmpty())
        assertTrue(loadVectors("datagrams.json").isNotEmpty())
    }
}
