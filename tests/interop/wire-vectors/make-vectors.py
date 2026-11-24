# Write vectors.tsv: CBOR inputs for the shapes in wire.csil, whether each must
# decode, and the bytes a decoded value must re-encode to.
#
# Kinds: `holder` decodes as Holder, `contact` as Contact, and `output` as the
# named union that the read-counts output becomes (`{ * text => int } / Err`,
# `LookupReadCountsOutput`).
import pathlib


def head(major, n):
    if n < 24:
        return bytes([major << 5 | n])
    if n < 0x100:
        return bytes([major << 5 | 24, n])
    return bytes([major << 5 | 25]) + n.to_bytes(2, "big")


def uint(n):
    return head(0, n) if n >= 0 else head(1, -1 - n)


def text(s):
    b = s.encode()
    return head(3, len(b)) + b


def cmap(pairs):
    """A map with its entries in the order given, not sorted."""
    return head(5, len(pairs)) + b"".join(k + v for k, v in pairs)


def tagged(index, value):
    """A union value: the two-element array [variant_index, value]."""
    return head(4, 2) + uint(index) + value


def counts(*pairs):
    return cmap([(text(k), uint(v)) for k, v in pairs])


def labels(*pairs):
    return cmap([(uint(k), text(v)) for k, v in pairs])


def holder(c, lab):
    return cmap([(text("counts"), c), (text("labels"), lab)])


EMPTY = cmap([])
SORTED_LABELS = labels((1, "one"), (10, "ten"), (-1, "minus"))

rows = [
    # Dynamic maps: keys in bytewise order of their encoding, so a shorter text key
    # sorts first ("b" is 61 62, "aa" is 62 61 61), and the int keys sort as
    # 1 (01), 10 (0a), -1 (20).
    ("sorted-maps", "holder", "ok", holder(counts(("b", 1), ("aa", 2)), SORTED_LABELS), None),
    ("empty-maps", "holder", "ok", holder(EMPTY, EMPTY), None),
    ("unsorted-text-keys", "holder", "err", holder(counts(("aa", 2), ("b", 1)), EMPTY), None),
    ("text-keys-in-string-order", "holder", "err", holder(counts(("a", 1), ("aa", 2), ("b", 3)), EMPTY), None),
    ("unsorted-int-keys", "holder", "err", holder(EMPTY, labels((-1, "minus"), (1, "one"))), None),
    ("int-keys-in-number-order", "holder", "err", holder(EMPTY, labels((-1, "minus"), (1, "one"), (10, "ten"))), None),
    ("duplicate-map-key", "holder", "err", holder(counts(("a", 1), ("a", 2)), EMPTY), None),
    # A map key must use its shortest head: `78 01 61` is "a" with a one-byte length,
    # and `18 01` is the integer 1 with a one-byte argument.
    ("long-form-text-key", "holder", "err",
     holder(cmap([(bytes.fromhex("780161"), uint(1))]), EMPTY), None),
    ("long-form-int-key", "holder", "err",
     holder(EMPTY, cmap([(bytes.fromhex("1801"), text("one"))])), None),
    ("long-form-record-key", "holder", "err",
     cmap([(bytes.fromhex("7806") + b"counts", EMPTY), (text("labels"), EMPTY)]), None),
    # Records: field order is free on decode, but a key twice is not valid.
    ("record-fields-reordered", "holder", "ok",
     cmap([(text("labels"), EMPTY), (text("counts"), EMPTY)]), holder(EMPTY, EMPTY)),
    ("duplicate-record-key", "holder", "err",
     cmap([(text("counts"), EMPTY), (text("counts"), EMPTY), (text("labels"), EMPTY)]), None),
    # Record union from `//=`. Like every CSIL union it is the pair
    # [variant_index, value], and the index is the arm's place in declaration
    # order: the base (0), this file's `//=` arm (1), then the included file's arm
    # (2). The decoder dispatches by the index and checks the value against that arm.
    ("union-base-arm", "contact", "ok", tagged(0, cmap([(text("id"), uint(1)), (text("name"), text("x"))])), None),
    ("union-own-file-arm", "contact", "ok", tagged(1, cmap([(text("id"), uint(1))])), None),
    ("union-included-arm", "contact", "ok", tagged(2, cmap([(text("email"), text("e"))])), None),
    ("union-index-out-of-range", "contact", "err", tagged(3, cmap([(text("id"), uint(1))])), None),
    ("union-value-does-not-fit-its-arm", "contact", "err", tagged(0, cmap([(text("id"), uint(1))])), None),
    ("union-untagged-map", "contact", "err", cmap([(text("id"), uint(1)), (text("name"), text("x"))]), None),
    # The mixed op output is a union of the map (index 0) and Err (index 1).
    ("output-map-arm", "output", "ok", tagged(0, counts(("a", 1))), None),
    ("output-error-arm", "output", "ok", tagged(1, cmap([(text("message"), text("boom"))])), None),
    ("output-unsorted-map", "output", "err", tagged(0, counts(("b", 1), ("a", 2))), None),
]

lines = ["# name\tkind\texpect\tinput_hex\tcanonical_hex"]
for name, kind, expect, data, canonical in rows:
    canonical = data if canonical is None and expect == "ok" else canonical
    lines.append("\t".join([name, kind, expect, data.hex(), canonical.hex() if canonical else "-"]))
out = pathlib.Path(__file__).with_name("vectors.tsv")
out.write_text("\n".join(lines) + "\n", encoding="utf-8")
