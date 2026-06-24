(* CSIL-RPC transport — request / response / push envelopes — see
   csil-rpc-transport.md. The codecs build canonical-CBOR maps so their bytes match
   the conformance vectors; the client/server pair drive a bring-your-own frame
   carrier synchronously (the host owns the I/O loop). *)

(* A CSIL-RPC request (client to server). [id] is absent on one-in-flight carriers
   and present on multiplexed ones; [auth] is the optional per-request credential. *)
type request = {
  service : string;
  op : string;
  id : int64 option;
  payload : bytes;
  auth : string option;
}

(* Build a request with no correlation id and no per-request auth. *)
val new_request : string -> string -> bytes -> request
val request_with_id : request -> int64 -> request
val request_with_auth : request -> string -> request
val encode_request : request -> bytes
val decode_request : bytes -> (request, Conventions.error) result

(* A CSIL-RPC response (server to client). [payload] is the opaque CBOR(output)
   bytes — present (possibly empty) regardless of status; [variant] names the
   output-choice arm on a status-0 reply; [error] carries a transport message. *)
type response = {
  id : int64 option;
  status : Conventions.status;
  variant : string option;
  error : string option;
  payload : bytes;
}

(* Build a successful (status-ok) typed reply naming its output-choice arm. *)
val new_response_ok : string -> bytes -> response

(* Build a transport-level failure carrying no typed payload. *)
val new_response_transport_error : Conventions.status -> string -> response
val response_with_id : response -> int64 option -> response
val encode_response : response -> bytes
val decode_response : bytes -> (response, Conventions.error) result

(* A [Status_error] for a non-ok response, or [None] when the response carries a
   typed reply (status 0). Lets callers separate transport failures from the
   application errors that ride inside a status-0 payload. *)
val as_transport_error : response -> Conventions.error option

(* A CSIL-RPC server push (server to client) for [<-] operations. *)
type push = { service : string; event : string; payload : bytes }

val new_push : string -> string -> bytes -> push
val encode_push : push -> bytes
val decode_push : bytes -> (push, Conventions.error) result

(* What a server handler returns for one request: a typed reply (variant name +
   encoded payload) on success, or a transport status on failure. *)
type handler_outcome =
  | Reply of { variant : string; payload : bytes }
  | Transport_status of { status : Conventions.status; message : string }

val reply : string -> bytes -> handler_outcome
val transport : Conventions.status -> string -> handler_outcome

(* A client over a frame carrier. It owns the envelope and a per-connection
   monotonic correlation id. *)
type client

(* Create a client. [multiplexed] true assigns a correlation id to every request
   (required on WS / pipelined streams); false omits it (one-in-flight carriers). *)
val new_client : Carrier.frame_carrier -> bool -> client

(* Invoke [service]/[op] with an encoded request payload, returning the decoded
   response. A non-zero transport status is surfaced as a [Status_error]. *)
val call :
  client ->
  service:string ->
  op:string ->
  payload:bytes ->
  ?auth:string ->
  unit ->
  (response, Conventions.error) result

(* A server over a frame carrier; the host supplies the request-to-outcome handler
   (the generated router is the natural implementation). *)
type server

val new_server : Carrier.frame_carrier -> server

(* Read one request, dispatch it through [handler], and write the response.
   Returns [Ok false] at a clean end of stream. *)
val serve_one :
  server -> (request -> handler_outcome) -> (bool, Conventions.error) result
