// A UDP-backed DatagramCarrier. Built-in convenience for the native datagram path; the
// browser path (WebRTC unreliable / WebTransport) implements the same DatagramCarrier
// interface in the host.
package community.catalyst.csilgen.transport;

import java.io.IOException;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.util.Arrays;

/**
 * A DatagramCarrier over a connected {@link DatagramSocket}. One datagram maps to one UDP
 * packet; ordering and delivery are not guaranteed.
 */
public final class UdpDatagramCarrier implements DatagramCarrier {
    private final DatagramSocket socket;
    // One byte larger than the max so an oversized datagram fills the buffer and can be
    // detected as truncated rather than silently accepted.
    private final byte[] recvBuf = new byte[Datagrams.MAX_DATAGRAM_DEFAULT + 1];

    public UdpDatagramCarrier(DatagramSocket socket) {
        this.socket = socket;
    }

    @Override
    public void sendDatagram(byte[] dgram) throws IOException {
        socket.send(new DatagramPacket(dgram, dgram.length));
    }

    @Override
    public byte[] recvDatagram() throws IOException {
        DatagramPacket packet = new DatagramPacket(recvBuf, recvBuf.length);
        socket.receive(packet);
        int n = packet.getLength();
        if (n > Datagrams.MAX_DATAGRAM_DEFAULT) {
            throw new CarrierException("datagram exceeds max size");
        }
        return Arrays.copyOf(recvBuf, n);
    }
}
