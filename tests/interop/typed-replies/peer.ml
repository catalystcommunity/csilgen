open Types

let handler : Services.Rp.handler =
  { decrypt_token = fun req ->
      if req.a = "limit" then Error (`Rp_error { code = Application_user_limit; message = "limit" })
      else Ok { b = "ok" } }

(* A fake transport: the generated client must pick the reply arm from the variant
   this seam returns, with no help from the resolver. *)
let call_reply ~service:_ ~op ~payload =
  match (Codec.decode_req_bytes payload).a with
  | "other" -> Ok (Some "Other", Codec.encode_resp_bytes { b = "other" })
  | "none" -> Ok (None, Codec.encode_resp_bytes { b = "none" })
  | a -> (
    let op = if a = "unknown" then "unknown" else op in
    match Services.Rp.route handler ~op ~payload with
    | Reply { variant; payload } ->
      if variant = "RpError" then assert (Char.code (Bytes.get payload 6) = 7);
      Ok (Some variant, payload)
    | Transport_error { status; _ } -> Error (Int64.to_string status))

let call ~service:_ ~op:_ ~payload:_ = failwith "multi-arm ops must use call_reply"

let () =
  assert (rp_error_code_to_number Account_rate_limited = 42L);
  assert (rp_error_code_of_number 42L = Some Account_rate_limited);
  assert (rp_error_code_of_number 1L = None);
  let client = Client.make_client ~call ~call_reply in
  assert (Client.Rp.decrypt_token client { a = "ok" } = Ok { b = "ok" });
  assert (Client.Rp.decrypt_token client { a = "limit" } =
    Error (`Rp_error { code = Application_user_limit; message = "limit" }));
  assert (Client.Rp.decrypt_token client { a = "unknown" } = Error (`Transport "2"));
  List.iter
    (fun a ->
      match Client.Rp.decrypt_token client { a } with
      | Error (`Decode _) -> ()
      | _ -> failwith ("accepted reply with " ^ a ^ " variant"))
    [ "other"; "none" ];
  print_endline "typed replies pass"
