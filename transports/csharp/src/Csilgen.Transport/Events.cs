// CSIL-Events transport -- typed bidirectional event streams -- see csil-events-transport.md.
// Verbose (text-keyed) and compact (positional) profiles, plus the control plane (service ordinal
// 0) lifecycle events.

namespace Csilgen.Transport;

// Profile is which wire profile a connection uses for its lifetime, fixed by hello.
public enum Profile
{
    Verbose,
    Compact,
}

public static class ProfileExtensions
{
    public static string WireName(this Profile profile) => profile switch
    {
        Profile.Verbose => "verbose",
        Profile.Compact => "compact",
        _ => "unknown",
    };

    // TryParse maps a wire profile name onto a Profile.
    public static bool TryParse(string name, out Profile profile)
    {
        switch (name)
        {
            case "verbose":
                profile = Profile.Verbose;
                return true;
            case "compact":
                profile = Profile.Compact;
                return true;
            default:
                profile = Profile.Verbose;
                return false;
        }
    }
}

// Event is one typed event flowing in either direction. It is identified by service+operation
// (verbose) or by their ordinals (compact), and carries an optional correlation id when it is a
// request expecting a reply, or that reply. Service is null on a single-service verbose connection;
// ServiceOrd/OpOrd are present in compact frames; Event is the operation name in verbose frames.
public sealed record Event(byte[] Payload)
{
    public string? Service { get; init; }

    public ulong? ServiceOrd { get; init; }

    public string? EventName { get; init; }

    public ulong? OpOrd { get; init; }

    public ulong? Id { get; init; }

    // Verbose builds a verbose event by name.
    public static Event Verbose(string? service, string eventName, byte[] payload) =>
        new(payload) { Service = service, EventName = eventName };

    // Compact builds a compact event by ordinals.
    public static Event Compact(ulong serviceOrd, ulong opOrd, byte[] payload) =>
        new(payload) { ServiceOrd = serviceOrd, OpOrd = opOrd };

    public Event WithId(ulong id) => this with { Id = id };

    // Encode serializes the event under the given profile.
    public byte[] Encode(Profile profile) => profile switch
    {
        Profile.Verbose => EncodeVerbose(),
        Profile.Compact => EncodeCompact(),
        _ => throw new MalformedException("unknown profile"),
    };

    private byte[] EncodeVerbose()
    {
        if (EventName is not string eventName)
        {
            throw new MalformedException("verbose event missing 'event' name");
        }

        var entries = new List<CborEntry>
        {
            new(new CborText("event"), new CborText(eventName)),
            new(new CborText("payload"), Conventions.Tag24(Payload)),
        };
        if (Service is string service)
        {
            entries.Add(new CborEntry(new CborText("service"), new CborText(service)));
        }

        if (Id is ulong id)
        {
            entries.Add(new CborEntry(new CborText("id"), new CborUint(id)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    private byte[] EncodeCompact()
    {
        if (ServiceOrd is not ulong serviceOrd)
        {
            throw new MalformedException("compact event missing service ordinal");
        }

        if (OpOrd is not ulong opOrd)
        {
            throw new MalformedException("compact event missing op ordinal");
        }

        var items = new List<CborValue> { new CborUint(serviceOrd), new CborUint(opOrd) };
        if (Id is ulong id)
        {
            items.Add(new CborUint(id));
        }

        items.Add(Conventions.Tag24(Payload));
        return Cbor.Encode(new CborArray(items));
    }

    // Decode parses an event under the given profile.
    public static Event Decode(ReadOnlySpan<byte> bytes, Profile profile) => profile switch
    {
        Profile.Verbose => DecodeVerbose(bytes),
        Profile.Compact => DecodeCompact(bytes),
        _ => throw new MalformedException("unknown profile"),
    };

    private static Event DecodeVerbose(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        CborValue? p = Conventions.MapGet(v, "payload")
            ?? throw new MalformedException("missing 'payload'");
        return new Event(Conventions.Untag24(p))
        {
            Service = Conventions.GetTextOpt(v, "service"),
            EventName = Conventions.GetText(v, "event"),
            Id = Conventions.GetUintOpt(v, "id"),
        };
    }

    private static Event DecodeCompact(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        if (v is not CborArray arr)
        {
            throw new MalformedException("compact event is not an array");
        }

        // 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
        CborValue serviceOrdV;
        CborValue opOrdV;
        CborValue payloadV;
        CborValue? idV = null;
        switch (arr.Items.Count)
        {
            case 3:
                serviceOrdV = arr.Items[0];
                opOrdV = arr.Items[1];
                payloadV = arr.Items[2];
                break;
            case 4:
                serviceOrdV = arr.Items[0];
                opOrdV = arr.Items[1];
                idV = arr.Items[2];
                payloadV = arr.Items[3];
                break;
            default:
                throw new MalformedException(
                    $"compact event array has {arr.Items.Count} elements, expected 3 or 4");
        }

        if (!Conventions.TryAsU64(serviceOrdV, out ulong serviceOrd)
            || !Conventions.TryAsU64(opOrdV, out ulong opOrd))
        {
            throw new MalformedException("ordinal is not an integer");
        }

        var ev = new Event(Conventions.Untag24(payloadV))
        {
            ServiceOrd = serviceOrd,
            OpOrd = opOrd,
        };
        if (idV is not null)
        {
            if (!Conventions.TryAsU64(idV, out ulong id))
            {
                throw new MalformedException("ordinal is not an integer");
            }

            ev = ev with { Id = id };
        }

        return ev;
    }
}

// Control-plane operation ordinals (under service ordinal 0) and their verbose `$`-sigil names.
public static class Control
{
    public const ulong Hello = 0;
    public const ulong HelloAck = 1;
    public const ulong Ping = 2;
    public const ulong Pong = 3;
    public const ulong Close = 4;
    public const ulong Error = 5;

    public const string HelloName = "$hello";
    public const string HelloAckName = "$hello-ack";
    public const string PingName = "$ping";
    public const string PongName = "$pong";
    public const string CloseName = "$close";
    public const string ErrorName = "$error";
}

// Hello is the `$hello` payload offered by the connection initiator.
public sealed record Hello(IReadOnlyList<ulong> Versions, IReadOnlyList<string> Profiles)
{
    public string? Service { get; init; }

    public string? Auth { get; init; }

    public byte[] Encode()
    {
        var versions = new List<CborValue>(Versions.Count);
        foreach (ulong v in Versions)
        {
            versions.Add(new CborUint(v));
        }

        var profiles = new List<CborValue>(Profiles.Count);
        foreach (string p in Profiles)
        {
            profiles.Add(new CborText(p));
        }

        var entries = new List<CborEntry>
        {
            new(new CborText("versions"), new CborArray(versions)),
            new(new CborText("profiles"), new CborArray(profiles)),
        };
        if (Service is string service)
        {
            entries.Add(new CborEntry(new CborText("service"), new CborText(service)));
        }

        if (Auth is string auth)
        {
            entries.Add(new CborEntry(new CborText("auth"), new CborText(auth)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static Hello Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        if (Conventions.MapGet(v, "versions") is not CborArray versArr)
        {
            throw new MalformedException("hello missing 'versions'");
        }

        var versions = new List<ulong>();
        foreach (CborValue x in versArr.Items)
        {
            if (Conventions.TryAsU64(x, out ulong n))
            {
                versions.Add(n);
            }
        }

        if (Conventions.MapGet(v, "profiles") is not CborArray profArr)
        {
            throw new MalformedException("hello missing 'profiles'");
        }

        var profiles = new List<string>();
        foreach (CborValue x in profArr.Items)
        {
            if (x is CborText t)
            {
                profiles.Add(t.Value);
            }
        }

        return new Hello(versions, profiles)
        {
            Service = Conventions.GetTextOpt(v, "service"),
            Auth = Conventions.GetTextOpt(v, "auth"),
        };
    }

    // Negotiate selects a profile from this hello's offers, honoring the peer's preference order and
    // what the receiver supports. Returns null if nothing is mutually supported.
    public (ulong Version, Profile Profile)? Negotiate(IReadOnlyList<Profile> supported)
    {
        ulong? version = null;
        foreach (ulong v in Versions)
        {
            if (v == Conventions.Version)
            {
                version = v;
                break;
            }
        }

        if (version is not ulong chosenVersion)
        {
            return null;
        }

        foreach (string offered in Profiles)
        {
            if (!ProfileExtensions.TryParse(offered, out Profile p))
            {
                continue;
            }

            foreach (Profile s in supported)
            {
                if (s == p)
                {
                    return (chosenVersion, p);
                }
            }
        }

        return null;
    }
}

// HelloAck is the `$hello-ack` payload returned by the peer.
public sealed record HelloAck(ulong V, string Profile)
{
    public string? Session { get; init; }

    public byte[] Encode()
    {
        var entries = new List<CborEntry>
        {
            new(new CborText("v"), new CborUint(V)),
            new(new CborText("profile"), new CborText(Profile)),
        };
        if (Session is string session)
        {
            entries.Add(new CborEntry(new CborText("session"), new CborText(session)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static HelloAck Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        return new HelloAck(Conventions.GetUint(v, "v"), Conventions.GetText(v, "profile"))
        {
            Session = Conventions.GetTextOpt(v, "session"),
        };
    }
}

// Heartbeat is a `$ping`/`$pong` heartbeat payload.
public sealed record Heartbeat(ulong Nonce)
{
    public ulong? At { get; init; }

    public byte[] Encode()
    {
        var entries = new List<CborEntry> { new(new CborText("nonce"), new CborUint(Nonce)) };
        if (At is ulong at)
        {
            entries.Add(new CborEntry(new CborText("at"), new CborUint(at)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static Heartbeat Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        return new Heartbeat(Conventions.GetUint(v, "nonce"))
        {
            At = Conventions.GetUintOpt(v, "at"),
        };
    }
}

// Close is a `$close` payload.
public sealed record Close(Status Status)
{
    public string? Reason { get; init; }

    public byte[] Encode()
    {
        var entries = new List<CborEntry> { new(new CborText("status"), new CborInt(Status.Code)) };
        if (Reason is string reason)
        {
            entries.Add(new CborEntry(new CborText("reason"), new CborText(reason)));
        }

        return Cbor.Encode(Cbor.CanonMap(entries));
    }

    public static Close Decode(ReadOnlySpan<byte> bytes)
    {
        CborValue v = Cbor.DecodeEnvelope(bytes);
        return new Close(Status.FromCode((int)Conventions.GetInt(v, "status")))
        {
            Reason = Conventions.GetTextOpt(v, "reason"),
        };
    }
}
