// Decode each vector as a `Stamp`; re-encode the rows that must decode.
// A TypeScript `Date` holds milliseconds, so this target holds 3 fraction digits.
import { readFileSync } from "node:fs";
import { fromStampCbor, toStampCbor } from "./codec.gen.ts";

const DIGITS = 3;

const fromHex = (hex: string): Uint8Array =>
  Uint8Array.from(hex.match(/../g) ?? [], (pair) => parseInt(pair, 16));
const toHex = (bytes: Uint8Array): string =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

const path = process.argv[2];
if (path === undefined) throw new Error("usage: peer.ts <vectors.tsv>");
let failures = 0;
for (const line of readFileSync(path, "utf8").split("\n")) {
  if (line === "" || line.startsWith("#")) continue;
  const [name, expect, precision, input, canonical] = line.split("\t");
  const mustDecode = expect === "ok" && Number(precision) <= DIGITS;
  let outcome: string;
  try {
    const encoded = toHex(toStampCbor(fromStampCbor(fromHex(input))));
    if (!mustDecode) outcome = `decoded, but must fail (re-encoded ${encoded})`;
    else if (encoded !== canonical) outcome = `re-encoded ${encoded}, want ${canonical}`;
    else continue;
  } catch (err) {
    if (!mustDecode) continue;
    outcome = `decode failed: ${(err as Error).message}`;
  }
  failures += 1;
  console.log(`FAIL ${name}: ${outcome}`);
}
if (failures > 0) {
  console.log(`${failures} vector(s) failed`);
  process.exit(1);
}
console.log("all timestamp vectors pass");
