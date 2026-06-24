(* Behavioural tests beyond the byte-exact vectors: the loopback client/server
   round-trip, the sequence tracker's classifications, and the unsigned-64-bit CBOR
   boundaries the vectors do not exercise (OCaml's native int is only 63-bit, so the
   codec carries integers as Int64 and must handle 2^63-1 / 2^63 / 2^64-1 exactly). *)

open Csilgen_transport

let check name cond = if not cond then failwith ("roundtrip: " ^ name)

let cbor_boundaries () =
  (* Encode-shape checks at the unsigned-64 boundaries. *)
  check "u64_max encode"
    (Conformance.tohex (Cbor.encode (Cbor.Uint (-1L))) = "1bffffffffffffffff");
  check "u63_max encode"
    (Conformance.tohex (Cbor.encode (Cbor.Uint Int64.max_int))
    = "1b7fffffffffffffff");
  check "two_pow_63 encode"
    (Conformance.tohex (Cbor.encode (Cbor.Uint Int64.min_int))
    = "1b8000000000000000");
  (* Exact round-trips through decode. *)
  let roundtrip name v =
    let b = Cbor.encode v in
    match Cbor.decode b with
    | Ok d -> check (name ^ " roundtrip") (d = v)
    | Error e -> failwith (name ^ " decode: " ^ e)
  in
  roundtrip "u64_max" (Cbor.Uint (-1L));
  roundtrip "u63_max" (Cbor.Uint Int64.max_int);
  roundtrip "two_pow_63" (Cbor.Uint Int64.min_int);
  roundtrip "nint_min" (Cbor.Int Int64.min_int);
  roundtrip "nint_neg1" (Cbor.Int (-1L));
  (* A nint whose magnitude exceeds the signed-64 floor must be rejected, not
     silently wrapped. *)
  check "nint_overflow_rejected"
    (match Cbor.decode (Conformance.unhex "3bffffffffffffffff") with
    | Error _ -> true
    | Ok _ -> false);
  (* An envelope is exactly one CBOR item; trailing bytes are malformed. *)
  check "trailing_rejected"
    (match Cbor.decode (Conformance.unhex "0000") with
    | Error _ -> true
    | Ok _ -> false);
  (* canon_map orders by encoded-key bytes: "v" sorts before "service". *)
  let m =
    Cbor.canon_map
      [ (Cbor.Text "service", Cbor.Uint 0L); (Cbor.Text "v", Cbor.Uint 1L) ]
  in
  check "canon_order"
    (match Cbor.decode (Cbor.encode m) with
    | Ok (Cbor.Map ((Cbor.Text k0, _) :: _)) -> k0 = "v"
    | _ -> false)

let rpc_loopback () =
  let lb = Carrier.make_loopback () in
  let server = Rpc.new_server (Carrier.loopback_frame_carrier lb) in
  let req =
    Rpc.request_with_id
      (Rpc.new_request "Attestation" "deposit-claim" (Conformance.unhex "a0"))
      7L
  in
  Carrier.loopback_push_inbound lb (Rpc.encode_request req);
  let handler (r : Rpc.request) =
    Rpc.reply "DepositClaimResponse" r.Rpc.payload
  in
  (match Rpc.serve_one server handler with
  | Ok true -> ()
  | Ok false -> failwith "server returned no frame"
  | Error e -> failwith ("server: " ^ Conventions.error_message e));
  match Carrier.loopback_take_outbound lb with
  | None -> failwith "no response frame produced"
  | Some out -> (
      match Rpc.decode_response out with
      | Error e -> failwith ("decode response: " ^ Conventions.error_message e)
      | Ok resp ->
          check "echoed id" (resp.Rpc.id = Some 7L);
          check "variant" (resp.Rpc.variant = Some "DepositClaimResponse");
          check "status ok" (Rpc.as_transport_error resp = None))

let seq_tracker () =
  let kind e = e.Datagrams.kind in
  let t = Datagrams.new_seq_tracker () in
  check "seq first" (kind (Datagrams.observe t 5L None) = Datagrams.Seq_first);
  let adv = Datagrams.observe t 8L None in
  check "seq advanced"
    (kind adv = Datagrams.Seq_advanced && adv.Datagrams.gap = 2L);
  check "seq late"
    (kind (Datagrams.observe t 8L None) = Datagrams.Seq_late_or_duplicate);
  check "seq unsequenced"
    (kind (Datagrams.observe t 0L None) = Datagrams.Seq_advanced);
  (* A prior epoch must exist and change for a restart to fire. *)
  let t2 = Datagrams.new_seq_tracker () in
  ignore (Datagrams.observe t2 5L (Some 1));
  check "seq restart"
    (kind (Datagrams.observe t2 1L (Some 2)) = Datagrams.Seq_restart);
  (* no-epoch -> first-epoch is NOT a restart. *)
  let t3 = Datagrams.new_seq_tracker () in
  check "no restart on first epoch"
    (kind (Datagrams.observe t3 5L (Some 1)) = Datagrams.Seq_first)

let datagram_carrier () =
  let lb = Carrier.make_loopback () in
  let dc = Carrier.loopback_datagram_carrier lb in
  (match dc.Carrier.send_datagram (Conformance.unhex "84010005d81841a0") with
  | Ok () -> ()
  | Error e -> failwith ("send_datagram: " ^ Conventions.error_message e));
  check "datagram queued" (Carrier.loopback_take_outbound lb <> None)

let run () =
  cbor_boundaries ();
  rpc_loopback ();
  seq_tracker ();
  datagram_carrier ();
  print_endline "roundtrip: all behavioural checks passed"
