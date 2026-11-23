# Decode each vector as a `Stamp`; re-encode the rows that must decode.
# A Python `datetime` holds microseconds, so this target holds 6 fraction digits.
import sys

from api.types import Stamp
import api.codec  # noqa: F401  (binds to_cbor/from_cbor onto the dataclasses)

DIGITS = 6

failures = 0
with open(sys.argv[1], encoding="utf-8") as vectors:
    for line in vectors:
        line = line.rstrip("\n")
        if not line or line.startswith("#"):
            continue
        name, expect, precision, input_hex, canonical = line.split("\t")
        must_decode = expect == "ok" and int(precision) <= DIGITS
        try:
            encoded = Stamp.from_cbor(bytes.fromhex(input_hex)).to_cbor().hex()
        except ValueError as exc:
            if must_decode:
                failures += 1
                print(f"FAIL {name}: decode failed: {exc}")
            continue
        if not must_decode:
            failures += 1
            print(f"FAIL {name}: decoded, but must fail (re-encoded {encoded})")
        elif encoded != canonical:
            failures += 1
            print(f"FAIL {name}: re-encoded {encoded}, want {canonical}")

if failures:
    print(f"{failures} vector(s) failed")
    sys.exit(1)
print("all timestamp vectors pass")
