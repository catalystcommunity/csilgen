// Carrier seams — the bring-your-own-carrier boundary (conventions doc §7).
//
// The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
// byte/datagram transport) is injected. A host supplies QUIC, WebRTC, a platform
// media stack, or anything else by implementing one of these interfaces — without
// changing the library.
package transport

import (
	"encoding/binary"
	"errors"
	"io"
)

// ErrCarrier is a carrier-level transport error (a wrapped I/O failure or a
// protocol violation by the carrier).
type ErrCarrier string

func (e ErrCarrier) Error() string { return "carrier error: " + string(e) }

// FrameCarrier sends and receives one *delimited message* at a time. Used by
// CSIL-RPC and CSIL-Events. Built-in implementations frame with a 4-byte
// big-endian length prefix; a host may implement this over WebSocket binary
// frames, a WebTransport stream, etc.
type FrameCarrier interface {
	SendFrame(bytes []byte) error
	// RecvFrame receives the next frame, or nil at a clean end of stream.
	RecvFrame() ([]byte, error)
}

// DatagramCarrier sends and receives one self-contained datagram (each within the
// channel MTU), with no delivery or ordering guarantee. Used by CSIL-Datagrams.
// Built-in over UDP; a host plugs WebRTC unreliable channels, QUIC datagrams, etc.
type DatagramCarrier interface {
	SendDatagram(bytes []byte) error
	// RecvDatagram receives the next datagram, or nil if the carrier is closed.
	RecvDatagram() ([]byte, error)
}

// WriteLengthPrefixed writes a 4-byte big-endian length prefix followed by bytes
// (CSIL stream framing), enforcing the max-frame guard before writing.
func WriteLengthPrefixed(w io.Writer, b []byte, max int) error {
	if len(b) > max {
		return ErrFrameTooLarge{Got: len(b), Max: max}
	}
	var prefix [4]byte
	binary.BigEndian.PutUint32(prefix[:], uint32(len(b)))
	if _, err := w.Write(prefix[:]); err != nil {
		return ErrCarrier(err.Error())
	}
	if _, err := w.Write(b); err != nil {
		return ErrCarrier(err.Error())
	}
	return nil
}

// ReadLengthPrefixed reads one length-prefixed frame, enforcing the max-frame
// guard before allocating. It returns nil at a clean EOF before any byte of a frame.
func ReadLengthPrefixed(r io.Reader, max int) ([]byte, error) {
	var lenBuf [4]byte
	_, err := io.ReadFull(r, lenBuf[:])
	if err != nil {
		// A clean EOF before any frame byte is an orderly end of stream.
		if errors.Is(err, io.EOF) {
			return nil, nil
		}
		return nil, ErrCarrier(err.Error())
	}
	// Compare the prefix as an unsigned value before narrowing to int: on a 32-bit
	// platform a length >= 0x80000000 would become a negative int that slips past a
	// signed > max guard and then panics in make([]byte, n).
	length := binary.BigEndian.Uint32(lenBuf[:])
	if uint64(length) > uint64(max) {
		return nil, ErrFrameTooLarge{Got: int(length), Max: max}
	}
	buf := make([]byte, length)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, ErrCarrier(err.Error())
	}
	return buf, nil
}

// StreamCarrier is a FrameCarrier over any io.ReadWriter byte stream (TCP, TLS,
// Unix socket), using the canonical 4-byte length-prefix framing.
type StreamCarrier struct {
	stream   io.ReadWriter
	maxFrame int
}

func NewStreamCarrier(stream io.ReadWriter) *StreamCarrier {
	return &StreamCarrier{stream: stream, maxFrame: MaxFrameDefault}
}

func NewStreamCarrierWithMaxFrame(stream io.ReadWriter, maxFrame int) *StreamCarrier {
	return &StreamCarrier{stream: stream, maxFrame: maxFrame}
}

func (c *StreamCarrier) SendFrame(b []byte) error {
	return WriteLengthPrefixed(c.stream, b, c.maxFrame)
}

func (c *StreamCarrier) RecvFrame() ([]byte, error) {
	return ReadLengthPrefixed(c.stream, c.maxFrame)
}

// LoopbackFrameCarrier is an in-memory FrameCarrier backed by queues of frames —
// for tests and for driving the codec without a socket.
type LoopbackFrameCarrier struct {
	Outbound [][]byte
	Inbound  [][]byte
}

func NewLoopbackFrameCarrier() *LoopbackFrameCarrier { return &LoopbackFrameCarrier{} }

// PushInbound queues a frame that a subsequent RecvFrame will return.
func (c *LoopbackFrameCarrier) PushInbound(b []byte) {
	c.Inbound = append(c.Inbound, b)
}

// TakeOutbound takes the next frame that was sent via SendFrame, or nil if none.
func (c *LoopbackFrameCarrier) TakeOutbound() []byte {
	if len(c.Outbound) == 0 {
		return nil
	}
	f := c.Outbound[0]
	c.Outbound = c.Outbound[1:]
	return f
}

func (c *LoopbackFrameCarrier) SendFrame(b []byte) error {
	cp := make([]byte, len(b))
	copy(cp, b)
	c.Outbound = append(c.Outbound, cp)
	return nil
}

func (c *LoopbackFrameCarrier) RecvFrame() ([]byte, error) {
	if len(c.Inbound) == 0 {
		return nil, nil
	}
	f := c.Inbound[0]
	c.Inbound = c.Inbound[1:]
	return f, nil
}

// LoopbackDatagramCarrier is an in-memory DatagramCarrier — for tests and codec drives.
type LoopbackDatagramCarrier struct {
	Outbound [][]byte
	Inbound  [][]byte
}

func NewLoopbackDatagramCarrier() *LoopbackDatagramCarrier { return &LoopbackDatagramCarrier{} }

func (c *LoopbackDatagramCarrier) PushInbound(b []byte) {
	c.Inbound = append(c.Inbound, b)
}

func (c *LoopbackDatagramCarrier) TakeOutbound() []byte {
	if len(c.Outbound) == 0 {
		return nil
	}
	f := c.Outbound[0]
	c.Outbound = c.Outbound[1:]
	return f
}

func (c *LoopbackDatagramCarrier) SendDatagram(b []byte) error {
	cp := make([]byte, len(b))
	copy(cp, b)
	c.Outbound = append(c.Outbound, cp)
	return nil
}

func (c *LoopbackDatagramCarrier) RecvDatagram() ([]byte, error) {
	if len(c.Inbound) == 0 {
		return nil, nil
	}
	f := c.Inbound[0]
	c.Inbound = c.Inbound[1:]
	return f, nil
}
