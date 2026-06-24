(* A UDP-backed datagram carrier — a built-in convenience for the native datagram
   path. The browser path (WebRTC unreliable / WebTransport) supplies its own
   [Carrier.datagram_carrier] record in the host. Uses the bundled [unix] library;
   all I/O is synchronous and blocking. *)

(* Wrap a connected UDP socket as a datagram carrier. One datagram maps to one UDP
   packet; ordering and delivery are not guaranteed. The receive buffer is one byte
   larger than the max so an oversized datagram is detected rather than silently
   truncated. *)
val udp_datagram_carrier : Unix.file_descr -> Carrier.datagram_carrier
