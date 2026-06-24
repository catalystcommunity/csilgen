(* See carrier.mli for the contract. Ported from transports/go/carrier.go, with
   the Go interfaces replaced by records of closures (the idiomatic OCaml seam). *)

type frame_carrier = {
  send_frame : bytes -> (unit, Conventions.error) result;
  recv_frame : unit -> (bytes option, Conventions.error) result;
}

type datagram_carrier = {
  send_datagram : bytes -> (unit, Conventions.error) result;
  recv_datagram : unit -> (bytes option, Conventions.error) result;
}

type loopback = { mutable inbound : bytes list; mutable outbound : bytes list }

let make_loopback () = { inbound = []; outbound = [] }
let loopback_push_inbound lb b = lb.inbound <- lb.inbound @ [ Bytes.copy b ]

let loopback_take_outbound lb =
  match lb.outbound with
  | [] -> None
  | f :: rest ->
      lb.outbound <- rest;
      Some f

let loopback_send lb b =
  lb.outbound <- lb.outbound @ [ Bytes.copy b ];
  Ok ()

let loopback_recv lb () =
  match lb.inbound with
  | [] -> Ok None
  | f :: rest ->
      lb.inbound <- rest;
      Ok (Some f)

let loopback_frame_carrier lb =
  { send_frame = loopback_send lb; recv_frame = loopback_recv lb }

let loopback_datagram_carrier lb =
  { send_datagram = loopback_send lb; recv_datagram = loopback_recv lb }

let write_length_prefixed out b ~max =
  let len = Bytes.length b in
  if len > max then
    Error (Conventions.Frame_too_large { got = len; maximum = max })
  else
    let prefix = Bytes.create 4 in
    Bytes.set_int32_be prefix 0 (Int32.of_int len);
    output_bytes out prefix;
    output_bytes out b;
    flush out;
    Ok ()

(* Read exactly [n] bytes, or [None] when the very first byte is already at a clean
   end of stream (an orderly close between frames). A short read mid-frame raises
   [End_of_file], surfaced as a carrier error by the caller. *)
let read_exact ic n =
  let buf = Bytes.create n in
  match input_char ic with
  | exception End_of_file -> None
  | first ->
      Bytes.set buf 0 first;
      really_input ic buf 1 (n - 1);
      Some buf

let read_length_prefixed ic ~max =
  match read_exact ic 4 with
  | None -> Ok None
  | Some prefix -> (
      (* Compare as an unsigned 32-bit value before narrowing: a length >= 2^31
         would otherwise become a negative [int] and slip past a signed guard. *)
      let len64 =
        Int64.logand (Int64.of_int32 (Bytes.get_int32_be prefix 0)) 0xFFFF_FFFFL
      in
      if Int64.unsigned_compare len64 (Int64.of_int max) > 0 then
        Error
          (Conventions.Frame_too_large
             { got = Int64.to_int len64; maximum = max })
      else
        let len = Int64.to_int len64 in
        match try Some (Bytes.create len) with _ -> None with
        | None -> Error (Conventions.Carrier "frame allocation failed")
        | Some buf -> (
            match really_input ic buf 0 len with
            | () -> Ok (Some buf)
            | exception End_of_file ->
                Error (Conventions.Carrier "truncated frame body")))

let stream_carrier ?(max_frame = Conventions.max_frame_default) ic out =
  {
    send_frame = (fun b -> write_length_prefixed out b ~max:max_frame);
    recv_frame = (fun () -> read_length_prefixed ic ~max:max_frame);
  }
