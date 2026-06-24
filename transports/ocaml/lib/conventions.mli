(* Conventions shared by every CSIL transport — see csil-transport-conventions.md.
   The version constant, the transport status registry, tag-24 payload wrap/unwrap,
   the max-frame guard, and the canonical-CBOR field accessors the envelopes build
   on so their bytes match the conformance vectors regardless of record layout. *)

(* The current transport version. A new value is minted only for a breaking change
   to envelope layout or semantics. *)
val version : int64

(* The CBOR semantic tag wrapping an embedded, opaque CBOR data item (24). *)
val tag_encoded_cbor : int64

(* The reserved service ordinal for the transport control plane. *)
val control_service_ord : int64

(* Default max encoded envelope size for stream/message carriers (16 MiB). *)
val max_frame_default : int

(* A transport-level status, distinct from application errors (which ride inside
   the payload as a declared [/ ErrorType] arm). Carried as the raw wire code so
   host-defined extension codes (>= 64) and unknown codes compare correctly. *)
type status = int64

val status_ok : status
val status_malformed_envelope : status
val status_unknown_service_or_op : status
val status_unauthenticated : status
val status_forbidden : status
val status_version_unsupported : status
val status_internal : status
val status_unavailable : status
val status_deadline_exceeded : status
val status_from_code : int64 -> status
val status_code : status -> int64
val status_is_ok : status -> bool

(* The registry name for a status, or "other" for codes outside it. *)
val status_name : status -> string

type error =
  | Malformed of string
  | Frame_too_large of { got : int; maximum : int }
  | Unsupported_version of int64
  | Carrier of string
  | Status_error of { status : string; code : int64; message : string }

val error_message : error -> string

(* Build a [Malformed] error from a format string (the "malformed envelope:" prefix
   is added by [error_message]). Shared by the per-transport envelope decoders. *)
val malformed : ('a, unit, string, error) format4 -> 'a

(* Wrap opaque payload bytes (themselves a CBOR item) in tag 24. *)
val tag24 : bytes -> Cbor.t

(* Extract the opaque payload bytes from a tag-24 value. *)
val untag24 : Cbor.t -> (bytes, error) result

(* Look up a text key in a CBOR map value. *)
val map_get : Cbor.t -> string -> Cbor.t option

(* Read a non-negative / signed integer from a decoded CBOR integer value. *)
val as_u64 : Cbor.t -> int64 option
val as_i64 : Cbor.t -> int64 option
val get_uint : Cbor.t -> string -> (int64, error) result
val get_int : Cbor.t -> string -> (int64, error) result
val get_text : Cbor.t -> string -> (string, error) result
val get_text_opt : Cbor.t -> string -> string option
val get_uint_opt : Cbor.t -> string -> int64 option

(* Verify a decoded envelope's version field. *)
val check_version : int64 -> (unit, error) result

(* Decode one envelope, mapping a CBOR-level failure into a [Malformed] error. *)
val decode_envelope : bytes -> (Cbor.t, error) result

(* [Result.bind] as a binding operator, so the envelope decoders read as a linear
   sequence of fallible steps. *)
val ( let* ) : ('a, 'e) result -> ('a -> ('b, 'e) result) -> ('b, 'e) result
