// CSIL-Events transport — typed bidirectional event streams — see
// csil-events-transport.md. Verbose (text-keyed) and compact (positional) profiles, plus
// the control plane (service ordinal 0) lifecycle events. Synchronous; the codec is pure
// and the byte layout is independent of any Java object layout.
package community.catalyst.csilgen.transport;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Objects;

import static community.catalyst.csilgen.transport.Conventions.*;

public final class Events {
    private Events() {}

    /** Which wire profile a connection uses for its lifetime, fixed by hello. */
    public enum Profile {
        VERBOSE("verbose"),
        COMPACT("compact");

        private final String wire;

        Profile(String wire) {
            this.wire = wire;
        }

        /** The wire profile name. */
        public String wire() {
            return wire;
        }

        /** Maps a wire profile name onto a Profile, or null if unknown. */
        public static Profile parse(String s) {
            for (Profile p : values()) {
                if (p.wire.equals(s)) {
                    return p;
                }
            }
            return null;
        }
    }

    /**
     * One typed event flowing in either direction, identified by service+operation
     * (verbose) or by their ordinals (compact). A null field is absent on the wire. A
     * correlation {@code id} is present only when the event is a request expecting a reply,
     * or that reply.
     */
    public record Event(String service, Long serviceOrd, String event, Long opOrd, Long id,
            byte[] payload) {
        /** A verbose event by name; {@code service} is null on a single-service connection. */
        public static Event verbose(String service, String event, byte[] payload) {
            return new Event(service, null, event, null, null, payload);
        }

        /** A compact event by ordinals. */
        public static Event compact(long serviceOrd, long opOrd, byte[] payload) {
            return new Event(null, serviceOrd, null, opOrd, null, payload);
        }

        public Event withId(long id) {
            return new Event(service, serviceOrd, event, opOrd, id, payload);
        }

        /** Serializes the event under the given profile. */
        public byte[] encode(Profile profile) {
            return switch (profile) {
                case VERBOSE -> encodeVerbose();
                case COMPACT -> encodeCompact();
            };
        }

        private byte[] encodeVerbose() {
            if (event == null) {
                throw new MalformedException("verbose event missing 'event' name");
            }
            List<CEntry> entries = new ArrayList<>();
            entries.add(textEntry("event", event));
            entries.add(entry("payload", tag24(payload)));
            if (service != null) {
                entries.add(textEntry("service", service));
            }
            if (id != null) {
                entries.add(entry("id", new CUint(id)));
            }
            return encodeMap(entries);
        }

        private byte[] encodeCompact() {
            if (serviceOrd == null) {
                throw new MalformedException("compact event missing service ordinal");
            }
            if (opOrd == null) {
                throw new MalformedException("compact event missing op ordinal");
            }
            List<CborValue> arr = new ArrayList<>();
            arr.add(new CUint(serviceOrd));
            arr.add(new CUint(opOrd));
            if (id != null) {
                arr.add(new CUint(id));
            }
            arr.add(tag24(payload));
            return Cbor.encode(new CArray(arr));
        }

        /** Parses an event under the given profile. */
        public static Event decode(byte[] b, Profile profile) {
            return switch (profile) {
                case VERBOSE -> decodeVerbose(b);
                case COMPACT -> decodeCompact(b);
            };
        }

        private static Event decodeVerbose(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            CborValue p = mapGet(v, "payload");
            if (p == null) {
                throw new MalformedException("missing 'payload'");
            }
            return new Event(
                    getTextOpt(v, "service"),
                    null,
                    getText(v, "event"),
                    null,
                    getUintOpt(v, "id"),
                    untag24(p));
        }

        private static Event decodeCompact(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            if (!(v instanceof CArray a)) {
                throw new MalformedException("compact event is not an array");
            }
            List<CborValue> arr = a.items();
            CborValue serviceOrdV;
            CborValue opOrdV;
            CborValue payloadV;
            CborValue idV = null;
            // 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
            switch (arr.size()) {
                case 3 -> {
                    serviceOrdV = arr.get(0);
                    opOrdV = arr.get(1);
                    payloadV = arr.get(2);
                }
                case 4 -> {
                    serviceOrdV = arr.get(0);
                    opOrdV = arr.get(1);
                    idV = arr.get(2);
                    payloadV = arr.get(3);
                }
                default -> throw new MalformedException(
                        "compact event array has " + arr.size() + " elements, expected 3 or 4");
            }
            Long serviceOrd = asU64(serviceOrdV);
            Long opOrd = asU64(opOrdV);
            if (serviceOrd == null || opOrd == null) {
                throw new MalformedException("ordinal is not an integer");
            }
            Long id = null;
            if (idV != null) {
                id = asU64(idV);
                if (id == null) {
                    throw new MalformedException("ordinal is not an integer");
                }
            }
            return new Event(null, serviceOrd, null, opOrd, id, untag24(payloadV));
        }

        // byte[] payload needs content equality so a decoded event equals its original.
        @Override
        public boolean equals(Object o) {
            return o instanceof Event e
                    && Objects.equals(service, e.service)
                    && Objects.equals(serviceOrd, e.serviceOrd)
                    && Objects.equals(event, e.event)
                    && Objects.equals(opOrd, e.opOrd)
                    && Objects.equals(id, e.id)
                    && Arrays.equals(payload, e.payload);
        }

        @Override
        public int hashCode() {
            return Objects.hash(service, serviceOrd, event, opOrd, id, Arrays.hashCode(payload));
        }
    }

    // Control-plane operation ordinals (under service ordinal 0).
    public static final long CONTROL_HELLO = 0;
    public static final long CONTROL_HELLO_ACK = 1;
    public static final long CONTROL_PING = 2;
    public static final long CONTROL_PONG = 3;
    public static final long CONTROL_CLOSE = 4;
    public static final long CONTROL_ERROR = 5;

    // Verbose control-event names (the `$`-sigil names).
    public static final String HELLO_NAME = "$hello";
    public static final String HELLO_ACK_NAME = "$hello-ack";
    public static final String PING_NAME = "$ping";
    public static final String PONG_NAME = "$pong";
    public static final String CLOSE_NAME = "$close";
    public static final String ERROR_NAME = "$error";

    /** The `$hello` payload offered by the connection initiator. */
    public record Hello(List<Long> versions, List<String> profiles, String service, String auth) {
        public byte[] encode() {
            List<CborValue> vers = new ArrayList<>();
            for (long v : versions) {
                vers.add(new CUint(v));
            }
            List<CborValue> profs = new ArrayList<>();
            for (String p : profiles) {
                profs.add(new CText(p));
            }
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("versions", new CArray(vers)));
            entries.add(entry("profiles", new CArray(profs)));
            if (service != null) {
                entries.add(textEntry("service", service));
            }
            if (auth != null) {
                entries.add(textEntry("auth", auth));
            }
            return encodeMap(entries);
        }

        public static Hello decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            if (!(mapGet(v, "versions") instanceof CArray versArr)) {
                throw new MalformedException("hello missing 'versions'");
            }
            List<Long> versions = new ArrayList<>();
            for (CborValue x : versArr.items()) {
                Long n = asU64(x);
                if (n != null) {
                    versions.add(n);
                }
            }
            if (!(mapGet(v, "profiles") instanceof CArray profArr)) {
                throw new MalformedException("hello missing 'profiles'");
            }
            List<String> profiles = new ArrayList<>();
            for (CborValue x : profArr.items()) {
                if (x instanceof CText t) {
                    profiles.add(t.value());
                }
            }
            return new Hello(versions, profiles, getTextOpt(v, "service"), getTextOpt(v, "auth"));
        }

        /**
         * Selects a profile from this hello's offers, honoring the peer's preference order
         * and what the receiver supports. Returns null if nothing is mutually supported.
         */
        public Negotiation negotiate(List<Profile> supported) {
            boolean hasVersion = versions.contains(VERSION);
            if (!hasVersion) {
                return null;
            }
            for (String offered : profiles) {
                Profile p = Profile.parse(offered);
                if (p != null && supported.contains(p)) {
                    return new Negotiation(VERSION, p);
                }
            }
            return null;
        }
    }

    /** The outcome of a successful {@link Hello#negotiate}. */
    public record Negotiation(long version, Profile profile) {}

    /** The `$hello-ack` payload returned by the peer. */
    public record HelloAck(long v, String profile, String session) {
        public byte[] encode() {
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("v", new CUint(v)));
            entries.add(textEntry("profile", profile));
            if (session != null) {
                entries.add(textEntry("session", session));
            }
            return encodeMap(entries);
        }

        public static HelloAck decode(byte[] b) {
            CborValue val = Cbor.decodeEnvelope(b);
            return new HelloAck(getUint(val, "v"), getText(val, "profile"),
                    getTextOpt(val, "session"));
        }
    }

    /** A `$ping`/`$pong` heartbeat payload. */
    public record Heartbeat(long nonce, Long at) {
        public byte[] encode() {
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("nonce", new CUint(nonce)));
            if (at != null) {
                entries.add(entry("at", new CUint(at)));
            }
            return encodeMap(entries);
        }

        public static Heartbeat decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            return new Heartbeat(getUint(v, "nonce"), getUintOpt(v, "at"));
        }
    }

    /** A `$close` payload. */
    public record Close(Status status, String reason) {
        public byte[] encode() {
            List<CEntry> entries = new ArrayList<>();
            entries.add(entry("status", new CInt(status.code())));
            if (reason != null) {
                entries.add(textEntry("reason", reason));
            }
            return encodeMap(entries);
        }

        public static Close decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            return new Close(Status.fromCode(getInt(v, "status")), getTextOpt(v, "reason"));
        }
    }
}
