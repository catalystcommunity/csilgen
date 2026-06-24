// Minimal canonical CBOR codec (RFC 8949). Hand-written and dependency-free so the transport
// library stays stdlib-only and offline-testable. It supports exactly what the CSIL envelopes
// need -- unsigned ints, negative ints, text strings, byte strings, arrays, maps, and tag 24 --
// and nothing else.
//
// Why hand-rolled instead of System.Formats.Cbor: that BCL type is an *out-of-band* NuGet package
// (versioned per-runtime), NOT part of the net8.0 shared framework, so taking it would add a real
// external dependency and a restore step -- contradicting the house zero-dep rule that every other
// transport follows. Its canonical writer also buffers and re-sorts maps internally, meaning our
// normative bytes would hinge on an external package's option flags. Owning the encoder is the only
// way to guarantee determinism without trusting a dependency.
//
// Maps are encoded with core deterministic encoding: entries sorted by the bytewise lexicographic
// order of their *encoded* keys (RFC 8949 §4.2.1), matching the Rust/Go references so bytes are
// byte-identical to the conformance vectors.

using System.Buffers.Binary;
using System.Text;

namespace Csilgen.Transport;

// CborValue is the in-memory model of the CBOR items the envelopes use. Decoding produces these and
// encoding consumes them; transports build envelopes from the canonical helpers so byte layout is
// independent of record/class layout. Mirrors Go's `cborValue` interface.
public abstract record CborValue;

public sealed record CborUint(ulong Value) : CborValue;        // major type 0

public sealed record CborInt(long Value) : CborValue;          // signed; negatives -> major type 1

public sealed record CborText(string Value) : CborValue;       // major type 3

public sealed record CborBytes(byte[] Value) : CborValue;      // major type 2

public sealed record CborArray(IReadOnlyList<CborValue> Items) : CborValue;

public sealed record CborEntry(CborValue Key, CborValue Val);

public sealed record CborMap(IReadOnlyList<CborEntry> Entries) : CborValue;

public sealed record CborTag(ulong Num, CborValue Content) : CborValue;

public static class Cbor
{
    // GetBytes never emits a BOM, but an explicit non-BOM encoding documents that no preamble can
    // ever leak into the wire bytes.
    private static readonly UTF8Encoding Utf8 = new(encoderShouldEmitUTF8Identifier: false);

    // Encode serializes a value to canonical CBOR bytes.
    public static byte[] Encode(CborValue value)
    {
        var buf = new List<byte>(64);
        EncodeInto(buf, value);
        return buf.ToArray();
    }

    // CanonMap builds a deterministically-keyed CBOR map: entries are sorted by the bytewise
    // lexicographic order of their encoded keys (RFC 8949 §4.2.1), so the same logical envelope
    // always yields the same bytes. Callers may append entries in any order.
    public static CborMap CanonMap(IEnumerable<CborEntry> entries)
    {
        var sorted = entries.ToList();
        sorted.Sort(static (a, b) =>
            Encode(a.Key).AsSpan().SequenceCompareTo(Encode(b.Key).AsSpan()));
        return new CborMap(sorted);
    }

    // DecodeEnvelope decodes a complete envelope: one self-contained CBOR item with no trailing
    // bytes. The conventions doc says an envelope is a single CBOR item, so any leftover bytes are a
    // malformed frame and rejected rather than silently ignored.
    public static CborValue DecodeEnvelope(ReadOnlySpan<byte> bytes)
    {
        var (value, consumed) = DecodeValue(bytes);
        if (consumed != bytes.Length)
        {
            throw new MalformedException("CBOR decode error: trailing bytes after CBOR item");
        }

        return value;
    }

    // writeHead emits the initial byte (major type in the high three bits) plus the shortest-form
    // argument bytes for n, per deterministic encoding. BinaryPrimitives handles big-endian framing
    // so there is no hand-rolled bit-shifting to get wrong.
    private static void WriteHead(List<byte> buf, int major, ulong n)
    {
        byte mt = (byte)(major << 5);
        if (n < 24)
        {
            buf.Add((byte)(mt | (byte)n));
            return;
        }

        if (n < 0x100UL)
        {
            buf.Add((byte)(mt | 24));
            buf.Add((byte)n);
            return;
        }

        if (n < 0x1_0000UL)
        {
            buf.Add((byte)(mt | 25));
            Span<byte> s = stackalloc byte[2];
            BinaryPrimitives.WriteUInt16BigEndian(s, (ushort)n);
            AppendSpan(buf, s);
            return;
        }

        if (n < 0x1_0000_0000UL)
        {
            buf.Add((byte)(mt | 26));
            Span<byte> s = stackalloc byte[4];
            BinaryPrimitives.WriteUInt32BigEndian(s, (uint)n);
            AppendSpan(buf, s);
            return;
        }

        buf.Add((byte)(mt | 27));
        Span<byte> s8 = stackalloc byte[8];
        BinaryPrimitives.WriteUInt64BigEndian(s8, n);
        AppendSpan(buf, s8);
    }

    private static void AppendSpan(List<byte> buf, ReadOnlySpan<byte> src)
    {
        foreach (byte b in src)
        {
            buf.Add(b);
        }
    }

    private static void EncodeInto(List<byte> buf, CborValue value)
    {
        switch (value)
        {
            case CborUint u:
                WriteHead(buf, 0, u.Value);
                break;
            case CborInt i:
                if (i.Value >= 0)
                {
                    WriteHead(buf, 0, (ulong)i.Value);
                }
                else
                {
                    // CBOR negative ints encode -1-n; the argument is the magnitude minus one. The
                    // magnitude is computed in unsigned space ((-(v+1)) widened to ulong) so the
                    // long.MinValue case does not overflow.
                    WriteHead(buf, 1, (ulong)(-(i.Value + 1)));
                }

                break;
            case CborText t:
                {
                    byte[] textBytes = Utf8.GetBytes(t.Value);
                    WriteHead(buf, 3, (ulong)textBytes.Length);
                    buf.AddRange(textBytes);
                    break;
                }

            case CborBytes b:
                WriteHead(buf, 2, (ulong)b.Value.Length);
                buf.AddRange(b.Value);
                break;
            case CborArray a:
                WriteHead(buf, 4, (ulong)a.Items.Count);
                foreach (CborValue item in a.Items)
                {
                    EncodeInto(buf, item);
                }

                break;
            case CborMap m:
                WriteHead(buf, 5, (ulong)m.Entries.Count);
                foreach (CborEntry e in m.Entries)
                {
                    EncodeInto(buf, e.Key);
                    EncodeInto(buf, e.Val);
                }

                break;
            case CborTag g:
                WriteHead(buf, 6, g.Num);
                EncodeInto(buf, g.Content);
                break;
            default:
                throw new EncodeException($"unsupported cbor value {value.GetType().Name}");
        }
    }

    // DecodeValue parses one CBOR item from the front of bytes, returning it and the number of bytes
    // consumed. It may leave trailing bytes (it is the recursive workhorse for nested items);
    // envelope decoders use DecodeEnvelope, which rejects trailing bytes.
    private static (CborValue Value, int Consumed) DecodeValue(ReadOnlySpan<byte> b)
    {
        if (b.IsEmpty)
        {
            throw new MalformedException("CBOR decode error: empty input");
        }

        byte ib = b[0];
        int major = ib >> 5;
        byte low = (byte)(ib & 0x1f);
        var (arg, n) = ReadArg(b, low);

        switch (major)
        {
            case 0:
                return (new CborUint(arg), n);
            case 1:
                // CBOR negative ints encode -1-arg; an arg above long.MaxValue names a value below
                // long.MinValue, which would silently wrap if forced into a long.
                if (arg > (ulong)long.MaxValue)
                {
                    throw new MalformedException("CBOR decode error: negative integer out of int64 range");
                }

                return (new CborInt(-1L - (long)arg), n);
            case 2:
                {
                    if ((ulong)b.Length < (ulong)n + arg)
                    {
                        throw new MalformedException("CBOR decode error: truncated byte string");
                    }

                    byte[] outBytes = b.Slice(n, (int)arg).ToArray();
                    return (new CborBytes(outBytes), n + (int)arg);
                }

            case 3:
                {
                    if ((ulong)b.Length < (ulong)n + arg)
                    {
                        throw new MalformedException("CBOR decode error: truncated text string");
                    }

                    string s = Utf8.GetString(b.Slice(n, (int)arg));
                    return (new CborText(s), n + (int)arg);
                }

            case 4:
                {
                    var items = new List<CborValue>((int)arg);
                    int off = n;
                    for (ulong i = 0; i < arg; i++)
                    {
                        var (item, m) = DecodeValue(b[off..]);
                        items.Add(item);
                        off += m;
                    }

                    return (new CborArray(items), off);
                }

            case 5:
                {
                    var entries = new List<CborEntry>((int)arg);
                    int off = n;
                    for (ulong i = 0; i < arg; i++)
                    {
                        var (key, m) = DecodeValue(b[off..]);
                        off += m;
                        var (val, m2) = DecodeValue(b[off..]);
                        off += m2;
                        entries.Add(new CborEntry(key, val));
                    }

                    return (new CborMap(entries), off);
                }

            case 6:
                {
                    var (content, m) = DecodeValue(b[n..]);
                    return (new CborTag(arg, content), n + m);
                }

            default:
                throw new MalformedException($"CBOR decode error: unsupported major type {major}");
        }
    }

    // ReadArg reads the additional-information argument for a head byte whose low five bits are low,
    // returning the argument value and the total head length (including the initial byte).
    private static (ulong Arg, int HeadLen) ReadArg(ReadOnlySpan<byte> b, byte low)
    {
        if (low < 24)
        {
            return (low, 1);
        }

        switch (low)
        {
            case 24:
                if (b.Length < 2)
                {
                    throw new MalformedException("CBOR decode error: truncated 1-byte argument");
                }

                return (b[1], 2);
            case 25:
                if (b.Length < 3)
                {
                    throw new MalformedException("CBOR decode error: truncated 2-byte argument");
                }

                return (BinaryPrimitives.ReadUInt16BigEndian(b.Slice(1, 2)), 3);
            case 26:
                if (b.Length < 5)
                {
                    throw new MalformedException("CBOR decode error: truncated 4-byte argument");
                }

                return (BinaryPrimitives.ReadUInt32BigEndian(b.Slice(1, 4)), 5);
            case 27:
                if (b.Length < 9)
                {
                    throw new MalformedException("CBOR decode error: truncated 8-byte argument");
                }

                return (BinaryPrimitives.ReadUInt64BigEndian(b.Slice(1, 8)), 9);
            default:
                // 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes.
                throw new MalformedException($"CBOR decode error: indefinite or reserved additional info {low}");
        }
    }
}
