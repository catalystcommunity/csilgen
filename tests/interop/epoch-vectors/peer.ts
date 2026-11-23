// Decode each vector as a `Moment`; re-encode the rows that must decode.
// A TypeScript `Date` holds milliseconds, so this target holds 3 fraction digits.
import { readFileSync } from "node:fs";
import { fromMomentCbor, toMomentCbor } from "./codec.gen.ts";

const DIGITS = "3";

const fromHex = (hex: string): Uint8Array =>
  Uint8Array.from(hex.match(/../g) ?? [], (pair) => parseInt(pair, 16));
const toHex = (bytes: Uint8Array): string =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

const path = process.argv[2];
if (path === undefined) throw new Error("usage: peer.ts <vectors.tsv>");
let rows = 0;
let failures = 0;
for (const line of readFileSync(path, "utf8").split("\n")) {
  if (line === "" || line.startsWith("#")) continue;
  const [name, expect, digits, input, canonical] = line.split("\t");
  if (digits !== "*" && digits !== DIGITS) continue;
  rows += 1;
  let outcome: string;
  try {
    const encoded = toHex(toMomentCbor(fromMomentCbor(fromHex(input))));
    if (expect !== "ok") outcome = `decoded, but must fail (re-encoded ${encoded})`;
    else if (encoded !== canonical) outcome = `re-encoded ${encoded}, want ${canonical}`;
    else continue;
  } catch (err) {
    if (expect === "err") continue;
    outcome = `decode failed: ${(err as Error).message}`;
  }
  failures += 1;
  console.log(`FAIL ${name}: ${outcome}`);
}
console.log(`typescript: ${rows - failures} of ${rows} epoch vectors pass`);
if (failures > 0) process.exit(1);
