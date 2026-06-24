(* Minimal canonical CBOR codec (RFC 8949), hand-rolled and dependency-free, so
   the transport library stays offline-testable. It supports exactly what the CSIL
   envelopes need — unsigned ints, negative ints, text strings, byte strings,
   arrays, maps, and tags — and nothing else. The in-memory model is public so the
   transports can build envelopes from it; the head writer and decode cursor stay
   private. *)

(* CBOR integers reach 2^64-1, beyond OCaml's 63-bit native [int], so integer
   items carry an [int64]. [Uint] holds the unsigned bit pattern (a value >= 2^63
   is a negative [int64]); [Int] holds the signed logical value of a negative
   integer (it encodes as CBOR major type 1). *)
type t =
  | Uint of int64
  | Int of int64
  | Text of string
  | Bytes of bytes
  | Array of t list
  | Map of (t * t) list
  | Tag of int64 * t

(* Serialize a value to canonical CBOR bytes. *)
val encode : t -> bytes

(* Decode one complete envelope: a single self-contained CBOR item with no
   trailing bytes (an envelope is exactly one item per the conventions doc). Any
   leftover bytes are a malformed frame and rejected. *)
val decode : bytes -> (t, string) result

(* Build a deterministically-keyed CBOR map: entries are sorted by the bytewise
   lexicographic order of their *encoded* keys (RFC 8949 §4.2.1), so the same
   logical envelope always yields the same bytes. *)
val canon_map : (t * t) list -> t
