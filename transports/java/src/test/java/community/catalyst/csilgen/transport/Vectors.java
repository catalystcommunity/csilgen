// Shared loading/decoding helpers for the conformance vectors. Locates
// transports/conformance/<file> relative to the test working directory by walking up the
// tree, so the tests run whether launched from the module dir or the repo root.
package community.catalyst.csilgen.transport;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

final class Vectors {
    private Vectors() {}

    /** One conformance vector: its name, the expected hex, and the language-neutral input. */
    record Case(String name, String hex, Map<String, Object> input) {
        @Override
        public String toString() {
            return name;
        }
    }

    @SuppressWarnings("unchecked")
    static List<Case> load(String file) {
        Path path = locate(file);
        String text;
        try {
            text = Files.readString(path, StandardCharsets.UTF_8);
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
        Map<String, Object> doc = (Map<String, Object>) Json.parse(text);
        List<Object> raw = (List<Object>) doc.get("vectors");
        List<Case> cases = new ArrayList<>(raw.size());
        for (Object o : raw) {
            Map<String, Object> entry = (Map<String, Object>) o;
            cases.add(new Case(
                    (String) entry.get("name"),
                    (String) entry.get("hex"),
                    (Map<String, Object>) entry.get("input")));
        }
        return cases;
    }

    private static Path locate(String file) {
        Path dir = Paths.get("").toAbsolutePath();
        for (Path p = dir; p != null; p = p.getParent()) {
            Path candidate = p.resolve("transports").resolve("conformance").resolve(file);
            if (Files.exists(candidate)) {
                return candidate;
            }
            // Also handle running from inside transports/java where ../conformance applies.
            Path sibling = p.resolve("conformance").resolve(file);
            if (Files.exists(sibling)) {
                return sibling;
            }
        }
        throw new IllegalStateException("could not locate conformance vector " + file);
    }

    static byte[] hex(String s) {
        int n = s.length();
        byte[] out = new byte[n / 2];
        for (int i = 0; i < out.length; i++) {
            int hi = Character.digit(s.charAt(i * 2), 16);
            int lo = Character.digit(s.charAt(i * 2 + 1), 16);
            out[i] = (byte) ((hi << 4) | lo);
        }
        return out;
    }

    static String toHex(byte[] b) {
        StringBuilder sb = new StringBuilder(b.length * 2);
        for (byte x : b) {
            sb.append(Character.forDigit((x >> 4) & 0xF, 16));
            sb.append(Character.forDigit(x & 0xF, 16));
        }
        return sb.toString();
    }

    static String optStr(Map<String, Object> m, String k) {
        Object v = m.get(k);
        return v == null ? null : (String) v;
    }

    static Long optLong(Map<String, Object> m, String k) {
        Object v = m.get(k);
        return v == null ? null : (long) (double) (Double) v;
    }

    static String reqStr(Map<String, Object> m, String k) {
        Object v = m.get(k);
        if (v == null) {
            throw new IllegalArgumentException("missing required string field " + k);
        }
        return (String) v;
    }

    static long reqLong(Map<String, Object> m, String k) {
        Object v = m.get(k);
        if (v == null) {
            throw new IllegalArgumentException("missing required integer field " + k);
        }
        return (long) (double) (Double) v;
    }
}
