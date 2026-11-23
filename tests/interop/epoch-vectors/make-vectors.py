# Writes vectors.tsv for cddl_time (CBOR tag 1 epoch seconds). The decode and
# encode functions below are the normative rule in docs/cbor-wire-contract.md;
# every target must produce the same results with IEEE 754 binary64 arithmetic.
import math, struct, sys

FIRST = -62135596800  # 0001-01-01T00:00:00Z
LAST = 253402300799  # 9999-12-31T23:59:59Z
DIGITS = (3, 6, 7, 9)

def head(major, n):
    if n < 24: return bytes([major << 5 | n])
    if n < 256: return bytes([major << 5 | 24, n])
    if n < 65536: return bytes([major << 5 | 25]) + struct.pack(">H", n)
    if n < 2**32: return bytes([major << 5 | 26]) + struct.pack(">I", n)
    return bytes([major << 5 | 27]) + struct.pack(">Q", n)

def integer(n): return head(0, n) if n >= 0 else head(1, -1 - n)
def f16(bits): return bytes([0xf9]) + struct.pack(">H", bits)
def f32(x): return bytes([0xfa]) + struct.pack(">f", x)
def f64(x): return bytes([0xfb]) + struct.pack(">d", x)
def text(s):
    b = s.encode()
    return head(3, len(b)) + b
def tag(n, v): return head(6, n) + v
def moment(v): return bytes([0xa1]) + text("at") + v
def t1(v): return moment(tag(1, v))

def decode(value, p):
    """Return (seconds, units of 10**-p s) or None for a decode error."""
    if isinstance(value, int):
        secs, units = value, 0
    else:
        if not math.isfinite(value) or value < FIRST - 1 or value > LAST + 1:
            return None
        secs = math.floor(value)
        frac = value - secs
        units = math.floor(frac * float(10**p) + 0.5)
        if units == 10**p:
            secs, units = secs + 1, 0
    if not FIRST <= secs <= LAST:
        return None
    return secs, units

def encode(secs, units, p):
    if units == 0:
        return integer(secs)
    return f64(float(secs) + float(units) / float(10**p))

# Rows that give the same result at every precision: name, expect, input value bytes, decode value.
COMMON = [
    ("int-seconds", integer(951825600), 951825600),
    ("int-zero", integer(0), 0),
    ("negative-int", integer(-1), -1),
    ("first-second", integer(FIRST), FIRST),
    ("last-second", integer(LAST), LAST),
    ("before-first", integer(FIRST - 1), FIRST - 1),
    ("after-last", integer(LAST + 1), LAST + 1),
    ("float64-whole", f64(951825600.0), 951825600.0),
    ("float32-whole", f32(16777216.0), 16777216.0),
    ("float16-whole", f16(0x3C00), 1.0),
    ("float-half", f64(951825600.5), 951825600.5),
    ("float16-half", f16(0x3E00), 1.5),
    ("negative-half", f64(-1.5), -1.5),
    ("negative-zero", f64(-0.0), -0.0),
    ("float-first-second", f64(float(FIRST)), float(FIRST)),
    ("float-before-first", f64(FIRST - 0.5), FIRST - 0.5),
    ("float-last-half", f64(LAST + 0.5), LAST + 0.5),
    ("float-after-last", f64(float(LAST + 1)), float(LAST + 1)),
    ("float-huge", f64(1e300), 1e300),
    ("float16-nan", f16(0x7E00), math.nan),
    ("float64-nan", f64(math.nan), math.nan),
    ("float16-infinity", f16(0x7C00), math.inf),
    ("float16-negative-infinity", f16(0xFC00), -math.inf),
]
# Rows whose result depends on the fraction digits the target holds.
ROUNDED = [
    ("sub-second", 951825600.123456),
    ("near-carry", 951825600.9999999),
    ("half-millisecond", 1.0005),
    ("negative-fraction", -0.0001),
    ("tiny-fraction", 2.0**-30),
]
REJECTED = [
    ("uint64-max", tag(1, head(0, 2**64 - 1))),
    ("nint64-min", tag(1, head(1, 2**64 - 1))),
    ("tag-0-text", tag(0, text("2000-02-29T12:00:00Z"))),
    ("untagged-int", integer(951825600)),
    ("untagged-float", f64(951825600.5)),
    ("tag-1-text", tag(1, text("951825600"))),
    ("tag-1-bytes", tag(1, head(2, 1) + b"\x01")),
    ("tag-1-true", tag(1, bytes([0xF5]))),
    ("tag-1-null", tag(1, bytes([0xF6]))),
    ("tag-1-array", tag(1, head(4, 1) + integer(1))),
    ("wrong-tag", tag(1004, integer(951825600))),
]

rows = []
for name, data, value in COMMON:
    results = {decode(value, p) and encode(*decode(value, p), p) for p in DIGITS}
    assert len(results) == 1, name
    (canon,) = results
    rows.append((name, "ok" if canon else "err", "*", t1(data), moment(tag(1, canon)) if canon else None))
for name, value in ROUNDED:
    for p in DIGITS:
        d = decode(value, p)
        rows.append((f"{name}@{p}", "ok", str(p), t1(f64(value)), moment(tag(1, encode(*d, p)))))
for name, data in REJECTED:
    rows.append((name, "err", "*", moment(data), None))

out = sys.stdout
out.write("# name\texpect\tdigits\tinput_hex\tcanonical_hex\n")
out.write("# digits: * applies to every target; a number applies only to a target that holds that many fraction digits.\n")
for name, expect, digits, data, canon in rows:
    out.write(f"{name}\t{expect}\t{digits}\t{data.hex()}\t{canon.hex() if canon else '-'}\n")
