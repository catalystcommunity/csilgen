(* Verify the OCaml reference library against the checked-in conformance vectors.
   Reconstruct each vector's envelope from its language-neutral [input], assert
   encode -> [hex], and assert decode([hex]) -> the same envelope. Mirrors
   transports/go/conformance_test.go and transports/rust/tests/conformance.rs.

   The vectors live at transports/conformance/, outside this dune project, so the
   directory is taken from CSILGEN_CONFORMANCE_DIR (set by run-tests.sh); a few
   relative fallbacks cover an in-tree run. *)

module J = Json
open Csilgen_transport

let read_file path =
  let ic = open_in_bin path in
  Fun.protect
    ~finally:(fun () -> close_in ic)
    (fun () -> really_input_string ic (in_channel_length ic))

let vectors_path name =
  let dir =
    match Sys.getenv_opt "CSILGEN_CONFORMANCE_DIR" with
    | Some d -> d
    | None -> (
        let candidates =
          [ "../conformance"; "../../conformance"; "../../../conformance" ]
        in
        match List.find_opt Sys.file_exists candidates with
        | Some d -> d
        | None -> "../conformance")
  in
  Filename.concat dir name

let load name =
  J.to_list (J.member "vectors" (J.parse (read_file (vectors_path name))))

let tohex b =
  let buf = Buffer.create (Bytes.length b * 2) in
  for i = 0 to Bytes.length b - 1 do
    Buffer.add_string buf (Printf.sprintf "%02x" (Bytes.get_uint8 b i))
  done;
  Buffer.contents buf

let unhex s =
  let n = String.length s / 2 in
  let b = Bytes.create n in
  for i = 0 to n - 1 do
    Bytes.set_uint8 b i (int_of_string ("0x" ^ String.sub s (i * 2) 2))
  done;
  b

let expect_hex name ~got ~want =
  if got <> want then (
    Printf.eprintf "FAIL %s encode:\n  got  %s\n  want %s\n" name got want;
    failwith ("conformance encode mismatch: " ^ name))

let expect name cond =
  if not cond then (
    Printf.eprintf "FAIL %s\n" name;
    failwith ("conformance mismatch: " ^ name))

(* Accessors over a vector's language-neutral [input] object. *)
let req_str input k = J.to_string (J.member k input)
let req_i64 input k = J.to_int64 (J.member k input)
let req_int input k = J.to_int (J.member k input)

let opt_str input k =
  match J.member k input with J.Str s -> Some s | _ -> None

let opt_i64 input k =
  match J.member k input with J.Num f -> Some (Int64.of_float f) | _ -> None

let opt_int input k =
  match J.member k input with J.Num f -> Some (int_of_float f) | _ -> None

let die name e = failwith (name ^ ": " ^ Conventions.error_message e)

let rpc_vectors () =
  List.iter
    (fun vec ->
      let name = J.to_string (J.member "name" vec) in
      let hex = J.to_string (J.member "hex" vec) in
      let input = J.member "input" vec in
      match req_str input "kind" with
      | "request" -> (
          let r =
            Rpc.new_request (req_str input "service") (req_str input "op")
              (unhex (req_str input "payload_hex"))
          in
          let r =
            match opt_i64 input "id" with
            | Some id -> Rpc.request_with_id r id
            | None -> r
          in
          let r =
            match opt_str input "auth" with
            | Some a -> Rpc.request_with_auth r a
            | None -> r
          in
          let bytes = Rpc.encode_request r in
          expect_hex name ~got:(tohex bytes) ~want:hex;
          match Rpc.decode_request bytes with
          | Ok dec -> expect (name ^ " decode") (dec = r)
          | Error e -> die (name ^ " decode") e)
      | "response" -> (
          let r : Rpc.response =
            {
              Rpc.id = opt_i64 input "id";
              status = Conventions.status_from_code (req_i64 input "status");
              variant = opt_str input "variant";
              error = opt_str input "error";
              payload = unhex (req_str input "payload_hex");
            }
          in
          let bytes = Rpc.encode_response r in
          expect_hex name ~got:(tohex bytes) ~want:hex;
          match Rpc.decode_response bytes with
          | Ok dec -> expect (name ^ " decode") (dec = r)
          | Error e -> die (name ^ " decode") e)
      | "push" -> (
          let p =
            Rpc.new_push (req_str input "service") (req_str input "event")
              (unhex (req_str input "payload_hex"))
          in
          let bytes = Rpc.encode_push p in
          expect_hex name ~got:(tohex bytes) ~want:hex;
          match Rpc.decode_push bytes with
          | Ok dec -> expect (name ^ " decode") (dec = p)
          | Error e -> die (name ^ " decode") e)
      | other -> failwith ("unknown rpc kind " ^ other))
    (load "rpc.json")

let events_vectors () =
  List.iter
    (fun vec ->
      let name = J.to_string (J.member "name" vec) in
      let hex = J.to_string (J.member "hex" vec) in
      let input = J.member "input" vec in
      let control = J.member "control" input in
      if not (J.is_null control) then
        let bytes =
          match J.to_string control with
          | "hello" ->
              let versions =
                List.map J.to_int64 (J.to_list (J.member "versions" input))
              in
              let profiles =
                List.map J.to_string (J.to_list (J.member "profiles" input))
              in
              Events.encode_hello
                Events.
                  {
                    versions;
                    profiles;
                    hello_service = opt_str input "service";
                    hello_auth = opt_str input "auth";
                  }
          | "hello_ack" ->
              Events.encode_hello_ack
                Events.
                  {
                    v = req_i64 input "v";
                    ack_profile = req_str input "profile";
                    session = opt_str input "session";
                  }
          | "ping" ->
              Events.encode_heartbeat
                Events.
                  { nonce = req_i64 input "nonce"; at = opt_i64 input "at" }
          | "close" ->
              Events.encode_close
                Events.
                  {
                    status =
                      Conventions.status_from_code (req_i64 input "status");
                    reason = opt_str input "reason";
                  }
          | other -> failwith ("unknown control " ^ other)
        in
        expect_hex name ~got:(tohex bytes) ~want:hex
      else
        let profile =
          match Events.parse_profile (req_str input "profile") with
          | Some p -> p
          | None -> failwith "unknown profile"
        in
        let payload = unhex (req_str input "payload_hex") in
        let ev =
          match profile with
          | Events.Verbose -> (
              let e =
                Events.new_verbose_event (opt_str input "service")
                  (req_str input "event") payload
              in
              match opt_i64 input "id" with
              | Some id -> Events.event_with_id e id
              | None -> e)
          | Events.Compact -> (
              let e =
                Events.new_compact_event
                  (req_i64 input "service_ord")
                  (req_i64 input "op_ord") payload
              in
              match opt_i64 input "id" with
              | Some id -> Events.event_with_id e id
              | None -> e)
        in
        match Events.encode_event ev profile with
        | Ok bytes -> (
            expect_hex name ~got:(tohex bytes) ~want:hex;
            match Events.decode_event bytes profile with
            | Ok dec -> expect (name ^ " decode") (dec = ev)
            | Error e -> die (name ^ " decode") e)
        | Error e -> die (name ^ " encode") e)
    (load "events.json")

let datagram_vectors () =
  List.iter
    (fun vec ->
      let name = J.to_string (J.member "name" vec) in
      let hex = J.to_string (J.member "hex" vec) in
      let input = J.member "input" vec in
      match req_str input "profile" with
      | "cbor-array" -> (
          let d =
            Datagrams.new_datagram (req_i64 input "op_ord")
              (req_i64 input "seq")
              (unhex (req_str input "payload_hex"))
          in
          let bytes = Datagrams.encode_datagram d in
          expect_hex name ~got:(tohex bytes) ~want:hex;
          match Datagrams.decode_datagram bytes with
          | Ok dec -> expect (name ^ " decode") (dec = d)
          | Error e -> die (name ^ " decode") e)
      | "compact-header" -> (
          let d =
            Datagrams.new_compact_datagram (req_int input "op_ord")
              (req_int input "seq")
              (unhex (req_str input "body_hex"))
          in
          let d =
            match opt_int input "epoch" with
            | Some e -> Datagrams.compact_with_epoch d e
            | None -> d
          in
          let bytes = Datagrams.encode_compact_datagram d in
          expect_hex name ~got:(tohex bytes) ~want:hex;
          match Datagrams.decode_compact_datagram bytes with
          | Ok dec -> expect (name ^ " decode") (dec = d)
          | Error e -> die (name ^ " decode") e)
      | other -> failwith ("unknown datagram profile " ^ other))
    (load "datagrams.json")

let run () =
  rpc_vectors ();
  events_vectors ();
  datagram_vectors ();
  print_endline "conformance: all vectors passed"
