// Minimal canonical CBOR codec (RFC 8949). Hand-written and dependency-free so the
// transport library stays offline-testable. It supports exactly what the CSIL
// envelopes need — unsigned ints, negative ints, text strings, byte strings, arrays,
// maps, and tag 24 — and nothing else. Maps use core deterministic encoding: entries
// are sorted by the bytewise-unsigned order of their encoded keys, matching the Go and
// Rust references so bytes are byte-identical to the conformance vectors.
package community.catalyst.csilgen.transport;

import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/** Canonical CBOR encode/decode over the {@link CborValue} model. */
final class Cbor {
    private Cbor() {}

    /** Serializes a value to canonical CBOR bytes. */
    static byte[] encode(CborValue v) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        encodeInto(out, v);
        return out.toByteArray();
    }

    private static void encodeInto(ByteArrayOutputStream out, CborValue v) {
        if (v instanceof CUint u) {
            writeHead(out, 0, u.value());
        } else if (v instanceof CInt i) {
            long n = i.value();
            if (n >= 0) {
                writeHead(out, 0, n);
            } else {
                // CBOR negative ints encode -1-n; the argument is the magnitude minus one.
                writeHead(out, 1, -1L - n);
            }
        } else if (v instanceof CText t) {
            byte[] bytes = t.value().getBytes(StandardCharsets.UTF_8);
            writeHead(out, 3, bytes.length);
            out.write(bytes, 0, bytes.length);
        } else if (v instanceof CBytes b) {
            byte[] bytes = b.value();
            writeHead(out, 2, bytes.length);
            out.write(bytes, 0, bytes.length);
        } else if (v instanceof CArray a) {
            writeHead(out, 4, a.items().size());
            for (CborValue e : a.items()) {
                encodeInto(out, e);
            }
        } else if (v instanceof CMap m) {
            writeHead(out, 5, m.entries().size());
            for (CEntry e : m.entries()) {
                encodeInto(out, e.key());
                encodeInto(out, e.val());
            }
        } else if (v instanceof CTag tag) {
            writeHead(out, 6, tag.tag());
            encodeInto(out, tag.content());
        } else {
            throw new EncodeException("unsupported cbor value " + v);
        }
    }

    // writeHead emits the initial byte (major type in the high three bits) plus the
    // shortest-form argument bytes for n, per deterministic encoding. n is treated as
    // an unsigned 64-bit value: the ladder uses unsigned comparisons and the 8-byte
    // path uses the unsigned right shift, so a value in the top half of the u64 range
    // is encoded correctly rather than as a spuriously-negative long.
    private static void writeHead(ByteArrayOutputStream out, int major, long n) {
        int mt = major << 5;
        if (Long.compareUnsigned(n, 24) < 0) {
            out.write(mt | (int) n);
        } else if (Long.compareUnsigned(n, 1L << 8) < 0) {
            out.write(mt | 24);
            out.write((int) n & 0xFF);
        } else if (Long.compareUnsigned(n, 1L << 16) < 0) {
            out.write(mt | 25);
            out.write((int) (n >>> 8) & 0xFF);
            out.write((int) n & 0xFF);
        } else if (Long.compareUnsigned(n, 1L << 32) < 0) {
            out.write(mt | 26);
            for (int shift = 24; shift >= 0; shift -= 8) {
                out.write((int) (n >>> shift) & 0xFF);
            }
        } else {
            out.write(mt | 27);
            for (int shift = 56; shift >= 0; shift -= 8) {
                out.write((int) (n >>> shift) & 0xFF);
            }
        }
    }

    /**
     * Builds a deterministically-keyed CBOR map: entries are sorted by the bytewise
     * lexicographic (unsigned) order of their encoded keys (RFC 8949 §4.2.1), so the
     * same logical envelope always yields the same bytes.
     */
    static CMap canonMap(List<CEntry> entries) {
        List<CEntry> sorted = new ArrayList<>(entries);
        // A stable sort keyed by the encoded key bytes; Arrays.compareUnsigned is exactly
        // the bytewise-unsigned ordering the contract requires (length-then-bytes for text).
        sorted.sort((a, b) -> Arrays.compareUnsigned(encode(a.key()), encode(b.key())));
        return new CMap(sorted);
    }

    /**
     * Decodes a complete envelope: one self-contained CBOR item with no trailing bytes.
     * An envelope is a single CBOR item, so any leftover bytes are a malformed frame and
     * rejected — matching the Go and Rust references rather than silently ignoring them.
     */
    static CborValue decodeEnvelope(byte[] b) {
        Decoder d = new Decoder(b);
        CborValue v = d.value();
        if (d.pos != b.length) {
            throw new DecodeException("trailing bytes after CBOR item");
        }
        return v;
    }

    /** A single-pass cursor over the input bytes; {@link #value()} advances {@link #pos}. */
    private static final class Decoder {
        final byte[] b;
        int pos;

        Decoder(byte[] b) {
            this.b = b;
        }

        CborValue value() {
            if (pos >= b.length) {
                throw new DecodeException("empty input");
            }
            int ib = b[pos] & 0xFF;
            int major = ib >> 5;
            int low = ib & 0x1f;
            long arg = readArg(low);
            switch (major) {
                case 0:
                    return new CUint(arg);
                case 1:
                    // CBOR negative ints encode -1-arg; an arg above Long.MAX_VALUE (unsigned)
                    // names a value below Long.MIN_VALUE, which would silently wrap into a
                    // long — reject it, exactly as the Go reference guards math.MaxInt64.
                    if (Long.compareUnsigned(arg, Long.MAX_VALUE) > 0) {
                        throw new DecodeException("negative integer out of int64 range");
                    }
                    return new CInt(-1L - arg);
                case 2: {
                    int len = lengthGuard(arg, "byte string");
                    byte[] out = Arrays.copyOfRange(b, pos, pos + len);
                    pos += len;
                    return new CBytes(out);
                }
                case 3: {
                    int len = lengthGuard(arg, "text string");
                    String s = new String(b, pos, len, StandardCharsets.UTF_8);
                    pos += len;
                    return new CText(s);
                }
                case 4: {
                    int count = lengthGuard(arg, "array");
                    List<CborValue> items = new ArrayList<>(count);
                    for (int i = 0; i < count; i++) {
                        items.add(value());
                    }
                    return new CArray(items);
                }
                case 5: {
                    int count = lengthGuard(arg, "map");
                    List<CEntry> entries = new ArrayList<>(count);
                    for (int i = 0; i < count; i++) {
                        CborValue k = value();
                        CborValue val = value();
                        entries.add(new CEntry(k, val));
                    }
                    return new CMap(entries);
                }
                case 6: {
                    CborValue content = value();
                    return new CTag(arg, content);
                }
                default:
                    throw new DecodeException("unsupported major type " + major);
            }
        }

        // lengthGuard narrows a CBOR length argument to an int after confirming the bytes
        // are actually present, so a hostile huge length can never drive an out-of-bounds
        // read or a negative array allocation.
        private int lengthGuard(long arg, String what) {
            long remaining = (long) b.length - pos;
            if (Long.compareUnsigned(arg, remaining) > 0) {
                throw new DecodeException("truncated " + what);
            }
            return (int) arg;
        }

        // readArg reads the additional-information argument for the current head byte,
        // advancing pos past the whole head. Each byte is masked with 0xFF because Java's
        // byte is signed and would otherwise sign-extend into the accumulated value.
        private long readArg(int low) {
            if (low < 24) {
                pos += 1;
                return low;
            }
            switch (low) {
                case 24:
                    need(2);
                    long v1 = b[pos + 1] & 0xFFL;
                    pos += 2;
                    return v1;
                case 25:
                    need(3);
                    long v2 = ((b[pos + 1] & 0xFFL) << 8) | (b[pos + 2] & 0xFFL);
                    pos += 3;
                    return v2;
                case 26:
                    need(5);
                    long v4 = 0;
                    for (int i = 1; i <= 4; i++) {
                        v4 = (v4 << 8) | (b[pos + i] & 0xFFL);
                    }
                    pos += 5;
                    return v4;
                case 27:
                    need(9);
                    long v8 = 0;
                    for (int i = 1; i <= 8; i++) {
                        v8 = (v8 << 8) | (b[pos + i] & 0xFFL);
                    }
                    pos += 9;
                    return v8;
                default:
                    // 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes.
                    throw new DecodeException("indefinite or reserved additional info " + low);
            }
        }

        private void need(int headLen) {
            if (b.length - pos < headLen) {
                throw new DecodeException("truncated argument");
            }
        }
    }
}
