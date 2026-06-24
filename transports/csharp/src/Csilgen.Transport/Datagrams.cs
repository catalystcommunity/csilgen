// CSIL-Datagrams transport -- unreliable, unordered, message-oriented -- see
// csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header profiles. A datagram
// channel is single-service: the service is bound at channel setup, so datagrams carry no service
// ordinal.

namespace Csilgen.Transport;

public static class Datagrams
{
    // MaxDatagramDefault is the conservative max datagram size (envelope + payload) safe across
    // UDP/WebRTC/QUIC.
    public const int MaxDatagramDefault = 1200;
}

// Datagram is a datagram in the CBOR-array (default) profile: [v, op_ord, seq, payload]. Seq is the
// per-channel sequence; 0 means "unsequenced". Payload is the opaque CBOR(message type) bytes.
public sealed record Datagram(ulong OpOrd, ulong Seq, byte[] Payload)
{
    public static Datagram New(ulong opOrd, ulong seq, byte[] payload) => new(opOrd, seq, payload);

    public byte[] Encode()
    {
        var arr = new CborArray(new List<CborValue>
        {
            new CborUint(Conventions.Version),
            new CborUint(OpOrd),
            new CborUint(Seq),
            Conventions.Tag24(Payload),
        });
        return Cbor.Encode(arr);
    }

    public static Datagram Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        if (v is not CborArray arr)
        {
            throw new MalformedException("datagram is not an array");
        }

        if (arr.Items.Count != 4)
        {
            throw new MalformedException(
                $"datagram array has {arr.Items.Count} elements, expected 4");
        }

        if (!Conventions.TryAsU64(arr.Items[0], out ulong ver))
        {
            throw new MalformedException("datagram field not an integer");
        }

        Conventions.CheckVersion(ver);
        if (!Conventions.TryAsU64(arr.Items[1], out ulong opOrd)
            || !Conventions.TryAsU64(arr.Items[2], out ulong seq))
        {
            throw new MalformedException("datagram field not an integer");
        }

        return new Datagram(opOrd, seq, Conventions.Untag24(arr.Items[3]));
    }
}

// CompactDatagram is a datagram in the compact fixed-header profile. Header layout:
// [ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8]) then the opaque body. Epoch is present when the
// sender tracks restarts (sets the flags epoch bit). Body is opaque (tag-24 CBOR or a raw media
// frame, by channel agreement).
public sealed record CompactDatagram(byte OpOrd, ushort Seq, byte[] Body)
{
    private const byte CompactVer = 1;
    private const byte FlagEpoch = 0b0001;

    public byte? Epoch { get; init; }

    public static CompactDatagram New(byte opOrd, ushort seq, byte[] body) =>
        new(opOrd, seq, body);

    // WithEpoch sets the epoch byte (and the flags bit that signals its presence).
    public CompactDatagram WithEpoch(byte epoch) => this with { Epoch = epoch };

    public byte[] Encode()
    {
        byte flags = Epoch is not null ? FlagEpoch : (byte)0;
        var outBytes = new List<byte>(5 + Body.Length)
        {
            (byte)((CompactVer << 4) | (flags & 0x0f)),
            OpOrd,
            (byte)(Seq >> 8),
            (byte)Seq,
        };
        if (Epoch is byte epoch)
        {
            outBytes.Add(epoch);
        }

        outBytes.AddRange(Body);
        return outBytes.ToArray();
    }

    public static CompactDatagram Decode(ReadOnlySpan<byte> b)
    {
        if (b.Length < 4)
        {
            throw new MalformedException("compact datagram shorter than the 4-byte header");
        }

        byte ver = (byte)(b[0] >> 4);
        if (ver != CompactVer)
        {
            throw new UnsupportedVersionException(ver);
        }

        byte flags = (byte)(b[0] & 0x0f);
        byte opOrd = b[1];
        ushort seq = (ushort)((b[2] << 8) | b[3]);
        byte? epoch = null;
        int bodyStart = 4;
        if ((flags & FlagEpoch) != 0)
        {
            if (b.Length < 5)
            {
                throw new MalformedException(
                    "compact datagram flags claim an epoch byte that is absent");
            }

            epoch = b[4];
            bodyStart = 5;
        }

        return new CompactDatagram(opOrd, seq, b[bodyStart..].ToArray()) { Epoch = epoch };
    }
}

// SeqEventKind classifies an incoming sequence number relative to what was last seen, for
// loss/reorder/restart detection. The transport detects; the app decides.
public enum SeqEventKind
{
    // First is the first datagram seen on the channel.
    First,

    // Advanced is strictly newer than the last (possibly skipping some -- a gap/loss).
    Advanced,

    // LateOrDuplicate is not newer (a late or duplicate datagram).
    LateOrDuplicate,

    // Restart means the sender restarted (epoch changed); seq numbering reset.
    Restart,
}

// SeqEvent is a sequence classification plus the gap count for Advanced.
public readonly record struct SeqEvent(SeqEventKind Kind, ulong Gap = 0);

// SeqTracker tracks the last sequence/epoch per channel to classify arrivals. Unsequenced datagrams
// (seq 0) are reported as Advanced with gap 0.
public sealed class SeqTracker
{
    private ulong? _lastSeq;
    private byte? _lastEpoch;

    // Observe classifies an arriving (seq, epoch) and updates the tracker state.
    public SeqEvent Observe(ulong seq, byte? epoch)
    {
        // A restart fires only when a prior epoch existed and changed; no-epoch -> first epoch is
        // not a restart.
        if (!EpochEqual(epoch, _lastEpoch) && _lastEpoch is not null)
        {
            _lastEpoch = epoch;
            _lastSeq = seq;
            return new SeqEvent(SeqEventKind.Restart);
        }

        _lastEpoch = epoch;

        // seq 0 marks an unsequenced datagram: it carries no ordering information, so it is never
        // late or duplicate. Report a zero-gap advance and leave the running sequence untouched so a
        // mix of sequenced and unsequenced still tracks the sequenced ones.
        if (seq == 0)
        {
            return new SeqEvent(SeqEventKind.Advanced, 0);
        }

        if (_lastSeq is not ulong last)
        {
            _lastSeq = seq;
            return new SeqEvent(SeqEventKind.First);
        }

        if (seq > last)
        {
            ulong gap = seq - last - 1;
            _lastSeq = seq;
            return new SeqEvent(SeqEventKind.Advanced, gap);
        }

        return new SeqEvent(SeqEventKind.LateOrDuplicate);
    }

    private static bool EpochEqual(byte? a, byte? b)
    {
        if (a is null || b is null)
        {
            return a is null && b is null;
        }

        return a.Value == b.Value;
    }
}
