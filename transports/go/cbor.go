// Minimal canonical CBOR codec (RFC 8949). Hand-written and dependency-free so the
// transport library stays offline-testable. It supports exactly what the CSIL
// envelopes need — unsigned ints, negative ints, text strings, byte strings,
// arrays, maps, and tag 24 — and nothing else. Maps are encoded with core
// deterministic encoding: entries sorted by the bytewise lexicographic order of
// their encoded keys, matching the Rust reference's canon_map so bytes are
// byte-identical to the conformance vectors.
package transport

import (
	"bytes"
	"fmt"
	"math"
	"sort"
)

// cborValue is the in-memory model of the CBOR items the envelopes use. Decoding
// produces these and encoding consumes them; transports build envelopes from the
// canonical helpers here so byte layout is independent of Go struct layout.
type cborValue interface{ isCbor() }

type cUint uint64  // major type 0
type cInt int64    // signed; encodes negative values as major type 1
type cText string  // major type 3
type cBytes []byte // major type 2
type cArray []cborValue
type cEntry struct{ Key, Val cborValue }
type cMap []cEntry
type cTag struct {
	Num     uint64
	Content cborValue
}

func (cUint) isCbor()  {}
func (cInt) isCbor()   {}
func (cText) isCbor()  {}
func (cBytes) isCbor() {}
func (cArray) isCbor() {}
func (cMap) isCbor()   {}
func (cTag) isCbor()   {}

// writeHead emits the initial byte (major type in the high three bits) plus the
// shortest-form argument bytes for n, per deterministic encoding.
func writeHead(buf *[]byte, major byte, n uint64) {
	mt := major << 5
	switch {
	case n < 24:
		*buf = append(*buf, mt|byte(n))
	case n < 1<<8:
		*buf = append(*buf, mt|24, byte(n))
	case n < 1<<16:
		*buf = append(*buf, mt|25, byte(n>>8), byte(n))
	case n < 1<<32:
		*buf = append(*buf, mt|26, byte(n>>24), byte(n>>16), byte(n>>8), byte(n))
	default:
		*buf = append(*buf, mt|27,
			byte(n>>56), byte(n>>48), byte(n>>40), byte(n>>32),
			byte(n>>24), byte(n>>16), byte(n>>8), byte(n))
	}
}

func encodeInto(buf *[]byte, v cborValue) {
	switch x := v.(type) {
	case cUint:
		writeHead(buf, 0, uint64(x))
	case cInt:
		if x >= 0 {
			writeHead(buf, 0, uint64(x))
		} else {
			// CBOR negative ints encode -1-n; the argument is the magnitude minus one.
			writeHead(buf, 1, uint64(-1-int64(x)))
		}
	case cText:
		writeHead(buf, 3, uint64(len(x)))
		*buf = append(*buf, x...)
	case cBytes:
		writeHead(buf, 2, uint64(len(x)))
		*buf = append(*buf, x...)
	case cArray:
		writeHead(buf, 4, uint64(len(x)))
		for _, e := range x {
			encodeInto(buf, e)
		}
	case cMap:
		writeHead(buf, 5, uint64(len(x)))
		for _, e := range x {
			encodeInto(buf, e.Key)
			encodeInto(buf, e.Val)
		}
	case cTag:
		writeHead(buf, 6, x.Num)
		encodeInto(buf, x.Content)
	default:
		panic(fmt.Sprintf("unsupported cbor value %T", v))
	}
}

// encodeValue serializes a value to canonical CBOR bytes.
func encodeValue(v cborValue) []byte {
	var buf []byte
	encodeInto(&buf, v)
	return buf
}

// canonMap builds a deterministically-keyed CBOR map: entries are sorted by the
// bytewise lexicographic order of their *encoded* keys (RFC 8949 §4.2.1), so the
// same logical envelope always yields the same bytes.
func canonMap(entries []cEntry) cMap {
	sorted := make([]cEntry, len(entries))
	copy(sorted, entries)
	sort.SliceStable(sorted, func(i, j int) bool {
		return bytes.Compare(encodeValue(sorted[i].Key), encodeValue(sorted[j].Key)) < 0
	})
	return cMap(sorted)
}

// decodeEnvelope decodes a complete envelope: one self-contained CBOR item with no
// trailing bytes. Conventions doc §1 says an envelope is a single CBOR item, so any
// leftover bytes are a malformed frame and rejected — matching the Rust and Python
// references rather than silently ignoring them.
func decodeEnvelope(b []byte) (cborValue, int, error) {
	v, n, err := decodeValue(b)
	if err != nil {
		return nil, 0, err
	}
	if n != len(b) {
		return nil, 0, fmt.Errorf("CBOR decode error: trailing bytes after CBOR item")
	}
	return v, n, nil
}

// decodeValue parses one CBOR item from the front of bytes, returning it and the
// number of bytes consumed. It may leave trailing bytes (it is the recursive
// workhorse used for nested items); envelope decoders use decodeEnvelope, which
// rejects trailing bytes.
func decodeValue(b []byte) (cborValue, int, error) {
	if len(b) == 0 {
		return nil, 0, fmt.Errorf("CBOR decode error: empty input")
	}
	ib := b[0]
	major := ib >> 5
	low := ib & 0x1f
	arg, n, err := readArg(b, low)
	if err != nil {
		return nil, 0, err
	}
	switch major {
	case 0:
		return cUint(arg), n, nil
	case 1:
		// CBOR negative ints encode -1-arg; an arg above math.MaxInt64 names a value
		// below math.MinInt64, which would silently wrap if forced into an int64.
		if arg > math.MaxInt64 {
			return nil, 0, fmt.Errorf("CBOR decode error: negative integer out of int64 range")
		}
		return cInt(-1 - int64(arg)), n, nil
	case 2:
		if uint64(len(b)) < uint64(n)+arg {
			return nil, 0, fmt.Errorf("CBOR decode error: truncated byte string")
		}
		out := make([]byte, arg)
		copy(out, b[n:uint64(n)+arg])
		return cBytes(out), n + int(arg), nil
	case 3:
		if uint64(len(b)) < uint64(n)+arg {
			return nil, 0, fmt.Errorf("CBOR decode error: truncated text string")
		}
		return cText(string(b[n : uint64(n)+arg])), n + int(arg), nil
	case 4:
		items := make(cArray, 0, arg)
		off := n
		for i := uint64(0); i < arg; i++ {
			item, m, err := decodeValue(b[off:])
			if err != nil {
				return nil, 0, err
			}
			items = append(items, item)
			off += m
		}
		return items, off, nil
	case 5:
		entries := make(cMap, 0, arg)
		off := n
		for i := uint64(0); i < arg; i++ {
			k, m, err := decodeValue(b[off:])
			if err != nil {
				return nil, 0, err
			}
			off += m
			v, m2, err := decodeValue(b[off:])
			if err != nil {
				return nil, 0, err
			}
			off += m2
			entries = append(entries, cEntry{Key: k, Val: v})
		}
		return entries, off, nil
	case 6:
		content, m, err := decodeValue(b[n:])
		if err != nil {
			return nil, 0, err
		}
		return cTag{Num: arg, Content: content}, n + m, nil
	default:
		return nil, 0, fmt.Errorf("CBOR decode error: unsupported major type %d", major)
	}
}

// readArg reads the additional-information argument for a head byte whose low five
// bits are low, returning the argument value and the total head length (including
// the initial byte).
func readArg(b []byte, low byte) (uint64, int, error) {
	switch {
	case low < 24:
		return uint64(low), 1, nil
	case low == 24:
		if len(b) < 2 {
			return 0, 0, fmt.Errorf("CBOR decode error: truncated 1-byte argument")
		}
		return uint64(b[1]), 2, nil
	case low == 25:
		if len(b) < 3 {
			return 0, 0, fmt.Errorf("CBOR decode error: truncated 2-byte argument")
		}
		return uint64(b[1])<<8 | uint64(b[2]), 3, nil
	case low == 26:
		if len(b) < 5 {
			return 0, 0, fmt.Errorf("CBOR decode error: truncated 4-byte argument")
		}
		return uint64(b[1])<<24 | uint64(b[2])<<16 | uint64(b[3])<<8 | uint64(b[4]), 5, nil
	case low == 27:
		if len(b) < 9 {
			return 0, 0, fmt.Errorf("CBOR decode error: truncated 8-byte argument")
		}
		var v uint64
		for i := 1; i <= 8; i++ {
			v = v<<8 | uint64(b[i])
		}
		return v, 9, nil
	default:
		// 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes.
		return 0, 0, fmt.Errorf("CBOR decode error: indefinite or reserved additional info %d", low)
	}
}
