# Decode each vector as a `Moment`; re-encode the rows that must decode.
# A Python `datetime` holds microseconds, so this target holds 6 fraction digits.
import sys

from api.types import Moment
import api.codec  # noqa: F401  (binds to_cbor/from_cbor onto the dataclasses)

DIGITS = 6

checked = 0
failures = 0
with open(sys.argv[1], encoding="utf-8") as vectors:
    for line in vectors:
        line = line.rstrip("\n")
        if not line or line.startswith("#"):
            continue
        name, expect, digits, input_hex, canonical = line.split("\t")
        if digits != "*" and int(digits) != DIGITS:
            continue
        checked += 1
        try:
            encoded = Moment.from_cbor(bytes.fromhex(input_hex)).to_cbor().hex()
        except ValueError as exc:
            if expect == "ok":
                failures += 1
                print(f"FAIL {name}: decode failed: {exc}")
            continue
        if expect != "ok":
            failures += 1
            print(f"FAIL {name}: decoded, but must fail (re-encoded {encoded})")
        elif encoded != canonical:
            failures += 1
            print(f"FAIL {name}: re-encoded {encoded}, want {canonical}")

if failures:
    print(f"{failures} of {checked} epoch vector(s) failed")
    sys.exit(1)
print(f"all {checked} epoch vectors pass")
