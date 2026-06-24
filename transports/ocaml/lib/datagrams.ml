(* See datagrams.mli for the contract. Ported from transports/go/datagrams.go. *)

open Conventions

let max_datagram_default = 1200

type datagram = { op_ord : int64; seq : int64; payload : bytes }

let new_datagram op_ord seq payload = { op_ord; seq; payload }

let encode_datagram (d : datagram) =
  Cbor.encode
    (Cbor.Array
       [
         Cbor.Uint version; Cbor.Uint d.op_ord; Cbor.Uint d.seq; tag24 d.payload;
       ])

let int_field v =
  match as_u64 v with
  | Some n -> Ok n
  | None -> Error (malformed "datagram field not an integer")

let decode_datagram b =
  let* v = decode_envelope b in
  match v with
  | Cbor.Array [ vv; opv; seqv; plv ] ->
      let* ver = int_field vv in
      let* () = check_version ver in
      let* op_ord = int_field opv in
      let* seq = int_field seqv in
      let* payload = untag24 plv in
      Ok { op_ord; seq; payload }
  | Cbor.Array arr ->
      Error
        (malformed "datagram array has %d elements, expected 4"
           (List.length arr))
  | _ -> Error (malformed "datagram is not an array")

type compact_datagram = {
  c_op_ord : int;
  c_seq : int;
  epoch : int option;
  body : bytes;
}

(* The compact header packs a 4-bit version and 4-bit flags in the first byte. *)
let compact_ver = 1
let flag_epoch = 0b0001

let new_compact_datagram c_op_ord c_seq body =
  { c_op_ord; c_seq; epoch = None; body }

let compact_with_epoch d epoch = { d with epoch = Some epoch }

let encode_compact_datagram (d : compact_datagram) =
  let buf = Buffer.create (5 + Bytes.length d.body) in
  let flags = match d.epoch with Some _ -> flag_epoch | None -> 0 in
  Buffer.add_char buf (Char.chr ((compact_ver lsl 4) lor (flags land 0x0f)));
  Buffer.add_char buf (Char.chr (d.c_op_ord land 0xff));
  Buffer.add_char buf (Char.chr ((d.c_seq lsr 8) land 0xff));
  Buffer.add_char buf (Char.chr (d.c_seq land 0xff));
  (match d.epoch with
  | Some e -> Buffer.add_char buf (Char.chr (e land 0xff))
  | None -> ());
  Buffer.add_bytes buf d.body;
  Buffer.to_bytes buf

let decode_compact_datagram b =
  let n = Bytes.length b in
  if n < 4 then
    Error (malformed "compact datagram shorter than the 4-byte header")
  else
    let b0 = Bytes.get_uint8 b 0 in
    let ver = b0 lsr 4 in
    if ver <> compact_ver then Error (Unsupported_version (Int64.of_int ver))
    else
      let flags = b0 land 0x0f in
      let op_ord = Bytes.get_uint8 b 1 in
      let seq = (Bytes.get_uint8 b 2 lsl 8) lor Bytes.get_uint8 b 3 in
      if flags land flag_epoch <> 0 then
        if n < 5 then
          Error
            (malformed
               "compact datagram flags claim an epoch byte that is absent")
        else
          Ok
            {
              c_op_ord = op_ord;
              c_seq = seq;
              epoch = Some (Bytes.get_uint8 b 4);
              body = Bytes.sub b 5 (n - 5);
            }
      else
        Ok
          {
            c_op_ord = op_ord;
            c_seq = seq;
            epoch = None;
            body = Bytes.sub b 4 (n - 4);
          }

type seq_event_kind =
  | Seq_first
  | Seq_advanced
  | Seq_late_or_duplicate
  | Seq_restart

type seq_event = { kind : seq_event_kind; gap : int64 }

type seq_tracker = {
  mutable last_seq : int64 option;
  mutable last_epoch : int option;
}

let new_seq_tracker () = { last_seq = None; last_epoch = None }

let epoch_equal a b =
  match (a, b) with None, None -> true | Some x, Some y -> x = y | _ -> false

let observe t seq epoch =
  (* A restart fires only when a *prior* epoch existed and changed; no-epoch to
     first-epoch is not a restart. *)
  if (not (epoch_equal epoch t.last_epoch)) && t.last_epoch <> None then (
    t.last_epoch <- epoch;
    t.last_seq <- Some seq;
    { kind = Seq_restart; gap = 0L })
  else (
    t.last_epoch <- epoch;
    (* seq 0 is unsequenced: it carries no ordering, so it is never late or
       duplicate, and it leaves the running sequence untouched. *)
    if Int64.equal seq 0L then { kind = Seq_advanced; gap = 0L }
    else
      match t.last_seq with
      | None ->
          t.last_seq <- Some seq;
          { kind = Seq_first; gap = 0L }
      | Some last ->
          if Int64.unsigned_compare seq last > 0 then (
            let gap = Int64.sub (Int64.sub seq last) 1L in
            t.last_seq <- Some seq;
            { kind = Seq_advanced; gap })
          else { kind = Seq_late_or_duplicate; gap = 0L })
