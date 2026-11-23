using Csilgen.Transport;

class Peer
{
    // System.DateTimeOffset holds 100 ns ticks.
    const string Digits = "7";

    static int Main(string[] args)
    {
        int failures = 0;
        int checked_ = 0;
        foreach (var line in System.IO.File.ReadAllLines(args[0]))
        {
            if (line.Length == 0 || line.StartsWith('#')) continue;
            var row = line.Split('\t');
            if (row[2] != "*" && row[2] != Digits) continue;
            checked_++;
            bool mustDecode = row[1] == "ok";
            string? problem = null;
            try
            {
                var moment = Codec.Decode<Moment>(System.Convert.FromHexString(row[3]));
                if (!mustDecode) problem = $"decoded {moment.At:O}, want error";
                else
                {
                    var got = System.Convert.ToHexString(Codec.Encode(moment)).ToLowerInvariant();
                    if (got != row[4]) problem = $"re-encoded {got}, want {row[4]}";
                }
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
        System.Console.WriteLine($"csharp epoch vectors: {checked_} ok");
        return 0;
    }
}
