package csilgen.generated;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;

public class Peer {
    // The peer sits in the generated package, so it reaches the union codecs, which
    // have no public byte helpers.
    static byte[] roundTrip(String kind, byte[] input) {
        CsilCbor.CborValue value = CsilCbor.decode(input);
        switch (kind) {
            case "holder":
                return CsilCbor.encode(CsilCbor.encHolder(CsilCbor.decHolder(value)));
            case "contact":
                return CsilCbor.encode(CsilCbor.encContact(CsilCbor.decContact(value)));
            case "output":
                return CsilCbor.encode(CsilCbor.encLookupReadCountsOutput(
                        CsilCbor.decLookupReadCountsOutput(value)));
            default:
                throw new IllegalStateException("unknown kind " + kind);
        }
    }

    public static void main(String[] args) throws Exception {
        int failures = 0;
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            if (line.isEmpty() || line.startsWith("#")) continue;
            String[] row = line.split("\t", -1);
            boolean mustDecode = row[2].equals("ok");
            String problem = null;
            try {
                String got = HexFormat.of().formatHex(roundTrip(row[1], HexFormat.of().parseHex(row[3])));
                if (!mustDecode) {
                    problem = "decoded, want error";
                } else if (!got.equals(row[4])) {
                    problem = "re-encoded " + got + ", want " + row[4];
                }
            } catch (CsilCbor.CsilCborException e) {
                if (mustDecode) problem = "decode failed: " + e.getMessage();
            } catch (RuntimeException e) {
                problem = "unexpected " + e;
            }
            if (problem != null) {
                failures++;
                System.out.println("FAIL " + row[0] + ": " + problem);
            }
        }
        if (failures > 0) System.exit(1);
        System.out.println("all wire vectors pass");
    }
}
