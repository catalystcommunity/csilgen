(* CSIL-Events transport — typed bidirectional event streams — see
   csil-events-transport.md. Verbose (text-keyed) and compact (positional) wire
   profiles, plus the control-plane (service ordinal 0) lifecycle payloads. *)

(* Which wire profile a connection uses for its lifetime, fixed by the hello
   exchange. *)
type profile = Verbose | Compact

val profile_name : profile -> string
val parse_profile : string -> profile option

(* One typed event in either direction, identified by service+op (verbose) or by
   their ordinals (compact), with an optional correlation id. In verbose,
   [service] is absent on a single-service connection; in compact, [service_ord]
   and [op_ord] are always present. *)
type event = {
  service : string option;
  service_ord : int64 option;
  event : string option;
  op_ord : int64 option;
  id : int64 option;
  payload : bytes;
}

val new_verbose_event : string option -> string -> bytes -> event
val new_compact_event : int64 -> int64 -> bytes -> event
val event_with_id : event -> int64 -> event
val encode_event : event -> profile -> (bytes, Conventions.error) result
val decode_event : bytes -> profile -> (event, Conventions.error) result

(* Control-plane operation ordinals (under service ordinal 0). *)
val control_hello : int64
val control_hello_ack : int64
val control_ping : int64
val control_pong : int64
val control_close : int64
val control_error : int64

(* Verbose control-event names (the [$]-sigil names). *)
val hello_name : string
val hello_ack_name : string
val ping_name : string
val pong_name : string
val close_name : string
val error_name : string

(* The [$hello] payload offered by the connection initiator. *)
type hello = {
  versions : int64 list;
  profiles : string list;
  hello_service : string option;
  hello_auth : string option;
}

val encode_hello : hello -> bytes
val decode_hello : bytes -> (hello, Conventions.error) result

(* Select a version+profile from this hello's offers given what the receiver
   supports, honoring the peer's preference order. [None] if nothing is mutually
   supported. *)
val negotiate : hello -> profile list -> (int64 * profile) option

(* The [$hello-ack] payload returned by the peer. *)
type hello_ack = { v : int64; ack_profile : string; session : string option }

val encode_hello_ack : hello_ack -> bytes
val decode_hello_ack : bytes -> (hello_ack, Conventions.error) result

(* A [$ping]/[$pong] heartbeat payload. *)
type heartbeat = { nonce : int64; at : int64 option }

val encode_heartbeat : heartbeat -> bytes
val decode_heartbeat : bytes -> (heartbeat, Conventions.error) result

(* A [$close] payload. *)
type close = { status : Conventions.status; reason : string option }

val encode_close : close -> bytes
val decode_close : bytes -> (close, Conventions.error) result
