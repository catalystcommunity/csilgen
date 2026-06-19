// A UDP-backed DatagramCarrier. Built-in convenience for the native datagram path;
// the browser path (WebRTC unreliable / WebTransport) implements the same
// DatagramCarrier interface in the host.
package transport

import "net"

// UDPDatagramCarrier is a DatagramCarrier over a connected *net.UDPConn. One
// datagram maps to one UDP packet; ordering and delivery are not guaranteed.
type UDPDatagramCarrier struct {
	conn    *net.UDPConn
	recvBuf []byte
}

// NewUDPDatagramCarrier wraps a connected UDP socket (e.g. from net.DialUDP). The
// receive buffer is one byte larger than the max so an oversized datagram reads as
// MaxDatagramDefault+1 and can be detected rather than silently truncated.
func NewUDPDatagramCarrier(conn *net.UDPConn) *UDPDatagramCarrier {
	return &UDPDatagramCarrier{conn: conn, recvBuf: make([]byte, MaxDatagramDefault+1)}
}

// checkRecvSize rejects a read that filled past MaxDatagramDefault: with a
// MaxDatagramDefault+1 buffer that signals the kernel truncated an oversized
// datagram, so the bytes are incomplete and must not be handed to the decoder.
func checkRecvSize(n int) error {
	if n > MaxDatagramDefault {
		return ErrCarrier("datagram exceeds max size")
	}
	return nil
}

func (c *UDPDatagramCarrier) SendDatagram(b []byte) error {
	if _, err := c.conn.Write(b); err != nil {
		return ErrCarrier(err.Error())
	}
	return nil
}

func (c *UDPDatagramCarrier) RecvDatagram() ([]byte, error) {
	n, err := c.conn.Read(c.recvBuf)
	if err != nil {
		return nil, ErrCarrier(err.Error())
	}
	if err := checkRecvSize(n); err != nil {
		return nil, err
	}
	out := make([]byte, n)
	copy(out, c.recvBuf[:n])
	return out, nil
}
