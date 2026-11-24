using Csilgen.Transport;

class Peer
{
    static byte[] RoundTrip(string kind, byte[] input)
    {
        var value = Cbor.Decode(input);
        return kind switch
        {
            "holder" => Cbor.Encode(Codec.HolderToCborValue(Codec.HolderFromCborValue(value))),
            "contact" => Cbor.Encode(Codec.ContactToCborValue(Codec.ContactFromCborValue(value))),
            "output" => Cbor.Encode(Codec.LookupReadCountsOutputToCborValue(
                Codec.LookupReadCountsOutputFromCborValue(value))),
            _ => throw new System.InvalidOperationException($"unknown kind {kind}"),
        };
    }

    static int Main(string[] args)
    {
        int failures = 0;
        foreach (var line in System.IO.File.ReadAllLines(args[0]))
        {
            if (line.Length == 0 || line.StartsWith('#')) continue;
            var row = line.Split('\t');
            bool mustDecode = row[2] == "ok";
            string? problem = null;
            try
            {
                var got = System.Convert.ToHexString(RoundTrip(row[1], System.Convert.FromHexString(row[3]))).ToLowerInvariant();
                if (!mustDecode) problem = "decoded, want error";
                else if (got != row[4]) problem = $"re-encoded {got}, want {row[4]}";
            }
            catch (CborException e)
            {
                if (mustDecode) problem = $"decode failed: {e.Message}";
            }
            catch (System.Exception e) when (e is not System.OutOfMemoryException)
            {
                problem = $"unexpected {e.GetType().Name}: {e.Message}";
            }
            if (problem != null)
            {
                failures++;
                System.Console.WriteLine($"FAIL {row[0]}: {problem}");
            }
        }
        if (failures > 0) return 1;
        System.Console.WriteLine("all wire vectors pass");
        return 0;
    }
}
