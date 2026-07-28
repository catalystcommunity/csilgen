// The configurable max-frame guard (conventions doc §5): a host sets the limit up
// or down through the carrier's public API, the limit applies to reads and writes
// alike, an oversized inbound length is rejected before allocation, and an invalid
// limit fails at construction rather than on the first frame.
package transport

import (
	"bytes"
	"errors"
	"io"
	"testing"
)

// countingReader reports how many bytes a reader was asked for, so a test can prove
// the guard fires before the frame body is ever pulled off the wire.
type countingReader struct {
	inner io.Reader
	read  int
}

func (r *countingReader) Read(p []byte) (int, error) {
	n, err := r.inner.Read(p)
	r.read += n
	return n, err
}

func TestDefaultLimitAcceptsFrameBelowIt(t *testing.T) {
	var buf bytes.Buffer
	carrier := NewStreamCarrier(&buf)
	frame := bytes.Repeat([]byte{0xAB}, 1024)
	if err := carrier.SendFrame(frame); err != nil {
		t.Fatalf("frame under the default limit should send: %v", err)
	}
	got, err := carrier.RecvFrame()
	if err != nil {
		t.Fatalf("frame under the default limit should receive: %v", err)
	}
	if !bytes.Equal(got, frame) {
		t.Fatalf("round trip changed the frame")
	}
}

func TestDefaultLimitRejectsFrameAboveIt(t *testing.T) {
	var buf bytes.Buffer
	carrier := NewStreamCarrier(&buf)
	// One byte over the default guard, allocated once and reused by reference.
	frame := make([]byte, MaxFrameDefault+1)
	err := carrier.SendFrame(frame)
	if err == nil {
		t.Fatal("expected the default limit to reject an oversized frame")
	}
	if !errors.As(err, &ErrFrameTooLarge{}) {
		t.Fatalf("expected ErrFrameTooLarge, got %v", err)
	}
	if buf.Len() != 0 {
		t.Fatalf("a rejected frame must not put bytes on the wire, wrote %d", buf.Len())
	}
}

func TestLargerCustomLimitAcceptsWhatDefaultRejects(t *testing.T) {
	var buf bytes.Buffer
	carrier, err := NewStreamCarrierWithMaxFrame(&buf, MaxFrameDefault+4096)
	if err != nil {
		t.Fatalf("a limit above the default is valid: %v", err)
	}
	frame := make([]byte, MaxFrameDefault+1) // rejected by the default guard
	if err := carrier.SendFrame(frame); err != nil {
		t.Fatalf("raised limit should accept the frame: %v", err)
	}
	got, err := carrier.RecvFrame()
	if err != nil {
		t.Fatalf("raised limit should read the frame back: %v", err)
	}
	if len(got) != len(frame) {
		t.Fatalf("expected %d bytes back, got %d", len(frame), len(got))
	}
}

func TestSmallerCustomLimitRejectsWhatDefaultAccepts(t *testing.T) {
	var buf bytes.Buffer
	carrier, err := NewStreamCarrierWithMaxFrame(&buf, 64)
	if err != nil {
		t.Fatalf("64 is a valid limit: %v", err)
	}
	frame := bytes.Repeat([]byte{0xCD}, 1024) // well under the default guard
	if err := carrier.SendFrame(frame); err == nil {
		t.Fatal("expected the lowered limit to reject the frame")
	} else if !errors.As(err, &ErrFrameTooLarge{}) {
		t.Fatalf("expected ErrFrameTooLarge, got %v", err)
	}
}

func TestOversizedIncomingLengthRejectedBeforeAllocation(t *testing.T) {
	// A prefix claiming 4 GiB followed by no body at all: if the guard ran after
	// the read, this would block or allocate; it must fail on the prefix alone.
	prefix := []byte{0xFF, 0xFF, 0xFF, 0xFF}
	reader := &countingReader{inner: bytes.NewReader(prefix)}
	carrier, err := NewStreamCarrierWithMaxFrame(readWriter{r: reader}, 4096)
	if err != nil {
		t.Fatalf("4096 is a valid limit: %v", err)
	}
	if _, err := carrier.RecvFrame(); err == nil {
		t.Fatal("expected an oversized length prefix to be rejected")
	} else if !errors.As(err, &ErrFrameTooLarge{}) {
		t.Fatalf("expected ErrFrameTooLarge, got %v", err)
	}
	if reader.read != 4 {
		t.Fatalf("guard must fire on the 4-byte prefix alone, read %d bytes", reader.read)
	}
}

func TestInvalidLimitsRejectedAtConstruction(t *testing.T) {
	for _, limit := range []int{0, -1, -4096, MaxFrameLimit + 1} {
		var buf bytes.Buffer
		if _, err := NewStreamCarrierWithMaxFrame(&buf, limit); err == nil {
			t.Fatalf("limit %d must be rejected", limit)
		} else if !errors.As(err, &ErrInvalidMaxFrame{}) {
			t.Fatalf("limit %d: expected ErrInvalidMaxFrame, got %v", limit, err)
		}
	}
	// The boundary values are valid.
	for _, limit := range []int{1, MaxFrameDefault, MaxFrameLimit} {
		var buf bytes.Buffer
		if _, err := NewStreamCarrierWithMaxFrame(&buf, limit); err != nil {
			t.Fatalf("limit %d must be accepted, got %v", limit, err)
		}
	}
}

// readWriter adapts a bare reader to the io.ReadWriter a StreamCarrier takes; the
// write half is never exercised by the read-guard test.
type readWriter struct{ r io.Reader }

func (rw readWriter) Read(p []byte) (int, error)  { return rw.r.Read(p) }
func (rw readWriter) Write(p []byte) (int, error) { return len(p), nil }
