(* See rpc.mli for the contract. Ported from transports/go/rpc.go. *)

open Conventions

type request = {
  service : string;
  op : string;
  id : int64 option;
  payload : bytes;
  auth : string option;
}

let new_request service op payload =
  { service; op; id = None; payload; auth = None }

let request_with_id r id = { r with id = Some id }
let request_with_auth r auth = { r with auth = Some auth }

let encode_request (r : request) =
  let entries =
    [
      (Cbor.Text "v", Cbor.Uint version);
      (Cbor.Text "service", Cbor.Text r.service);
      (Cbor.Text "op", Cbor.Text r.op);
      (Cbor.Text "payload", tag24 r.payload);
    ]
  in
  let entries =
    match r.id with
    | Some id -> entries @ [ (Cbor.Text "id", Cbor.Uint id) ]
    | None -> entries
  in
  let entries =
    match r.auth with
    | Some a -> entries @ [ (Cbor.Text "auth", Cbor.Text a) ]
    | None -> entries
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_request b =
  let* v = decode_envelope b in
  let* ver = get_uint v "v" in
  let* () = check_version ver in
  match map_get v "payload" with
  | None -> Error (malformed "missing 'payload'")
  | Some p ->
      let* payload = untag24 p in
      let* service = get_text v "service" in
      let* op = get_text v "op" in
      Ok
        {
          service;
          op;
          id = get_uint_opt v "id";
          payload;
          auth = get_text_opt v "auth";
        }

type response = {
  id : int64 option;
  status : Conventions.status;
  variant : string option;
  error : string option;
  payload : bytes;
}

let new_response_ok variant payload =
  {
    id = None;
    status = status_ok;
    variant = Some variant;
    error = None;
    payload;
  }

let new_response_transport_error status message =
  {
    id = None;
    status;
    variant = None;
    error = Some message;
    payload = Bytes.create 0;
  }

let response_with_id r id = { r with id }

let encode_response (r : response) =
  let entries =
    [
      (Cbor.Text "v", Cbor.Uint version);
      (Cbor.Text "status", Cbor.Int (status_code r.status));
      (Cbor.Text "payload", tag24 r.payload);
    ]
  in
  let entries =
    match r.id with
    | Some id -> entries @ [ (Cbor.Text "id", Cbor.Uint id) ]
    | None -> entries
  in
  let entries =
    match r.variant with
    | Some v -> entries @ [ (Cbor.Text "variant", Cbor.Text v) ]
    | None -> entries
  in
  let entries =
    match r.error with
    | Some e -> entries @ [ (Cbor.Text "error", Cbor.Text e) ]
    | None -> entries
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_response b =
  let* v = decode_envelope b in
  let* ver = get_uint v "v" in
  let* () = check_version ver in
  (* payload is present but may wrap an empty byte string on a non-zero status. *)
  let* payload =
    match map_get v "payload" with
    | Some p -> untag24 p
    | None -> Ok (Bytes.create 0)
  in
  let* status = get_int v "status" in
  Ok
    {
      id = get_uint_opt v "id";
      status = status_from_code status;
      variant = get_text_opt v "variant";
      error = get_text_opt v "error";
      payload;
    }

let as_transport_error (r : response) =
  if status_is_ok r.status then None
  else
    let message = match r.error with Some m -> m | None -> "" in
    Some
      (Status_error
         { status = status_name r.status; code = status_code r.status; message })

type push = { service : string; event : string; payload : bytes }

let new_push service event payload = { service; event; payload }

let encode_push (p : push) =
  let entries =
    [
      (Cbor.Text "v", Cbor.Uint version);
      (Cbor.Text "service", Cbor.Text p.service);
      (Cbor.Text "event", Cbor.Text p.event);
      (Cbor.Text "payload", tag24 p.payload);
    ]
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_push b =
  let* v = decode_envelope b in
  let* ver = get_uint v "v" in
  let* () = check_version ver in
  match map_get v "payload" with
  | None -> Error (malformed "missing 'payload'")
  | Some p ->
      let* payload = untag24 p in
      let* service = get_text v "service" in
      let* event = get_text v "event" in
      Ok { service; event; payload }

type handler_outcome =
  | Reply of { variant : string; payload : bytes }
  | Transport_status of { status : Conventions.status; message : string }

let reply variant payload = Reply { variant; payload }
let transport status message = Transport_status { status; message }

type client = {
  carrier : Carrier.frame_carrier;
  mutable next_id : int64;
  multiplexed : bool;
}

let new_client carrier multiplexed = { carrier; next_id = 1L; multiplexed }

let call (client : client) ~service ~op ~payload ?auth () =
  let req = { (new_request service op payload) with auth } in
  let req =
    if client.multiplexed then (
      let id = client.next_id in
      client.next_id <- Int64.add id 1L;
      { req with id = Some id })
    else req
  in
  let frame = encode_request req in
  let* () = client.carrier.Carrier.send_frame frame in
  let* incoming = client.carrier.Carrier.recv_frame () in
  match incoming with
  | None -> Error (Carrier "connection closed before response")
  | Some inb -> (
      let* resp = decode_response inb in
      match as_transport_error resp with Some e -> Error e | None -> Ok resp)

type server = { carrier : Carrier.frame_carrier }

let new_server carrier = { carrier }

let serve_one (server : server) handler =
  let* frame = server.carrier.Carrier.recv_frame () in
  match frame with
  | None -> Ok false
  | Some f ->
      let resp =
        match decode_request f with
        | Ok req -> (
            let id = req.id in
            match handler req with
            | Reply { variant; payload } ->
                response_with_id (new_response_ok variant payload) id
            | Transport_status { status; message } ->
                response_with_id
                  (new_response_transport_error status message)
                  id)
        | Error e ->
            new_response_transport_error status_malformed_envelope
              (error_message e)
      in
      let out = encode_response resp in
      let* () = server.carrier.Carrier.send_frame out in
      Ok true
