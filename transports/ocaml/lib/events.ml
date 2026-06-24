(* See events.mli for the contract. Ported from transports/go/events.go. *)

open Conventions

type profile = Verbose | Compact

let profile_name = function Verbose -> "verbose" | Compact -> "compact"

let parse_profile = function
  | "verbose" -> Some Verbose
  | "compact" -> Some Compact
  | _ -> None

type event = {
  service : string option;
  service_ord : int64 option;
  event : string option;
  op_ord : int64 option;
  id : int64 option;
  payload : bytes;
}

let new_verbose_event service event payload =
  {
    service;
    service_ord = None;
    event = Some event;
    op_ord = None;
    id = None;
    payload;
  }

let new_compact_event service_ord op_ord payload =
  {
    service = None;
    service_ord = Some service_ord;
    event = None;
    op_ord = Some op_ord;
    id = None;
    payload;
  }

let event_with_id e id = { e with id = Some id }

let encode_verbose (e : event) =
  match e.event with
  | None -> Error (malformed "verbose event missing 'event' name")
  | Some ev ->
      let entries =
        [
          (Cbor.Text "event", Cbor.Text ev);
          (Cbor.Text "payload", tag24 e.payload);
        ]
      in
      let entries =
        match e.service with
        | Some s -> entries @ [ (Cbor.Text "service", Cbor.Text s) ]
        | None -> entries
      in
      let entries =
        match e.id with
        | Some id -> entries @ [ (Cbor.Text "id", Cbor.Uint id) ]
        | None -> entries
      in
      Ok (Cbor.encode (Cbor.canon_map entries))

let encode_compact (e : event) =
  match (e.service_ord, e.op_ord) with
  | None, _ -> Error (malformed "compact event missing service ordinal")
  | _, None -> Error (malformed "compact event missing op ordinal")
  | Some so, Some oo ->
      let head = [ Cbor.Uint so; Cbor.Uint oo ] in
      let with_id =
        match e.id with Some id -> head @ [ Cbor.Uint id ] | None -> head
      in
      Ok (Cbor.encode (Cbor.Array (with_id @ [ tag24 e.payload ])))

let encode_event e = function
  | Verbose -> encode_verbose e
  | Compact -> encode_compact e

let decode_verbose b =
  let* v = decode_envelope b in
  match map_get v "payload" with
  | None -> Error (malformed "missing 'payload'")
  | Some p ->
      let* payload = untag24 p in
      let* ev = get_text v "event" in
      Ok
        {
          service = get_text_opt v "service";
          service_ord = None;
          event = Some ev;
          op_ord = None;
          id = get_uint_opt v "id";
          payload;
        }

let ord_of v =
  match as_u64 v with
  | Some n -> Ok n
  | None -> Error (malformed "ordinal is not an integer")

let decode_compact b =
  let* v = decode_envelope b in
  match v with
  | Cbor.Array arr -> (
      match arr with
      | [ so; oo; pl ] ->
          let* service_ord = ord_of so in
          let* op_ord = ord_of oo in
          let* payload = untag24 pl in
          Ok
            {
              service = None;
              service_ord = Some service_ord;
              event = None;
              op_ord = Some op_ord;
              id = None;
              payload;
            }
      | [ so; oo; idv; pl ] ->
          let* service_ord = ord_of so in
          let* op_ord = ord_of oo in
          let* id = ord_of idv in
          let* payload = untag24 pl in
          Ok
            {
              service = None;
              service_ord = Some service_ord;
              event = None;
              op_ord = Some op_ord;
              id = Some id;
              payload;
            }
      | _ ->
          Error
            (malformed "compact event array has %d elements, expected 3 or 4"
               (List.length arr)))
  | _ -> Error (malformed "compact event is not an array")

let decode_event b = function
  | Verbose -> decode_verbose b
  | Compact -> decode_compact b

let control_hello = 0L
let control_hello_ack = 1L
let control_ping = 2L
let control_pong = 3L
let control_close = 4L
let control_error = 5L
let hello_name = "$hello"
let hello_ack_name = "$hello-ack"
let ping_name = "$ping"
let pong_name = "$pong"
let close_name = "$close"
let error_name = "$error"

type hello = {
  versions : int64 list;
  profiles : string list;
  hello_service : string option;
  hello_auth : string option;
}

let encode_hello (h : hello) =
  let versions = List.map (fun v -> Cbor.Uint v) h.versions in
  let profiles = List.map (fun p -> Cbor.Text p) h.profiles in
  let entries =
    [
      (Cbor.Text "versions", Cbor.Array versions);
      (Cbor.Text "profiles", Cbor.Array profiles);
    ]
  in
  let entries =
    match h.hello_service with
    | Some s -> entries @ [ (Cbor.Text "service", Cbor.Text s) ]
    | None -> entries
  in
  let entries =
    match h.hello_auth with
    | Some a -> entries @ [ (Cbor.Text "auth", Cbor.Text a) ]
    | None -> entries
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_hello b =
  let* v = decode_envelope b in
  let* versions =
    match map_get v "versions" with
    | Some (Cbor.Array xs) -> Ok (List.filter_map as_u64 xs)
    | _ -> Error (malformed "hello missing 'versions'")
  in
  let* profiles =
    match map_get v "profiles" with
    | Some (Cbor.Array xs) ->
        Ok (List.filter_map (function Cbor.Text t -> Some t | _ -> None) xs)
    | _ -> Error (malformed "hello missing 'profiles'")
  in
  Ok
    {
      versions;
      profiles;
      hello_service = get_text_opt v "service";
      hello_auth = get_text_opt v "auth";
    }

let negotiate (h : hello) supported =
  if not (List.exists (fun v -> Int64.equal v version) h.versions) then None
  else
    let rec find = function
      | [] -> None
      | offered :: rest -> (
          match parse_profile offered with
          | Some p when List.mem p supported -> Some (version, p)
          | _ -> find rest)
    in
    find h.profiles

type hello_ack = { v : int64; ack_profile : string; session : string option }

let encode_hello_ack (a : hello_ack) =
  let entries =
    [
      (Cbor.Text "v", Cbor.Uint a.v);
      (Cbor.Text "profile", Cbor.Text a.ack_profile);
    ]
  in
  let entries =
    match a.session with
    | Some s -> entries @ [ (Cbor.Text "session", Cbor.Text s) ]
    | None -> entries
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_hello_ack b =
  let* v = decode_envelope b in
  let* ver = get_uint v "v" in
  let* prof = get_text v "profile" in
  Ok { v = ver; ack_profile = prof; session = get_text_opt v "session" }

type heartbeat = { nonce : int64; at : int64 option }

let encode_heartbeat (h : heartbeat) =
  let entries = [ (Cbor.Text "nonce", Cbor.Uint h.nonce) ] in
  let entries =
    match h.at with
    | Some a -> entries @ [ (Cbor.Text "at", Cbor.Uint a) ]
    | None -> entries
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_heartbeat b =
  let* v = decode_envelope b in
  let* nonce = get_uint v "nonce" in
  Ok { nonce; at = get_uint_opt v "at" }

type close = { status : Conventions.status; reason : string option }

let encode_close (c : close) =
  let entries = [ (Cbor.Text "status", Cbor.Int (status_code c.status)) ] in
  let entries =
    match c.reason with
    | Some r -> entries @ [ (Cbor.Text "reason", Cbor.Text r) ]
    | None -> entries
  in
  Cbor.encode (Cbor.canon_map entries)

let decode_close b =
  let* v = decode_envelope b in
  let* status = get_int v "status" in
  Ok { status = status_from_code status; reason = get_text_opt v "reason" }
