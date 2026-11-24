(* Decode each wire vector as the shape its kind names; re-encode the rows that
   must decode and compare with the canonical bytes. *)

let of_hex h =
  Bytes.init (String.length h / 2) (fun i -> Char.chr (int_of_string ("0x" ^ String.sub h (i * 2) 2)))

let to_hex b =
  String.concat "" (List.init (Bytes.length b) (fun i -> Printf.sprintf "%02x" (Char.code (Bytes.get b i))))

(* Each kind gives a round trip from input bytes to re-encoded bytes. *)
let round_trip kind bytes =
  match kind with
  | "holder" -> Codec.encode_holder_bytes (Codec.decode_holder_bytes bytes)
  | "contact" -> Codec.encode_contact_bytes (Codec.decode_contact_bytes bytes)
  | "output" ->
    Codec.encode_lookup_read_counts_output_bytes (Codec.decode_lookup_read_counts_output_bytes bytes)
  | other -> failwith ("unknown kind " ^ other)

let () =
  let ic = open_in Sys.argv.(1) in
  let failures = ref 0 in
  let fail fmt = incr failures; Printf.printf fmt in
  (try
     while true do
       let line = input_line ic in
       if line <> "" && line.[0] <> '#' then
         match String.split_on_char '\t' line with
         | [ name; kind; expect; input_hex; canonical ] -> (
           match round_trip kind (of_hex input_hex) with
           | exception (Failure e | Invalid_argument e) ->
             if expect = "ok" then fail "FAIL %s: decode failed: %s\n" name e
           | encoded ->
             let encoded = to_hex encoded in
             if expect <> "ok" then fail "FAIL %s: decoded, but must fail (re-encoded %s)\n" name encoded
             else if encoded <> canonical then fail "FAIL %s: re-encoded %s, want %s\n" name encoded canonical)
         | _ -> fail "FAIL malformed vector line: %s\n" line
     done
   with End_of_file -> close_in ic);
  if !failures > 0 then begin
    Printf.printf "%d vector(s) failed\n" !failures;
    exit 1
  end;
  print_endline "all wire vectors pass"
