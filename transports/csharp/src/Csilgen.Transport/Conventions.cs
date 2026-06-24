// Conventions shared by every CSIL transport -- see csil-transport-conventions.md.
//
// This file owns the parts the three transports agree on: the version constant, the transport
// status registry, tag-24 payload wrap/unwrap, the max-frame guard, the exception hierarchy, and
// the canonical-CBOR field accessors the envelopes build on so their bytes match the conformance
// vectors regardless of record layout.

namespace Csilgen.Transport;

// Status is a transport-level status. It is distinct from application errors, which ride inside the
// payload as a declared `/ ErrorType` arm. Equality is by the underlying code, so host-defined
// extension codes (>= 64) and unknown codes compare correctly. A struct because it is a single int.
public readonly record struct Status(int Code)
{
    public static readonly Status Ok = new(0);
    public static readonly Status MalformedEnvelope = new(1);
    public static readonly Status UnknownServiceOrOp = new(2);
    public static readonly Status Unauthenticated = new(3);
    public static readonly Status Forbidden = new(4);
    public static readonly Status VersionUnsupported = new(5);
    public static readonly Status Internal = new(6);
    public static readonly Status Unavailable = new(7);
    public static readonly Status DeadlineExceeded = new(8);

    // FromCode maps a wire code onto a Status, preserving host-defined and unknown codes verbatim.
    public static Status FromCode(int code) => new(code);

    // IsOk reports whether the status indicates a typed reply is present.
    public bool IsOk => Code == 0;

    // Name returns the registry name for the status, or "other" for codes outside it.
    public string Name => Code switch
    {
        0 => "ok",
        1 => "malformed-envelope",
        2 => "unknown-service-or-op",
        3 => "unauthenticated",
        4 => "forbidden",
        5 => "version-unsupported",
        6 => "internal",
        7 => "unavailable",
        8 => "deadline-exceeded",
        _ => "other",
    };
}

// TransportException is the base for errors surfaced by the transport layer (the wire), distinct
// from a host's application errors (which ride as a status-0 typed variant).
public class TransportException : Exception
{
    public TransportException(string message)
        : base(message)
    {
    }

    public TransportException(string message, Exception inner)
        : base(message, inner)
    {
    }
}

public sealed class EncodeException : TransportException
{
    public EncodeException(string message)
        : base(message)
    {
    }
}

public sealed class DecodeException : TransportException
{
    public DecodeException(string message)
        : base(message)
    {
    }
}

public sealed class MalformedException : TransportException
{
    public MalformedException(string message)
        : base("malformed envelope: " + message)
    {
    }
}

public sealed class FrameTooLargeException : TransportException
{
    public FrameTooLargeException(int got, int maximum)
        : base($"frame of {got} bytes exceeds max-frame guard of {maximum} bytes")
    {
        Got = got;
        Maximum = maximum;
    }

    public int Got { get; }

    public int Maximum { get; }
}

public sealed class UnsupportedVersionException : TransportException
{
    public UnsupportedVersionException(ulong version)
        : base($"unsupported transport version {version}")
    {
        Version = version;
    }

    public ulong Version { get; }
}

// StatusException carries a non-zero transport status returned by a peer, distinct from a host's
// application errors.
public sealed class StatusException : TransportException
{
    public StatusException(Status status, string? message)
        : base(message is null
            ? $"transport status {status.Name} ({status.Code})"
            : $"transport status {status.Name} ({status.Code}): {message}")
    {
        Status = status;
        StatusMessage = message;
    }

    public Status Status { get; }

    public string? StatusMessage { get; }
}

public sealed class CarrierException : TransportException
{
    public CarrierException(string message)
        : base("carrier error: " + message)
    {
    }
}

public static class Conventions
{
    // VERSION is the current transport version. A new value is minted only for a breaking change to
    // envelope layout or semantics.
    public const ulong Version = 1;

    // TagEncodedCbor is the CBOR semantic tag wrapping an embedded, opaque CBOR data item
    // (RFC 8949 §3.4.5.1).
    public const ulong TagEncodedCbor = 24;

    // ControlServiceOrd is the reserved service ordinal for the transport control plane (Events
    // lifecycle).
    public const ulong ControlServiceOrd = 0;

    // MaxFrameDefault is the default max encoded envelope size for stream/message carriers (16 MiB).
    // A carrier rejects anything larger before allocating for it.
    public const int MaxFrameDefault = 16 * 1024 * 1024;

    // Tag24 wraps opaque payload bytes (themselves a CBOR item) in tag 24.
    public static CborValue Tag24(byte[] payload) =>
        new CborTag(TagEncodedCbor, new CborBytes((byte[])payload.Clone()));

    // Untag24 extracts the opaque payload bytes from a tag-24 value.
    public static byte[] Untag24(CborValue value)
    {
        if (value is CborTag { Num: TagEncodedCbor, Content: CborBytes bytes })
        {
            return (byte[])bytes.Value.Clone();
        }

        throw new MalformedException("expected a tag-24 (encoded-cbor) payload");
    }

    // MapGet looks up a text key in a CBOR map value.
    public static CborValue? MapGet(CborValue value, string key)
    {
        if (value is CborMap map)
        {
            foreach (CborEntry entry in map.Entries)
            {
                if (entry.Key is CborText text && text.Value == key)
                {
                    return entry.Val;
                }
            }
        }

        return null;
    }

    // TryAsU64 reads a non-negative integer from a decoded CBOR integer value.
    public static bool TryAsU64(CborValue value, out ulong result)
    {
        switch (value)
        {
            case CborUint u:
                result = u.Value;
                return true;
            case CborInt i when i.Value >= 0:
                result = (ulong)i.Value;
                return true;
            default:
                result = 0;
                return false;
        }
    }

    // TryAsI64 reads a signed integer from a decoded CBOR integer value.
    public static bool TryAsI64(CborValue value, out long result)
    {
        switch (value)
        {
            case CborUint u:
                result = (long)u.Value;
                return true;
            case CborInt i:
                result = i.Value;
                return true;
            default:
                result = 0;
                return false;
        }
    }

    public static ulong GetUint(CborValue map, string key)
    {
        CborValue? v = MapGet(map, key);
        if (v is not null && TryAsU64(v, out ulong n))
        {
            return n;
        }

        throw new MalformedException($"missing or non-integer field '{key}'");
    }

    public static long GetInt(CborValue map, string key)
    {
        CborValue? v = MapGet(map, key);
        if (v is not null && TryAsI64(v, out long n))
        {
            return n;
        }

        throw new MalformedException($"missing or non-integer field '{key}'");
    }

    public static string GetText(CborValue map, string key)
    {
        CborValue? v = MapGet(map, key);
        if (v is CborText t)
        {
            return t.Value;
        }

        throw new MalformedException($"missing or non-text field '{key}'");
    }

    public static string? GetTextOpt(CborValue map, string key)
    {
        CborValue? v = MapGet(map, key);
        return v is CborText t ? t.Value : null;
    }

    public static ulong? GetUintOpt(CborValue map, string key)
    {
        CborValue? v = MapGet(map, key);
        if (v is not null && TryAsU64(v, out ulong n))
        {
            return n;
        }

        return null;
    }

    // CheckVersion verifies a decoded envelope's version field, throwing a clear error otherwise so
    // an unknown version is never silently misparsed.
    public static void CheckVersion(ulong version)
    {
        if (version != Version)
        {
            throw new UnsupportedVersionException(version);
        }
    }
}
