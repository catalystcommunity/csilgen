package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"

	rpc "github.com/catalystcommunity/csilgen/transports/go"
	api "replycheck/go"
)

type handler struct{}

var _ api.Rp = handler{}

func (handler) DecryptToken(_ context.Context, req api.Req) (api.Resp, error) {
	if req.A == "app" {
		return api.Resp{}, &api.RPCApplicationError[api.RpError]{Value: api.RpError{Code: api.RpErrorCodeApplicationUserLimit, Message: "limit"}}
	}
	return api.Resp{B: "yes"}, nil
}

type carrier []byte

func (wire carrier) Call(_ context.Context, service, op string, _ []byte) ([]byte, error) {
	reply, err := rpc.DecodeRpcResponse(wire)
	if err != nil {
		return nil, err
	}
	if err := reply.AsTransportError(); err != nil {
		return nil, err
	}
	return api.ResolveRPCReply(service, op, reply.Variant, reply.Payload)
}

func run(name string) error {
	ctx := context.Background()
	req := api.Req{A: name}
	variant, payload, err := api.EncodeRpDecryptTokenReply(handler{}.DecryptToken(ctx, req))
	if err != nil {
		return err
	}
	reply := rpc.NewRpcResponseOk(variant, payload)
	switch name {
	case "transport":
		reply = rpc.NewRpcResponseTransportError(rpc.StatusUnknownServiceOrOp, "unknown")
	case "unknown":
		other := "Other"
		reply.Variant = &other
	case "missing":
		reply.Variant = nil
	case "malformed":
		reply.Payload = []byte{0xff}
	}
	wire, err := reply.Encode()
	if err != nil {
		return err
	}
	if err := os.WriteFile("go-"+name+".cbor", wire, 0600); err != nil {
		return err
	}
	peer, err := os.ReadFile("rust-" + name + ".cbor")
	if err != nil {
		return err
	}
	if !bytes.Equal(wire, peer) {
		return fmt.Errorf("different %s envelopes", name)
	}
	value, err := api.NewRpClient(carrier(peer)).DecryptToken(ctx, req)
	var app *api.RPCApplicationError[api.RpError]
	var transport rpc.StatusError
	var decode *api.RPCDecodeError
	switch name {
	case "ok":
		if err == nil && value.B == "yes" {
			return nil
		}
	case "app":
		if errors.As(err, &app) && app.Value.Code == api.RpErrorCodeApplicationUserLimit && app.Value.Message == "limit" {
			return nil
		}
	case "transport":
		if errors.As(err, &transport) && transport.Code == 2 {
			return nil
		}
	default:
		if errors.As(err, &decode) {
			return nil
		}
	}
	return fmt.Errorf("unexpected %s result: %+v %v", name, value, err)
}

func main() {
	codeNumber := api.RpErrorCodeAccountRateLimited.ToNumber()
	member, err := api.RpErrorCodeFromNumber(codeNumber)
	if err != nil || codeNumber != 42 || member != api.RpErrorCodeAccountRateLimited {
		panic("incorrect enum number lookup")
	}
	if _, err := api.RpErrorCodeFromNumber(1); err == nil {
		panic("accepted an unknown enum number")
	}
	label, err := json.Marshal(api.RpErrorCodeAccountRateLimited)
	if err != nil || string(label) != `"account_rate_limited"` {
		panic("incorrect JSON enum label")
	}
	var code api.RpErrorCode
	if err := json.Unmarshal(label, &code); err != nil || code != api.RpErrorCodeAccountRateLimited {
		panic("incorrect JSON enum decode")
	}
	value := api.RpError{Code: code, Message: "limit"}
	if !bytes.Equal(api.EncodeRpError(value), []byte("\xa2\x64code\x18\x2a\x67message\x65limit")) {
		panic("incorrect CBOR enum number")
	}

	for _, name := range []string{"ok", "app", "transport", "unknown", "missing", "malformed"} {
		if err := run(name); err != nil {
			panic(err)
		}
	}
}
