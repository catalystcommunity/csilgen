(* Carrier seams — the bring-your-own-carrier boundary (conventions doc §7).

   The library owns envelope codecs, framing, and lifecycle; the *carrier* (the
   byte/datagram transport) is injected as a record of closures. A host supplies
   QUIC, WebRTC, a platform media stack, or anything else by building one of these
   records — without changing the library. All I/O is synchronous and blocking;
   the host owns the I/O loop and threads (no Lwt/Async/Eio). *)

(* Sends and receives one *delimited message* at a time. Used by CSIL-RPC and
   CSIL-Events. [recv_frame] returns [Ok None] at a clean end of stream. *)
type frame_carrier = {
  send_frame : bytes -> (unit, Conventions.error) result;
  recv_frame : unit -> (bytes option, Conventions.error) result;
}

(* Sends and receives one self-contained datagram, with no delivery or ordering
   guarantee. Used by CSIL-Datagrams. *)
type datagram_carrier = {
  send_datagram : bytes -> (unit, Conventions.error) result;
  recv_datagram : unit -> (bytes option, Conventions.error) result;
}

(* An in-memory loopback backed by queues of frames — for tests and for driving
   the codec without a socket. *)
type loopback

val make_loopback : unit -> loopback

(* Queue a frame a subsequent [recv_*] will return. *)
val loopback_push_inbound : loopback -> bytes -> unit

(* Take the next frame that was sent, or [None] if none. *)
val loopback_take_outbound : loopback -> bytes option
val loopback_frame_carrier : loopback -> frame_carrier
val loopback_datagram_carrier : loopback -> datagram_carrier

(* Write/read one frame with the canonical 4-byte big-endian length prefix,
   enforcing the max-frame guard before writing / before allocating on read. *)
val write_length_prefixed :
  out_channel -> bytes -> max:int -> (unit, Conventions.error) result

val read_length_prefixed :
  in_channel -> max:int -> (bytes option, Conventions.error) result

(* A frame carrier over any byte stream (TCP, TLS, Unix socket) using the
   canonical length-prefix framing. [max_frame] defaults to [max_frame_default]. *)
val stream_carrier :
  ?max_frame:int -> in_channel -> out_channel -> frame_carrier
