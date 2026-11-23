using Csilgen.Transport;

class Peer
{
    // System.DateTimeOffset holds 100 ns ticks.
    const int Digits = 7;

    static int NeededDigits(string canonicalHex)
    {
        var text = System.Text.Encoding.ASCII.GetString(System.Convert.FromHexString(canonicalHex));
        int dot = text.IndexOf('.');
        return dot < 0 ? 0 : text.TrimEnd('Z').TrimEnd('0').Length - dot - 1;
    }

    static int Main(string[] args)
    {
        int failures = 0;
        foreach (var line in System.IO.File.ReadAllLines(args[0]))
        {
            if (line.Length == 0 || line.StartsWith('#')) continue;
            var row = line.Split('\t');
            // The precision column names the 3/6/9 group of the canonical text, but the
            // contract asks whether the nonzero digits fit, so count them from that text.
            bool mustDecode = row[1] == "ok" && NeededDigits(row[4]) <= Digits;
            string? problem = null;
            try
            {
                var stamp = Codec.Decode<Stamp>(System.Convert.FromHexString(row[3]));
                var got = System.Convert.ToHexString(Codec.Encode(stamp)).ToLowerInvariant();
                if (!mustDecode) problem = "decoded, want error";
                else if (got != row[4]) problem = $"re-encoded {got}, want {row[4]}";
            }
            catch (System.Exception e) when (e is not System.OutOfMemoryException)
            {
                if (mustDecode) problem = $"decode failed: {e.Message}";
            }
            if (problem != null)
            {
                failures++;
                System.Console.WriteLine($"FAIL {row[0]}: {problem}");
            }
        }
        if (failures > 0) return 1;
        System.Console.WriteLine("csharp timestamp vectors: ok");
        return 0;
    }
}
