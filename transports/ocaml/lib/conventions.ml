(* See conventions.mli for the contract. Ported from transports/go/conventions.go. *)

let version = 1L
let tag_encoded_cbor = 24L
let control_service_ord = 0L
let max_frame_default = 16 * 1024 * 1024

(* The 4-byte length prefix can express more, but the limit lives in a signed
   32-bit integer in several supported languages, so this is the portable ceiling
   every CSIL transport agrees on. Anything above it would also disable the receive
   guard outright, since no wire length could exceed it. *)
let max_frame_limit = 2_147_483_647

type status = int64

let status_ok = 0L
let status_malformed_envelope = 1L
let status_unknown_service_or_op = 2L
let status_unauthenticated = 3L
let status_forbidden = 4L
let status_version_unsupported = 5L
let status_internal = 6L
let status_unavailable = 7L
let status_deadline_exceeded = 8L
let status_from_code code = code
let status_code s = s
let status_is_ok s = Int64.equal s 0L

let status_name s =
  match Int64.to_int s with
  | 0 -> "ok"
  | 1 -> "malformed-envelope"
  | 2 -> "unknown-service-or-op"
  | 3 -> "unauthenticated"
  | 4 -> "forbidden"
  | 5 -> "version-unsupported"
  | 6 -> "internal"
  | 7 -> "unavailable"
  | 8 -> "deadline-exceeded"
  | _ -> "other"

type error =
  | Malformed of string
  | Frame_too_large of { got : int; maximum : int }
  | Invalid_max_frame of { got : int; limit : int }
  | Unsupported_version of int64
  | Carrier of string
  | Status_error of { status : string; code : int64; message : string }

let error_message = function
  | Malformed m -> "malformed envelope: " ^ m
  | Frame_too_large { got; maximum } ->
      Printf.sprintf "frame of %d bytes exceeds max-frame guard of %d bytes" got
        maximum
  | Invalid_max_frame { got; limit } ->
      Printf.sprintf "max-frame limit of %d is outside the valid range 1..=%d" got
        limit
  | Unsupported_version v ->
      Printf.sprintf "unsupported transport version %Ld" v
  | Carrier m -> "carrier error: " ^ m
  | Status_error { status; code; message } ->
      if String.length message > 0 then
        Printf.sprintf "transport status %s (%Ld): %s" status code message
      else Printf.sprintf "transport status %s (%Ld)" status code

let malformed fmt = Printf.ksprintf (fun s -> Malformed s) fmt

(* Check a host-supplied max-frame limit against the conventions doc range, so an
   unusable carrier fails at construction rather than on its first frame. *)
let validate_max_frame max_frame =
  if max_frame < 1 || max_frame > max_frame_limit then
    Error (Invalid_max_frame { got = max_frame; limit = max_frame_limit })
  else Ok max_frame

let tag24 payload =
  (* Copy so a later mutation of the caller's buffer can't change the envelope. *)
  Cbor.Tag (tag_encoded_cbor, Cbor.Bytes (Bytes.copy payload))

let untag24 (v : Cbor.t) : (bytes, error) result =
  match v with
  | Cbor.Tag (num, Cbor.Bytes b) when Int64.equal num tag_encoded_cbor ->
      Ok (Bytes.copy b)
  | Cbor.Tag (num, _) when Int64.equal num tag_encoded_cbor ->
      Error (Malformed "tag-24 payload is not a byte string")
  | _ -> Error (Malformed "expected a tag-24 (encoded-cbor) payload")

let map_get (v : Cbor.t) key =
  match v with
  | Cbor.Map entries ->
      List.find_map
        (fun (k, value) ->
          match k with
          | Cbor.Text t when String.equal t key -> Some value
          | _ -> None)
        entries
  | _ -> None

let as_u64 (v : Cbor.t) =
  match v with
  | Cbor.Uint n -> Some n
  | Cbor.Int n when Int64.compare n 0L >= 0 -> Some n
  | _ -> None

let as_i64 (v : Cbor.t) =
  match v with Cbor.Uint n | Cbor.Int n -> Some n | _ -> None

let get_uint v key =
  match map_get v key with
  | None -> Error (malformed "missing or non-integer field '%s'" key)
  | Some value -> (
      match as_u64 value with
      | Some n -> Ok n
      | None -> Error (malformed "field '%s' is not a non-negative integer" key)
      )

let get_int v key =
  match map_get v key with
  | None -> Error (malformed "missing or non-integer field '%s'" key)
  | Some value -> (
      match as_i64 value with
      | Some n -> Ok n
      | None -> Error (malformed "missing or non-integer field '%s'" key))

let get_text v key =
  match map_get v key with
  | Some (Cbor.Text t) -> Ok t
  | _ -> Error (malformed "missing or non-text field '%s'" key)

let get_text_opt v key =
  match map_get v key with Some (Cbor.Text t) -> Some t | _ -> None

let get_uint_opt v key =
  match map_get v key with Some value -> as_u64 value | None -> None

let check_version v =
  if Int64.equal v version then Ok () else Error (Unsupported_version v)

(* An envelope is a single CBOR item; a CBOR-level failure (including trailing
   bytes) is a malformed frame at the transport layer. *)
let decode_envelope b =
  match Cbor.decode b with Ok v -> Ok v | Error m -> Error (Malformed m)

let ( let* ) = Result.bind
