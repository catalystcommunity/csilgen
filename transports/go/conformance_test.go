// Verify the Go reference library against the checked-in conformance vectors.
//
// This both guards the Go encoders/decoders against drift and demonstrates the
// contract every reference library follows: reconstruct each vector's envelope
// from its language-neutral `input`, assert encode → `hex`, and assert
// decode(`hex`) → the same envelope. Ported from transports/rust/tests/conformance.rs.
package transport

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

type vectorEntry struct {
	Name  string         `json:"name"`
	Hex   string         `json:"hex"`
	Input map[string]any `json:"input"`
}

type vectorFile struct {
	Vectors []vectorEntry `json:"vectors"`
}

func loadVectors(t *testing.T, name string) []vectorEntry {
	t.Helper()
	path := filepath.Join("..", "conformance", name)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	var doc vectorFile
	if err := json.Unmarshal(data, &doc); err != nil {
		t.Fatalf("parse %s: %v", path, err)
	}
	return doc.Vectors
}

func unhex(t *testing.T, s string) []byte {
	t.Helper()
	b, err := hex.DecodeString(s)
	if err != nil {
		t.Fatalf("bad hex %q: %v", s, err)
	}
	if b == nil {
		b = []byte{}
	}
	return b
}

func optStr(m map[string]any, k string) *string {
	v, ok := m[k]
	if !ok || v == nil {
		return nil
	}
	s := v.(string)
	return &s
}

func optU64(m map[string]any, k string) *uint64 {
	v, ok := m[k]
	if !ok || v == nil {
		return nil
	}
	n := uint64(v.(float64))
	return &n
}

func reqStr(t *testing.T, m map[string]any, k string) string {
	t.Helper()
	v, ok := m[k]
	if !ok || v == nil {
		t.Fatalf("missing required string field %q", k)
	}
	return v.(string)
}

func reqU64(t *testing.T, m map[string]any, k string) uint64 {
	t.Helper()
	v, ok := m[k]
	if !ok || v == nil {
		t.Fatalf("missing required integer field %q", k)
	}
	return uint64(v.(float64))
}

func reqI64(t *testing.T, m map[string]any, k string) int64 {
	t.Helper()
	v, ok := m[k]
	if !ok || v == nil {
		t.Fatalf("missing required integer field %q", k)
	}
	return int64(v.(float64))
}

func TestRpcVectors(t *testing.T) {
	for _, vec := range loadVectors(t, "rpc.json") {
		in := vec.Input
		var actual []byte
		switch kind := reqStr(t, in, "kind"); kind {
		case "request":
			r := NewRpcRequest(reqStr(t, in, "service"), reqStr(t, in, "op"), unhex(t, reqStr(t, in, "payload_hex")))
			r.ID = optU64(in, "id")
			r.Auth = optStr(in, "auth")
			bytes, err := r.Encode()
			if err != nil {
				t.Fatalf("%s encode: %v", vec.Name, err)
			}
			dec, err := DecodeRpcRequest(bytes)
			if err != nil {
				t.Fatalf("%s decode: %v", vec.Name, err)
			}
			if !reflect.DeepEqual(dec, r) {
				t.Errorf("%s decode mismatch: got %+v want %+v", vec.Name, dec, r)
			}
			actual = bytes
		case "response":
			r := RpcResponse{
				ID:      optU64(in, "id"),
				Status:  StatusFromCode(reqI64(t, in, "status")),
				Variant: optStr(in, "variant"),
				Error:   optStr(in, "error"),
				Payload: unhex(t, reqStr(t, in, "payload_hex")),
			}
			bytes, err := r.Encode()
			if err != nil {
				t.Fatalf("%s encode: %v", vec.Name, err)
			}
			dec, err := DecodeRpcResponse(bytes)
			if err != nil {
				t.Fatalf("%s decode: %v", vec.Name, err)
			}
			if !reflect.DeepEqual(dec, r) {
				t.Errorf("%s decode mismatch: got %+v want %+v", vec.Name, dec, r)
			}
			actual = bytes
		case "push":
			p := NewRpcPush(reqStr(t, in, "service"), reqStr(t, in, "event"), unhex(t, reqStr(t, in, "payload_hex")))
			bytes, err := p.Encode()
			if err != nil {
				t.Fatalf("%s encode: %v", vec.Name, err)
			}
			dec, err := DecodeRpcPush(bytes)
			if err != nil {
				t.Fatalf("%s decode: %v", vec.Name, err)
			}
			if !reflect.DeepEqual(dec, p) {
				t.Errorf("%s decode mismatch: got %+v want %+v", vec.Name, dec, p)
			}
			actual = bytes
		default:
			t.Fatalf("unknown rpc kind %q", kind)
		}
		if got := hex.EncodeToString(actual); got != vec.Hex {
			t.Errorf("%s encode mismatch:\n got %s\nwant %s", vec.Name, got, vec.Hex)
		}
	}
}

func TestEventsVectors(t *testing.T) {
	for _, vec := range loadVectors(t, "events.json") {
		in := vec.Input

		// Control payloads are keyed by "control"; application events by "profile".
		if control := optStr(in, "control"); control != nil {
			var bytes []byte
			var err error
			switch *control {
			case "hello":
				versions := []uint64{}
				for _, x := range in["versions"].([]any) {
					versions = append(versions, uint64(x.(float64)))
				}
				profiles := []string{}
				for _, x := range in["profiles"].([]any) {
					profiles = append(profiles, x.(string))
				}
				bytes, err = Hello{
					Versions: versions,
					Profiles: profiles,
					Service:  optStr(in, "service"),
					Auth:     optStr(in, "auth"),
				}.Encode()
			case "hello_ack":
				bytes, err = HelloAck{
					V:       reqU64(t, in, "v"),
					Profile: reqStr(t, in, "profile"),
					Session: optStr(in, "session"),
				}.Encode()
			case "ping":
				bytes, err = Heartbeat{
					Nonce: reqU64(t, in, "nonce"),
					At:    optU64(in, "at"),
				}.Encode()
			case "close":
				bytes, err = Close{
					Status: StatusFromCode(reqI64(t, in, "status")),
					Reason: optStr(in, "reason"),
				}.Encode()
			default:
				t.Fatalf("unknown control %q", *control)
			}
			if err != nil {
				t.Fatalf("%s encode: %v", vec.Name, err)
			}
			if got := hex.EncodeToString(bytes); got != vec.Hex {
				t.Errorf("%s encode mismatch:\n got %s\nwant %s", vec.Name, got, vec.Hex)
			}
			continue
		}

		profileStr := reqStr(t, in, "profile")
		profile, ok := ParseProfile(profileStr)
		if !ok {
			t.Fatalf("unknown profile %q", profileStr)
		}
		payload := unhex(t, reqStr(t, in, "payload_hex"))
		var ev Event
		switch profile {
		case ProfileVerbose:
			ev = NewVerboseEvent(optStr(in, "service"), reqStr(t, in, "event"), payload)
			ev.ID = optU64(in, "id")
		case ProfileCompact:
			ev = NewCompactEvent(reqU64(t, in, "service_ord"), reqU64(t, in, "op_ord"), payload)
			ev.ID = optU64(in, "id")
		}
		bytes, err := ev.Encode(profile)
		if err != nil {
			t.Fatalf("%s encode: %v", vec.Name, err)
		}
		if got := hex.EncodeToString(bytes); got != vec.Hex {
			t.Errorf("%s encode mismatch:\n got %s\nwant %s", vec.Name, got, vec.Hex)
		}
		dec, err := DecodeEvent(bytes, profile)
		if err != nil {
			t.Fatalf("%s decode: %v", vec.Name, err)
		}
		if !reflect.DeepEqual(dec, ev) {
			t.Errorf("%s decode mismatch: got %+v want %+v", vec.Name, dec, ev)
		}
	}
}

func TestDatagramVectors(t *testing.T) {
	for _, vec := range loadVectors(t, "datagrams.json") {
		in := vec.Input
		switch profile := reqStr(t, in, "profile"); profile {
		case "cbor-array":
			d := NewDatagram(reqU64(t, in, "op_ord"), reqU64(t, in, "seq"), unhex(t, reqStr(t, in, "payload_hex")))
			bytes, err := d.Encode()
			if err != nil {
				t.Fatalf("%s encode: %v", vec.Name, err)
			}
			if got := hex.EncodeToString(bytes); got != vec.Hex {
				t.Errorf("%s encode mismatch:\n got %s\nwant %s", vec.Name, got, vec.Hex)
			}
			dec, err := DecodeDatagram(bytes)
			if err != nil {
				t.Fatalf("%s decode: %v", vec.Name, err)
			}
			if !reflect.DeepEqual(dec, d) {
				t.Errorf("%s decode mismatch: got %+v want %+v", vec.Name, dec, d)
			}
		case "compact-header":
			d := NewCompactDatagram(uint8(reqU64(t, in, "op_ord")), uint16(reqU64(t, in, "seq")), unhex(t, reqStr(t, in, "body_hex")))
			if e := optU64(in, "epoch"); e != nil {
				d = d.WithEpoch(uint8(*e))
			}
			bytes := d.Encode()
			if got := hex.EncodeToString(bytes); got != vec.Hex {
				t.Errorf("%s encode mismatch:\n got %s\nwant %s", vec.Name, got, vec.Hex)
			}
			dec, err := DecodeCompactDatagram(bytes)
			if err != nil {
				t.Fatalf("%s decode: %v", vec.Name, err)
			}
			if !reflect.DeepEqual(dec, d) {
				t.Errorf("%s decode mismatch: got %+v want %+v", vec.Name, dec, d)
			}
		default:
			t.Fatalf("unknown datagram profile %q", profile)
		}
	}
}
