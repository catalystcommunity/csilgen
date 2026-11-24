// Decode each vector as the shape its kind names; re-encode the rows that must decode.
import { readFileSync } from "node:fs";
import {
  fromContactCbor,
  fromHolderCbor,
  fromLookupReadCountsOutputCbor,
  toContactCbor,
  toHolderCbor,
  toLookupReadCountsOutputCbor,
} from "./codec.gen.ts";

const fromHex = (hex: string): Uint8Array =>
  Uint8Array.from(hex.match(/../g) ?? [], (pair) => parseInt(pair, 16));
const toHex = (bytes: Uint8Array): string =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

const roundTrip: Record<string, (bytes: Uint8Array) => Uint8Array> = {
  holder: (b) => toHolderCbor(fromHolderCbor(b)),
  contact: (b) => toContactCbor(fromContactCbor(b)),
  output: (b) => toLookupReadCountsOutputCbor(fromLookupReadCountsOutputCbor(b)),
};

const path = process.argv[2];
if (path === undefined) throw new Error("usage: peer.ts <vectors.tsv>");
let failures = 0;
for (const line of readFileSync(path, "utf8").split("\n")) {
  if (line === "" || line.startsWith("#")) continue;
  const [name, kind, expect, input, canonical] = line.split("\t");
  const run = roundTrip[kind];
  let outcome: string;
  if (run === undefined) {
    outcome = `unknown kind ${kind}`;
  } else {
    try {
      const encoded = toHex(run(fromHex(input)));
      if (expect !== "ok") outcome = `decoded, but must fail (re-encoded ${encoded})`;
      else if (encoded !== canonical) outcome = `re-encoded ${encoded}, want ${canonical}`;
      else continue;
    } catch (err) {
      if (expect === "err") continue;
      outcome = `decode failed: ${(err as Error).message}`;
    }
  }
  failures += 1;
  console.log(`FAIL ${name}: ${outcome}`);
}
if (failures > 0) {
  console.log(`${failures} vector(s) failed`);
  process.exit(1);
}
console.log("all wire vectors pass");
