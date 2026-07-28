(* The configurable max-frame guard (conventions doc section 5): a host sets the
   limit up or down through the carrier's public API, the limit applies to reads and
   writes alike, an oversized inbound length is rejected before allocation, and an
   invalid limit fails at construction rather than on the first frame.

   The framing functions take channels, so each case drives a real temp-file channel
   pair rather than a mock: the guard is proven against the same I/O path a socket
   carrier uses. *)

open Csilgen_transport

let check name cond = if not cond then failwith ("max_frame: " ^ name)

(* Run [f] with an out_channel, then reopen the same file as an in_channel so a
   written frame can be read back through the real framing path. *)
let with_roundtrip_channels f =
  let path = Filename.temp_file "csil_max_frame" ".bin" in
  let oc = open_out_bin path in
  let result = f oc path in
  close_out oc;
  Sys.remove path;
  result

let is_frame_too_large = function
  | Error (Conventions.Frame_too_large _) -> true
  | _ -> false

let is_invalid_max_frame = function
  | Error (Conventions.Invalid_max_frame _) -> true
  | _ -> false

(* Write [body] with the given guard, then read it back with the same guard. *)
let write_then_read ?max_frame body =
  let path = Filename.temp_file "csil_max_frame" ".bin" in
  let oc = open_out_bin path in
  let max = match max_frame with Some m -> m | None -> Conventions.max_frame_default in
  let wrote = Carrier.write_length_prefixed oc body ~max in
  close_out oc;
  let read =
    match wrote with
    | Error _ -> Ok None
    | Ok () ->
        let ic = open_in_bin path in
        let r = Carrier.read_length_prefixed ic ~max in
        close_in ic;
        r
  in
  Sys.remove path;
  (wrote, read)

let default_accepts_below () =
  let body = Bytes.make 1024 '\xAB' in
  let wrote, read = write_then_read body in
  check "default accepts a frame below it" (wrote = Ok ());
  check "default reads the frame back" (read = Ok (Some body))

let default_rejects_above () =
  let body = Bytes.make (Conventions.max_frame_default + 1) '\x00' in
  let wrote, _ = write_then_read body in
  check "default rejects a frame above it" (is_frame_too_large wrote)

let larger_limit_accepts () =
  let body = Bytes.make (Conventions.max_frame_default + 1) '\x00' in
  let raised = Conventions.max_frame_default + 4096 in
  let wrote, read = write_then_read ~max_frame:raised body in
  check "raised limit accepts the frame" (wrote = Ok ());
  check "raised limit reads the frame back" (read = Ok (Some body))

let smaller_limit_rejects () =
  let body = Bytes.make 1024 '\xCD' in
  let wrote, _ = write_then_read ~max_frame:64 body in
  check "lowered limit rejects the frame" (is_frame_too_large wrote)

let oversized_length_rejected_before_allocation () =
  (* A prefix claiming ~4 GiB followed by no body: if the guard ran after the read
     this would allocate; it must fail on the prefix alone. *)
  let path = Filename.temp_file "csil_max_frame" ".bin" in
  let oc = open_out_bin path in
  output_bytes oc (Bytes.of_string "\xFF\xFF\xFF\xFF");
  close_out oc;
  let ic = open_in_bin path in
  let read = Carrier.read_length_prefixed ic ~max:4096 in
  close_in ic;
  Sys.remove path;
  check "oversized inbound length rejected" (is_frame_too_large read)

let invalid_limits_rejected () =
  List.iter
    (fun limit ->
      check
        (Printf.sprintf "limit %d must be rejected" limit)
        (is_invalid_max_frame (Conventions.validate_max_frame limit)))
    [ 0; -1; -4096; Conventions.max_frame_limit + 1 ];
  (* And at construction, through the carrier the host actually builds. *)
  with_roundtrip_channels (fun oc path ->
      let ic = open_in_bin path in
      let built =
        Carrier.stream_carrier_with_max_frame ~max_frame:0 ic oc
      in
      close_in ic;
      check "carrier construction rejects an invalid limit"
        (match built with
        | Error (Conventions.Invalid_max_frame _) -> true
        | _ -> false))

let boundary_limits_accepted () =
  List.iter
    (fun limit ->
      check
        (Printf.sprintf "limit %d must be accepted" limit)
        (Conventions.validate_max_frame limit = Ok limit))
    [ 1; Conventions.max_frame_default; Conventions.max_frame_limit ]

let run () =
  default_accepts_below ();
  default_rejects_above ();
  larger_limit_accepts ();
  smaller_limit_rejects ();
  oversized_length_rejected_before_allocation ();
  invalid_limits_rejected ();
  boundary_limits_accepted ();
  print_endline "max_frame: all guard checks passed"
