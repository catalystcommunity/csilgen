// CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
// csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header profiles. A
// datagram channel is single-service: the service is bound at channel setup, so datagrams
// carry no service ordinal.
package community.catalyst.csilgen.transport;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Objects;

import static community.catalyst.csilgen.transport.Conventions.*;

public final class Datagrams {
    private Datagrams() {}

    /** The conservative max datagram size (envelope + payload) safe across UDP/WebRTC/QUIC. */
    public static final int MAX_DATAGRAM_DEFAULT = 1200;

    /**
     * A datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]. {@code seq}
     * 0 means "unsequenced". The op_ord and seq are full CBOR uints in this profile.
     */
    public record Datagram(long opOrd, long seq, byte[] payload) {
        public static Datagram of(long opOrd, long seq, byte[] payload) {
            return new Datagram(opOrd, seq, payload);
        }

        public byte[] encode() {
            List<CborValue> arr = new ArrayList<>(4);
            arr.add(new CUint(VERSION));
            arr.add(new CUint(opOrd));
            arr.add(new CUint(seq));
            arr.add(tag24(payload));
            return Cbor.encode(new CArray(arr));
        }

        public static Datagram decode(byte[] b) {
            CborValue v = Cbor.decodeEnvelope(b);
            if (!(v instanceof CArray a)) {
                throw new MalformedException("datagram is not an array");
            }
            List<CborValue> arr = a.items();
            if (arr.size() != 4) {
                throw new MalformedException(
                        "datagram array has " + arr.size() + " elements, expected 4");
            }
            Long ver = asU64(arr.get(0));
            Long opOrd = asU64(arr.get(1));
            Long seq = asU64(arr.get(2));
            if (ver == null || opOrd == null || seq == null) {
                throw new MalformedException("datagram field not an integer");
            }
            checkVersion(ver);
            return new Datagram(opOrd, seq, untag24(arr.get(3)));
        }

        @Override
        public boolean equals(Object o) {
            return o instanceof Datagram d
                    && opOrd == d.opOrd
                    && seq == d.seq
                    && Arrays.equals(payload, d.payload);
        }

        @Override
        public int hashCode() {
            return Objects.hash(opOrd, seq, Arrays.hashCode(payload));
        }
    }

    // Compact fixed-header layout: [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then body.
    private static final int COMPACT_VER = 1;
    private static final int FLAG_EPOCH = 0b0001;

    /**
     * A datagram in the compact fixed-header profile. {@code opOrd} is a u8, {@code seq} a
     * u16, {@code epoch} an optional u8 (present when the sender tracks restarts). {@code
     * body} is the opaque body (tag-24 CBOR or a raw media frame, by channel agreement). The
     * int-typed header fields are validated to their wire widths on construction.
     */
    public record CompactDatagram(int opOrd, int seq, Integer epoch, byte[] body) {
        public CompactDatagram {
            if (opOrd < 0 || opOrd > 0xFF) {
                throw new IllegalArgumentException("op_ord out of u8 range: " + opOrd);
            }
            if (seq < 0 || seq > 0xFFFF) {
                throw new IllegalArgumentException("seq out of u16 range: " + seq);
            }
            if (epoch != null && (epoch < 0 || epoch > 0xFF)) {
                throw new IllegalArgumentException("epoch out of u8 range: " + epoch);
            }
        }

        public static CompactDatagram of(int opOrd, int seq, byte[] body) {
            return new CompactDatagram(opOrd, seq, null, body);
        }

        /** Sets the epoch byte (and the flags bit that signals its presence). */
        public CompactDatagram withEpoch(int epoch) {
            return new CompactDatagram(opOrd, seq, epoch, body);
        }

        public byte[] encode() {
            int flags = epoch != null ? FLAG_EPOCH : 0;
            int headLen = epoch != null ? 5 : 4;
            byte[] out = new byte[headLen + body.length];
            out[0] = (byte) ((COMPACT_VER << 4) | (flags & 0x0f));
            out[1] = (byte) opOrd;
            out[2] = (byte) (seq >>> 8);
            out[3] = (byte) seq;
            int bodyStart = 4;
            if (epoch != null) {
                out[4] = (byte) (int) epoch;
                bodyStart = 5;
            }
            System.arraycopy(body, 0, out, bodyStart, body.length);
            return out;
        }

        public static CompactDatagram decode(byte[] b) {
            if (b.length < 4) {
                throw new MalformedException("compact datagram shorter than the 4-byte header");
            }
            int ver = (b[0] & 0xFF) >> 4;
            if (ver != COMPACT_VER) {
                throw new UnsupportedVersionException(ver);
            }
            int flags = b[0] & 0x0f;
            int opOrd = b[1] & 0xFF;
            int seq = ((b[2] & 0xFF) << 8) | (b[3] & 0xFF);
            Integer epoch = null;
            int bodyStart = 4;
            if ((flags & FLAG_EPOCH) != 0) {
                if (b.length < 5) {
                    throw new MalformedException(
                            "compact datagram flags claim an epoch byte that is absent");
                }
                epoch = b[4] & 0xFF;
                bodyStart = 5;
            }
            byte[] body = Arrays.copyOfRange(b, bodyStart, b.length);
            return new CompactDatagram(opOrd, seq, epoch, body);
        }

        @Override
        public boolean equals(Object o) {
            return o instanceof CompactDatagram d
                    && opOrd == d.opOrd
                    && seq == d.seq
                    && Objects.equals(epoch, d.epoch)
                    && Arrays.equals(body, d.body);
        }

        @Override
        public int hashCode() {
            return Objects.hash(opOrd, seq, epoch, Arrays.hashCode(body));
        }
    }

    /**
     * Classifies an incoming sequence number relative to what was last seen, for
     * loss/reorder/restart detection. The transport detects; the app decides.
     */
    public enum SeqEventKind {
        /** The first datagram seen on the channel. */
        FIRST,
        /** Strictly newer than the last (possibly skipping some — a gap/loss). */
        ADVANCED,
        /** Not newer (a late or duplicate datagram). */
        LATE_OR_DUPLICATE,
        /** The sender restarted (epoch changed); seq numbering reset. */
        RESTART
    }

    /** A sequence classification plus the gap count (meaningful only for ADVANCED). */
    public record SeqEvent(SeqEventKind kind, long gap) {}

    /**
     * Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced datagrams
     * (seq 0) are reported as ADVANCED with gap 0. Not synchronized; one tracker per channel
     * thread, matching the blocking, threads-only model.
     */
    public static final class SeqTracker {
        private Long lastSeq;
        private Integer lastEpoch;

        private static boolean epochEqual(Integer a, Integer b) {
            return Objects.equals(a, b);
        }

        /** Classifies an arriving (seq, epoch) and updates the tracker state. */
        public SeqEvent observe(long seq, Integer epoch) {
            // A restart fires only when a prior epoch existed and changed; no-epoch → first
            // epoch is not a restart.
            if (!epochEqual(epoch, lastEpoch) && lastEpoch != null) {
                lastEpoch = epoch;
                lastSeq = seq;
                return new SeqEvent(SeqEventKind.RESTART, 0);
            }
            lastEpoch = epoch;
            // seq 0 marks an unsequenced datagram: it carries no ordering information, so it is
            // never late or duplicate. Report a zero-gap advance and leave the running sequence
            // untouched so a mix of sequenced and unsequenced still tracks the sequenced ones.
            if (seq == 0) {
                return new SeqEvent(SeqEventKind.ADVANCED, 0);
            }
            if (lastSeq == null) {
                lastSeq = seq;
                return new SeqEvent(SeqEventKind.FIRST, 0);
            }
            long last = lastSeq;
            if (seq > last) {
                long gap = seq - last - 1;
                lastSeq = seq;
                return new SeqEvent(SeqEventKind.ADVANCED, gap);
            }
            return new SeqEvent(SeqEventKind.LATE_OR_DUPLICATE, 0);
        }
    }
}
