package csilgen.generated;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;

public class Peer {
    // java.time.Instant holds nanoseconds.
    private static final int DIGITS = 9;

    public static void main(String[] args) throws Exception {
        int failures = 0;
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            if (line.isEmpty() || line.startsWith("#")) continue;
            String[] row = line.split("\t", -1);
            String name = row[0];
            boolean mustDecode = row[1].equals("ok") && Integer.parseInt(row[2]) <= DIGITS;
            byte[] input = HexFormat.of().parseHex(row[3]);
            String problem = null;
            try {
                Stamp stamp = CsilCbor.decodeStamp(input);
                if (!mustDecode) {
                    problem = "decoded, want error";
                } else {
                    String got = HexFormat.of().formatHex(CsilCbor.encodeStamp(stamp));
                    if (!got.equals(row[4])) problem = "re-encoded " + got + ", want " + row[4];
                }
            } catch (RuntimeException e) {
                if (mustDecode) problem = "decode failed: " + e.getMessage();
            }
            if (problem != null) {
                failures++;
                System.out.println("FAIL " + name + ": " + problem);
            }
        }
        if (failures > 0) System.exit(1);
        System.out.println("java timestamp vectors: ok");
    }
}
