package community.catalyst.csilgen.generated

import java.io.File
import kotlin.system.exitProcess

// java.time.Instant holds nanoseconds.
private const val DIGITS = "9"

private fun hex(text: String): ByteArray = ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

fun main(args: Array<String>) {
    var failures = 0
    var checked = 0
    for (line in File(args[0]).readLines()) {
        if (line.isEmpty() || line.startsWith("#")) continue
        val row = line.split("\t")
        if (row[2] != "*" && row[2] != DIGITS) continue
        checked++
        val mustDecode = row[1] == "ok"
        val problem: String? = try {
            val moment = decode<Moment>(hex(row[3]))
            when {
                !mustDecode -> "decoded ${moment.at}, want error"
                hex(encode(moment)) != row[4] -> "re-encoded ${hex(encode(moment))}, want ${row[4]}"
                else -> null
            }
        } catch (e: RuntimeException) {
            if (mustDecode) "decode failed: ${e.message}" else null
        }
        if (problem != null) {
            failures++
            println("FAIL ${row[0]}: $problem")
        }
    }
    if (failures > 0) exitProcess(1)
    println("kotlin epoch vectors: $checked ok")
}
