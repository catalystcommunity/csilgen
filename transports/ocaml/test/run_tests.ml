(* Entry point for the plain dune [(test)] stanza. Any failure raises and exits
   non-zero, which dune surfaces as a failed [runtest]; no test framework is used,
   keeping the test target dependency-free (stdlib only). *)

let () =
  Conformance.run ();
  Roundtrip.run ();
  print_endline "all OCaml transport tests passed"
