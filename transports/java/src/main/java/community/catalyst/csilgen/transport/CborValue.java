// The in-memory model of the CBOR items the CSIL envelopes use. Decoding produces
// these and encoding consumes them; the transports build envelopes from the canonical
// helpers so the byte layout is independent of any Java object layout.
package community.catalyst.csilgen.transport;

import java.util.Arrays;
import java.util.List;

/** A decoded/encodable CBOR item. Sealed so decode dispatch can be exhaustive. */
sealed interface CborValue permits CUint, CInt, CText, CBytes, CArray, CMap, CTag {}

/** Major type 0: an unsigned integer; the long holds the 64 unsigned bits. */
record CUint(long value) implements CborValue {}

/** A signed integer; negative values encode as major type 1. */
record CInt(long value) implements CborValue {}

/** Major type 3: a UTF-8 text string. */
record CText(String value) implements CborValue {}

/** Major type 2: a byte string. */
record CBytes(byte[] value) implements CborValue {
    // A record's generated equals/hashCode compare the array by reference; override them
    // so value-equal byte strings (e.g. two decodes of the same payload) compare equal.
    @Override
    public boolean equals(Object o) {
        return o instanceof CBytes other && Arrays.equals(value, other.value);
    }

    @Override
    public int hashCode() {
        return Arrays.hashCode(value);
    }

    @Override
    public String toString() {
        return "CBytes" + Arrays.toString(value);
    }
}

/** Major type 4: a definite-length array. */
record CArray(List<CborValue> items) implements CborValue {}

/** Major type 5: a definite-length map, kept as an ordered entry list. */
record CMap(List<CEntry> entries) implements CborValue {}

/** One key/value pair in a {@link CMap}. */
record CEntry(CborValue key, CborValue val) {}

/** Major type 6: a tagged item. */
record CTag(long tag, CborValue content) implements CborValue {}
