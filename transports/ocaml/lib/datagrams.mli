(* CSIL-Datagrams transport — unreliable, unordered, message-oriented — see
   csil-datagrams-transport.md. CBOR-array (default) and compact fixed-header
   profiles. A datagram channel is single-service: the service is bound at channel
   setup, so datagrams carry no service ordinal. *)

(* Conservative max datagram size (envelope + payload) safe across
   UDP/WebRTC/QUIC. *)
val max_datagram_default : int

(* A datagram in the CBOR-array (default) profile: [[v, op_ord, seq, payload]].
   [op_ord]/[seq] are full CBOR uints, so they are carried as [int64]. [seq] 0
   means "unsequenced". *)
type datagram = { op_ord : int64; seq : int64; payload : bytes }

val new_datagram : int64 -> int64 -> bytes -> datagram
val encode_datagram : datagram -> bytes
val decode_datagram : bytes -> (datagram, Conventions.error) result

(* A datagram in the compact fixed-header profile. Header layout:
   [[ver|flags][op_ord:u8][seq:u16 BE]([epoch:u8])] then the opaque body. The
   fixed-width header fields fit native [int]; [epoch] is present when the sender
   tracks restarts. *)
type compact_datagram = {
  c_op_ord : int;
  c_seq : int;
  epoch : int option;
  body : bytes;
}

val new_compact_datagram : int -> int -> bytes -> compact_datagram
val compact_with_epoch : compact_datagram -> int -> compact_datagram
val encode_compact_datagram : compact_datagram -> bytes

val decode_compact_datagram :
  bytes -> (compact_datagram, Conventions.error) result

(* Classifies an arriving sequence number relative to what was last seen, for
   loss/reorder/restart detection. The transport detects; the app decides. *)
type seq_event_kind =
  | Seq_first
  | Seq_advanced
  | Seq_late_or_duplicate
  | Seq_restart

(* A classification plus the gap count (meaningful only for [Seq_advanced]). *)
type seq_event = { kind : seq_event_kind; gap : int64 }

(* Tracks the last sequence/epoch per channel to classify arrivals. Unsequenced
   datagrams (seq 0) are reported as [Seq_advanced] with gap 0. *)
type seq_tracker

val new_seq_tracker : unit -> seq_tracker

(* Classify an arriving (seq, epoch) and update the tracker state. *)
val observe : seq_tracker -> int64 -> int option -> seq_event
