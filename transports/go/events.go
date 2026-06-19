// CSIL-Events transport — typed bidirectional event streams — see
// csil-events-transport.md. Verbose (text-keyed) and compact (positional)
// profiles, plus the control plane (service ordinal 0) lifecycle events.
package transport

// Profile is which wire profile a connection uses for its lifetime, fixed by hello.
type Profile int

const (
	ProfileVerbose Profile = iota
	ProfileCompact
)

func (p Profile) String() string {
	switch p {
	case ProfileVerbose:
		return "verbose"
	case ProfileCompact:
		return "compact"
	default:
		return "unknown"
	}
}

// ParseProfile maps a wire profile name onto a Profile.
func ParseProfile(s string) (Profile, bool) {
	switch s {
	case "verbose":
		return ProfileVerbose, true
	case "compact":
		return ProfileCompact, true
	default:
		return 0, false
	}
}

// Event is one typed event flowing in either direction. It is identified by
// service+operation (verbose) or by their ordinals (compact), and carries an
// optional correlation id when it is a request expecting a reply, or that reply.
type Event struct {
	// Service is the CSIL service name (verbose). nil on a single-service verbose connection.
	Service *string
	// ServiceOrd is the service ordinal (compact). Always present in compact frames.
	ServiceOrd *uint64
	// Event is the CSIL operation name (verbose).
	Event *string
	// OpOrd is the operation ordinal (compact).
	OpOrd   *uint64
	ID      *uint64
	Payload []byte
}

// NewVerboseEvent builds a verbose event by name.
func NewVerboseEvent(service *string, event string, payload []byte) Event {
	return Event{Service: service, Event: &event, Payload: payload}
}

// NewCompactEvent builds a compact event by ordinals.
func NewCompactEvent(serviceOrd, opOrd uint64, payload []byte) Event {
	return Event{ServiceOrd: &serviceOrd, OpOrd: &opOrd, Payload: payload}
}

// WithID sets the correlation id.
func (e Event) WithID(id uint64) Event {
	e.ID = &id
	return e
}

// Encode serializes the event under the given profile.
func (e Event) Encode(profile Profile) ([]byte, error) {
	switch profile {
	case ProfileVerbose:
		return e.encodeVerbose()
	case ProfileCompact:
		return e.encodeCompact()
	default:
		return nil, malformed("unknown profile")
	}
}

func (e Event) encodeVerbose() ([]byte, error) {
	if e.Event == nil {
		return nil, malformed("verbose event missing 'event' name")
	}
	entries := []cEntry{
		{cText("event"), cText(*e.Event)},
		{cText("payload"), tag24(e.Payload)},
	}
	if e.Service != nil {
		entries = append(entries, cEntry{cText("service"), cText(*e.Service)})
	}
	if e.ID != nil {
		entries = append(entries, cEntry{cText("id"), cUint(*e.ID)})
	}
	return encodeValue(canonMap(entries)), nil
}

func (e Event) encodeCompact() ([]byte, error) {
	if e.ServiceOrd == nil {
		return nil, malformed("compact event missing service ordinal")
	}
	if e.OpOrd == nil {
		return nil, malformed("compact event missing op ordinal")
	}
	arr := cArray{cUint(*e.ServiceOrd), cUint(*e.OpOrd)}
	if e.ID != nil {
		arr = append(arr, cUint(*e.ID))
	}
	arr = append(arr, tag24(e.Payload))
	return encodeValue(arr), nil
}

// DecodeEvent parses an event under the given profile.
func DecodeEvent(b []byte, profile Profile) (Event, error) {
	switch profile {
	case ProfileVerbose:
		return decodeVerboseEvent(b)
	case ProfileCompact:
		return decodeCompactEvent(b)
	default:
		return Event{}, malformed("unknown profile")
	}
}

func decodeVerboseEvent(b []byte) (Event, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return Event{}, err
	}
	p, ok := mapGet(v, "payload")
	if !ok {
		return Event{}, malformed("missing 'payload'")
	}
	payload, err := untag24(p)
	if err != nil {
		return Event{}, err
	}
	event, err := getText(v, "event")
	if err != nil {
		return Event{}, err
	}
	return Event{
		Service: getTextOpt(v, "service"),
		Event:   &event,
		ID:      getUintOpt(v, "id"),
		Payload: payload,
	}, nil
}

func decodeCompactEvent(b []byte) (Event, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return Event{}, err
	}
	arr, ok := v.(cArray)
	if !ok {
		return Event{}, malformed("compact event is not an array")
	}
	// 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
	var serviceOrdV, opOrdV, payloadV cborValue
	var idV cborValue
	switch len(arr) {
	case 3:
		serviceOrdV, opOrdV, payloadV = arr[0], arr[1], arr[2]
	case 4:
		serviceOrdV, opOrdV, idV, payloadV = arr[0], arr[1], arr[2], arr[3]
	default:
		return Event{}, malformed("compact event array has %d elements, expected 3 or 4", len(arr))
	}
	serviceOrd, ok := asU64(serviceOrdV)
	if !ok {
		return Event{}, malformed("ordinal is not an integer")
	}
	opOrd, ok := asU64(opOrdV)
	if !ok {
		return Event{}, malformed("ordinal is not an integer")
	}
	payload, err := untag24(payloadV)
	if err != nil {
		return Event{}, err
	}
	ev := Event{ServiceOrd: &serviceOrd, OpOrd: &opOrd, Payload: payload}
	if idV != nil {
		id, ok := asU64(idV)
		if !ok {
			return Event{}, malformed("ordinal is not an integer")
		}
		ev.ID = &id
	}
	return ev, nil
}

// Control-plane operation ordinals (under service ordinal 0).
const (
	ControlHello    uint64 = 0
	ControlHelloAck uint64 = 1
	ControlPing     uint64 = 2
	ControlPong     uint64 = 3
	ControlClose    uint64 = 4
	ControlError    uint64 = 5
)

// Verbose control-event names (the `$`-sigil names).
const (
	HelloName    = "$hello"
	HelloAckName = "$hello-ack"
	PingName     = "$ping"
	PongName     = "$pong"
	CloseName    = "$close"
	ErrorName    = "$error"
)

// Hello is the `$hello` payload offered by the connection initiator.
type Hello struct {
	Versions []uint64
	Profiles []string
	Service  *string
	Auth     *string
}

func (h Hello) Encode() ([]byte, error) {
	versions := make(cArray, 0, len(h.Versions))
	for _, v := range h.Versions {
		versions = append(versions, cUint(v))
	}
	profiles := make(cArray, 0, len(h.Profiles))
	for _, p := range h.Profiles {
		profiles = append(profiles, cText(p))
	}
	entries := []cEntry{
		{cText("versions"), versions},
		{cText("profiles"), profiles},
	}
	if h.Service != nil {
		entries = append(entries, cEntry{cText("service"), cText(*h.Service)})
	}
	if h.Auth != nil {
		entries = append(entries, cEntry{cText("auth"), cText(*h.Auth)})
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeHello(b []byte) (Hello, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return Hello{}, err
	}
	versRaw, ok := mapGet(v, "versions")
	if !ok {
		return Hello{}, malformed("hello missing 'versions'")
	}
	versArr, ok := versRaw.(cArray)
	if !ok {
		return Hello{}, malformed("hello missing 'versions'")
	}
	versions := []uint64{}
	for _, x := range versArr {
		if n, ok := asU64(x); ok {
			versions = append(versions, n)
		}
	}
	profRaw, ok := mapGet(v, "profiles")
	if !ok {
		return Hello{}, malformed("hello missing 'profiles'")
	}
	profArr, ok := profRaw.(cArray)
	if !ok {
		return Hello{}, malformed("hello missing 'profiles'")
	}
	profiles := []string{}
	for _, x := range profArr {
		if t, ok := x.(cText); ok {
			profiles = append(profiles, string(t))
		}
	}
	return Hello{
		Versions: versions,
		Profiles: profiles,
		Service:  getTextOpt(v, "service"),
		Auth:     getTextOpt(v, "auth"),
	}, nil
}

// Negotiate selects a profile from this hello's offers, honoring the peer's
// preference order and what the receiver supports. It returns the chosen version
// and profile, or ok=false if nothing is mutually supported.
func (h Hello) Negotiate(supported []Profile) (version uint64, profile Profile, ok bool) {
	hasVersion := false
	for _, v := range h.Versions {
		if v == VERSION {
			version = v
			hasVersion = true
			break
		}
	}
	if !hasVersion {
		return 0, 0, false
	}
	for _, offered := range h.Profiles {
		p, valid := ParseProfile(offered)
		if !valid {
			continue
		}
		for _, s := range supported {
			if s == p {
				return version, p, true
			}
		}
	}
	return 0, 0, false
}

// HelloAck is the `$hello-ack` payload returned by the peer.
type HelloAck struct {
	V       uint64
	Profile string
	Session *string
}

func (a HelloAck) Encode() ([]byte, error) {
	entries := []cEntry{
		{cText("v"), cUint(a.V)},
		{cText("profile"), cText(a.Profile)},
	}
	if a.Session != nil {
		entries = append(entries, cEntry{cText("session"), cText(*a.Session)})
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeHelloAck(b []byte) (HelloAck, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return HelloAck{}, err
	}
	ver, err := getUint(v, "v")
	if err != nil {
		return HelloAck{}, err
	}
	profile, err := getText(v, "profile")
	if err != nil {
		return HelloAck{}, err
	}
	return HelloAck{V: ver, Profile: profile, Session: getTextOpt(v, "session")}, nil
}

// Heartbeat is a `$ping`/`$pong` heartbeat payload.
type Heartbeat struct {
	Nonce uint64
	At    *uint64
}

func (h Heartbeat) Encode() ([]byte, error) {
	entries := []cEntry{{cText("nonce"), cUint(h.Nonce)}}
	if h.At != nil {
		entries = append(entries, cEntry{cText("at"), cUint(*h.At)})
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeHeartbeat(b []byte) (Heartbeat, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return Heartbeat{}, err
	}
	nonce, err := getUint(v, "nonce")
	if err != nil {
		return Heartbeat{}, err
	}
	return Heartbeat{Nonce: nonce, At: getUintOpt(v, "at")}, nil
}

// Close is a `$close` payload.
type Close struct {
	Status Status
	Reason *string
}

func (c Close) Encode() ([]byte, error) {
	entries := []cEntry{{cText("status"), cInt(c.Status.Code())}}
	if c.Reason != nil {
		entries = append(entries, cEntry{cText("reason"), cText(*c.Reason)})
	}
	return encodeValue(canonMap(entries)), nil
}

func DecodeClose(b []byte) (Close, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return Close{}, err
	}
	status, err := getInt(v, "status")
	if err != nil {
		return Close{}, err
	}
	return Close{Status: StatusFromCode(status), Reason: getTextOpt(v, "reason")}, nil
}
