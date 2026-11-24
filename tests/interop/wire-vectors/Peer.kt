package community.catalyst.csilgen.generated

import java.io.File
import kotlin.system.exitProcess

private fun hex(text: String): ByteArray = ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

private fun roundTrip(kind: String, input: ByteArray): ByteArray {
    val value = CsilCbor.decode(input)
    return when (kind) {
        "holder" -> CsilCbor.encode(holderFromCborValue(value).toCborValue())
        "contact" -> CsilCbor.encode(contactFromCborValue(value).toCborValue())
        "output" -> CsilCbor.encode(lookupReadCountsOutputFromCborValue(value).toCborValue())
        else -> error("unknown kind $kind")
    }
}

fun main(args: Array<String>) {
    var failures = 0
    for (line in File(args[0]).readLines()) {
        if (line.isEmpty() || line.startsWith("#")) continue
        val row = line.split("\t")
        val mustDecode = row[2] == "ok"
        val problem: String? = try {
            val got = hex(roundTrip(row[1], hex(row[3])))
            when {
                !mustDecode -> "decoded, want error"
                got != row[4] -> "re-encoded $got, want ${row[4]}"
                else -> null
            }
        } catch (e: CborError) {
            if (mustDecode) "decode failed: ${e.message}" else null
        } catch (e: RuntimeException) {
            "unexpected $e"
        }
        if (problem != null) {
            failures++
            println("FAIL ${row[0]}: $problem")
        }
    }
    if (failures > 0) exitProcess(1)
    println("all wire vectors pass")
}
