import struct, sys

def head(major, n):
    if n < 24: return bytes([major << 5 | n])
    if n < 256: return bytes([major << 5 | 24, n])
    if n < 65536: return bytes([major << 5 | 25]) + struct.pack(">H", n)
    return bytes([major << 5 | 26]) + struct.pack(">I", n)

def text(s):
    b = s.encode()
    return head(3, len(b)) + b

def tag(n, v): return head(6, n) + v
def stamp(v): return bytes([0xa1]) + text("at") + v
def t0(s): return stamp(tag(0, text(s)))

# name, expect, precision (unused; computed from the canonical text), input, canonical re-encoding
V = [
    ("whole-seconds", "ok", 0, t0("2000-02-29T12:00:00Z"), "2000-02-29T12:00:00Z"),
    ("milliseconds", "ok", 3, t0("2000-02-29T12:00:00.123Z"), "2000-02-29T12:00:00.123Z"),
    ("microseconds", "ok", 6, t0("2000-02-29T12:00:00.000500Z"), "2000-02-29T12:00:00.000500Z"),
    ("nanoseconds", "ok", 9, t0("2000-02-29T12:00:00.000000500Z"), "2000-02-29T12:00:00.000000500Z"),
    ("nanoseconds-only", "ok", 9, t0("2000-02-29T12:00:00.000000005Z"), "2000-02-29T12:00:00.000000005Z"),
    ("short-fraction", "ok", 3, t0("2000-02-29T12:00:00.5Z"), "2000-02-29T12:00:00.500Z"),
    ("trailing-zeros", "ok", 3, t0("2000-02-29T12:00:00.100000000Z"), "2000-02-29T12:00:00.100Z"),
    ("zero-fraction", "ok", 0, t0("2000-02-29T12:00:00.000Z"), "2000-02-29T12:00:00Z"),
    ("positive-offset", "ok", 0, t0("2000-02-29T13:30:00+01:30"), "2000-02-29T12:00:00Z"),
    ("negative-offset", "ok", 0, t0("2000-02-29T07:00:00-05:00"), "2000-02-29T12:00:00Z"),
    ("offset-crosses-day", "ok", 0, t0("2000-03-01T00:30:00+01:00"), "2000-02-29T23:30:00Z"),
    ("lowercase-t-z", "ok", 0, t0("2000-02-29t12:00:00z"), "2000-02-29T12:00:00Z"),
    ("first-year", "ok", 0, t0("0001-01-01T00:00:00Z"), "0001-01-01T00:00:00Z"),
    ("last-second", "ok", 0, t0("9999-12-31T23:59:59Z"), "9999-12-31T23:59:59Z"),
    ("space-no-offset", "err", 0, t0("2000-02-29 12:00:00"), ""),
    ("space-separator", "err", 0, t0("2000-02-29 12:00:00Z"), ""),
    ("not-a-time", "err", 0, t0("not a time"), ""),
    ("no-offset", "err", 0, t0("2000-02-29T12:00:00"), ""),
    ("untagged-text", "err", 0, stamp(text("2000-02-29T12:00:00Z")), ""),
    ("tag-1-epoch", "err", 0, stamp(tag(1, head(0, 951825600))), ""),
    ("tag-0-integer", "err", 0, stamp(tag(0, head(0, 951825600))), ""),
    ("tag-0-bytes", "err", 0, stamp(tag(0, head(2, 20) + b"2000-02-29T12:00:00Z")), ""),
    ("wrong-tag", "err", 0, stamp(tag(1004, text("2000-02-29T12:00:00Z"))), ""),
    ("invalid-day", "err", 0, t0("2001-02-29T12:00:00Z"), ""),
    ("invalid-month", "err", 0, t0("2000-13-01T12:00:00Z"), ""),
    ("hour-24", "err", 0, t0("2000-02-29T24:00:00Z"), ""),
    ("minute-60", "err", 0, t0("2000-02-29T12:60:00Z"), ""),
    ("leap-second", "err", 0, t0("2000-02-29T23:59:60Z"), ""),
    ("empty-fraction", "err", 0, t0("2000-02-29T12:00:00.Z"), ""),
    ("ten-digit-fraction", "err", 0, t0("2000-02-29T12:00:00.1234567890Z"), ""),
    ("comma-fraction", "err", 0, t0("2000-02-29T12:00:00,5Z"), ""),
    ("offset-without-colon", "err", 0, t0("2000-02-29T13:00:00+0100"), ""),
    ("offset-hour-only", "err", 0, t0("2000-02-29T13:00:00+01"), ""),
    ("offset-hour-24", "err", 0, t0("2000-02-29T12:00:00+24:00"), ""),
    ("offset-minute-60", "err", 0, t0("2000-02-29T12:00:00+01:60"), ""),
    ("short-year", "err", 0, t0("200-02-29T12:00:00Z"), ""),
    ("five-digit-year", "err", 0, t0("20000-02-29T12:00:00Z"), ""),
    ("unpadded-day", "err", 0, t0("2000-02-9T12:00:00Z"), ""),
    ("date-only", "err", 0, t0("2000-02-29"), ""),
    ("trailing-space", "err", 0, t0("2000-02-29T12:00:00Z "), ""),
    ("year-zero-written", "err", 0, t0("0000-12-31T23:30:00-01:00"), ""),
    ("year-zero-utc", "err", 0, t0("0001-01-01T00:30:00+01:00"), ""),
    ("year-10000-utc", "err", 0, t0("9999-12-31T23:30:00-01:00"), ""),
    ("non-ascii-digit", "err", 0, t0("２000-02-29T12:00:00Z"), ""),
]

out = sys.stdout
out.write("# name\texpect\tprecision\tinput_hex\tcanonical_hex\n")
def needed(canon):
    # Digits a decoder must hold: the position of the last nonzero fraction digit.
    # A 7-digit target (C#) can hold .000000500 but not .000000005.
    if "." not in canon:
        return 0
    return len(canon.split(".")[1].rstrip("Z").rstrip("0"))

for name, expect, _, data, canon in V:
    prec = needed(canon) if canon else 0
    c = t0(canon).hex() if canon else "-"
    out.write(f"{name}\t{expect}\t{prec}\t{data.hex()}\t{c}\n")
