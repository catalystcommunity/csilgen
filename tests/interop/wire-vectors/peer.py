# Decode each vector as the shape its kind names; re-encode the rows that must decode.
# A union rule has no public byte-level entry point in Python, so the value-tree
# helpers of the codec module are called with `cbor_decode`/`cbor_encode`.
import sys

from api.types import Holder
from api import codec

SHAPES = {
    "holder": (Holder.from_cbor, lambda value: value.to_cbor()),
    "contact": (
        lambda data: codec._decode_contact_value(codec.cbor_decode(data)),
        lambda value: codec.cbor_encode(codec._encode_contact_value(value)),
    ),
    "output": (
        lambda data: codec._decode_lookup_read_counts_output_value(codec.cbor_decode(data)),
        lambda value: codec.cbor_encode(codec._encode_lookup_read_counts_output_value(value)),
    ),
}

failures = 0
with open(sys.argv[1], encoding="utf-8") as vectors:
    for line in vectors:
        line = line.rstrip("\n")
        if not line or line.startswith("#"):
            continue
        name, kind, expect, input_hex, canonical = line.split("\t")
        decode, encode = SHAPES[kind]
        try:
            value = decode(bytes.fromhex(input_hex))
        except codec.CsilDecodeError as exc:
            if expect == "ok":
                failures += 1
                print(f"FAIL {name}: decode failed: {exc}")
            continue
        if expect != "ok":
            failures += 1
            print(f"FAIL {name}: decoded, but must fail ({value!r})")
            continue
        encoded = encode(value).hex()
        if encoded != canonical:
            failures += 1
            print(f"FAIL {name}: re-encoded {encoded}, want {canonical}")

if failures:
    print(f"{failures} vector(s) failed")
    sys.exit(1)
print("all wire vectors pass")
