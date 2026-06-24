// Conventions shared by every CSIL transport — see csil-transport-conventions.md.
//
// This file owns the parts the three transports agree on: the version constant, the
// tag-24 payload wrap/unwrap, the max-frame guard, and the canonical-CBOR field
// accessors the envelopes build on so their bytes match the conformance vectors
// regardless of any Java object layout.
package community.catalyst.csilgen.transport;

import java.util.List;

final class Conventions {
    private Conventions() {}

    /** The current transport version; a new value is minted only for a breaking change. */
    static final long VERSION = 1;

    /** The CBOR semantic tag wrapping an embedded, opaque CBOR data item (RFC 8949 §3.4.5.1). */
    static final long TAG_ENCODED_CBOR = 24;

    /** The reserved service ordinal for the transport control plane (Events lifecycle). */
    static final long CONTROL_SERVICE_ORD = 0;

    /** The default max encoded envelope size for stream/message carriers (16 MiB). */
    static final int MAX_FRAME_DEFAULT = 16 * 1024 * 1024;

    /** Wraps opaque payload bytes (themselves a CBOR item) in tag 24. */
    static CborValue tag24(byte[] payload) {
        return new CTag(TAG_ENCODED_CBOR, new CBytes(payload.clone()));
    }

    /** Extracts the opaque payload bytes from a tag-24 value. */
    static byte[] untag24(CborValue v) {
        if (!(v instanceof CTag tag) || tag.tag() != TAG_ENCODED_CBOR) {
            throw new MalformedException("expected a tag-24 (encoded-cbor) payload");
        }
        if (!(tag.content() instanceof CBytes b)) {
            throw new MalformedException("tag-24 payload is not a byte string");
        }
        return b.value().clone();
    }

    /** Looks up a text key in a CBOR map value, or null if absent. */
    static CborValue mapGet(CborValue v, String key) {
        if (!(v instanceof CMap m)) {
            return null;
        }
        for (CEntry e : m.entries()) {
            if (e.key() instanceof CText t && t.value().equals(key)) {
                return e.val();
            }
        }
        return null;
    }

    /** Reads a non-negative integer from a decoded CBOR integer value, or null. */
    static Long asU64(CborValue v) {
        if (v instanceof CUint u) {
            return u.value();
        }
        if (v instanceof CInt i && i.value() >= 0) {
            return i.value();
        }
        return null;
    }

    /** Reads a signed integer from a decoded CBOR integer value, or null. */
    static Long asI64(CborValue v) {
        if (v instanceof CUint u) {
            return u.value();
        }
        if (v instanceof CInt i) {
            return i.value();
        }
        return null;
    }

    static long getUint(CborValue m, String key) {
        Long n = asU64(mapGet(m, key));
        if (n == null) {
            throw new MalformedException("missing or non-integer field '" + key + "'");
        }
        return n;
    }

    static long getInt(CborValue m, String key) {
        Long n = asI64(mapGet(m, key));
        if (n == null) {
            throw new MalformedException("missing or non-integer field '" + key + "'");
        }
        return n;
    }

    static String getText(CborValue m, String key) {
        CborValue v = mapGet(m, key);
        if (!(v instanceof CText t)) {
            throw new MalformedException("missing or non-text field '" + key + "'");
        }
        return t.value();
    }

    /** A present text field, or null when the key is absent or not text. */
    static String getTextOpt(CborValue m, String key) {
        CborValue v = mapGet(m, key);
        return v instanceof CText t ? t.value() : null;
    }

    /** A present non-negative integer field, or null when absent. */
    static Long getUintOpt(CborValue m, String key) {
        CborValue v = mapGet(m, key);
        return v == null ? null : asU64(v);
    }

    /** Verifies a decoded envelope's version, so an unknown version is never misparsed. */
    static void checkVersion(long v) {
        if (v != VERSION) {
            throw new UnsupportedVersionException(v);
        }
    }

    /** Convenience: a text entry for a canonical map. */
    static CEntry entry(String key, CborValue val) {
        return new CEntry(new CText(key), val);
    }

    /** Convenience: a text-keyed, text-valued entry. */
    static CEntry textEntry(String key, String val) {
        return new CEntry(new CText(key), new CText(val));
    }

    /** Convenience: encode an ordered entry list as a canonically-keyed map. */
    static byte[] encodeMap(List<CEntry> entries) {
        return Cbor.encode(Cbor.canonMap(entries));
    }
}
