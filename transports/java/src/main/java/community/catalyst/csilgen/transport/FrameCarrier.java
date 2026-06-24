// The bring-your-own-carrier boundary (conventions doc §7). The library owns envelope
// codecs, framing, and lifecycle; the carrier (the byte/datagram transport) is injected.
// Blocking I/O can legitimately fail, so the seam declares a checked IOException — the
// idiomatic Java contract for blocking I/O.
package community.catalyst.csilgen.transport;

import java.io.IOException;

/**
 * Sends and receives one delimited message at a time. Used by CSIL-RPC and CSIL-Events.
 * Built-in implementations frame with a 4-byte big-endian length prefix; a host may
 * implement this over WebSocket binary frames, a WebTransport stream, etc.
 */
public interface FrameCarrier {
    void sendFrame(byte[] frame) throws IOException;

    /** The next frame, or null at a clean end of stream. */
    byte[] recvFrame() throws IOException;
}
