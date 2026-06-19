// Conventions shared by every CSIL transport — see csil-transport-conventions.md.
//
// This file owns the parts the three transports agree on: the version constant,
// the transport status registry, tag-24 payload wrap/unwrap, the max-frame guard,
// and the canonical-CBOR field accessors the envelopes build on so their bytes
// match the conformance vectors regardless of struct layout.
package transport

import "fmt"

// VERSION is the current transport version. A new value is minted only for a
// breaking change to envelope layout or semantics.
const VERSION uint64 = 1

// TagEncodedCBOR is the CBOR semantic tag wrapping an embedded, opaque CBOR data
// item (RFC 8949 §3.4.5.1).
const TagEncodedCBOR uint64 = 24

// ControlServiceOrd is the reserved service ordinal for the transport control
// plane (Events lifecycle).
const ControlServiceOrd uint64 = 0

// MaxFrameDefault is the default max encoded envelope size for stream/message
// carriers (16 MiB). A carrier rejects anything larger before allocating for it.
const MaxFrameDefault = 16 * 1024 * 1024

// Status is a transport-level status. It is distinct from application errors,
// which ride inside the payload as a declared `/ ErrorType` arm. See the
// conventions doc registry. Equality is by the underlying code, so host-defined
// extension codes (>= 64) and unknown codes compare correctly.
type Status struct{ code int64 }

// The registry codes (conventions doc §4).
var (
	StatusOk                 = Status{0}
	StatusMalformedEnvelope  = Status{1}
	StatusUnknownServiceOrOp = Status{2}
	StatusUnauthenticated    = Status{3}
	StatusForbidden          = Status{4}
	StatusVersionUnsupported = Status{5}
	StatusInternal           = Status{6}
	StatusUnavailable        = Status{7}
	StatusDeadlineExceeded   = Status{8}
)

// StatusFromCode maps a wire code onto a Status, preserving host-defined and
// unknown codes verbatim.
func StatusFromCode(code int64) Status { return Status{code} }

// Code returns the wire status code.
func (s Status) Code() int64 { return s.code }

// IsOk reports whether the status indicates a typed reply is present.
func (s Status) IsOk() bool { return s.code == 0 }

// Name returns the registry name for the status, or "other" for codes outside it.
func (s Status) Name() string {
	switch s.code {
	case 0:
		return "ok"
	case 1:
		return "malformed-envelope"
	case 2:
		return "unknown-service-or-op"
	case 3:
		return "unauthenticated"
	case 4:
		return "forbidden"
	case 5:
		return "version-unsupported"
	case 6:
		return "internal"
	case 7:
		return "unavailable"
	case 8:
		return "deadline-exceeded"
	default:
		return "other"
	}
}

// ErrFrameTooLarge is returned when a frame exceeds the max-frame guard; the
// receiver rejects it before allocating for it.
type ErrFrameTooLarge struct {
	Got int
	Max int
}

func (e ErrFrameTooLarge) Error() string {
	return fmt.Sprintf("frame of %d bytes exceeds max-frame guard of %d bytes", e.Got, e.Max)
}

// ErrUnsupportedVersion is returned when an envelope's version is not supported.
type ErrUnsupportedVersion struct{ V uint64 }

func (e ErrUnsupportedVersion) Error() string {
	return fmt.Sprintf("unsupported transport version %d", e.V)
}

// StatusError carries a non-zero transport status returned by a peer, distinct
// from a host's application errors.
type StatusError struct {
	Status  string
	Code    int64
	Message string
}

func (e StatusError) Error() string {
	if e.Message != "" {
		return fmt.Sprintf("transport status %s (%d): %s", e.Status, e.Code, e.Message)
	}
	return fmt.Sprintf("transport status %s (%d)", e.Status, e.Code)
}

// malformed builds a malformed-envelope error.
func malformed(format string, args ...any) error {
	return fmt.Errorf("malformed envelope: "+format, args...)
}

// tag24 wraps opaque payload bytes (themselves a CBOR item) in tag 24.
func tag24(payload []byte) cborValue {
	b := make([]byte, len(payload))
	copy(b, payload)
	return cTag{Num: TagEncodedCBOR, Content: cBytes(b)}
}

// untag24 extracts the opaque payload bytes from a tag-24 value.
func untag24(v cborValue) ([]byte, error) {
	t, ok := v.(cTag)
	if !ok || t.Num != TagEncodedCBOR {
		return nil, malformed("expected a tag-24 (encoded-cbor) payload")
	}
	b, ok := t.Content.(cBytes)
	if !ok {
		return nil, malformed("tag-24 payload is not a byte string")
	}
	out := make([]byte, len(b))
	copy(out, b)
	return out, nil
}

// mapGet looks up a text key in a CBOR map value.
func mapGet(v cborValue, key string) (cborValue, bool) {
	m, ok := v.(cMap)
	if !ok {
		return nil, false
	}
	for _, e := range m {
		if t, ok := e.Key.(cText); ok && string(t) == key {
			return e.Val, true
		}
	}
	return nil, false
}

// asU64 reads a non-negative integer from a decoded CBOR integer value.
func asU64(v cborValue) (uint64, bool) {
	switch x := v.(type) {
	case cUint:
		return uint64(x), true
	case cInt:
		if x >= 0 {
			return uint64(x), true
		}
	}
	return 0, false
}

// asI64 reads a signed integer from a decoded CBOR integer value.
func asI64(v cborValue) (int64, bool) {
	switch x := v.(type) {
	case cUint:
		return int64(x), true
	case cInt:
		return int64(x), true
	}
	return 0, false
}

func getUint(m cborValue, key string) (uint64, error) {
	v, ok := mapGet(m, key)
	if !ok {
		return 0, malformed("missing or non-integer field '%s'", key)
	}
	n, ok := asU64(v)
	if !ok {
		return 0, malformed("field '%s' is not a non-negative integer", key)
	}
	return n, nil
}

func getInt(m cborValue, key string) (int64, error) {
	v, ok := mapGet(m, key)
	if !ok {
		return 0, malformed("missing or non-integer field '%s'", key)
	}
	n, ok := asI64(v)
	if !ok {
		return 0, malformed("missing or non-integer field '%s'", key)
	}
	return n, nil
}

func getText(m cborValue, key string) (string, error) {
	v, ok := mapGet(m, key)
	if !ok {
		return "", malformed("missing or non-text field '%s'", key)
	}
	t, ok := v.(cText)
	if !ok {
		return "", malformed("missing or non-text field '%s'", key)
	}
	return string(t), nil
}

func getTextOpt(m cborValue, key string) *string {
	v, ok := mapGet(m, key)
	if !ok {
		return nil
	}
	if t, ok := v.(cText); ok {
		s := string(t)
		return &s
	}
	return nil
}

func getUintOpt(m cborValue, key string) *uint64 {
	v, ok := mapGet(m, key)
	if !ok {
		return nil
	}
	if n, ok := asU64(v); ok {
		return &n
	}
	return nil
}

// checkVersion verifies a decoded envelope's version field, returning a clear
// error otherwise so an unknown version is never silently misparsed.
func checkVersion(v uint64) error {
	if v == VERSION {
		return nil
	}
	return ErrUnsupportedVersion{V: v}
}
