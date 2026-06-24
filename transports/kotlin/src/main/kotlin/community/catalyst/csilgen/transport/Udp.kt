// A UDP-backed DatagramCarrier. Built-in convenience for the native datagram path; the
// browser path (WebRTC unreliable / WebTransport) implements the same DatagramCarrier
// interface in the host. Blocking by design — no coroutines; the host owns any threads.
package community.catalyst.csilgen.transport

import java.net.DatagramPacket
import java.net.DatagramSocket

/** A DatagramCarrier over a connected [DatagramSocket]. One datagram maps to one UDP
 *  packet; ordering and delivery are not guaranteed. The receive buffer is one byte larger
 *  than the max so an oversized datagram can be detected rather than silently truncated. */
class UdpDatagramCarrier(private val socket: DatagramSocket) : DatagramCarrier {
    private val recvBuf = ByteArray(MAX_DATAGRAM_DEFAULT + 1)

    override fun sendDatagram(bytes: ByteArray) {
        try {
            socket.send(DatagramPacket(bytes, bytes.size))
        } catch (e: java.io.IOException) {
            throw CarrierException(e.message ?: "datagram send failed", e)
        }
    }

    override fun recvDatagram(): ByteArray? {
        val packet = DatagramPacket(recvBuf, recvBuf.size)
        try {
            socket.receive(packet)
        } catch (e: java.io.IOException) {
            throw CarrierException(e.message ?: "datagram receive failed", e)
        }
        // With a MAX_DATAGRAM_DEFAULT+1 buffer, a full read signals the kernel truncated an
        // oversized datagram, so the bytes are incomplete and must not reach the decoder.
        if (packet.length > MAX_DATAGRAM_DEFAULT) throw CarrierException("datagram exceeds max size")
        return recvBuf.copyOfRange(0, packet.length)
    }
}
