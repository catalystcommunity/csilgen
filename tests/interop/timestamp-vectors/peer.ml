(* Decode each vector as a `Stamp`; re-encode the rows that must decode.
   OCaml keeps a timestamp as canonical UTC text, which holds 9 fraction digits. *)

let digits = 9

let of_hex h =
  Bytes.init (String.length h / 2) (fun i -> Char.chr (int_of_string ("0x" ^ String.sub h (i * 2) 2)))

let to_hex b =
  String.concat "" (List.init (Bytes.length b) (fun i -> Printf.sprintf "%02x" (Char.code (Bytes.get b i))))

let () =
  let ic = open_in Sys.argv.(1) in
  let failures = ref 0 in
  let fail fmt = incr failures; Printf.printf fmt in
  (try
     while true do
       let line = input_line ic in
       if line <> "" && line.[0] <> '#' then
         match String.split_on_char '\t' line with
         | [ name; expect; precision; input_hex; canonical ] -> (
           let must_decode = expect = "ok" && int_of_string precision <= digits in
           match Codec.decode_stamp_bytes (of_hex input_hex) with
           | exception (Failure e | Invalid_argument e) ->
             if must_decode then fail "FAIL %s: decode failed: %s\n" name e
           | stamp ->
             let encoded = to_hex (Codec.encode_stamp_bytes stamp) in
             if not must_decode then fail "FAIL %s: decoded, but must fail (re-encoded %s)\n" name encoded
             else if encoded <> canonical then fail "FAIL %s: re-encoded %s, want %s\n" name encoded canonical)
         | _ -> fail "FAIL malformed vector line: %s\n" line
     done
   with End_of_file -> close_in ic);
  if !failures > 0 then begin
    Printf.printf "%d vector(s) failed\n" !failures;
    exit 1
  end;
  print_endline "all timestamp vectors pass"
