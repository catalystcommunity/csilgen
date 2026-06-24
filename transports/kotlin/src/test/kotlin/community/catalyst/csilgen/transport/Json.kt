// A tiny recursive-descent JSON parser for the conformance vectors only.
//
// The library has zero third-party runtime deps and we keep the test surface dependency-
// light too, so rather than pull a JSON library we parse the small, well-formed vector
// files by hand. It produces Map<String, Any?> / List<Any?> / String / Double / Boolean /
// null — enough to read the language-neutral `input` blocks.
package community.catalyst.csilgen.transport

internal object Json {
    fun parse(text: String): Any? {
        val p = Parser(text)
        val v = p.parseValue()
        p.skipWs()
        if (!p.atEnd()) error("trailing content at ${p.pos}")
        return v
    }

    private class Parser(val s: String) {
        var pos = 0

        fun atEnd(): Boolean = pos >= s.length

        fun skipWs() {
            while (pos < s.length && s[pos].isWhitespace()) pos++
        }

        fun parseValue(): Any? {
            skipWs()
            return when (val c = s[pos]) {
                '{' -> parseObject()
                '[' -> parseArray()
                '"' -> parseString()
                't', 'f' -> parseBool()
                'n' -> parseNull()
                else -> if (c == '-' || c in '0'..'9') parseNumber() else error("unexpected '$c' at $pos")
            }
        }

        private fun parseObject(): Map<String, Any?> {
            val out = LinkedHashMap<String, Any?>()
            pos++ // {
            skipWs()
            if (s[pos] == '}') { pos++; return out }
            while (true) {
                skipWs()
                val key = parseString()
                skipWs()
                require(s[pos] == ':') { "expected ':' at $pos" }
                pos++
                out[key] = parseValue()
                skipWs()
                when (s[pos]) {
                    ',' -> pos++
                    '}' -> { pos++; return out }
                    else -> error("expected ',' or '}' at $pos")
                }
            }
        }

        private fun parseArray(): List<Any?> {
            val out = ArrayList<Any?>()
            pos++ // [
            skipWs()
            if (s[pos] == ']') { pos++; return out }
            while (true) {
                out.add(parseValue())
                skipWs()
                when (s[pos]) {
                    ',' -> pos++
                    ']' -> { pos++; return out }
                    else -> error("expected ',' or ']' at $pos")
                }
            }
        }

        private fun parseString(): String {
            require(s[pos] == '"') { "expected string at $pos" }
            pos++
            val sb = StringBuilder()
            while (s[pos] != '"') {
                val c = s[pos]
                if (c == '\\') {
                    pos++
                    when (val e = s[pos]) {
                        '"' -> sb.append('"')
                        '\\' -> sb.append('\\')
                        '/' -> sb.append('/')
                        'b' -> sb.append('\b')
                        'n' -> sb.append('\n')
                        'r' -> sb.append('\r')
                        't' -> sb.append('\t')
                        'u' -> {
                            val hex = s.substring(pos + 1, pos + 5)
                            sb.append(hex.toInt(16).toChar())
                            pos += 4
                        }
                        else -> error("bad escape '\\$e' at $pos")
                    }
                } else {
                    sb.append(c)
                }
                pos++
            }
            pos++ // closing quote
            return sb.toString()
        }

        private fun parseNumber(): Double {
            val start = pos
            while (pos < s.length && (s[pos] in "0123456789+-.eE")) pos++
            return s.substring(start, pos).toDouble()
        }

        private fun parseBool(): Boolean {
            return if (s.startsWith("true", pos)) { pos += 4; true } else { pos += 5; false }
        }

        private fun parseNull(): Any? {
            pos += 4
            return null
        }
    }
}

/** Decode a lowercase hex string to bytes (empty string → empty array). */
internal fun unhex(s: String): ByteArray {
    if (s.isEmpty()) return ByteArray(0)
    val out = ByteArray(s.length / 2)
    for (i in out.indices) {
        out[i] = ((s[i * 2].digitToInt(16) shl 4) or s[i * 2 + 1].digitToInt(16)).toByte()
    }
    return out
}

/** Encode bytes to lowercase hex. */
internal fun toHex(b: ByteArray): String {
    val sb = StringBuilder(b.size * 2)
    for (x in b) {
        val v = x.toInt() and 0xFF
        sb.append("0123456789abcdef"[v shr 4])
        sb.append("0123456789abcdef"[v and 0xF])
    }
    return sb.toString()
}
