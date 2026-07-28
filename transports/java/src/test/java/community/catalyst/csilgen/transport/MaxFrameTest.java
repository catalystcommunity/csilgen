// The configurable max-frame guard (conventions doc §5): a host sets the limit up or down
// through the carrier's public API, the limit applies to reads and writes alike, an oversized
// inbound length is rejected before allocation, and an invalid limit fails at construction
// rather than on the first frame.
package community.catalyst.csilgen.transport;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.Arrays;

class MaxFrameTest {

    /** Counts bytes handed out, so a test can prove the guard fires before the body is read. */
    private static final class CountingInputStream extends InputStream {
        private final InputStream inner;
        int read;

        CountingInputStream(byte[] bytes) {
            this.inner = new ByteArrayInputStream(bytes);
        }

        @Override
        public int read() throws IOException {
            int b = inner.read();
            if (b != -1) {
                read++;
            }
            return b;
        }

        @Override
        public int read(byte[] b, int off, int len) throws IOException {
            int n = inner.read(b, off, len);
            if (n > 0) {
                read += n;
            }
            return n;
        }
    }

    @org.junit.jupiter.api.Test
    void defaultLimitAcceptsFrameBelowIt() throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] frame = new byte[1024];
        Arrays.fill(frame, (byte) 0xAB);
        new Carriers.StreamCarrier(InputStream.nullInputStream(), out).sendFrame(frame);

        Carriers.StreamCarrier reader =
                new Carriers.StreamCarrier(new ByteArrayInputStream(out.toByteArray()), out);
        assertArrayEquals(frame, reader.recvFrame());
    }

    @org.junit.jupiter.api.Test
    void defaultLimitRejectsFrameAboveIt() {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        Carriers.StreamCarrier carrier =
                new Carriers.StreamCarrier(InputStream.nullInputStream(), out);
        byte[] frame = new byte[Conventions.MAX_FRAME_DEFAULT + 1];
        assertThrows(FrameTooLargeException.class, () -> carrier.sendFrame(frame));
        assertEquals(0, out.size(), "a rejected frame must not put bytes on the wire");
    }

    @org.junit.jupiter.api.Test
    void largerCustomLimitAcceptsWhatDefaultRejects() throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        int raised = Conventions.MAX_FRAME_DEFAULT + 4096;
        byte[] frame = new byte[Conventions.MAX_FRAME_DEFAULT + 1];
        new Carriers.StreamCarrier(InputStream.nullInputStream(), out, raised).sendFrame(frame);

        Carriers.StreamCarrier reader =
                new Carriers.StreamCarrier(new ByteArrayInputStream(out.toByteArray()), out, raised);
        assertEquals(frame.length, reader.recvFrame().length);
    }

    @org.junit.jupiter.api.Test
    void smallerCustomLimitRejectsWhatDefaultAccepts() {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        Carriers.StreamCarrier carrier =
                new Carriers.StreamCarrier(InputStream.nullInputStream(), out, 64);
        byte[] frame = new byte[1024];
        assertThrows(FrameTooLargeException.class, () -> carrier.sendFrame(frame));
    }

    @org.junit.jupiter.api.Test
    void oversizedIncomingLengthRejectedBeforeAllocation() {
        // A prefix claiming ~4 GiB followed by no body: if the guard ran after the read this
        // would allocate; it must fail on the prefix alone.
        CountingInputStream in =
                new CountingInputStream(new byte[] {(byte) 0xFF, (byte) 0xFF, (byte) 0xFF, (byte) 0xFF});
        Carriers.StreamCarrier carrier =
                new Carriers.StreamCarrier(in, new ByteArrayOutputStream(), 4096);
        assertThrows(FrameTooLargeException.class, carrier::recvFrame);
        assertEquals(4, in.read, "guard must fire on the 4-byte prefix alone");
    }

    @org.junit.jupiter.api.Test
    void invalidLimitsRejectedAtConstruction() {
        // MAX_FRAME_LIMIT is Integer.MAX_VALUE, so no int argument can exceed it; the
        // reachable invalid values are all at or below zero.
        for (int limit : new int[] {0, -1, -4096, Integer.MIN_VALUE}) {
            assertThrows(
                    InvalidMaxFrameException.class,
                    () ->
                            new Carriers.StreamCarrier(
                                    InputStream.nullInputStream(), new ByteArrayOutputStream(), limit),
                    "limit " + limit + " must be rejected");
        }
    }

    @org.junit.jupiter.api.Test
    void boundaryLimitsAccepted() {
        for (int limit :
                new int[] {1, Conventions.MAX_FRAME_DEFAULT, Conventions.MAX_FRAME_LIMIT}) {
            Carriers.StreamCarrier carrier =
                    new Carriers.StreamCarrier(
                            InputStream.nullInputStream(), new ByteArrayOutputStream(), limit);
            assertEquals(limit, carrier.maxFrame());
        }
    }
}
