// CSIL-RPC transport — request/response/push envelopes — see csil-rpc-transport.md.
package transport

// RpcRequest is a CSIL-RPC request (client → server).
type RpcRequest struct {
	Service string
	Op      string
	ID      *uint64
	// Payload is the opaque CBOR(request type) bytes (wrapped in tag 24 on the wire).
	Payload []byte
	Auth    *string
}

// RpcResponse is a CSIL-RPC response (server → client).
type RpcResponse struct {
	ID     *uint64
	Status Status
	// Variant names which declared output-choice arm Payload decodes to (the CSIL type name).
	Variant *string
	Error   *string
	// Payload is the opaque CBOR(output type) bytes; empty when Status is non-zero.
	Payload []byte
}

// RpcPush is a CSIL-RPC server push (server → client) for `<-` operations.
type RpcPush struct {
	Service string
	Event   string
	Payload []byte
}

// NewRpcRequest builds a request with no correlation id and no per-request auth.
func NewRpcRequest(service, op string, payload []byte) RpcRequest {
	return RpcRequest{Service: service, Op: op, Payload: payload}
}

// WithID sets the correlation id (required on multiplexed carriers).
func (r RpcRequest) WithID(id uint64) RpcRequest {
	r.ID = &id
	return r
}

// WithAuth sets the per-request credential for caller-scoped operations.
func (r RpcRequest) WithAuth(auth string) RpcRequest {
	r.Auth = &auth
	return r
}

func (r RpcRequest) Encode() ([]byte, error) {
	entries := []cEntry{
		{cText("v"), cUint(VERSION)},
		{cText("service"), cText(r.Service)},
		{cText("op"), cText(r.Op)},
		{cText("payload"), tag24(r.Payload)},
	}
	if r.ID != nil {
		entries = append(entries, cEntry{cText("id"), cUint(*r.ID)})
	}
	if r.Auth != nil {
		entries = append(entries, cEntry{cText("auth"), cText(*r.Auth)})
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeRpcRequest(b []byte) (RpcRequest, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return RpcRequest{}, err
	}
	ver, err := getUint(v, "v")
	if err != nil {
		return RpcRequest{}, err
	}
	if err := checkVersion(ver); err != nil {
		return RpcRequest{}, err
	}
	p, ok := mapGet(v, "payload")
	if !ok {
		return RpcRequest{}, malformed("missing 'payload'")
	}
	payload, err := untag24(p)
	if err != nil {
		return RpcRequest{}, err
	}
	service, err := getText(v, "service")
	if err != nil {
		return RpcRequest{}, err
	}
	op, err := getText(v, "op")
	if err != nil {
		return RpcRequest{}, err
	}
	return RpcRequest{
		Service: service,
		Op:      op,
		ID:      getUintOpt(v, "id"),
		Payload: payload,
		Auth:    getTextOpt(v, "auth"),
	}, nil
}

// NewRpcResponseOk builds a successful (Status ok) typed reply.
func NewRpcResponseOk(variant string, payload []byte) RpcResponse {
	return RpcResponse{Status: StatusOk, Variant: &variant, Payload: payload}
}

// NewRpcResponseTransportError builds a transport-level failure (no typed payload).
func NewRpcResponseTransportError(status Status, message string) RpcResponse {
	return RpcResponse{Status: status, Error: &message, Payload: []byte{}}
}

// WithID sets the echoed correlation id.
func (r RpcResponse) WithID(id *uint64) RpcResponse {
	r.ID = id
	return r
}

func (r RpcResponse) Encode() ([]byte, error) {
	entries := []cEntry{
		{cText("v"), cUint(VERSION)},
		{cText("status"), cInt(r.Status.Code())},
		{cText("payload"), tag24(r.Payload)},
	}
	if r.ID != nil {
		entries = append(entries, cEntry{cText("id"), cUint(*r.ID)})
	}
	if r.Variant != nil {
		entries = append(entries, cEntry{cText("variant"), cText(*r.Variant)})
	}
	if r.Error != nil {
		entries = append(entries, cEntry{cText("error"), cText(*r.Error)})
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeRpcResponse(b []byte) (RpcResponse, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return RpcResponse{}, err
	}
	ver, err := getUint(v, "v")
	if err != nil {
		return RpcResponse{}, err
	}
	if err := checkVersion(ver); err != nil {
		return RpcResponse{}, err
	}
	// payload is present but may be an empty byte string on error.
	payload := []byte{}
	if p, ok := mapGet(v, "payload"); ok {
		payload, err = untag24(p)
		if err != nil {
			return RpcResponse{}, err
		}
	}
	status, err := getInt(v, "status")
	if err != nil {
		return RpcResponse{}, err
	}
	return RpcResponse{
		ID:      getUintOpt(v, "id"),
		Status:  StatusFromCode(status),
		Variant: getTextOpt(v, "variant"),
		Error:   getTextOpt(v, "error"),
		Payload: payload,
	}, nil
}

// AsTransportError returns a StatusError for a non-ok response, or nil when the
// response carries a typed reply (status 0). Callers use this after decode to
// surface transport failures distinctly from application errors.
func (r RpcResponse) AsTransportError() error {
	if r.Status.IsOk() {
		return nil
	}
	msg := ""
	if r.Error != nil {
		msg = *r.Error
	}
	return StatusError{Status: r.Status.Name(), Code: r.Status.Code(), Message: msg}
}

// NewRpcPush builds a server-push envelope for a `<-` operation.
func NewRpcPush(service, event string, payload []byte) RpcPush {
	return RpcPush{Service: service, Event: event, Payload: payload}
}

func (p RpcPush) Encode() ([]byte, error) {
	entries := []cEntry{
		{cText("v"), cUint(VERSION)},
		{cText("service"), cText(p.Service)},
		{cText("event"), cText(p.Event)},
		{cText("payload"), tag24(p.Payload)},
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeRpcPush(b []byte) (RpcPush, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return RpcPush{}, err
	}
	ver, err := getUint(v, "v")
	if err != nil {
		return RpcPush{}, err
	}
	if err := checkVersion(ver); err != nil {
		return RpcPush{}, err
	}
	p, ok := mapGet(v, "payload")
	if !ok {
		return RpcPush{}, malformed("missing 'payload'")
	}
	payload, err := untag24(p)
	if err != nil {
		return RpcPush{}, err
	}
	service, err := getText(v, "service")
	if err != nil {
		return RpcPush{}, err
	}
	event, err := getText(v, "event")
	if err != nil {
		return RpcPush{}, err
	}
	return RpcPush{Service: service, Event: event, Payload: payload}, nil
}

// RpcClient is a CSIL-RPC client over a frame carrier. The carrier is injected
// (bring your own); the client owns the envelope and a per-connection monotonic
// correlation id.
type RpcClient struct {
	carrier     FrameCarrier
	nextID      uint64
	multiplexed bool
}

// NewRpcClient creates a client. multiplexed true assigns a correlation id to
// every request (required on WS / pipelined streams); false omits it
// (one-in-flight carriers such as HTTP).
func NewRpcClient(carrier FrameCarrier, multiplexed bool) *RpcClient {
	return &RpcClient{carrier: carrier, nextID: 1, multiplexed: multiplexed}
}

// Call invokes service/op with an encoded request payload, returning the decoded
// response. A non-zero transport status is surfaced as a StatusError.
func (c *RpcClient) Call(service, op string, payload []byte, auth *string) (RpcResponse, error) {
	req := NewRpcRequest(service, op, payload)
	req.Auth = auth
	if c.multiplexed {
		id := c.nextID
		req.ID = &id
		c.nextID++
	}
	frame, err := req.Encode()
	if err != nil {
		return RpcResponse{}, err
	}
	if err := c.carrier.SendFrame(frame); err != nil {
		return RpcResponse{}, err
	}
	in, err := c.carrier.RecvFrame()
	if err != nil {
		return RpcResponse{}, err
	}
	if in == nil {
		return RpcResponse{}, ErrCarrier("connection closed before response")
	}
	resp, err := DecodeRpcResponse(in)
	if err != nil {
		return RpcResponse{}, err
	}
	if err := resp.AsTransportError(); err != nil {
		return RpcResponse{}, err
	}
	return resp, nil
}

// Carrier returns the underlying carrier.
func (c *RpcClient) Carrier() FrameCarrier { return c.carrier }

// HandlerOutcome is what a server handler returns for one request: a typed reply
// (variant name + encoded payload) on success, or a transport status on failure.
type HandlerOutcome struct {
	// IsReply distinguishes a typed reply from a transport-status outcome.
	IsReply bool
	Variant string
	Payload []byte
	Status  Status
	Message string
}

// Reply builds a successful handler outcome.
func Reply(variant string, payload []byte) HandlerOutcome {
	return HandlerOutcome{IsReply: true, Variant: variant, Payload: payload}
}

// Transport builds a transport-status handler outcome.
func Transport(status Status, message string) HandlerOutcome {
	return HandlerOutcome{Status: status, Message: message}
}

// RpcServer is a CSIL-RPC server over a frame carrier. The host supplies a handler
// mapping (service, op, request-payload) to an outcome; the generated router is the
// natural implementation of that handler.
type RpcServer struct {
	carrier FrameCarrier
}

func NewRpcServer(carrier FrameCarrier) *RpcServer {
	return &RpcServer{carrier: carrier}
}

// ServeOne reads one request, dispatches it through handler, and writes the
// response. It returns served=false at a clean end of stream.
func (s *RpcServer) ServeOne(handler func(*RpcRequest) HandlerOutcome) (served bool, err error) {
	frame, err := s.carrier.RecvFrame()
	if err != nil {
		return false, err
	}
	if frame == nil {
		return false, nil
	}
	var resp RpcResponse
	if req, derr := DecodeRpcRequest(frame); derr == nil {
		id := req.ID
		outcome := handler(&req)
		if outcome.IsReply {
			resp = NewRpcResponseOk(outcome.Variant, outcome.Payload).WithID(id)
		} else {
			resp = NewRpcResponseTransportError(outcome.Status, outcome.Message).WithID(id)
		}
	} else {
		resp = NewRpcResponseTransportError(StatusMalformedEnvelope, derr.Error())
	}
	out, err := resp.Encode()
	if err != nil {
		return false, err
	}
	if err := s.carrier.SendFrame(out); err != nil {
		return false, err
	}
	return true, nil
}
