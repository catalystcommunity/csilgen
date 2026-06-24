(* A tiny, dependency-free JSON reader used only by the conformance harness, so the
   library and its test target stay installable with nothing but the OCaml compiler
   (no yojson, no opam deps). It supports exactly what the conformance vector files
   contain: objects, arrays, strings, numbers, booleans, and null. *)

type t =
  | Null
  | Bool of bool
  | Num of float
  | Str of string
  | Arr of t list
  | Obj of (string * t) list

exception Parse_error of string

let parse (s : string) : t =
  let n = String.length s in
  let pos = ref 0 in
  let error msg =
    raise (Parse_error (Printf.sprintf "%s at offset %d" msg !pos))
  in
  let peek () = if !pos < n then s.[!pos] else '\000' in
  let advance () = incr pos in
  let rec skip_ws () =
    if !pos < n then
      match s.[!pos] with
      | ' ' | '\t' | '\n' | '\r' ->
          advance ();
          skip_ws ()
      | _ -> ()
  in
  let expect c =
    if peek () = c then advance () else error (Printf.sprintf "expected '%c'" c)
  in
  let expect_lit lit =
    let l = String.length lit in
    if !pos + l <= n && String.sub s !pos l = lit then pos := !pos + l
    else error ("expected literal " ^ lit)
  in
  let parse_hex4 () =
    let h = ref 0 in
    for _ = 1 to 4 do
      let c = peek () in
      advance ();
      let d =
        match c with
        | '0' .. '9' -> Char.code c - Char.code '0'
        | 'a' .. 'f' -> Char.code c - Char.code 'a' + 10
        | 'A' .. 'F' -> Char.code c - Char.code 'A' + 10
        | _ -> error "bad \\u escape"
      in
      h := (!h * 16) + d
    done;
    !h
  in
  (* The vectors are ASCII, but handle the BMP correctly for any \u escape. *)
  let utf8_add buf cp =
    if cp < 0x80 then Buffer.add_char buf (Char.chr cp)
    else if cp < 0x800 then (
      Buffer.add_char buf (Char.chr (0xC0 lor (cp lsr 6)));
      Buffer.add_char buf (Char.chr (0x80 lor (cp land 0x3F))))
    else (
      Buffer.add_char buf (Char.chr (0xE0 lor (cp lsr 12)));
      Buffer.add_char buf (Char.chr (0x80 lor ((cp lsr 6) land 0x3F)));
      Buffer.add_char buf (Char.chr (0x80 lor (cp land 0x3F))))
  in
  let parse_string () =
    expect '"';
    let buf = Buffer.create 16 in
    let rec loop () =
      if !pos >= n then error "unterminated string";
      let c = s.[!pos] in
      advance ();
      match c with
      | '"' -> Buffer.contents buf
      | '\\' -> (
          let e = peek () in
          advance ();
          match e with
          | '"' ->
              Buffer.add_char buf '"';
              loop ()
          | '\\' ->
              Buffer.add_char buf '\\';
              loop ()
          | '/' ->
              Buffer.add_char buf '/';
              loop ()
          | 'n' ->
              Buffer.add_char buf '\n';
              loop ()
          | 't' ->
              Buffer.add_char buf '\t';
              loop ()
          | 'r' ->
              Buffer.add_char buf '\r';
              loop ()
          | 'b' ->
              Buffer.add_char buf '\b';
              loop ()
          | 'f' ->
              Buffer.add_char buf '\012';
              loop ()
          | 'u' ->
              utf8_add buf (parse_hex4 ());
              loop ()
          | _ -> error "bad escape")
      | _ ->
          Buffer.add_char buf c;
          loop ()
    in
    loop ()
  in
  let parse_number () =
    let start = !pos in
    let is_num_char = function
      | '0' .. '9' | '-' | '+' | '.' | 'e' | 'E' -> true
      | _ -> false
    in
    while !pos < n && is_num_char s.[!pos] do
      advance ()
    done;
    match float_of_string_opt (String.sub s start (!pos - start)) with
    | Some f -> Num f
    | None -> error "bad number"
  in
  let rec parse_value () =
    skip_ws ();
    match peek () with
    | '{' -> parse_object ()
    | '[' -> parse_array ()
    | '"' -> Str (parse_string ())
    | 't' ->
        expect_lit "true";
        Bool true
    | 'f' ->
        expect_lit "false";
        Bool false
    | 'n' ->
        expect_lit "null";
        Null
    | c when c = '-' || (c >= '0' && c <= '9') -> parse_number ()
    | _ -> error "unexpected character"
  and parse_object () =
    expect '{';
    skip_ws ();
    if peek () = '}' then (
      advance ();
      Obj [])
    else
      let rec loop acc =
        skip_ws ();
        let key = parse_string () in
        skip_ws ();
        expect ':';
        let v = parse_value () in
        skip_ws ();
        match peek () with
        | ',' ->
            advance ();
            loop ((key, v) :: acc)
        | '}' ->
            advance ();
            Obj (List.rev ((key, v) :: acc))
        | _ -> error "expected ',' or '}'"
      in
      loop []
  and parse_array () =
    expect '[';
    skip_ws ();
    if peek () = ']' then (
      advance ();
      Arr [])
    else
      let rec loop acc =
        let v = parse_value () in
        skip_ws ();
        match peek () with
        | ',' ->
            advance ();
            loop (v :: acc)
        | ']' ->
            advance ();
            Arr (List.rev (v :: acc))
        | _ -> error "expected ',' or ']'"
      in
      loop []
  in
  let v = parse_value () in
  skip_ws ();
  if !pos <> n then error "trailing content after JSON value";
  v

(* A field of an object, or [Null] when absent — so a missing key and an explicit
   null read the same, matching the "null means absent" vector convention. *)
let member key = function
  | Obj kvs -> ( match List.assoc_opt key kvs with Some v -> v | None -> Null)
  | _ -> Null

let to_string = function Str s -> s | _ -> failwith "json: not a string"

let to_int = function
  | Num f -> int_of_float f
  | _ -> failwith "json: not a number"

let to_int64 = function
  | Num f -> Int64.of_float f
  | _ -> failwith "json: not a number"

let to_list = function Arr xs -> xs | _ -> failwith "json: not an array"
let is_null = function Null -> true | _ -> false
