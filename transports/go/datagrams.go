// CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
// csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header
// profiles. A datagram channel is single-service: the service is bound at channel
// setup, so datagrams carry no service ordinal.
package transport

// MaxDatagramDefault is the conservative max datagram size (envelope + payload)
// safe across UDP/WebRTC/QUIC.
const MaxDatagramDefault = 1200

// Datagram is a datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload].
type Datagram struct {
	OpOrd uint64
	// Seq is the per-channel sequence; 0 means "unsequenced".
	Seq uint64
	// Payload is the opaque CBOR(message type) bytes.
	Payload []byte
}

func NewDatagram(opOrd, seq uint64, payload []byte) Datagram {
	return Datagram{OpOrd: opOrd, Seq: seq, Payload: payload}
}

func (d Datagram) Encode() ([]byte, error) {
	arr := cArray{
		cUint(VERSION),
		cUint(d.OpOrd),
		cUint(d.Seq),
		tag24(d.Payload),
	}
	return encodeValue(arr), nil
}

func DecodeDatagram(b []byte) (Datagram, error) {
	v, _, err := decodeEnvelope(b)
	if err != nil {
		return Datagram{}, err
	}
	arr, ok := v.(cArray)
	if !ok {
		return Datagram{}, malformed("datagram is not an array")
	}
	if len(arr) != 4 {
		return Datagram{}, malformed("datagram array has %d elements, expected 4", len(arr))
	}
	ver, ok := asU64(arr[0])
	if !ok {
		return Datagram{}, malformed("datagram field not an integer")
	}
	if err := checkVersion(ver); err != nil {
		return Datagram{}, err
	}
	opOrd, ok := asU64(arr[1])
	if !ok {
		return Datagram{}, malformed("datagram field not an integer")
	}
	seq, ok := asU64(arr[2])
	if !ok {
		return Datagram{}, malformed("datagram field not an integer")
	}
	payload, err := untag24(arr[3])
	if err != nil {
		return Datagram{}, err
	}
	return Datagram{OpOrd: opOrd, Seq: seq, Payload: payload}, nil
}

// CompactDatagram is a datagram in the compact fixed-header profile. Header layout:
// [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then the opaque body.
type CompactDatagram struct {
	OpOrd uint8
	Seq   uint16
	// Epoch is present when the sender tracks restarts (sets the flags epoch bit).
	Epoch *uint8
	// Body is the opaque body bytes (tag-24 CBOR or a raw media frame, by channel agreement).
	Body []byte
}

const compactVer uint8 = 1
const flagEpoch uint8 = 0b0001

func NewCompactDatagram(opOrd uint8, seq uint16, body []byte) CompactDatagram {
	return CompactDatagram{OpOrd: opOrd, Seq: seq, Body: body}
}

// WithEpoch sets the epoch byte (and the flags bit that signals its presence).
func (d CompactDatagram) WithEpoch(epoch uint8) CompactDatagram {
	d.Epoch = &epoch
	return d
}

func (d CompactDatagram) Encode() []byte {
	var flags uint8
	if d.Epoch != nil {
		flags = flagEpoch
	}
	out := make([]byte, 0, 5+len(d.Body))
	out = append(out, (compactVer<<4)|(flags&0x0f))
	out = append(out, d.OpOrd)
	out = append(out, byte(d.Seq>>8), byte(d.Seq))
	if d.Epoch != nil {
		out = append(out, *d.Epoch)
	}
	out = append(out, d.Body...)
	return out
}

func DecodeCompactDatagram(b []byte) (CompactDatagram, error) {
	if len(b) < 4 {
		return CompactDatagram{}, malformed("compact datagram shorter than the 4-byte header")
	}
	ver := b[0] >> 4
	if ver != compactVer {
		return CompactDatagram{}, ErrUnsupportedVersion{V: uint64(ver)}
	}
	flags := b[0] & 0x0f
	opOrd := b[1]
	seq := uint16(b[2])<<8 | uint16(b[3])
	var epoch *uint8
	bodyStart := 4
	if flags&flagEpoch != 0 {
		if len(b) < 5 {
			return CompactDatagram{}, malformed("compact datagram flags claim an epoch byte that is absent")
		}
		e := b[4]
		epoch = &e
		bodyStart = 5
	}
	body := make([]byte, len(b)-bodyStart)
	copy(body, b[bodyStart:])
	return CompactDatagram{OpOrd: opOrd, Seq: seq, Epoch: epoch, Body: body}, nil
}

// SeqEventKind classifies an incoming sequence number relative to what was last
// seen, for loss/reorder/restart detection. The transport detects; the app decides.
type SeqEventKind int

const (
	// SeqFirst is the first datagram seen on the channel.
	SeqFirst SeqEventKind = iota
	// SeqAdvanced is strictly newer than the last (possibly skipping some — a gap/loss).
	SeqAdvanced
	// SeqLateOrDuplicate is not newer (a late or duplicate datagram).
	SeqLateOrDuplicate
	// SeqRestart means the sender restarted (epoch changed); seq numbering reset.
	SeqRestart
)

// SeqEvent is a sequence classification plus the gap count for SeqAdvanced.
type SeqEvent struct {
	Kind SeqEventKind
	// Gap is the number of skipped sequence numbers (meaningful only for SeqAdvanced).
	Gap uint64
}

// SeqTracker tracks the last sequence/epoch per channel to classify arrivals.
// Unsequenced datagrams (seq 0) are reported as SeqAdvanced with gap 0.
type SeqTracker struct {
	lastSeq   *uint64
	lastEpoch *uint8
}

func NewSeqTracker() *SeqTracker { return &SeqTracker{} }

func epochEqual(a, b *uint8) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

// Observe classifies an arriving (seq, epoch) and updates the tracker state.
func (t *SeqTracker) Observe(seq uint64, epoch *uint8) SeqEvent {
	if !epochEqual(epoch, t.lastEpoch) && t.lastEpoch != nil {
		t.lastEpoch = epoch
		s := seq
		t.lastSeq = &s
		return SeqEvent{Kind: SeqRestart}
	}
	t.lastEpoch = epoch
	// seq 0 marks an unsequenced datagram: it carries no ordering information, so it
	// is never late or duplicate. Report a zero-gap advance and leave the running
	// sequence untouched so a mix of sequenced and unsequenced still tracks the
	// sequenced ones. Matches the Rust reference SeqTracker::observe.
	if seq == 0 {
		return SeqEvent{Kind: SeqAdvanced, Gap: 0}
	}
	if t.lastSeq == nil {
		s := seq
		t.lastSeq = &s
		return SeqEvent{Kind: SeqFirst}
	}
	last := *t.lastSeq
	if seq > last {
		gap := seq - last - 1
		s := seq
		t.lastSeq = &s
		return SeqEvent{Kind: SeqAdvanced, Gap: gap}
	}
	return SeqEvent{Kind: SeqLateOrDuplicate}
}
