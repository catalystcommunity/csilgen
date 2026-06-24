// Verify the C# reference library against the checked-in conformance vectors.
//
// This both guards the C# encoders/decoders against drift and demonstrates the contract every
// reference library follows: reconstruct each vector's envelope from its language-neutral `input`,
// assert encode -> `hex`, and assert decode(`hex`) -> the same envelope. Ported from
// transports/rust/tests/conformance.rs and transports/go/conformance_test.go.

using System.Text.Json;
using Csilgen.Transport;
using Xunit;

namespace Csilgen.Transport.Tests;

public sealed class ConformanceTests
{
    // Walk up from the test assembly's base directory to find transports/conformance, mirroring how
    // the Go test resolves "../conformance" relative to its source. Robust to bin/Debug/net8.0 depth.
    private static string ConformanceDir()
    {
        DirectoryInfo? dir = new(AppContext.BaseDirectory);
        while (dir is not null)
        {
            string nested = Path.Combine(dir.FullName, "transports", "conformance");
            if (File.Exists(Path.Combine(nested, "rpc.json")))
            {
                return nested;
            }

            string sibling = Path.Combine(dir.FullName, "conformance");
            if (File.Exists(Path.Combine(sibling, "rpc.json")))
            {
                return sibling;
            }

            dir = dir.Parent;
        }

        throw new DirectoryNotFoundException("could not locate transports/conformance");
    }

    private static IReadOnlyList<JsonElement> LoadVectors(string name)
    {
        string path = Path.Combine(ConformanceDir(), name);
        using var doc = JsonDocument.Parse(File.ReadAllText(path));
        var vectors = new List<JsonElement>();
        foreach (JsonElement v in doc.RootElement.GetProperty("vectors").EnumerateArray())
        {
            // Clone so the elements outlive the JsonDocument's disposal.
            vectors.Add(v.Clone());
        }

        return vectors;
    }

    private static byte[] Unhex(string s)
    {
        if (s.Length == 0)
        {
            return Array.Empty<byte>();
        }

        byte[] result = new byte[s.Length / 2];
        for (int i = 0; i < result.Length; i++)
        {
            result[i] = Convert.ToByte(s.Substring(i * 2, 2), 16);
        }

        return result;
    }

    private static string Hex(byte[] bytes) => Convert.ToHexString(bytes).ToLowerInvariant();

    private static string ReqStr(JsonElement input, string key) => input.GetProperty(key).GetString()!;

    private static ulong ReqU64(JsonElement input, string key) => input.GetProperty(key).GetUInt64();

    private static long ReqI64(JsonElement input, string key) => input.GetProperty(key).GetInt64();

    private static string? OptStr(JsonElement input, string key) =>
        input.TryGetProperty(key, out JsonElement e) && e.ValueKind != JsonValueKind.Null
            ? e.GetString()
            : null;

    private static ulong? OptU64(JsonElement input, string key) =>
        input.TryGetProperty(key, out JsonElement e) && e.ValueKind != JsonValueKind.Null
            ? e.GetUInt64()
            : null;

    [Fact]
    public void RpcVectors()
    {
        foreach (JsonElement vec in LoadVectors("rpc.json"))
        {
            string name = vec.GetProperty("name").GetString()!;
            string expectedHex = vec.GetProperty("hex").GetString()!;
            JsonElement input = vec.GetProperty("input");
            byte[] actual;

            switch (ReqStr(input, "kind"))
            {
                case "request":
                    {
                        var r = RpcRequest.New(
                            ReqStr(input, "service"),
                            ReqStr(input, "op"),
                            Unhex(ReqStr(input, "payload_hex"))) with
                        {
                            Id = OptU64(input, "id"),
                            Auth = OptStr(input, "auth"),
                        };
                        actual = r.Encode();
                        RpcRequest dec = RpcRequest.Decode(actual);
                        Assert.Equal(r.Service, dec.Service);
                        Assert.Equal(r.Op, dec.Op);
                        Assert.Equal(r.Id, dec.Id);
                        Assert.Equal(r.Auth, dec.Auth);
                        Assert.True(r.Payload.AsSpan().SequenceEqual(dec.Payload), $"payload {name}");
                        break;
                    }

                case "response":
                    {
                        var r = new RpcResponse(
                            Status.FromCode((int)ReqI64(input, "status")),
                            Unhex(ReqStr(input, "payload_hex")))
                        {
                            Id = OptU64(input, "id"),
                            Variant = OptStr(input, "variant"),
                            Error = OptStr(input, "error"),
                        };
                        actual = r.Encode();
                        RpcResponse dec = RpcResponse.Decode(actual);
                        Assert.Equal(r.Id, dec.Id);
                        Assert.Equal(r.Status, dec.Status);
                        Assert.Equal(r.Variant, dec.Variant);
                        Assert.Equal(r.Error, dec.Error);
                        Assert.True(r.Payload.AsSpan().SequenceEqual(dec.Payload), $"payload {name}");
                        break;
                    }

                case "push":
                    {
                        var p = RpcPush.New(
                            ReqStr(input, "service"),
                            ReqStr(input, "event"),
                            Unhex(ReqStr(input, "payload_hex")));
                        actual = p.Encode();
                        RpcPush dec = RpcPush.Decode(actual);
                        Assert.Equal(p.Service, dec.Service);
                        Assert.Equal(p.Event, dec.Event);
                        Assert.True(p.Payload.AsSpan().SequenceEqual(dec.Payload), $"payload {name}");
                        break;
                    }

                default:
                    throw new Xunit.Sdk.XunitException($"unknown rpc kind in {name}");
            }

            Assert.Equal(expectedHex, Hex(actual));
        }
    }

    [Fact]
    public void EventsVectors()
    {
        foreach (JsonElement vec in LoadVectors("events.json"))
        {
            string name = vec.GetProperty("name").GetString()!;
            string expectedHex = vec.GetProperty("hex").GetString()!;
            JsonElement input = vec.GetProperty("input");

            // Control payloads are keyed by "control"; application events by "profile".
            if (OptStr(input, "control") is string control)
            {
                byte[] controlBytes = control switch
                {
                    "hello" => new Hello(
                            ReadU64Array(input, "versions"),
                            ReadStrArray(input, "profiles"))
                    {
                        Service = OptStr(input, "service"),
                        Auth = OptStr(input, "auth"),
                    }.Encode(),
                    "hello_ack" => new HelloAck(ReqU64(input, "v"), ReqStr(input, "profile"))
                    {
                        Session = OptStr(input, "session"),
                    }.Encode(),
                    "ping" => new Heartbeat(ReqU64(input, "nonce"))
                    {
                        At = OptU64(input, "at"),
                    }.Encode(),
                    "close" => new Close(Status.FromCode((int)ReqI64(input, "status")))
                    {
                        Reason = OptStr(input, "reason"),
                    }.Encode(),
                    _ => throw new Xunit.Sdk.XunitException($"unknown control in {name}"),
                };
                Assert.Equal(expectedHex, Hex(controlBytes));
                continue;
            }

            string profileStr = ReqStr(input, "profile");
            Assert.True(ProfileExtensions.TryParse(profileStr, out Profile profile), name);
            byte[] payload = Unhex(ReqStr(input, "payload_hex"));
            Event ev = profile switch
            {
                Profile.Verbose => Event.Verbose(
                    OptStr(input, "service"), ReqStr(input, "event"), payload) with
                {
                    Id = OptU64(input, "id"),
                },
                Profile.Compact => Event.Compact(
                    ReqU64(input, "service_ord"), ReqU64(input, "op_ord"), payload) with
                {
                    Id = OptU64(input, "id"),
                },
                _ => throw new Xunit.Sdk.XunitException($"unknown profile in {name}"),
            };
            byte[] bytes = ev.Encode(profile);
            Assert.Equal(expectedHex, Hex(bytes));

            Event dec = Event.Decode(bytes, profile);
            Assert.Equal(ev.Service, dec.Service);
            Assert.Equal(ev.ServiceOrd, dec.ServiceOrd);
            Assert.Equal(ev.EventName, dec.EventName);
            Assert.Equal(ev.OpOrd, dec.OpOrd);
            Assert.Equal(ev.Id, dec.Id);
            Assert.True(ev.Payload.AsSpan().SequenceEqual(dec.Payload), $"payload {name}");
        }
    }

    [Fact]
    public void DatagramVectors()
    {
        foreach (JsonElement vec in LoadVectors("datagrams.json"))
        {
            string name = vec.GetProperty("name").GetString()!;
            string expectedHex = vec.GetProperty("hex").GetString()!;
            JsonElement input = vec.GetProperty("input");

            switch (ReqStr(input, "profile"))
            {
                case "cbor-array":
                    {
                        var d = Datagram.New(
                            ReqU64(input, "op_ord"),
                            ReqU64(input, "seq"),
                            Unhex(ReqStr(input, "payload_hex")));
                        byte[] bytes = d.Encode();
                        Assert.Equal(expectedHex, Hex(bytes));
                        Datagram dec = Datagram.Decode(bytes);
                        Assert.Equal(d.OpOrd, dec.OpOrd);
                        Assert.Equal(d.Seq, dec.Seq);
                        Assert.True(d.Payload.AsSpan().SequenceEqual(dec.Payload), $"payload {name}");
                        break;
                    }

                case "compact-header":
                    {
                        var d = CompactDatagram.New(
                            (byte)ReqU64(input, "op_ord"),
                            (ushort)ReqU64(input, "seq"),
                            Unhex(ReqStr(input, "body_hex")));
                        if (OptU64(input, "epoch") is ulong epoch)
                        {
                            d = d.WithEpoch((byte)epoch);
                        }

                        byte[] bytes = d.Encode();
                        Assert.Equal(expectedHex, Hex(bytes));
                        CompactDatagram dec = CompactDatagram.Decode(bytes);
                        Assert.Equal(d.OpOrd, dec.OpOrd);
                        Assert.Equal(d.Seq, dec.Seq);
                        Assert.Equal(d.Epoch, dec.Epoch);
                        Assert.True(d.Body.AsSpan().SequenceEqual(dec.Body), $"body {name}");
                        break;
                    }

                default:
                    throw new Xunit.Sdk.XunitException($"unknown datagram profile in {name}");
            }
        }
    }

    private static List<ulong> ReadU64Array(JsonElement input, string key)
    {
        var result = new List<ulong>();
        foreach (JsonElement x in input.GetProperty(key).EnumerateArray())
        {
            result.Add(x.GetUInt64());
        }

        return result;
    }

    private static List<string> ReadStrArray(JsonElement input, string key)
    {
        var result = new List<string>();
        foreach (JsonElement x in input.GetProperty(key).EnumerateArray())
        {
            result.Add(x.GetString()!);
        }

        return result;
    }
}
