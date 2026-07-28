// Built-in carrier implementations and the canonical stream framing helpers. Mirrors the
// Go reference's carrier.go: in-memory loopbacks for tests, a length-prefixed stream
// carrier, and the read/write framing with the 16 MiB frame guard enforced before
// allocating.
package community.catalyst.csilgen.transport;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayDeque;
import java.util.Deque;

public final class Carriers {
    private Carriers() {}

    /**
     * Writes a 4-byte big-endian length prefix followed by the frame (CSIL stream
     * framing), enforcing the max-frame guard before writing.
     */
    public static void writeLengthPrefixed(OutputStream out, byte[] b, int max)
            throws IOException {
        if (b.length > max) {
            throw new FrameTooLargeException(b.length, max);
        }
        byte[] prefix = new byte[4];
        prefix[0] = (byte) (b.length >>> 24);
        prefix[1] = (byte) (b.length >>> 16);
        prefix[2] = (byte) (b.length >>> 8);
        prefix[3] = (byte) b.length;
        out.write(prefix);
        out.write(b);
        out.flush();
    }

    /**
     * Reads one length-prefixed frame, enforcing the max-frame guard before allocating.
     * Returns null at a clean EOF before any byte of a frame.
     */
    public static byte[] readLengthPrefixed(InputStream in, int max) throws IOException {
        byte[] lenBuf = new byte[4];
        int first = in.read();
        if (first == -1) {
            // A clean EOF before any frame byte is an orderly end of stream.
            return null;
        }
        lenBuf[0] = (byte) first;
        readFully(in, lenBuf, 1, 3);
        // Read the prefix as an unsigned 32-bit value, then compare against max as a long
        // before narrowing: a length >= 0x80000000 must not become a negative int that
        // slips past the guard and then drives a negative allocation.
        long length = ((lenBuf[0] & 0xFFL) << 24)
                | ((lenBuf[1] & 0xFFL) << 16)
                | ((lenBuf[2] & 0xFFL) << 8)
                | (lenBuf[3] & 0xFFL);
        if (length > max) {
            throw new FrameTooLargeException(length, max);
        }
        byte[] buf = new byte[(int) length];
        readFully(in, buf, 0, buf.length);
        return buf;
    }

    private static void readFully(InputStream in, byte[] buf, int off, int len)
            throws IOException {
        int read = 0;
        while (read < len) {
            int n = in.read(buf, off + read, len - read);
            if (n == -1) {
                throw new CarrierException("unexpected EOF mid-frame");
            }
            read += n;
        }
    }

    /** A FrameCarrier over any byte stream, using the canonical 4-byte length-prefix framing. */
    public static final class StreamCarrier implements FrameCarrier {
        private final InputStream in;
        private final OutputStream out;
        private final int maxFrame;

        public StreamCarrier(InputStream in, OutputStream out) {
            this(in, out, Conventions.MAX_FRAME_DEFAULT);
        }

        /**
         * Builds a carrier with a host-chosen max-frame limit. The limit is validated here
         * rather than at the first frame, so a misconfigured carrier is a construction-time
         * error the host can surface at startup.
         */
        public StreamCarrier(InputStream in, OutputStream out, int maxFrame) {
            this.in = in;
            this.out = out;
            this.maxFrame = Conventions.validateMaxFrame(maxFrame);
        }

        /** The limit this carrier enforces in both directions. */
        public int maxFrame() {
            return maxFrame;
        }

        @Override
        public void sendFrame(byte[] frame) throws IOException {
            writeLengthPrefixed(out, frame, maxFrame);
        }

        @Override
        public byte[] recvFrame() throws IOException {
            return readLengthPrefixed(in, maxFrame);
        }
    }

    /** An in-memory FrameCarrier backed by queues — for tests and driving the codec. */
    public static final class LoopbackFrameCarrier implements FrameCarrier {
        private final Deque<byte[]> outbound = new ArrayDeque<>();
        private final Deque<byte[]> inbound = new ArrayDeque<>();

        /** Queues a frame that a subsequent recvFrame will return. */
        public void pushInbound(byte[] b) {
            inbound.addLast(b.clone());
        }

        /** Takes the next frame that was sent via sendFrame, or null if none. */
        public byte[] takeOutbound() {
            return outbound.pollFirst();
        }

        @Override
        public void sendFrame(byte[] frame) {
            outbound.addLast(frame.clone());
        }

        @Override
        public byte[] recvFrame() {
            return inbound.pollFirst();
        }
    }

    /** An in-memory DatagramCarrier — for tests and codec drives. */
    public static final class LoopbackDatagramCarrier implements DatagramCarrier {
        private final Deque<byte[]> outbound = new ArrayDeque<>();
        private final Deque<byte[]> inbound = new ArrayDeque<>();

        public void pushInbound(byte[] b) {
            inbound.addLast(b.clone());
        }

        public byte[] takeOutbound() {
            return outbound.pollFirst();
        }

        @Override
        public void sendDatagram(byte[] dgram) {
            outbound.addLast(dgram.clone());
        }

        @Override
        public byte[] recvDatagram() {
            return inbound.pollFirst();
        }
    }
}
