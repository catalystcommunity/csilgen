// Go interop harness — mirrors tests/interop/harness/rust/src/main.rs exactly so
// a Go client interoperates with a Rust (or any-language) server and vice versa.
//
// Runs as a server (binds a loopback TCP/UDP port and serves one transport) or a
// client (connects, runs the case battery for one transport, prints JSON results).
// See tests/interop/README.md for the contract this implements.
package main

import (
	"bytes"
	"context"
	"fmt"
	"net"
	"os"
	"reflect"
	"strconv"
	"strings"
	"syscall"
	"time"

	transport "github.com/catalystcommunity/csilgen/transports/go"
	"interopapi"
)

const service = "Interop"

// ---------------------------------------------------------------------------
// Codec adapter — bridges the generated per-type Encode/Decode functions onto
// the generated channel router's Codec seam (services.gen.go).
// ---------------------------------------------------------------------------

type cborCodec struct{}

func (cborCodec) Encode(value any) ([]byte, error) {
	switch v := value.(type) {
	case interopapi.Tick:
		return interopapi.EncodeTick(v), nil
	case interopapi.Scalars:
		return interopapi.EncodeScalars(v), nil
	case interopapi.Collections:
		return interopapi.EncodeCollections(v), nil
	case interopapi.Constrained:
		return interopapi.EncodeConstrained(v), nil
	case interopapi.Nested:
		return interopapi.EncodeNested(v), nil
	case interopapi.EchoNestedResult:
		return interopapi.EncodeEchoNestedResult(v), nil
	default:
		return nil, fmt.Errorf("codec: unsupported encode type %T", value)
	}
}

func (cborCodec) Decode(data []byte, out any) error {
	switch o := out.(type) {
	case *interopapi.Tick:
		v, err := interopapi.DecodeTick(data)
		if err != nil {
			return err
		}
		*o = v
		return nil
	case *interopapi.Scalars:
		v, err := interopapi.DecodeScalars(data)
		if err != nil {
			return err
		}
		*o = v
		return nil
	case *interopapi.Collections:
		v, err := interopapi.DecodeCollections(data)
		if err != nil {
			return err
		}
		*o = v
		return nil
	default:
		return fmt.Errorf("codec: unsupported decode type %T", out)
	}
}

// ---------------------------------------------------------------------------
// Fixed language-neutral test vectors (see README "Fixed test vectors").
// ---------------------------------------------------------------------------

func goPtrInt64(v int64) *int64 { return &v }
func goPtrBool(v bool) *bool    { return &v }

func ts() time.Time {
	t, err := time.Parse(time.RFC3339, "2026-06-29T12:34:56Z")
	if err != nil {
		panic(err)
	}
	return t.UTC()
}

func mustDecimal(s string) interopapi.CsilDecimal {
	d, err := interopapi.ParseCsilDecimal(s)
	if err != nil {
		panic(err)
	}
	return d
}

func scalarsOK() interopapi.Scalars {
	return interopapi.Scalars{
		I:      -42,
		U:      42,
		N:      -7,
		F:      3.5,
		T:      "héllo 世界",
		Raw:    []byte{0x01, 0x02, 0xf0, 0xff},
		Flag:   true,
		When:   ts(),
		Amount: mustDecimal("123.45"),
		// mixed-union coverage: a value equal to a literal arm must wire as
		// [1,"pending"]; a value with no literal match must wire as [0,...].
		StatusLiteral: "pending",
		StatusFree:    "unlisted",
		// inline mixed choice (hoisted; Note is interface{} like MixedStatus above).
		Note: "info",
		// inline all-literal choice (not hoisted; plain string).
		Size: "medium",
		// named enum with a trailing `.default`-constrained last arm.
		Level: "high",
		// named enum assembled via base rule + `/=` extension.
		Season: "autumn",
		// named all-literal enum with mixed literal kinds (text + int arms);
		// ShipMode has no uniform Go type so it's `interface{}`, holding the
		// bare literal directly.
		ShipText: "ground",
		ShipInt:  int64(2),
	}
}

func collectionsOK() interopapi.Collections {
	return interopapi.Collections{
		Names:      []string{"a", "b"},
		AtLeastOne: []int64{1, 2, 3},
		Bounded:    []int64{10, 20},
		Exact3:     []int64{7, 8, 9},
		Scores:     map[string]int64{"x": 1, "y": 2},
		Color:      interopapi.Color("green"),
		Tone:       interopapi.Color("blue"),
		Prio:       interopapi.Priority(2),
		Who:        uint64(4242),
		Pair: struct {
			Field0 string
			Field1 int64
		}{Field0: "p", Field1: 5},
		Triple: struct {
			Field0 string
			Field1 *int64
			Field2 *bool
		}{Field0: "t", Field1: goPtrInt64(9), Field2: goPtrBool(true)},
		Extra: map[string]any{"k": "v"},
	}
}

func constrainedOK() interopapi.Constrained {
	return interopapi.Constrained{
		Code:     "PRD-AB12CD",
		Qty:      10,
		Rate:     mustDecimal("0.25"),
		Password: "hunter2hunter2",
		Tags:     []string{"one", "two"},
	}
}

func constrainedBad() interopapi.Constrained {
	return interopapi.Constrained{
		Code:     "bad",
		Qty:      0,
		Rate:     mustDecimal("9.9"),
		Password: "x",
		Tags:     []string{},
	}
}

func nestedOK() interopapi.Nested {
	c := constrainedOK()
	return interopapi.Nested{
		Inner: scalarsOK(),
		Maybe: &c,
		Many:  []interopapi.Scalars{scalarsOK()},
	}
}

// The three optional-bytes states. A nil Payload is *absent* (the key is omitted);
// a non-nil pointer to an empty slice is *present and empty* (the key carries 0x40).
// The pointer is what keeps those two distinguishable.
func optBytesAbsent() interopapi.OptBytes {
	return interopapi.OptBytes{Tag: "absent"}
}

func optBytesEmpty() interopapi.OptBytes {
	empty := []byte{}
	return interopapi.OptBytes{Tag: "empty", Payload: &empty}
}

func optBytesFull() interopapi.OptBytes {
	full := []byte{0x01, 0x02, 0xf0, 0xff}
	return interopapi.OptBytes{Tag: "full", Payload: &full}
}

// ---------------------------------------------------------------------------
// Result accumulation + JSON output (no third-party deps).
// ---------------------------------------------------------------------------

type caseResult struct {
	name   string
	ok     bool
	detail string
}

type cases struct {
	lang      string
	transport string
	out       []caseResult
}

func newCases(tr string) *cases {
	return &cases{lang: "go", transport: tr}
}

func (c *cases) pass(name string) {
	c.out = append(c.out, caseResult{name, true, ""})
}

func (c *cases) fail(name, detail string) {
	c.out = append(c.out, caseResult{name, false, detail})
}

func (c *cases) check(name string, cond bool, detail string) {
	if cond {
		c.pass(name)
	} else {
		c.fail(name, detail)
	}
}

func (c *cases) emit() {
	var b strings.Builder
	fmt.Fprintf(&b, "{\"lang\":\"%s\",\"transport\":\"%s\",\"cases\":[", c.lang, c.transport)
	for i, r := range c.out {
		if i > 0 {
			b.WriteByte(',')
		}
		fmt.Fprintf(&b, "{\"name\":\"%s\",\"ok\":%t,\"detail\":%s}", r.name, r.ok, jsonStr(r.detail))
	}
	b.WriteString("]}")
	fmt.Println(b.String())
}

func jsonStr(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, c := range s {
		switch c {
		case '"':
			b.WriteString("\\\"")
		case '\\':
			b.WriteString("\\\\")
		case '\n':
			b.WriteString("\\n")
		case '\t':
			b.WriteString("\\t")
		case '\r':
			b.WriteString("\\r")
		default:
			if c < 0x20 {
				fmt.Fprintf(&b, "\\u%04x", c)
			} else {
				b.WriteRune(c)
			}
		}
	}
	b.WriteByte('"')
	return b.String()
}

// ---------------------------------------------------------------------------
// Server-side handler implementing the generated Interop interface.
// ---------------------------------------------------------------------------

type handlers struct{}

func (handlers) EchoScalars(_ context.Context, req interopapi.Scalars) (interopapi.Scalars, error) {
	return req, nil
}

func (handlers) EchoCollections(_ context.Context, req interopapi.Collections) (interopapi.Collections, error) {
	return req, nil
}

func (handlers) EchoNested(_ context.Context, req interopapi.Nested) (interopapi.EchoNestedResult, error) {
	return interopapi.EchoNestedResult{Ok: true, Echo: req}, nil
}

func (handlers) EchoOptBytes(_ context.Context, req interopapi.OptBytes) (interopapi.OptBytes, error) {
	return req, nil
}

func (handlers) ValidateConstrained(_ context.Context, req interopapi.Constrained) (interopapi.Constrained, error) {
	if err := req.Validate(); err != nil {
		return interopapi.Constrained{}, err
	}
	return req, nil
}

func (handlers) Duplex(_ context.Context, _ interopapi.Scalars) error {
	return nil
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

// rpcTransport implements the generated interopapi.Transport over the lib's RpcClient.
type rpcTransport struct {
	client *transport.RpcClient
}

func (t *rpcTransport) Call(_ context.Context, svc, op string, req []byte) ([]byte, error) {
	resp, err := t.client.Call(svc, op, req, nil)
	if err != nil {
		return nil, err
	}
	// A status-0 reply whose variant is the ServiceError arm is a typed application
	// error; surface it as an error (the carrier owns this mapping). Otherwise hand
	// the success payload to the typed client.
	if resp.Variant != nil && *resp.Variant == "ServiceError" {
		se, derr := interopapi.DecodeServiceError(resp.Payload)
		if derr != nil {
			return nil, derr
		}
		return nil, fmt.Errorf("service error %d: %s", se.Code, se.Message)
	}
	return resp.Payload, nil
}

func rpcServer(ln net.Listener) {
	h := handlers{}
	for {
		conn, err := ln.Accept()
		if err != nil {
			continue
		}
		server := transport.NewRpcServer(transport.NewStreamCarrier(conn))
		dispatch := func(req *transport.RpcRequest) transport.HandlerOutcome {
			ctx := context.Background()
			switch req.Op {
			case "echo-scalars":
				v, derr := interopapi.DecodeScalars(req.Payload)
				if derr != nil {
					return transport.Transport(transport.StatusMalformedEnvelope, derr.Error())
				}
				r, herr := h.EchoScalars(ctx, v)
				if herr != nil {
					return transport.Transport(transport.StatusInternal, herr.Error())
				}
				return transport.Reply("Scalars", interopapi.EncodeScalars(r))
			case "echo-collections":
				v, derr := interopapi.DecodeCollections(req.Payload)
				if derr != nil {
					return transport.Transport(transport.StatusMalformedEnvelope, derr.Error())
				}
				r, herr := h.EchoCollections(ctx, v)
				if herr != nil {
					return transport.Transport(transport.StatusInternal, herr.Error())
				}
				return transport.Reply("Collections", interopapi.EncodeCollections(r))
			case "echo-nested":
				v, derr := interopapi.DecodeNested(req.Payload)
				if derr != nil {
					return transport.Transport(transport.StatusMalformedEnvelope, derr.Error())
				}
				r, herr := h.EchoNested(ctx, v)
				if herr != nil {
					return transport.Transport(transport.StatusInternal, herr.Error())
				}
				return transport.Reply("EchoNestedResult", interopapi.EncodeEchoNestedResult(r))
			case "echo-opt-bytes":
				v, derr := interopapi.DecodeOptBytes(req.Payload)
				if derr != nil {
					return transport.Transport(transport.StatusMalformedEnvelope, derr.Error())
				}
				r, herr := h.EchoOptBytes(ctx, v)
				if herr != nil {
					return transport.Transport(transport.StatusInternal, herr.Error())
				}
				return transport.Reply("OptBytes", interopapi.EncodeOptBytes(r))
			case "validate-constrained":
				v, derr := interopapi.DecodeConstrained(req.Payload)
				if derr != nil {
					return transport.Transport(transport.StatusMalformedEnvelope, derr.Error())
				}
				r, herr := h.ValidateConstrained(ctx, v)
				if herr != nil {
					// The typed error arm rides as a status-0 ServiceError variant.
					se := interopapi.ServiceError{Code: 422, Message: herr.Error()}
					return transport.Reply("ServiceError", interopapi.EncodeServiceError(se))
				}
				return transport.Reply("Constrained", interopapi.EncodeConstrained(r))
			default:
				return transport.Transport(transport.StatusUnknownServiceOrOp, fmt.Sprintf("unknown op %s", req.Op))
			}
		}
		for {
			served, err := server.ServeOne(dispatch)
			if err != nil || !served {
				break
			}
		}
	}
}

func rpcClient(conn net.Conn) *cases {
	c := newCases("rpc")
	tp := &rpcTransport{client: transport.NewRpcClient(transport.NewStreamCarrier(conn), false)}
	client := interopapi.NewInteropClient(tp)
	ctx := context.Background()

	if r, err := client.EchoScalars(ctx, scalarsOK()); err != nil {
		c.fail("echo-scalars/success", err.Error())
	} else {
		c.check("echo-scalars/success", reflect.DeepEqual(r, scalarsOK()), fmt.Sprintf("mismatch: %+v", r))
	}

	if r, err := client.EchoCollections(ctx, collectionsOK()); err != nil {
		c.fail("echo-collections/success", err.Error())
	} else {
		c.check("echo-collections/success", reflect.DeepEqual(r, collectionsOK()), fmt.Sprintf("mismatch: %+v", r))
	}

	if r, err := client.EchoNested(ctx, nestedOK()); err != nil {
		c.fail("echo-nested/success", err.Error())
	} else {
		c.check("echo-nested/success", r.Ok && reflect.DeepEqual(r.Echo, nestedOK()), fmt.Sprintf("mismatch: %+v", r))
	}

	if r, err := client.ValidateConstrained(ctx, constrainedOK()); err != nil {
		c.fail("validate-constrained/success", err.Error())
	} else {
		c.check("validate-constrained/success", reflect.DeepEqual(r, constrainedOK()), fmt.Sprintf("mismatch: %+v", r))
	}

	if _, err := client.ValidateConstrained(ctx, constrainedBad()); err != nil {
		c.pass("validate-constrained/failure")
	} else {
		c.fail("validate-constrained/failure", "server accepted invalid input")
	}

	// Optional-bytes presence: absent, present-empty, and present-non-empty are three
	// distinct states that must each survive the round trip unchanged.
	if r, err := client.EchoOptBytes(ctx, optBytesAbsent()); err != nil {
		c.fail("opt-bytes/absent", err.Error())
	} else {
		c.check("opt-bytes/absent", r.Payload == nil, fmt.Sprintf("expected absent, got %+v", r.Payload))
	}

	if r, err := client.EchoOptBytes(ctx, optBytesEmpty()); err != nil {
		c.fail("opt-bytes/present-empty", err.Error())
	} else {
		c.check("opt-bytes/present-empty", r.Payload != nil && len(*r.Payload) == 0,
			fmt.Sprintf("expected present-and-empty, got %+v", r.Payload))
	}

	if r, err := client.EchoOptBytes(ctx, optBytesFull()); err != nil {
		c.fail("opt-bytes/present-non-empty", err.Error())
	} else {
		c.check("opt-bytes/present-non-empty",
			r.Payload != nil && bytes.Equal(*r.Payload, []byte{0x01, 0x02, 0xf0, 0xff}),
			fmt.Sprintf("expected 0102f0ff, got %+v", r.Payload))
	}

	return c
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

const ticks = 3

func sendEvent(carrier *transport.StreamCarrier, ev transport.Event) {
	bytes, err := ev.Encode(transport.ProfileVerbose)
	if err != nil {
		return
	}
	_ = carrier.SendFrame(bytes)
}

func verboseEvent(svc *string, event string, payload []byte) transport.Event {
	return transport.NewVerboseEvent(svc, event, payload)
}

func eventsServer(ln net.Listener) {
	h := handlers{}
	codec := cborCodec{}
	svc := service
	for {
		conn, err := ln.Accept()
		if err != nil {
			continue
		}
		carrier := transport.NewStreamCarrier(conn)

		// Handshake: read $hello, reply $hello-ack (verbose).
		frame, err := carrier.RecvFrame()
		if err != nil || frame == nil {
			continue
		}
		ev, err := transport.DecodeEvent(frame, transport.ProfileVerbose)
		if err != nil || ev.Event == nil || *ev.Event != transport.HelloName {
			continue
		}
		_, _ = transport.DecodeHello(ev.Payload)
		ack := transport.HelloAck{V: 1, Profile: "verbose"}
		ackBytes, _ := ack.Encode()
		sendEvent(carrier, verboseEvent(nil, transport.HelloAckName, ackBytes))

		// Push N ticks (on-tick / server push).
		for seq := uint64(0); seq < ticks; seq++ {
			method, bytes, err := interopapi.EncodeInteropOnTick(codec, interopapi.Tick{Seq: seq})
			if err != nil {
				continue
			}
			sendEvent(carrier, verboseEvent(&svc, method, bytes))
		}

		// React loop.
		ctx := context.Background()
		for {
			frame, err := carrier.RecvFrame()
			if err != nil || frame == nil {
				break
			}
			ev, err := transport.DecodeEvent(frame, transport.ProfileVerbose)
			if err != nil {
				break
			}
			method := ""
			if ev.Event != nil {
				method = *ev.Event
			}
			switch method {
			case transport.PingName:
				sendEvent(carrier, verboseEvent(nil, transport.PongName, ev.Payload))
			case transport.CloseName:
				goto nextConn
			case "duplex":
				if s, derr := interopapi.DecodeScalars(ev.Payload); derr == nil {
					_ = h.Duplex(ctx, s)
					m, b, eerr := interopapi.EncodeInteropDuplex(codec, s)
					if eerr == nil {
						sendEvent(carrier, verboseEvent(&svc, m, b))
					}
				}
			default:
				// Exercise the generated router's unknown-method path.
				_ = interopapi.RouteInteropChannel(h, ctx, codec, method, ev.Payload)
				sendEvent(carrier, verboseEvent(nil, transport.ErrorName, []byte{}))
			}
		}
	nextConn:
	}
}

func eventsClient(conn net.Conn) *cases {
	c := newCases("events")
	carrier := transport.NewStreamCarrier(conn)
	codec := cborCodec{}
	svc := service

	// Handshake.
	hello := transport.Hello{
		Versions: []uint64{1},
		Profiles: []string{"verbose"},
		Service:  &svc,
	}
	helloBytes, _ := hello.Encode()
	sendEvent(carrier, verboseEvent(nil, transport.HelloName, helloBytes))

	ackOK := false
	if f, err := carrier.RecvFrame(); err == nil && f != nil {
		if ev, derr := transport.DecodeEvent(f, transport.ProfileVerbose); derr == nil && ev.Event != nil {
			ackOK = *ev.Event == transport.HelloAckName
		}
	}
	c.check("events/handshake", ackOK, "no $hello-ack")

	// on-tick: expect `ticks` pushes with seq 0..ticks.
	tickOK := true
	detail := ""
	for expect := uint64(0); expect < ticks; expect++ {
		f, err := carrier.RecvFrame()
		if err != nil || f == nil {
			tickOK = false
			detail = "stream closed during ticks"
			break
		}
		ev, derr := transport.DecodeEvent(f, transport.ProfileVerbose)
		if derr != nil {
			tickOK = false
			detail = "decode: " + derr.Error()
			break
		}
		if ev.Event == nil || *ev.Event != "on-tick" {
			tickOK = false
			detail = fmt.Sprintf("expected on-tick got %v", ev.Event)
			break
		}
		t, terr := interopapi.DecodeTick(ev.Payload)
		if terr != nil {
			tickOK = false
			detail = "tick decode: " + terr.Error()
			break
		}
		if t.Seq != expect {
			tickOK = false
			detail = fmt.Sprintf("tick seq %d != %d", t.Seq, expect)
			break
		}
	}
	c.check("on-tick/success", tickOK, detail)

	// duplex: send Scalars, expect echo.
	m, b, _ := interopapi.EncodeInteropDuplex(codec, scalarsOK())
	sendEvent(carrier, verboseEvent(&svc, m, b))
	if f, err := carrier.RecvFrame(); err == nil && f != nil {
		if ev, derr := transport.DecodeEvent(f, transport.ProfileVerbose); derr == nil && ev.Event != nil && *ev.Event == "duplex" {
			if s, serr := interopapi.DecodeScalars(ev.Payload); serr != nil {
				c.fail("duplex/success", serr.Error())
			} else {
				c.check("duplex/success", reflect.DeepEqual(s, scalarsOK()), "echo mismatch")
			}
		} else if derr != nil {
			c.fail("duplex/success", derr.Error())
		} else {
			c.fail("duplex/success", fmt.Sprintf("got %v", ev.Event))
		}
	} else {
		c.fail("duplex/success", "no echo")
	}

	// unknown-method: send a bogus channel method, expect $error.
	sendEvent(carrier, verboseEvent(&svc, "Bogus", []byte{}))
	if f, err := carrier.RecvFrame(); err == nil && f != nil {
		isErr := false
		if ev, derr := transport.DecodeEvent(f, transport.ProfileVerbose); derr == nil && ev.Event != nil {
			isErr = *ev.Event == transport.ErrorName
		}
		c.check("unknown-method/failure", isErr, "expected $error")
	} else {
		c.fail("unknown-method/failure", "no $error")
	}

	sendEvent(carrier, verboseEvent(nil, transport.CloseName, []byte{}))
	return c
}

// ---------------------------------------------------------------------------
// Datagrams
// ---------------------------------------------------------------------------

const (
	opEchoScalars     uint64 = 0
	opEchoCollections uint64 = 1
	opErrorSentinel   uint64 = 255
)

func datagramServer(conn *net.UDPConn) {
	buf := make([]byte, 65536)
	for {
		n, addr, err := conn.ReadFromUDP(buf)
		if err != nil {
			continue
		}
		dg, derr := transport.DecodeDatagram(buf[:n])
		if derr != nil {
			continue
		}
		var reply transport.Datagram
		switch dg.OpOrd {
		case opEchoScalars:
			if s, e := interopapi.DecodeScalars(dg.Payload); e == nil {
				reply = transport.NewDatagram(opEchoScalars, dg.Seq, interopapi.EncodeScalars(s))
			} else {
				reply = transport.NewDatagram(opErrorSentinel, dg.Seq, []byte{})
			}
		case opEchoCollections:
			if cl, e := interopapi.DecodeCollections(dg.Payload); e == nil {
				reply = transport.NewDatagram(opEchoCollections, dg.Seq, interopapi.EncodeCollections(cl))
			} else {
				reply = transport.NewDatagram(opErrorSentinel, dg.Seq, []byte{})
			}
		default:
			reply = transport.NewDatagram(opErrorSentinel, dg.Seq, []byte{})
		}
		out, eerr := reply.Encode()
		if eerr != nil {
			continue
		}
		_, _ = conn.WriteToUDP(out, addr)
	}
}

func datagramClient(conn *net.UDPConn) *cases {
	c := newCases("datagrams")
	_ = conn.SetReadDeadline(time.Now().Add(3 * time.Second))
	buf := make([]byte, 65536)

	roundtrip := func(op, seq uint64, payload []byte) (transport.Datagram, error) {
		dg := transport.NewDatagram(op, seq, payload)
		out, err := dg.Encode()
		if err != nil {
			return transport.Datagram{}, err
		}
		if _, err := conn.Write(out); err != nil {
			return transport.Datagram{}, err
		}
		_ = conn.SetReadDeadline(time.Now().Add(3 * time.Second))
		n, err := conn.Read(buf)
		if err != nil {
			return transport.Datagram{}, err
		}
		return transport.DecodeDatagram(buf[:n])
	}

	if d, err := roundtrip(opEchoScalars, 1, interopapi.EncodeScalars(scalarsOK())); err != nil {
		c.fail("echo-scalars/success", err.Error())
	} else if d.OpOrd != opEchoScalars {
		c.fail("echo-scalars/success", fmt.Sprintf("op_ord %d", d.OpOrd))
	} else if s, serr := interopapi.DecodeScalars(d.Payload); serr != nil {
		c.fail("echo-scalars/success", serr.Error())
	} else {
		c.check("echo-scalars/success", reflect.DeepEqual(s, scalarsOK()), "payload mismatch")
	}

	if d, err := roundtrip(opEchoCollections, 2, interopapi.EncodeCollections(collectionsOK())); err != nil {
		c.fail("echo-collections/success", err.Error())
	} else if d.OpOrd != opEchoCollections {
		c.fail("echo-collections/success", fmt.Sprintf("op_ord %d", d.OpOrd))
	} else if cl, cerr := interopapi.DecodeCollections(d.Payload); cerr != nil {
		c.fail("echo-collections/success", cerr.Error())
	} else {
		c.check("echo-collections/success", reflect.DeepEqual(cl, collectionsOK()), "payload mismatch")
	}

	if d, err := roundtrip(99, 3, []byte{}); err != nil {
		c.fail("bad-op-ord/failure", err.Error())
	} else {
		c.check("bad-op-ord/failure", d.OpOrd == opErrorSentinel,
			fmt.Sprintf("expected error sentinel, got op_ord %d", d.OpOrd))
	}

	return c
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

func main() {
	args := os.Args
	if len(args) < 4 {
		fmt.Fprintln(os.Stderr, "usage: csil-interop <server|client> <rpc|events|datagrams> <port>")
		os.Exit(2)
	}
	mode := args[1]
	tr := args[2]
	port, err := strconv.Atoi(args[3])
	if err != nil {
		fmt.Fprintln(os.Stderr, "bad port:", err)
		os.Exit(2)
	}
	addr := fmt.Sprintf("127.0.0.1:%d", port)

	switch {
	case mode == "server" && tr == "datagrams":
		conn := bindRetryUDP(addr)
		ready()
		datagramServer(conn)

	case mode == "server":
		ln := bindRetryTCP(addr)
		ready()
		switch tr {
		case "rpc":
			rpcServer(ln)
		case "events":
			eventsServer(ln)
		default:
			fmt.Fprintln(os.Stderr, "bad transport")
			os.Exit(2)
		}

	case mode == "client" && tr == "datagrams":
		// Bind an ephemeral local UDP port; the server replies to this source.
		raddr, err := net.ResolveUDPAddr("udp", addr)
		if err != nil {
			fmt.Fprintln(os.Stderr, "resolve server dgram:", err)
			os.Exit(1)
		}
		conn, err := net.DialUDP("udp", nil, raddr)
		if err != nil {
			fmt.Fprintln(os.Stderr, "connect dgram:", err)
			os.Exit(1)
		}
		c := datagramClient(conn)
		c.emit()
		_ = conn.Close()

	case mode == "client":
		conn := connectRetry(addr)
		var c *cases
		switch tr {
		case "rpc":
			c = rpcClient(conn)
		case "events":
			c = eventsClient(conn)
		default:
			fmt.Fprintln(os.Stderr, "bad transport")
			os.Exit(2)
		}
		c.emit()

	default:
		fmt.Fprintln(os.Stderr, "bad mode/transport")
		os.Exit(2)
	}
}

// ready prints the READY line the orchestrator waits on, after the port is bound.
func ready() {
	fmt.Println("READY")
	_ = os.Stdout.Sync()
}

// reuseControl sets SO_REUSEADDR so rapid rebind across the matrix's sequential
// per-transport runs doesn't hit a lingering TCP TIME_WAIT on the fixed port.
func reuseControl(_, _ string, c syscall.RawConn) error {
	var serr error
	if err := c.Control(func(fd uintptr) {
		serr = syscall.SetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_REUSEADDR, 1)
	}); err != nil {
		return err
	}
	return serr
}

func bindRetryTCP(addr string) net.Listener {
	lc := net.ListenConfig{Control: reuseControl}
	for i := 0; i < 200; i++ {
		ln, err := lc.Listen(context.Background(), "tcp", addr)
		if err == nil {
			return ln
		}
		time.Sleep(15 * time.Millisecond)
	}
	ln, err := lc.Listen(context.Background(), "tcp", addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "bind tcp:", err)
		os.Exit(1)
	}
	return ln
}

func bindRetryUDP(addr string) *net.UDPConn {
	lc := net.ListenConfig{Control: reuseControl}
	for i := 0; i < 200; i++ {
		pc, err := lc.ListenPacket(context.Background(), "udp", addr)
		if err == nil {
			return pc.(*net.UDPConn)
		}
		time.Sleep(15 * time.Millisecond)
	}
	pc, err := lc.ListenPacket(context.Background(), "udp", addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "bind udp:", err)
		os.Exit(1)
	}
	return pc.(*net.UDPConn)
}

func connectRetry(addr string) net.Conn {
	for i := 0; i < 200; i++ {
		conn, derr := net.Dial("tcp", addr)
		if derr == nil {
			return conn
		}
		time.Sleep(15 * time.Millisecond)
	}
	conn, derr := net.Dial("tcp", addr)
	if derr != nil {
		fmt.Fprintln(os.Stderr, "connect tcp:", derr)
		os.Exit(1)
	}
	return conn
}
