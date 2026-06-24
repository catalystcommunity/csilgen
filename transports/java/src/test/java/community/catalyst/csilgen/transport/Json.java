// A tiny, test-only JSON reader so the conformance tests stay dependency-free (the library
// jar carries no JSON dependency, and the tests need none either). It parses exactly the
// shapes the conformance vectors use: objects, arrays, strings, numbers, booleans, null.
package community.catalyst.csilgen.transport;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

final class Json {
    private final String s;
    private int pos;

    private Json(String s) {
        this.s = s;
    }

    /** Parses a JSON document into nested Map/List/String/Double/Boolean/null. */
    static Object parse(String s) {
        Json j = new Json(s);
        j.skipWs();
        Object v = j.value();
        j.skipWs();
        if (j.pos != s.length()) {
            throw new IllegalArgumentException("trailing JSON at " + j.pos);
        }
        return v;
    }

    private Object value() {
        char c = s.charAt(pos);
        return switch (c) {
            case '{' -> object();
            case '[' -> array();
            case '"' -> string();
            case 't', 'f' -> bool();
            case 'n' -> nul();
            default -> number();
        };
    }

    private Map<String, Object> object() {
        Map<String, Object> m = new LinkedHashMap<>();
        pos++; // {
        skipWs();
        if (s.charAt(pos) == '}') {
            pos++;
            return m;
        }
        while (true) {
            skipWs();
            String key = string();
            skipWs();
            expect(':');
            skipWs();
            m.put(key, value());
            skipWs();
            char c = s.charAt(pos++);
            if (c == '}') {
                return m;
            }
            if (c != ',') {
                throw new IllegalArgumentException("expected ',' or '}' at " + (pos - 1));
            }
        }
    }

    private List<Object> array() {
        List<Object> list = new ArrayList<>();
        pos++; // [
        skipWs();
        if (s.charAt(pos) == ']') {
            pos++;
            return list;
        }
        while (true) {
            skipWs();
            list.add(value());
            skipWs();
            char c = s.charAt(pos++);
            if (c == ']') {
                return list;
            }
            if (c != ',') {
                throw new IllegalArgumentException("expected ',' or ']' at " + (pos - 1));
            }
        }
    }

    private String string() {
        expect('"');
        StringBuilder sb = new StringBuilder();
        while (true) {
            char c = s.charAt(pos++);
            if (c == '"') {
                return sb.toString();
            }
            if (c == '\\') {
                char e = s.charAt(pos++);
                switch (e) {
                    case '"' -> sb.append('"');
                    case '\\' -> sb.append('\\');
                    case '/' -> sb.append('/');
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'u' -> {
                        sb.append((char) Integer.parseInt(s.substring(pos, pos + 4), 16));
                        pos += 4;
                    }
                    default -> throw new IllegalArgumentException("bad escape \\" + e);
                }
            } else {
                sb.append(c);
            }
        }
    }

    private Boolean bool() {
        if (s.startsWith("true", pos)) {
            pos += 4;
            return Boolean.TRUE;
        }
        if (s.startsWith("false", pos)) {
            pos += 5;
            return Boolean.FALSE;
        }
        throw new IllegalArgumentException("bad literal at " + pos);
    }

    private Object nul() {
        if (s.startsWith("null", pos)) {
            pos += 4;
            return null;
        }
        throw new IllegalArgumentException("bad literal at " + pos);
    }

    private Double number() {
        int start = pos;
        while (pos < s.length() && "+-0123456789.eE".indexOf(s.charAt(pos)) >= 0) {
            pos++;
        }
        return Double.parseDouble(s.substring(start, pos));
    }

    private void expect(char c) {
        if (s.charAt(pos++) != c) {
            throw new IllegalArgumentException("expected '" + c + "' at " + (pos - 1));
        }
    }

    private void skipWs() {
        while (pos < s.length() && Character.isWhitespace(s.charAt(pos))) {
            pos++;
        }
    }
}
