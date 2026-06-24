(* See cbor.mli for the contract. This is a faithful port of transports/go/cbor.go
   and transports/python/.../cbor.py to the OCaml stdlib (Buffer, Bytes, Int64). *)

type t =
  | Uint of int64
  | Int of int64
  | Text of string
  | Bytes of bytes
  | Array of t list
  | Map of (t * t) list
  | Tag of int64 * t

exception Decode_error of string

(* Append the low [n] bytes of [arg] big-endian. [Bytes.set_int64_be] does the
   endian work; the high (8 - n) bytes are dropped by taking the tail. *)
let add_be buf (arg : int64) n =
  let scratch = Bytes.create 8 in
  Bytes.set_int64_be scratch 0 arg;
  Buffer.add_subbytes buf scratch (8 - n) n

(* Emit the initial byte (major type in the high three bits) plus the shortest-form
   argument for [arg], per deterministic encoding. The [arg < threshold] tests use
   unsigned Int64 semantics: a CBOR uint >= 2^63 is a negative [int64], so a signed
   compare would wrongly pick the 1-byte branch for it. *)
let write_head buf major (arg : int64) =
  let mt = major lsl 5 in
  let ult n = Int64.unsigned_compare arg n < 0 in
  if ult 24L then Buffer.add_char buf (Char.chr (mt lor Int64.to_int arg))
  else if ult 0x100L then (
    Buffer.add_char buf (Char.chr (mt lor 24));
    Buffer.add_char buf (Char.chr (Int64.to_int (Int64.logand arg 0xffL))))
  else if ult 0x1_0000L then (
    Buffer.add_char buf (Char.chr (mt lor 25));
    add_be buf arg 2)
  else if ult 0x1_0000_0000L then (
    Buffer.add_char buf (Char.chr (mt lor 26));
    add_be buf arg 4)
  else (
    Buffer.add_char buf (Char.chr (mt lor 27));
    add_be buf arg 8)

let rec encode_into buf v =
  match v with
  | Uint n -> write_head buf 0 n
  | Int n ->
      if Int64.compare n 0L >= 0 then write_head buf 0 n
      else
        (* CBOR negative ints encode -1-n; the argument is the magnitude minus one. *)
        write_head buf 1 (Int64.sub (Int64.neg n) 1L)
  | Text s ->
      write_head buf 3 (Int64.of_int (String.length s));
      Buffer.add_string buf s
  | Bytes b ->
      write_head buf 2 (Int64.of_int (Bytes.length b));
      Buffer.add_bytes buf b
  | Array items ->
      write_head buf 4 (Int64.of_int (List.length items));
      List.iter (encode_into buf) items
  | Map entries ->
      write_head buf 5 (Int64.of_int (List.length entries));
      List.iter
        (fun (k, v) ->
          encode_into buf k;
          encode_into buf v)
        entries
  | Tag (num, content) ->
      write_head buf 6 num;
      encode_into buf content

let encode v =
  let buf = Buffer.create 64 in
  encode_into buf v;
  Buffer.to_bytes buf

let canon_map entries =
  let keyed = List.map (fun (k, v) -> (encode k, k, v)) entries in
  let sorted =
    List.stable_sort (fun (a, _, _) (b, _, _) -> Bytes.compare a b) keyed
  in
  Map (List.map (fun (_, k, v) -> (k, v)) sorted)

let need b off n =
  if off + n > Bytes.length b then raise (Decode_error "truncated input")

(* Read the additional-information argument for the head byte at [off] whose low
   five bits are [low], returning the argument (as the unsigned bit pattern in an
   [int64]) and the total head length including the initial byte. *)
let read_arg b off low =
  if low < 24 then (Int64.of_int low, 1)
  else if low = 24 then (
    need b off 2;
    (Int64.of_int (Bytes.get_uint8 b (off + 1)), 2))
  else if low = 25 then (
    need b off 3;
    (Int64.of_int (Bytes.get_uint16_be b (off + 1)), 3))
  else if low = 26 then (
    need b off 5;
    let v = Bytes.get_int32_be b (off + 1) in
    (* get_int32_be is signed; mask to the unsigned 32-bit value. *)
    (Int64.logand (Int64.of_int32 v) 0xFFFF_FFFFL, 5))
  else if low = 27 then (
    need b off 9;
    (Bytes.get_int64_be b (off + 1), 9))
  else
    (* 28..31 (indefinite-length / reserved) are forbidden in CSIL envelopes. *)
    raise (Decode_error "indefinite or reserved additional info")

(* A CBOR length/count argument narrowed to a native [int] for slicing, rejecting
   a value that does not fit (would only arise from a hostile/huge frame). *)
let to_len (arg : int64) =
  if
    Int64.compare arg 0L < 0
    || Int64.unsigned_compare arg (Int64.of_int max_int) > 0
  then raise (Decode_error "length out of range")
  else Int64.to_int arg

let rec decode_at b off =
  if off >= Bytes.length b then raise (Decode_error "empty input");
  let ib = Bytes.get_uint8 b off in
  let major = ib lsr 5 in
  let low = ib land 0x1f in
  let arg, hlen = read_arg b off low in
  let next = off + hlen in
  match major with
  | 0 -> (Uint arg, next)
  | 1 ->
      (* A negative int encodes -1-arg; an [arg] above 2^63-1 (a negative [int64]
         here) names a value below Int64.min_int, which cannot be represented. *)
      if Int64.compare arg 0L < 0 then
        raise (Decode_error "negative integer out of int64 range");
      (Int (Int64.sub (Int64.neg arg) 1L), next)
  | 2 ->
      let len = to_len arg in
      need b next len;
      (Bytes (Bytes.sub b next len), next + len)
  | 3 ->
      let len = to_len arg in
      need b next len;
      (Text (Bytes.sub_string b next len), next + len)
  | 4 ->
      let len = to_len arg in
      let rec loop i off acc =
        if i = len then (List.rev acc, off)
        else
          let item, o = decode_at b off in
          loop (i + 1) o (item :: acc)
      in
      let items, off' = loop 0 next [] in
      (Array items, off')
  | 5 ->
      let len = to_len arg in
      let rec loop i off acc =
        if i = len then (List.rev acc, off)
        else
          let k, o1 = decode_at b off in
          let v, o2 = decode_at b o1 in
          loop (i + 1) o2 ((k, v) :: acc)
      in
      let entries, off' = loop 0 next [] in
      (Map entries, off')
  | 6 ->
      let content, o = decode_at b next in
      (Tag (arg, content), o)
  | _ -> raise (Decode_error "unsupported major type")

let decode b =
  try
    let v, n = decode_at b 0 in
    if n <> Bytes.length b then
      Error "CBOR decode error: trailing bytes after CBOR item"
    else Ok v
  with Decode_error m -> Error ("CBOR decode error: " ^ m)
