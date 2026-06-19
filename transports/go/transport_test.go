// Round-trip unit tests mirroring the Rust reference's lib.rs tests: codecs over
// the loopback carrier, events negotiation, sequence tracking, version rejection,
// and stream framing.
package transport

import (
	"bytes"
	"errors"
	"reflect"
	"testing"
)

func ptrU64(n uint64) *uint64 { return &n }
func ptrStr(s string) *string { return &s }

func TestRpcRequestRoundTrip(t *testing.T) {
	req := NewRpcRequest("Attestation", "deposit-claim", []byte{0xa0}).WithID(7)
	b, err := req.Encode()
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeRpcRequest(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dec, req) {
		t.Fatalf("got %+v want %+v", dec, req)
	}
}

func TestRpcResponseSuccessAndError(t *testing.T) {
	ok := NewRpcResponseOk("DepositClaimResponse", []byte{0xa0}).WithID(ptrU64(7))
	b, err := ok.Encode()
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeRpcResponse(b)
	if err != nil {
		t.Fatal(err)
	}
	if dec.Status != StatusOk {
		t.Fatalf("status: %v", dec.Status)
	}
	if dec.Variant == nil || *dec.Variant != "DepositClaimResponse" {
		t.Fatalf("variant: %v", dec.Variant)
	}
	if err := dec.AsTransportError(); err != nil {
		t.Fatalf("expected ok, got %v", err)
	}

	e := NewRpcResponseTransportError(StatusUnknownServiceOrOp, "no such op")
	b, err = e.Encode()
	if err != nil {
		t.Fatal(err)
	}
	dec, err = DecodeRpcResponse(b)
	if err != nil {
		t.Fatal(err)
	}
	if dec.Status != StatusUnknownServiceOrOp {
		t.Fatalf("status: %v", dec.Status)
	}
	if dec.AsTransportError() == nil {
		t.Fatal("expected transport error")
	}
}

func TestRpcApplicationErrorIsStatusOk(t *testing.T) {
	// An application error rides as a typed variant with transport status 0.
	appErr := NewRpcResponseOk("ServiceError", []byte{0xa1, 0x00, 0x01})
	b, err := appErr.Encode()
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeRpcResponse(b)
	if err != nil {
		t.Fatal(err)
	}
	if dec.Status != StatusOk {
		t.Fatalf("status: %v", dec.Status)
	}
	if dec.Variant == nil || *dec.Variant != "ServiceError" {
		t.Fatalf("variant: %v", dec.Variant)
	}
	// AsTransportError leaves it nil — the caller routes it by variant.
	if err := dec.AsTransportError(); err != nil {
		t.Fatalf("expected ok, got %v", err)
	}
}

func TestRpcClientServerOverLoopback(t *testing.T) {
	// Server replies to a request placed on the loopback's inbound queue.
	req := NewRpcRequest("Echo", "say", []byte{0x63, 'h', 'i'}).WithID(1)
	serverCarrier := NewLoopbackFrameCarrier()
	enc, err := req.Encode()
	if err != nil {
		t.Fatal(err)
	}
	serverCarrier.PushInbound(enc)
	server := NewRpcServer(serverCarrier)
	served, err := server.ServeOne(func(r *RpcRequest) HandlerOutcome {
		return Reply("SayResponse", r.Payload)
	})
	if err != nil {
		t.Fatal(err)
	}
	if !served {
		t.Fatal("expected to serve a request")
	}

	// And the full client round-trip: feed the server's reply back to a client.
	resp := serverCarrier.TakeOutbound()
	clientCarrier := NewLoopbackFrameCarrier()
	clientCarrier.PushInbound(resp)
	client := NewRpcClient(clientCarrier, true)
	got, err := client.Call("Echo", "say", []byte{0x63, 'h', 'i'}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if got.Variant == nil || *got.Variant != "SayResponse" {
		t.Fatalf("variant: %v", got.Variant)
	}
	if !bytes.Equal(got.Payload, []byte{0x63, 'h', 'i'}) {
		t.Fatalf("payload: %v", got.Payload)
	}
}

func TestRpcVersionMismatchRejected(t *testing.T) {
	// Hand-craft an envelope with v=2; decode must reject, not misparse.
	bad := canonMap([]cEntry{
		{cText("v"), cUint(2)},
		{cText("service"), cText("S")},
		{cText("op"), cText("o")},
		{cText("payload"), tag24([]byte{0xa0})},
	})
	b := encodeValue(bad)
	if _, err := DecodeRpcRequest(b); err == nil {
		t.Fatal("expected version rejection")
	} else if !errors.As(err, &ErrUnsupportedVersion{}) {
		t.Fatalf("expected ErrUnsupportedVersion, got %v", err)
	}
}

func TestEventsVerboseRoundTrip(t *testing.T) {
	ev := NewVerboseEvent(ptrStr("World"), "chat", []byte{0x60}).WithID(3)
	b, err := ev.Encode(ProfileVerbose)
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeEvent(b, ProfileVerbose)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dec, ev) {
		t.Fatalf("got %+v want %+v", dec, ev)
	}
}

func TestEventsCompactRoundTripBothShapes(t *testing.T) {
	fire := NewCompactEvent(1, 0, []byte{0x60})
	b, err := fire.Encode(ProfileCompact)
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeEvent(b, ProfileCompact)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dec, fire) {
		t.Fatalf("got %+v want %+v", dec, fire)
	}

	corr := NewCompactEvent(1, 2, []byte{0x60}).WithID(42)
	b, err = corr.Encode(ProfileCompact)
	if err != nil {
		t.Fatal(err)
	}
	dec, err = DecodeEvent(b, ProfileCompact)
	if err != nil {
		t.Fatal(err)
	}
	if dec.ID == nil || *dec.ID != 42 {
		t.Fatalf("id: %v", dec.ID)
	}
	if !reflect.DeepEqual(dec, corr) {
		t.Fatalf("got %+v want %+v", dec, corr)
	}
}

func TestEventsHelloNegotiation(t *testing.T) {
	hello := Hello{
		Versions: []uint64{1},
		Profiles: []string{"compact", "verbose"},
		Service:  ptrStr("World"),
	}
	b, err := hello.Encode()
	if err != nil {
		t.Fatal(err)
	}
	round, err := DecodeHello(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(round, hello) {
		t.Fatalf("got %+v want %+v", round, hello)
	}
	v, p, ok := round.Negotiate([]Profile{ProfileVerbose, ProfileCompact})
	if !ok {
		t.Fatal("expected negotiation to succeed")
	}
	if v != 1 || p != ProfileCompact {
		t.Fatalf("negotiated v=%d p=%v", v, p)
	}
}

func TestEventsHelloNegotiationFailsOnVersion(t *testing.T) {
	hello := Hello{Versions: []uint64{99}, Profiles: []string{"verbose"}}
	if _, _, ok := hello.Negotiate([]Profile{ProfileVerbose}); ok {
		t.Fatal("expected negotiation to fail on version")
	}
}

func TestEventsControlRoundTrips(t *testing.T) {
	ack := HelloAck{V: 1, Profile: "compact", Session: ptrStr("s1")}
	b, err := ack.Encode()
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeHelloAck(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dec, ack) {
		t.Fatalf("got %+v want %+v", dec, ack)
	}

	ping := Heartbeat{Nonce: 9}
	b, _ = ping.Encode()
	hb, err := DecodeHeartbeat(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(hb, ping) {
		t.Fatalf("got %+v want %+v", hb, ping)
	}

	cl := Close{Status: StatusVersionUnsupported, Reason: ptrStr("bad v")}
	b, _ = cl.Encode()
	dc, err := DecodeClose(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dc, cl) {
		t.Fatalf("got %+v want %+v", dc, cl)
	}
}

func TestDatagramCborArrayRoundTrip(t *testing.T) {
	dg := NewDatagram(0, 5, []byte{0x60})
	b, err := dg.Encode()
	if err != nil {
		t.Fatal(err)
	}
	dec, err := DecodeDatagram(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dec, dg) {
		t.Fatalf("got %+v want %+v", dec, dg)
	}
	// Wrong arity rejected.
	three := encodeValue(cArray{cUint(1), cUint(0), tag24([]byte{0x60})})
	if _, err := DecodeDatagram(three); err == nil {
		t.Fatal("expected wrong-arity rejection")
	}
}

func TestDatagramCompactHeaderRoundTrip(t *testing.T) {
	dg := NewCompactDatagram(1, 0x1234, []byte{1, 2, 3})
	b := dg.Encode()
	if b[0]>>4 != 1 {
		t.Fatalf("version nibble: %d", b[0]>>4)
	}
	dec, err := DecodeCompactDatagram(b)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(dec, dg) {
		t.Fatalf("got %+v want %+v", dec, dg)
	}

	withEpoch := NewCompactDatagram(2, 7, []byte{9}).WithEpoch(4)
	dec, err = DecodeCompactDatagram(withEpoch.Encode())
	if err != nil {
		t.Fatal(err)
	}
	if dec.Epoch == nil || *dec.Epoch != 4 {
		t.Fatalf("epoch: %v", dec.Epoch)
	}
	if !reflect.DeepEqual(dec, withEpoch) {
		t.Fatalf("got %+v want %+v", dec, withEpoch)
	}
}

func TestSeqTrackerDetectsLossReorderRestart(t *testing.T) {
	tr := NewSeqTracker()
	e0 := uint8(0)
	e1 := uint8(1)
	if got := tr.Observe(1, &e0); got.Kind != SeqFirst {
		t.Fatalf("expected First, got %+v", got)
	}
	if got := tr.Observe(2, &e0); got.Kind != SeqAdvanced || got.Gap != 0 {
		t.Fatalf("expected Advanced gap 0, got %+v", got)
	}
	if got := tr.Observe(5, &e0); got.Kind != SeqAdvanced || got.Gap != 2 {
		t.Fatalf("expected Advanced gap 2, got %+v", got) // lost 3,4
	}
	if got := tr.Observe(3, &e0); got.Kind != SeqLateOrDuplicate {
		t.Fatalf("expected LateOrDuplicate, got %+v", got)
	}
	if got := tr.Observe(1, &e1); got.Kind != SeqRestart {
		t.Fatalf("expected Restart, got %+v", got) // epoch bumped
	}
}

func TestLengthPrefixFramingOverStream(t *testing.T) {
	var buf bytes.Buffer
	carrier := NewStreamCarrier(&buf)
	if err := carrier.SendFrame([]byte{1, 2, 3}); err != nil {
		t.Fatal(err)
	}
	got, err := carrier.RecvFrame()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, []byte{1, 2, 3}) {
		t.Fatalf("got %v", got)
	}
	// A clean EOF before any frame byte returns no frame.
	next, err := carrier.RecvFrame()
	if err != nil {
		t.Fatal(err)
	}
	if next != nil {
		t.Fatalf("expected nil at EOF, got %v", next)
	}
}

func TestLengthPrefixOverLimitRejected(t *testing.T) {
	// An over-limit length prefix is rejected before allocating.
	big := []byte{0x00, 0x00, 0x10, 0x00} // claims 4096 bytes
	if _, err := ReadLengthPrefixed(bytes.NewReader(big), 16); err == nil {
		t.Fatal("expected FrameTooLarge")
	} else if !errors.As(err, &ErrFrameTooLarge{}) {
		t.Fatalf("expected ErrFrameTooLarge, got %v", err)
	}
}

func TestSeqTrackerUnsequencedNeverLateOrDuplicate(t *testing.T) {
	// seq 0 is unsequenced and must never be classified as late/duplicate, even
	// repeatedly. Matches the Rust SeqTracker::observe behavior.
	tr := NewSeqTracker()
	for i := 0; i < 3; i++ {
		got := tr.Observe(0, nil)
		if got.Kind != SeqAdvanced || got.Gap != 0 {
			t.Fatalf("observe(0) #%d: expected Advanced gap 0, got %+v", i, got)
		}
	}
	// Unsequenced traffic must not perturb tracking of sequenced datagrams.
	if got := tr.Observe(1, nil); got.Kind != SeqFirst {
		t.Fatalf("expected First after unsequenced, got %+v", got)
	}
	if got := tr.Observe(0, nil); got.Kind != SeqAdvanced || got.Gap != 0 {
		t.Fatalf("interleaved observe(0): expected Advanced gap 0, got %+v", got)
	}
	if got := tr.Observe(2, nil); got.Kind != SeqAdvanced || got.Gap != 0 {
		t.Fatalf("expected Advanced gap 0 after unsequenced, got %+v", got)
	}
}

func TestDecodeRejectsTrailingBytes(t *testing.T) {
	dg := NewDatagram(0, 5, []byte{0x60})
	b, err := dg.Encode()
	if err != nil {
		t.Fatal(err)
	}
	// One extra trailing byte makes this no longer a single self-contained item.
	withTrailing := append(append([]byte{}, b...), 0x00)
	if _, err := DecodeDatagram(withTrailing); err == nil {
		t.Fatal("expected trailing-bytes rejection")
	}
}

func TestLengthPrefixHugeLengthRejected(t *testing.T) {
	// A length with the high bit set would become a negative int on a 32-bit build
	// and slip past a signed guard; the unsigned comparison must reject it instead
	// of panicking in make([]byte, n).
	huge := []byte{0xFF, 0xFF, 0xFF, 0xFF} // claims ~4 GiB
	if _, err := ReadLengthPrefixed(bytes.NewReader(huge), 16); err == nil {
		t.Fatal("expected FrameTooLarge")
	} else if !errors.As(err, &ErrFrameTooLarge{}) {
		t.Fatalf("expected ErrFrameTooLarge, got %v", err)
	}
}

func TestUDPRecvSizeCheck(t *testing.T) {
	// At or below the max is accepted; one byte over (the sentinel that the buffer's
	// extra byte exposes) is rejected rather than handed to the decoder truncated.
	if err := checkRecvSize(MaxDatagramDefault); err != nil {
		t.Fatalf("max-size datagram should be accepted, got %v", err)
	}
	if err := checkRecvSize(MaxDatagramDefault + 1); err == nil {
		t.Fatal("expected oversized datagram rejection")
	}
}

func TestNegativeIntOutOfInt64Range(t *testing.T) {
	// Major type 1 with an 8-byte argument of all-ones names -2^64, far below the
	// int64 floor; it must be rejected rather than silently wrapping.
	b := []byte{0x3B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF}
	if _, _, err := decodeValue(b); err == nil {
		t.Fatal("expected negative-integer-out-of-range rejection")
	}
}

func TestLoopbackFrameCarrierRoundTrip(t *testing.T) {
	c := NewLoopbackFrameCarrier()
	if err := c.SendFrame([]byte{1, 2, 3}); err != nil {
		t.Fatal(err)
	}
	framed := c.TakeOutbound()
	c.PushInbound(framed)
	got, err := c.RecvFrame()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, []byte{1, 2, 3}) {
		t.Fatalf("got %v", got)
	}
}
