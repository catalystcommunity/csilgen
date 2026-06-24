package community.catalyst.csilgen.transport;

import java.io.IOException;

/**
 * Sends and receives one self-contained datagram (each within the channel MTU), with no
 * delivery or ordering guarantee. Used by CSIL-Datagrams. A host plugs WebRTC unreliable
 * channels, QUIC datagrams, a UDP socket, etc.
 */
public interface DatagramCarrier {
    void sendDatagram(byte[] dgram) throws IOException;

    /** The next datagram, or null when the carrier is closed. */
    byte[] recvDatagram() throws IOException;
}
