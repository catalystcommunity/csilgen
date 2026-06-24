(* See udp.mli for the contract. Ported from transports/go/udp.go. *)

(* One byte past the max so a filled-past-max read flags a kernel-truncated,
   oversized datagram whose bytes are incomplete and must not reach the decoder. *)
let recv_buf_size = Datagrams.max_datagram_default + 1

let udp_datagram_carrier fd =
  let buf = Bytes.create recv_buf_size in
  Carrier.
    {
      send_datagram =
        (fun b ->
          try
            ignore (Unix.send fd b 0 (Bytes.length b) []);
            Ok ()
          with Unix.Unix_error (e, _, _) ->
            Error (Conventions.Carrier (Unix.error_message e)));
      recv_datagram =
        (fun () ->
          try
            let n = Unix.recv fd buf 0 recv_buf_size [] in
            if n > Datagrams.max_datagram_default then
              Error (Conventions.Carrier "datagram exceeds max size")
            else Ok (Some (Bytes.sub buf 0 n))
          with Unix.Unix_error (e, _, _) ->
            Error (Conventions.Carrier (Unix.error_message e)));
    }
