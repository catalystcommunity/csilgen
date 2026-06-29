(* OCaml interop harness — mirrors the Rust reference (tests/interop/harness/rust).

   Runs as a server (binds a loopback TCP/UDP port and serves one transport) or a
   client (connects, runs the case battery for one transport, prints JSON results).
   See tests/interop/README.md for the contract this implements. *)

open Csilgen_transport
module T = Interop_api.Types
module C = Interop_api.Codec
module CB = Interop_api.Csil_cbor

let service = "interop"

(* --------------------------------------------------------------------------- *)
(* Fixed language-neutral test vectors (see README "Fixed test vectors").       *)
(* --------------------------------------------------------------------------- *)

let scalars_ok () : T.scalars =
  {
    i = -42L;
    u = 42L;
    n = -7L;
    f = 3.5;
    t = "h\xc3\xa9llo \xe4\xb8\x96\xe7\x95\x8c";
    raw = Bytes.of_string "\x01\x02\xf0\xff";
    flag = true;
    when_ = "2026-06-29T12:34:56Z";
    amount = "123.45";
  }

let collections_ok () : T.collections =
  {
    names = [ "a"; "b" ];
    at_least_one = [ 1L; 2L; 3L ];
    bounded = [ 10L; 20L ];
    exact_3 = [ 7L; 8L; 9L ];
    scores = [ ("x", 1L); ("y", 2L) ];
    color = T.Green;
    prio = T.V_2;
    who = T.Uint 4242L;
    pair = ("p", 5L);
    triple = ("t", Some 9L, Some true);
    extra = [ ("k", CB.Text "v") ];
  }

let constrained_ok () : T.constrained =
  {
    code = "PRD-AB12CD";
    qty = 10L;
    rate = "0.25";
    password = "hunter2hunter2";
    tags = [ "one"; "two" ];
  }

let constrained_bad () : T.constrained =
  { code = "bad"; qty = 0L; rate = "9.9"; password = "x"; tags = [] }

let nested_ok () : T.nested =
  { inner = scalars_ok (); maybe = Some (constrained_ok ()); many = [ scalars_ok () ] }

(* The cross-language wire contract pins record key order but NOT inner-map
   (`{text => _}`) entry order: a peer echoing a CBOR map may iterate its own
   hash map in any order. So a map field is compared structurally (by sorted
   key), exactly as the Rust reference's `HashMap` equality does. *)
let sort_assoc l = List.sort (fun (k1, _) (k2, _) -> compare k1 k2) l

let collections_eq (a : T.collections) (b : T.collections) : bool =
  a.names = b.names
  && a.at_least_one = b.at_least_one
  && a.bounded = b.bounded
  && a.exact_3 = b.exact_3
  && sort_assoc a.scores = sort_assoc b.scores
  && a.color = b.color
  && a.prio = b.prio
  && a.who = b.who
  && a.pair = b.pair
  && a.triple = b.triple
  && sort_assoc a.extra = sort_assoc b.extra

(* The harness owns validation (the generator emits none for OCaml): accept the
   valid vector and reject the invalid one so the error-variant op exercises the
   ServiceError failure path. *)
let valid_constrained (c : T.constrained) : bool =
  let code_ok =
    let n = String.length c.code in
    n >= 10 && n <= 20
    && String.length c.code >= 4
    && String.sub c.code 0 4 = "PRD-"
    && String.for_all
         (fun ch ->
           (ch >= 'A' && ch <= 'Z') || (ch >= '0' && ch <= '9') || ch = '-')
         c.code
  in
  let rate_ok =
    match float_of_string_opt c.rate with Some f -> f >= 0.0 && f <= 1.0 | None -> false
  in
  code_ok
  && Int64.compare c.qty 1L >= 0
  && Int64.compare c.qty 1000L <= 0
  && rate_ok
  && String.length c.password >= 8
  && String.length c.password <= 128
  && (let n = List.length c.tags in n >= 1 && n <= 5)

(* --------------------------------------------------------------------------- *)
(* Result accumulation + one-line JSON (no JSON dependency).                    *)
(* --------------------------------------------------------------------------- *)

let json_str (s : string) : string =
  let buf = Buffer.create (String.length s + 2) in
  Buffer.add_char buf '"';
  String.iter
    (fun c ->
      match c with
      | '"' -> Buffer.add_string buf "\\\""
      | '\\' -> Buffer.add_string buf "\\\\"
      | '\n' -> Buffer.add_string buf "\\n"
      | '\t' -> Buffer.add_string buf "\\t"
      | '\r' -> Buffer.add_string buf "\\r"
      | c when Char.code c < 0x20 ->
        Buffer.add_string buf (Printf.sprintf "\\u%04x" (Char.code c))
      | c -> Buffer.add_char buf c)
    s;
  Buffer.add_char buf '"';
  Buffer.contents buf

type cases = { transport : string; mutable out : (string * bool * string) list }

let new_cases transport = { transport; out = [] }
let pass cs name = cs.out <- (name, true, "") :: cs.out
let fail cs name detail = cs.out <- (name, false, detail) :: cs.out
let check cs name cond detail = if cond then pass cs name else fail cs name detail

let emit cs =
  let buf = Buffer.create 256 in
  Buffer.add_string buf
    (Printf.sprintf "{\"lang\":\"ocaml\",\"transport\":\"%s\",\"cases\":[" cs.transport);
  List.iteri
    (fun i (name, ok, detail) ->
      if i > 0 then Buffer.add_char buf ',';
      Buffer.add_string buf
        (Printf.sprintf "{\"name\":\"%s\",\"ok\":%b,\"detail\":%s}" name ok
           (json_str detail)))
    (List.rev cs.out);
  Buffer.add_string buf "]}";
  print_string (Buffer.contents buf);
  print_newline ()

(* --------------------------------------------------------------------------- *)
(* Stream-socket plumbing (RPC + Events share SOCK_STREAM).                     *)
(* --------------------------------------------------------------------------- *)

let carrier_of_fd (fd : Unix.file_descr) : Carrier.frame_carrier =
  let ic = Unix.in_channel_of_descr fd in
  let oc = Unix.out_channel_of_descr fd in
  Carrier.stream_carrier ic oc

let listen_stream port =
  let sock = Unix.socket Unix.PF_INET Unix.SOCK_STREAM 0 in
  Unix.setsockopt sock Unix.SO_REUSEADDR true;
  Unix.bind sock (Unix.ADDR_INET (Unix.inet_addr_loopback, port));
  Unix.listen sock 16;
  sock

let connect_stream_retry port : Unix.file_descr =
  let addr = Unix.ADDR_INET (Unix.inet_addr_loopback, port) in
  let rec attempt n =
    let fd = Unix.socket Unix.PF_INET Unix.SOCK_STREAM 0 in
    match Unix.connect fd addr with
    | () -> fd
    | exception _ ->
      (try Unix.close fd with _ -> ());
      if n <= 0 then failwith "connect stream"
      else (
        Unix.sleepf 0.015;
        attempt (n - 1))
  in
  attempt 200

(* --------------------------------------------------------------------------- *)
(* RPC                                                                          *)
(* --------------------------------------------------------------------------- *)

let rpc_server listener =
  let handler (req : Rpc.request) : Rpc.handler_outcome =
    let malformed = Rpc.transport Conventions.status_malformed_envelope in
    match req.Rpc.op with
    | "EchoScalars" -> (
      match C.decode_scalars_bytes req.Rpc.payload with
      | v -> Rpc.reply "Scalars" (C.encode_scalars_bytes v)
      | exception Failure m -> malformed m)
    | "EchoCollections" -> (
      match C.decode_collections_bytes req.Rpc.payload with
      | v -> Rpc.reply "Collections" (C.encode_collections_bytes v)
      | exception Failure m -> malformed m)
    | "EchoNested" -> (
      match C.decode_nested_bytes req.Rpc.payload with
      | v ->
        let r : T.echo_nested_result = { ok = true; echo = v } in
        Rpc.reply "EchoNestedResult" (C.encode_echo_nested_result_bytes r)
      | exception Failure m -> malformed m)
    | "ValidateConstrained" -> (
      match C.decode_constrained_bytes req.Rpc.payload with
      | v ->
        if valid_constrained v then Rpc.reply "Constrained" (C.encode_constrained_bytes v)
        else
          let e : T.service_error = { code = 422L; message = "validation failed" } in
          Rpc.reply "ServiceError" (C.encode_service_error_bytes e)
      | exception Failure m -> malformed m)
    | other ->
      Rpc.transport Conventions.status_unknown_service_or_op ("unknown op " ^ other)
  in
  let rec accept_loop () =
    match Unix.accept listener with
    | exception _ -> ()
    | fd, _ ->
      let carrier = carrier_of_fd fd in
      let server = Rpc.new_server carrier in
      let rec serve () =
        match Rpc.serve_one server handler with Ok true -> serve () | _ -> ()
      in
      serve ();
      (try Unix.close fd with _ -> ());
      accept_loop ()
  in
  accept_loop ()

(* The generated client's transport seam over a connected stream. A status-0 reply
   whose variant is `ServiceError` is the typed application error path; map it to an
   error string distinct from a transport failure. *)
let rpc_carrier_call (client : Rpc.client) ~service ~op ~payload =
  match Rpc.call client ~service ~op ~payload () with
  | Error e -> Error ("transport: " ^ Conventions.error_message e)
  | Ok (resp : Rpc.response) -> (
    match resp.Rpc.variant with
    | Some "ServiceError" -> Error "ServiceError"
    | _ -> Ok resp.Rpc.payload)

let rpc_client fd =
  let cs = new_cases "rpc" in
  let carrier = carrier_of_fd fd in
  let rpc = Rpc.new_client carrier false in
  let client =
    Interop_api.Client.make_client ~call:(fun ~service ~op ~payload ->
        rpc_carrier_call rpc ~service ~op ~payload)
  in
  (match Interop_api.Client.Interop.echo_scalars client (scalars_ok ()) with
   | Ok r -> check cs "echo-scalars/success" (r = scalars_ok ()) "mismatch"
   | Error e -> fail cs "echo-scalars/success" e);
  (match Interop_api.Client.Interop.echo_collections client (collections_ok ()) with
   | Ok r -> check cs "echo-collections/success" (collections_eq r (collections_ok ())) "mismatch"
   | Error e -> fail cs "echo-collections/success" e);
  (match Interop_api.Client.Interop.echo_nested client (nested_ok ()) with
   | Ok r -> check cs "echo-nested/success" (r.ok && r.echo = nested_ok ()) "mismatch"
   | Error e -> fail cs "echo-nested/success" e);
  (match Interop_api.Client.Interop.validate_constrained client (constrained_ok ()) with
   | Ok r -> check cs "validate-constrained/success" (r = constrained_ok ()) "mismatch"
   | Error e -> fail cs "validate-constrained/success" e);
  (match Interop_api.Client.Interop.validate_constrained client (constrained_bad ()) with
   | Ok _ -> fail cs "validate-constrained/failure" "server accepted invalid input"
   | Error "ServiceError" -> pass cs "validate-constrained/failure"
   | Error e -> fail cs "validate-constrained/failure" ("expected service error, got " ^ e));
  cs

(* --------------------------------------------------------------------------- *)
(* Events                                                                       *)
(* --------------------------------------------------------------------------- *)

let ticks = 3

let send_event (carrier : Carrier.frame_carrier) (ev : Events.event) =
  match Events.encode_event ev Events.Verbose with
  | Ok frame -> ignore (carrier.Carrier.send_frame frame)
  | Error _ -> ()

let on_tick_event (t : T.tick) : Events.event =
  Events.new_verbose_event (Some service) "OnTick" (C.encode_tick_bytes t)

let duplex_event (s : T.scalars) : Events.event =
  Events.new_verbose_event (Some service) "Duplex" (C.encode_scalars_bytes s)

let events_server listener =
  let rec accept_loop () =
    match Unix.accept listener with
    | exception _ -> ()
    | fd, _ ->
      let carrier = carrier_of_fd fd in
      (* Handshake: read $hello, reply $hello-ack (verbose). *)
      let handshaked =
        match carrier.Carrier.recv_frame () with
        | Ok (Some frame) -> (
          match Events.decode_event frame Events.Verbose with
          | Ok ev when ev.Events.event = Some Events.hello_name ->
            ignore (Events.decode_hello ev.Events.payload);
            let ack : Events.hello_ack =
              { v = 1L; ack_profile = "verbose"; session = None }
            in
            send_event carrier
              (Events.new_verbose_event None Events.hello_ack_name
                 (Events.encode_hello_ack ack));
            true
          | _ -> false)
        | _ -> false
      in
      if handshaked then begin
        (* Push N ticks (on-tick / server push). *)
        for seq = 0 to ticks - 1 do
          send_event carrier (on_tick_event { seq = Int64.of_int seq })
        done;
        (* React loop. *)
        let rec loop () =
          match carrier.Carrier.recv_frame () with
          | Ok (Some frame) -> (
            match Events.decode_event frame Events.Verbose with
            | Ok ev ->
              let name = match ev.Events.event with Some n -> n | None -> "" in
              if name = Events.ping_name then begin
                send_event carrier
                  (Events.new_verbose_event None Events.pong_name ev.Events.payload);
                loop ()
              end
              else if name = Events.close_name then ()
              else if name = "Duplex" then begin
                (match C.decode_scalars_bytes ev.Events.payload with
                 | s -> send_event carrier (duplex_event s)
                 | exception _ -> ());
                loop ()
              end
              else begin
                (* Unknown channel method: answer with $error. *)
                send_event carrier (Events.new_verbose_event None Events.error_name Bytes.empty);
                loop ()
              end
            | Error _ -> ())
          | _ -> ()
        in
        loop ()
      end;
      (try Unix.close fd with _ -> ());
      accept_loop ()
  in
  accept_loop ()

let events_client fd =
  let cs = new_cases "events" in
  let carrier = carrier_of_fd fd in
  (* Handshake. *)
  let hello : Events.hello =
    {
      versions = [ 1L ];
      profiles = [ "verbose" ];
      hello_service = Some service;
      hello_auth = None;
    }
  in
  send_event carrier (Events.new_verbose_event None Events.hello_name (Events.encode_hello hello));
  let ack_ok =
    match carrier.Carrier.recv_frame () with
    | Ok (Some f) -> (
      match Events.decode_event f Events.Verbose with
      | Ok ev -> ev.Events.event = Some Events.hello_ack_name
      | Error _ -> false)
    | _ -> false
  in
  check cs "events/handshake" ack_ok "no $hello-ack";

  (* on-tick: expect TICKS pushes with seq 0..TICKS. *)
  let tick_ok = ref true and detail = ref "" in
  for expect = 0 to ticks - 1 do
    match carrier.Carrier.recv_frame () with
    | Ok (Some f) -> (
      match Events.decode_event f Events.Verbose with
      | Ok ev when ev.Events.event = Some "OnTick" -> (
        match C.decode_tick_bytes ev.Events.payload with
        | t when t.T.seq = Int64.of_int expect -> ()
        | t ->
          tick_ok := false;
          detail := Printf.sprintf "tick seq %Ld != %d" t.T.seq expect
        | exception Failure m ->
          tick_ok := false;
          detail := "tick decode: " ^ m)
      | Ok ev ->
        tick_ok := false;
        detail :=
          Printf.sprintf "expected OnTick got %s"
            (match ev.Events.event with Some n -> n | None -> "<none>")
      | Error e ->
        tick_ok := false;
        detail := "decode: " ^ Conventions.error_message e)
    | _ ->
      tick_ok := false;
      detail := "stream closed during ticks"
  done;
  check cs "on-tick/success" !tick_ok !detail;

  (* duplex: send Scalars, expect echo. *)
  send_event carrier (duplex_event (scalars_ok ()));
  (match carrier.Carrier.recv_frame () with
   | Ok (Some f) -> (
     match Events.decode_event f Events.Verbose with
     | Ok ev when ev.Events.event = Some "Duplex" -> (
       match C.decode_scalars_bytes ev.Events.payload with
       | s -> check cs "duplex/success" (s = scalars_ok ()) "echo mismatch"
       | exception Failure m -> fail cs "duplex/success" m)
     | Ok ev ->
       fail cs "duplex/success"
         (match ev.Events.event with Some n -> n | None -> "<none>")
     | Error e -> fail cs "duplex/success" (Conventions.error_message e))
   | _ -> fail cs "duplex/success" "no echo");

  (* unknown-method: send a bogus channel method, expect $error. *)
  send_event carrier (Events.new_verbose_event (Some service) "Bogus" Bytes.empty);
  (match carrier.Carrier.recv_frame () with
   | Ok (Some f) ->
     let is_err =
       match Events.decode_event f Events.Verbose with
       | Ok ev -> ev.Events.event = Some Events.error_name
       | Error _ -> false
     in
     check cs "unknown-method/failure" is_err "expected $error"
   | _ -> fail cs "unknown-method/failure" "no $error");

  send_event carrier (Events.new_verbose_event None Events.close_name Bytes.empty);
  cs

(* --------------------------------------------------------------------------- *)
(* Datagrams                                                                    *)
(* --------------------------------------------------------------------------- *)

let op_echo_scalars = 0L
let op_echo_collections = 1L
let op_error_sentinel = 255L

let datagram_server sock =
  let buf = Bytes.create 65536 in
  let rec loop () =
    match Unix.recvfrom sock buf 0 (Bytes.length buf) [] with
    | exception _ -> loop ()
    | n, addr ->
      (match Datagrams.decode_datagram (Bytes.sub buf 0 n) with
       | Ok (dg : Datagrams.datagram) ->
         let reply =
           if dg.Datagrams.op_ord = op_echo_scalars then
             match C.decode_scalars_bytes dg.Datagrams.payload with
             | s -> Datagrams.new_datagram op_echo_scalars dg.Datagrams.seq (C.encode_scalars_bytes s)
             | exception _ -> Datagrams.new_datagram op_error_sentinel dg.Datagrams.seq Bytes.empty
           else if dg.Datagrams.op_ord = op_echo_collections then
             match C.decode_collections_bytes dg.Datagrams.payload with
             | c -> Datagrams.new_datagram op_echo_collections dg.Datagrams.seq (C.encode_collections_bytes c)
             | exception _ -> Datagrams.new_datagram op_error_sentinel dg.Datagrams.seq Bytes.empty
           else Datagrams.new_datagram op_error_sentinel dg.Datagrams.seq Bytes.empty
         in
         let bytes = Datagrams.encode_datagram reply in
         (try ignore (Unix.sendto sock bytes 0 (Bytes.length bytes) [] addr) with _ -> ())
       | Error _ -> ());
      loop ()
  in
  loop ()

let datagram_client sock =
  let cs = new_cases "datagrams" in
  Unix.setsockopt_float sock Unix.SO_RCVTIMEO 3.0;
  let buf = Bytes.create 65536 in
  let roundtrip op seq payload : (Datagrams.datagram, string) result =
    let dg = Datagrams.new_datagram op seq payload in
    let out = Datagrams.encode_datagram dg in
    match Unix.send sock out 0 (Bytes.length out) [] with
    | exception e -> Error (Printexc.to_string e)
    | _ -> (
      match Unix.recv sock buf 0 (Bytes.length buf) [] with
      | exception e -> Error (Printexc.to_string e)
      | n -> (
        match Datagrams.decode_datagram (Bytes.sub buf 0 n) with
        | Ok d -> Ok d
        | Error e -> Error (Conventions.error_message e)))
  in
  (match roundtrip op_echo_scalars 1L (C.encode_scalars_bytes (scalars_ok ())) with
   | Ok d when d.Datagrams.op_ord = op_echo_scalars -> (
     match C.decode_scalars_bytes d.Datagrams.payload with
     | s -> check cs "echo-scalars/success" (s = scalars_ok ()) "payload mismatch"
     | exception Failure m -> fail cs "echo-scalars/success" m)
   | Ok d -> fail cs "echo-scalars/success" (Printf.sprintf "op_ord %Ld" d.Datagrams.op_ord)
   | Error e -> fail cs "echo-scalars/success" e);
  (match roundtrip op_echo_collections 2L (C.encode_collections_bytes (collections_ok ())) with
   | Ok d when d.Datagrams.op_ord = op_echo_collections -> (
     match C.decode_collections_bytes d.Datagrams.payload with
     | c -> check cs "echo-collections/success" (collections_eq c (collections_ok ())) "payload mismatch"
     | exception Failure m -> fail cs "echo-collections/success" m)
   | Ok d -> fail cs "echo-collections/success" (Printf.sprintf "op_ord %Ld" d.Datagrams.op_ord)
   | Error e -> fail cs "echo-collections/success" e);
  (match roundtrip 99L 3L Bytes.empty with
   | Ok d ->
     check cs "bad-op-ord/failure"
       (d.Datagrams.op_ord = op_error_sentinel)
       (Printf.sprintf "expected error sentinel, got op_ord %Ld" d.Datagrams.op_ord)
   | Error e -> fail cs "bad-op-ord/failure" e);
  cs

(* --------------------------------------------------------------------------- *)
(* main                                                                         *)
(* --------------------------------------------------------------------------- *)

let () =
  if Array.length Sys.argv < 4 then begin
    prerr_endline "usage: main <server|client> <rpc|events|datagrams> <port>";
    exit 2
  end;
  let mode = Sys.argv.(1) and transport = Sys.argv.(2) in
  let port = int_of_string Sys.argv.(3) in
  match (mode, transport) with
  | "server", "datagrams" ->
    let sock = Unix.socket Unix.PF_INET Unix.SOCK_DGRAM 0 in
    Unix.setsockopt sock Unix.SO_REUSEADDR true;
    Unix.bind sock (Unix.ADDR_INET (Unix.inet_addr_loopback, port));
    print_endline "READY";
    flush stdout;
    datagram_server sock
  | "server", ("rpc" | "events") ->
    let listener = listen_stream port in
    print_endline "READY";
    flush stdout;
    if transport = "rpc" then rpc_server listener else events_server listener
  | "client", "datagrams" ->
    let sock = Unix.socket Unix.PF_INET Unix.SOCK_DGRAM 0 in
    Unix.bind sock (Unix.ADDR_INET (Unix.inet_addr_loopback, 0));
    Unix.connect sock (Unix.ADDR_INET (Unix.inet_addr_loopback, port));
    let cs = datagram_client sock in
    emit cs
  | "client", ("rpc" | "events") ->
    let fd = connect_stream_retry port in
    let cs = if transport = "rpc" then rpc_client fd else events_client fd in
    emit cs
  | _ ->
    prerr_endline "bad mode/transport";
    exit 2
