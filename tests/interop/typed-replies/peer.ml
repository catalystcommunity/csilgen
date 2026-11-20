open Types

let handler : Services.Rp.handler =
  { decrypt_token = fun req ->
      if req.a = "limit" then Error (`Rp_error { code = Application_user_limit; message = "limit" })
      else Ok { b = "ok" } }

let call ~service ~op ~payload =
  let op = if (Codec.decode_req_bytes payload).a = "unknown" then "unknown" else op in
  match Services.Rp.route handler ~op ~payload with
  | Reply { variant; payload } ->
      if variant = "RpError" then assert (Char.code (Bytes.get payload 6) = 7);
      Ok (Codec.resolve_rpc_reply ~service ~op ~variant:(Some variant) payload)
  | Transport_error { status; _ } -> Error (Int64.to_string status)

let () =
  assert (rp_error_code_to_number Account_rate_limited = 42L);
  assert (rp_error_code_of_number 42L = Some Account_rate_limited);
  assert (rp_error_code_of_number 1L = None);
  let client = Client.make_client ~call in
  assert (Client.Rp.decrypt_token client { a = "ok" } = Ok { b = "ok" });
  assert (Client.Rp.decrypt_token client { a = "limit" } =
    Error (`Rp_error { code = Application_user_limit; message = "limit" }));
  assert (Client.Rp.decrypt_token client { a = "unknown" } = Error (`Transport "2"))
