//! Unit tests for the OCaml generator. They exercise the private emitters
//! directly (asserting on the emitted OCaml source), mirroring the go generator's
//! inline `mod tests` style.

use super::*;
use csilgen_common::{
    CsilPosition, CsilRule, CsilServiceOperation, GeneratorConfig, GeneratorMetadata,
};
use std::collections::HashMap;

// --- identifier mapping -----------------------------------------------------

#[test]
fn ident_maps_snake_and_kebab() {
    assert_eq!(ocaml_ident("deposit-claim"), "deposit_claim");
    assert_eq!(ocaml_ident("current_state"), "current_state");
    assert_eq!(ocaml_ident("DepositClaim"), "deposit_claim");
}

#[test]
fn ident_escapes_keywords() {
    assert_eq!(ocaml_ident("type"), "type_");
    assert_eq!(ocaml_ident("method"), "method_");
    assert_eq!(ocaml_ident("end"), "end_");
    assert_eq!(ocaml_ident("val"), "val_");
}

#[test]
fn ident_fixes_leading_digit() {
    assert_eq!(ocaml_ident("3d"), "v_3_d");
}

#[test]
fn type_name_is_snake() {
    assert_eq!(
        ocaml_type_name("DepositClaimRequest"),
        "deposit_claim_request"
    );
}

#[test]
fn ctor_is_capitalized_snake() {
    assert_eq!(ocaml_ctor_name("not-found"), "Not_found");
    assert_eq!(
        ocaml_ctor_name("DepositClaimResponse"),
        "Deposit_claim_response"
    );
}

#[test]
fn module_is_capitalized() {
    assert_eq!(ocaml_module_name("attestation"), "Attestation");
    assert_eq!(
        ocaml_module_name("attestation-service"),
        "Attestation_service"
    );
}

// --- type mapping -----------------------------------------------------------

#[test]
fn builtin_type_mapping() {
    assert_eq!(map_type(&builtin("int")), "int64");
    assert_eq!(map_type(&builtin("uint")), "int64");
    assert_eq!(map_type(&builtin("text")), "string");
    assert_eq!(map_type(&builtin("tstr")), "string");
    assert_eq!(map_type(&builtin("bytes")), "bytes");
    assert_eq!(map_type(&builtin("bstr")), "bytes");
    assert_eq!(map_type(&builtin("bool")), "bool");
    assert_eq!(map_type(&builtin("float")), "float");
    assert_eq!(map_type(&builtin("timestamp")), "string");
    assert_eq!(map_type(&builtin("decimal")), "string");
    assert_eq!(map_type(&builtin("null")), "unit");
}

#[test]
fn reference_and_container_mapping() {
    assert_eq!(
        map_type(&CsilTypeExpression::Reference("User".into())),
        "user"
    );
    let arr = CsilTypeExpression::Array {
        element_type: Box::new(builtin("int")),
        occurrence: None,
    };
    assert_eq!(map_type(&arr), "int64 list");
    let map = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("int")),
        occurrence: None,
    };
    assert_eq!(map_type(&map), "(string * int64) list");
}

#[test]
fn optional_field_wraps_in_option() {
    let req = bare_entry("a", builtin("text"));
    assert_eq!(map_field_type(&req), "string");
    let opt = CsilGroupEntry {
        occurrence: Some(CsilOccurrence::Optional),
        ..bare_entry("b", builtin("text"))
    };
    assert_eq!(map_field_type(&opt), "string option");
    let opt_list = CsilGroupEntry {
        occurrence: Some(CsilOccurrence::Optional),
        ..bare_entry(
            "c",
            CsilTypeExpression::Array {
                element_type: Box::new(builtin("int")),
                occurrence: None,
            },
        )
    };
    assert_eq!(map_field_type(&opt_list), "int64 list option");
}

// --- records & variants -----------------------------------------------------

#[test]
fn record_emits_labelled_fields() {
    let group = CsilGroupExpression {
        entries: vec![
            bare_entry("subject", builtin("text")),
            bare_entry("weight", builtin("int")),
            CsilGroupEntry {
                occurrence: Some(CsilOccurrence::Optional),
                ..bare_entry("note", builtin("text"))
            },
        ],
    };
    let out = generate_record("DepositClaimRequest", &group);
    assert!(out.contains("type deposit_claim_request = {"));
    assert!(out.contains("subject : string;"));
    assert!(out.contains("weight : int64;"));
    assert!(out.contains("note : string option;"));
}

#[test]
fn empty_record_is_unit() {
    let group = CsilGroupExpression { entries: vec![] };
    assert_eq!(generate_record("Empty", &group), "type empty = unit");
}

#[test]
fn type_choice_emits_capitalized_constructors() {
    let choices = vec![
        CsilTypeExpression::Reference("DepositClaimResponse".into()),
        CsilTypeExpression::Reference("ServiceError".into()),
    ];
    let out = generate_type_choice("ClaimResult", &choices);
    assert!(out.contains("type claim_result ="));
    assert!(out.contains("| Deposit_claim_response of deposit_claim_response"));
    assert!(out.contains("| Service_error of service_error"));
}

#[test]
fn string_enum_emits_nullary_constructors() {
    let choices = vec![
        text_literal("active"),
        text_literal("suspended"),
        text_literal("closed"),
    ];
    let out = generate_type_choice("status", &choices);
    // A short enum collapses to the formatter's one-line form.
    assert_eq!(out, "type status = Active | Suspended | Closed");
    // A closed enum has no opaque catch-all and no duplicate constructors.
    assert!(!out.contains("Other"));
    assert!(!out.contains("Cbor.t"));
}

#[test]
fn open_string_enum_adds_other_arm() {
    // A leading `text` base means any string is valid, so unknown values ride an
    // `Other of string` arm rather than collapsing the type to an opaque blob.
    let choices = vec![
        builtin("text"),
        text_literal("created"),
        text_literal("updated"),
        text_literal("deleted"),
    ];
    let out = generate_type_choice("update_type", &choices);
    assert_eq!(
        out,
        "type update_type = Created | Updated | Deleted | Other of string"
    );
}

#[test]
fn success_type_drops_named_error_arm() {
    // The `Response / FooError` convention must reduce to the response, so the
    // generated client decodes the typed reply rather than an opaque CBOR value.
    let output = CsilTypeExpression::Choice(vec![
        CsilTypeExpression::Reference("account".into()),
        CsilTypeExpression::Reference("account_error".into()),
    ]);
    assert_eq!(map_type(&success_type(&output)), "account");
}

#[test]
fn type_decls_join_with_and() {
    let spec = CsilSpecSerialized {
        rules: vec![
            group_rule("A", vec![bare_entry("x", builtin("int"))]),
            group_rule("B", vec![bare_entry("y", builtin("text"))]),
        ],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    };
    let (ml, mli) = generate_types(&spec);
    assert!(ml.contains("type a = {"));
    assert!(ml.contains("and b = {"));
    // The interface mirrors the implementation's declarations.
    assert!(mli.contains("type a = {"));
    assert!(mli.contains("and b = {"));
}

#[test]
fn timestamp_and_decimal_get_wire_doc() {
    let group = CsilGroupExpression {
        entries: vec![
            bare_entry("created_at", builtin("timestamp")),
            bare_entry("amount", builtin("decimal")),
        ],
    };
    let out = generate_record("Money", &group);
    assert!(out.contains("CBOR tag 0 RFC3339"));
    assert!(out.contains("CBOR tag 4 exact decimal"));
}

// --- services: server -------------------------------------------------------

fn attestation_service() -> CsilServiceDefinition {
    CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "deposit-claim".into(),
            input_type: CsilTypeExpression::Reference("DepositClaimRequest".into()),
            output_type: CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Reference("DepositClaimResponse".into()),
                CsilTypeExpression::Reference("ServiceError".into()),
            ]),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: vec![],
            wire_id: Some(1),
        }],
        wire_id: Some(3),
    }
}

#[test]
fn server_module_emits_handler_and_routers() {
    let out = emit_service_module("Attestation", &attestation_service());
    assert!(out.contains("module Attestation = struct"));
    // Wire-id ordinals.
    assert!(out.contains("let service_wire_id = 3L"));
    assert!(out.contains("let op_deposit_claim_wire_id = 1L"));
    // Handler record with a bytes-taking field that returns an outcome.
    assert!(out.contains("type handler = {"));
    assert!(out.contains("deposit_claim : bytes -> outcome;"));
    // Verbose router dispatches by the verbatim wire op name (kebab preserved).
    assert!(out.contains("let route (h : handler) ~(op : string) ~(payload : bytes) ="));
    assert!(out.contains("| \"deposit-claim\" -> h.deposit_claim payload"));
    // Compact router dispatches by ordinal, emitted because the service has a wire id.
    assert!(out.contains("let route_compact (h : handler) ~(op_ord : int64)"));
    assert!(out.contains("| 1L -> h.deposit_claim payload"));
}

#[test]
fn compact_router_absent_without_wire_id() {
    let mut svc = attestation_service();
    svc.wire_id = None;
    svc.operations[0].wire_id = None;
    let out = emit_service_module("Attestation", &svc);
    assert!(!out.contains("route_compact"));
    assert!(!out.contains("service_wire_id"));
    // The verbose router is always present.
    assert!(out.contains("let route (h : handler)"));
}

#[test]
fn push_op_handler_takes_unit_payload() {
    let svc = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "subscribe".into(),
            input_type: builtin("null"),
            output_type: CsilTypeExpression::Reference("Event".into()),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: vec![],
            wire_id: None,
        }],
        wire_id: None,
    };
    let out = emit_service_module("Feed", &svc);
    assert!(out.contains("| \"subscribe\" -> ignore payload; h.subscribe Bytes.empty"));
}

// --- services: client -------------------------------------------------------

#[test]
fn client_module_emits_typed_calls() {
    let spec = CsilSpecSerialized {
        rules: vec![CsilRule {
            name: "Attestation".into(),
            rule_type: CsilRuleType::ServiceDef(attestation_service()),
            position: pos(),
            doc_comments: vec![],
        }],
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    };
    let out = generate_client(&spec);
    assert!(out.contains("type client = {"));
    assert!(out.contains("module Attestation = struct"));
    // The success type drops the ServiceError arm.
    assert!(out.contains("decode_response : bytes -> deposit_claim_response"));
    // The wire service/op strings are the CSIL names verbatim; the OCaml fn name is
    // snake_case.
    assert!(out.contains("let deposit_claim (c : client)"));
    assert!(out.contains("~op:\"deposit-claim\""));
    assert!(out.contains("~service:\"Attestation\""));
}

// --- surfaces ---------------------------------------------------------------

fn service_input(target: &str) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![
                group_rule(
                    "DepositClaimRequest",
                    vec![bare_entry("subject", builtin("text"))],
                ),
                CsilRule {
                    name: "Attestation".into(),
                    rule_type: CsilRuleType::ServiceDef(attestation_service()),
                    position: pos(),
                    doc_comments: vec![],
                },
            ],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: target.into(),
            output_dir: "/tmp".into(),
            options: HashMap::new(),
        },
        generator_metadata: meta(),
    }
}

#[test]
fn server_surface_emits_types_and_services() {
    let files = generate_ocaml(
        &service_input("ocaml").csil_spec,
        &service_input("ocaml").config,
    )
    .unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"types.ml"));
    assert!(paths.contains(&"types.mli"));
    assert!(paths.contains(&"services.ml"));
    assert!(!paths.contains(&"client.ml"));
}

#[test]
fn client_surface_emits_client_not_services() {
    let input = service_input("ocaml-client");
    let files = generate_ocaml(&input.csil_spec, &input.config).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"client.ml"));
    assert!(!paths.contains(&"services.ml"));
}

#[test]
fn typesonly_surface_emits_only_types() {
    let input = service_input("ocaml-typesonly");
    let files = generate_ocaml(&input.csil_spec, &input.config).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    // The codec (and its standalone `csil_cbor.ml` value model) ride alongside the
    // types (the records' (de)serializers), so a typesonly consumer gets usable types
    // without a service surface.
    assert_eq!(
        paths,
        vec!["types.ml", "types.mli", "csil_cbor.ml", "codec.ml"]
    );
}

#[test]
fn unknown_subtarget_errors() {
    let input = service_input("ocaml-bogus");
    assert!(generate_ocaml(&input.csil_spec, &input.config).is_err());
}

#[test]
fn metadata_targets_ocaml() {
    // The discovery layer keys on the metadata target; guard it stays "ocaml".
    assert!(resolve_surface("ocaml").is_ok());
    assert!(resolve_surface("ocaml-server").is_ok());
}

// --- fixtures ---------------------------------------------------------------

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

fn text_literal(value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(value.to_string()))
}

fn bare_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: vec![],
    }
}

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: vec![],
    }
}

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn meta() -> GeneratorMetadata {
    GeneratorMetadata {
        name: "ocaml".into(),
        version: "1.0.0".into(),
        description: String::new(),
        target: "ocaml".into(),
        capabilities: vec![],
        author: None,
        homepage: None,
    }
}

// --- codec ------------------------------------------------------------------

fn optional_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: vec![],
    }
}

/// A corndogs-shaped spec: text, bytes, an optional int, an assoc-list map, a list,
/// a nested record, and a service whose output is a `Res / ServiceError` choice.
fn corndogs_spec() -> CsilSpecSerialized {
    let text = || builtin("text");
    let task = group_rule(
        "Task",
        vec![
            bare_entry("uuid", text()),
            bare_entry("current_state", text()),
            bare_entry("payload", builtin("bytes")),
            optional_entry("priority", builtin("int")),
            bare_entry(
                "labels",
                CsilTypeExpression::Map {
                    key: Box::new(text()),
                    value: Box::new(builtin("int")),
                    occurrence: None,
                },
            ),
            bare_entry(
                "tags",
                CsilTypeExpression::Array {
                    element_type: Box::new(text()),
                    occurrence: None,
                },
            ),
        ],
    );
    let req = group_rule(
        "SubmitTaskRequest",
        vec![
            bare_entry("task", CsilTypeExpression::Reference("Task".into())),
            bare_entry("queue", text()),
        ],
    );
    let err = group_rule(
        "ServiceError",
        vec![
            bare_entry("code", builtin("int")),
            bare_entry("message", text()),
        ],
    );
    // A transparent map alias (`StringInt64Map = {* text => int}`) and a
    // map-of-record alias (`M = {* text => SomeRecord}`): a field typed as either
    // must encode/decode through its underlying shape, not the `failwith` stub.
    let string_int64_map = CsilRule {
        name: "StringInt64Map".into(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
            key: Box::new(text()),
            value: Box::new(builtin("int")),
            occurrence: None,
        }),
        position: pos(),
        doc_comments: vec![],
    };
    let some_record = group_rule("SomeRecord", vec![bare_entry("name", text())]);
    let m_alias = CsilRule {
        name: "M".into(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
            key: Box::new(text()),
            value: Box::new(CsilTypeExpression::Reference("SomeRecord".into())),
            occurrence: None,
        }),
        position: pos(),
        doc_comments: vec![],
    };
    let alias_holder = group_rule(
        "AliasHolder",
        vec![
            bare_entry(
                "counts",
                CsilTypeExpression::Reference("StringInt64Map".into()),
            ),
            bare_entry("by_name", CsilTypeExpression::Reference("M".into())),
        ],
    );
    let svc = CsilRule {
        name: "CorndogsService".into(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "submit-task".into(),
                input_type: CsilTypeExpression::Reference("SubmitTaskRequest".into()),
                output_type: CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Task".into()),
                    CsilTypeExpression::Reference("ServiceError".into()),
                ]),
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: vec![],
    };
    CsilSpecSerialized {
        rules: vec![
            task,
            req,
            err,
            string_int64_map,
            some_record,
            m_alias,
            alias_holder,
            svc,
        ],
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

#[test]
fn codec_emitted_with_typed_client() {
    let spec = corndogs_spec();
    let codec = generate_codec(&spec).expect("codec emitted");
    // The canonical-CBOR value model now lives in its own `csil_cbor.ml`; the codec
    // aliases it so the `any` core type and the codec share one value type.
    assert!(codec.contains("module Cbor = Csil_cbor"));
    assert!(codec.contains("let encode_task_bytes (v : task) : bytes"));
    assert!(codec.contains("let decode_submit_task_request_bytes"));
    // bytes -> Cbor.Bytes (major type 2); text -> Cbor.Text.
    assert!(codec.contains("(Cbor.Bytes v.payload)"));
    assert!(codec.contains("(Cbor.Text v.uuid)"));

    let client = generate_client(&spec);
    // Typed seam: a typed request in, a typed reply out, codec called internally.
    assert!(client.contains(
        "let submit_task (c : client) (req : submit_task_request) : (task, string) result"
    ));
    assert!(client.contains("Codec.encode_submit_task_request_bytes req"));
    assert!(client.contains("Codec.decode_task_bytes payload"));
}

/// Compile the generated OCaml and round-trip a typed request/response with dune.
/// Skips cleanly when dune/ocaml is not on PATH so the suite stays portable.
#[test]
fn codec_round_trips_through_dune() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = corndogs_spec();
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-ocaml-codec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("dune-project"), "(lang dune 3.0)\n").unwrap();
    std::fs::write(
        dir.join("dune"),
        "(executable (name test) (modules types csil_cbor codec client test))\n",
    )
    .unwrap();
    std::fs::write(dir.join("test.ml"), CODEC_DRIVER_OCAML).unwrap();

    let run = std::process::Command::new("dune")
        .arg("exec")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .arg("./test.exe")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ocaml round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const CODEC_DRIVER_OCAML: &str = r#"let () =
  let payload = Bytes.of_string "\xde\xad\xbe" in
  let task : Types.task =
    {
      uuid = "u-123";
      current_state = "PENDING";
      payload;
      priority = Some 7L;
      labels = [ ("a", 1L); ("b", 2L) ];
      tags = [ "x"; "y" ];
    }
  in
  let req : Types.submit_task_request = { task; queue = "default" } in
  (* direct codec round-trip through the nested record *)
  let bytes = Codec.encode_submit_task_request_bytes req in
  let back = Codec.decode_submit_task_request_bytes bytes in
  assert (back.task.uuid = "u-123");
  assert (back.task.current_state = "PENDING");
  assert (Bytes.equal back.task.payload payload);
  assert (back.task.priority = Some 7L);
  assert (back.task.labels = [ ("a", 1L); ("b", 2L) ]);
  assert (back.task.tags = [ "x"; "y" ]);
  assert (back.queue = "default");
  (* an absent optional must round-trip to None *)
  let req2 = { req with task = { task with priority = None } } in
  let back2 = Codec.decode_submit_task_request_bytes (Codec.encode_submit_task_request_bytes req2) in
  assert (back2.task.priority = None);
  (* typed client over a loopback carrier: decode the request, encode its task *)
  let call ~service:_ ~op:_ ~payload =
    let in_ = Codec.decode_submit_task_request_bytes payload in
    Ok (Codec.encode_task_bytes in_.task)
  in
  let c = Client.make_client ~call in
  (match Client.Corndogs_service.submit_task c req with
   | Ok t ->
     assert (t.uuid = "u-123");
     assert (Bytes.equal t.payload payload);
     assert (t.priority = Some 7L)
   | Error e -> failwith e);
  (* a named map alias and a map-of-record alias must survive the round-trip
     intact rather than being stubbed to an empty list / failwith *)
  let holder : Types.alias_holder =
    {
      counts = [ ("a", 1L); ("b", 2L) ];
      by_name = [ ("first", { Types.name = "alice" }); ("second", { Types.name = "bob" }) ];
    }
  in
  let holder_back =
    Codec.decode_alias_holder_bytes (Codec.encode_alias_holder_bytes holder)
  in
  assert (holder_back.counts = [ ("a", 1L); ("b", 2L) ]);
  assert (List.length holder_back.by_name = 2);
  assert ((List.assoc "first" holder_back.by_name).name = "alice");
  assert ((List.assoc "second" holder_back.by_name).name = "bob");
  print_string "ok\n"
"#;

/// A spec with a mixed-union type-choice (`OrderStatus = text / "pending" /
/// "confirmed" / "cancelled"`), matching `OrderStatus` in
/// examples/real-world-api/e-commerce-api.csil, referenced by a minimal `Order`
/// record so `generate_codec` emits the choice's own codec pair.
fn mixed_union_spec() -> CsilSpecSerialized {
    let status = CsilRule {
        name: "OrderStatus".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            builtin("text"),
            text_literal("pending"),
            text_literal("confirmed"),
            text_literal("cancelled"),
        ])),
        position: pos(),
        doc_comments: vec![],
    };
    let order = group_rule(
        "Order",
        vec![bare_entry(
            "status",
            CsilTypeExpression::Reference("OrderStatus".to_string()),
        )],
    );
    CsilSpecSerialized {
        rules: vec![status, order],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

/// Regression test for the bare-text wire-incompatibility bug: `OrderStatus` is
/// modeled internally as a proper ADT (`Pending | Confirmed | Cancelled | Other of
/// string`, an *open* string enum — see `classify_choice`), but the generated codec
/// used to ENCODE the bare CBOR text for every arm, never the tagged sum, which is
/// wire-incompatible with go/python/rust/c/csharp/kotlin for this shape (any choice
/// with a non-literal arm). Compiles and runs the generated codec with dune to prove
/// the actual wire bytes, not just the generated source text. Skips cleanly when
/// dune is not on PATH.
#[test]
fn mixed_union_encodes_tagged_sum_not_bare_text() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = mixed_union_spec();
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-ocaml-mixedunion-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("dune-project"), "(lang dune 3.0)\n").unwrap();
    std::fs::write(
        dir.join("dune"),
        "(executable (name test) (modules types csil_cbor codec test))\n",
    )
    .unwrap();
    std::fs::write(dir.join("test.ml"), MIXED_UNION_DRIVER_OCAML).unwrap();

    let run = std::process::Command::new("dune")
        .arg("exec")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .arg("./test.exe")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ocaml mixed-union check failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_UNION_DRIVER_OCAML: &str = r#"let () =
  (* A declared literal must win its own index over the general `text` arm, even
     though both live in the same `order_status` type. *)
  (match Codec.encode_order_status Types.Pending with
   | Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "pending" ] -> ()
   | _ -> failwith "pending: expected [1; \"pending\"]");
  (match Codec.encode_order_status Types.Confirmed with
   | Csil_cbor.Array [ Csil_cbor.Uint 2L; Csil_cbor.Text "confirmed" ] -> ()
   | _ -> failwith "confirmed: expected [2; \"confirmed\"]");
  (match Codec.encode_order_status Types.Cancelled with
   | Csil_cbor.Array [ Csil_cbor.Uint 3L; Csil_cbor.Text "cancelled" ] -> ()
   | _ -> failwith "cancelled: expected [3; \"cancelled\"]");
  (* A string matching no literal falls back to the general arm, index 0. *)
  (match Codec.encode_order_status (Types.Other "on-hold") with
   | Csil_cbor.Array [ Csil_cbor.Uint 0L; Csil_cbor.Text "on-hold" ] -> ()
   | _ -> failwith "on-hold: expected [0; \"on-hold\"]");
  (* Every declared index decodes back to its value, literal arms included. *)
  assert (
    Codec.decode_order_status (Codec.encode_order_status (Types.Other "on-hold"))
    = Types.Other "on-hold");
  assert (
    Codec.decode_order_status (Codec.encode_order_status Types.Pending) = Types.Pending);
  assert (
    Codec.decode_order_status (Codec.encode_order_status Types.Confirmed) = Types.Confirmed);
  assert (
    Codec.decode_order_status (Codec.encode_order_status Types.Cancelled) = Types.Cancelled);
  (* A full round-trip through a record field. *)
  let order : Types.order = { Types.status = Types.Pending } in
  let order_back = Codec.decode_order_bytes (Codec.encode_order_bytes order) in
  assert (order_back.status = Types.Pending);
  (* A literal arm still validates its payload rather than trusting the index: an
     index that claims "pending" but carries a different string must be rejected. *)
  (try
     let (_ : Types.order_status) =
       Codec.decode_order_status
         (Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "confirmed" ])
     in
     failwith "expected failure for literal/value mismatch"
   with Failure _ -> ());
  (* An out-of-range index is rejected too. *)
  (try
     let (_ : Types.order_status) =
       Codec.decode_order_status
         (Csil_cbor.Array [ Csil_cbor.Uint 99L; Csil_cbor.Text "pending" ])
     in
     failwith "expected failure for unknown variant index"
   with Failure _ -> ());
  print_string "ok\n"
"#;

// --- inline (anonymous) group/choice fields ----------------------------------

/// A field whose type is directly an inline choice (no named rule behind it) used
/// to collapse to the opaque `Csil_cbor.t` and its codec fell to a `failwith`
/// field-shape stub — found verifying
/// examples/real-world-api/e-commerce-api.csil's `APIError.error_type`. It is now
/// hoisted to its own `<Record>_<field>` type (`csilgen_common::hoist_inline_composites`),
/// exactly like the corresponding shape already was for a *named* choice. The
/// hoist pass runs once in `generate_ocaml` (see that function), so a test
/// exercising `generate_types`/`generate_codec` directly must hoist first, same as
/// every real caller.
#[test]
fn inline_choice_field_hoists_to_synthesized_type() {
    let spec = CsilSpecSerialized {
        rules: vec![group_rule(
            "APIError",
            vec![bare_entry(
                "error_type",
                CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    text_literal("not_found"),
                    text_literal("invalid_input"),
                ]),
            )],
        )],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    };
    let spec = csilgen_common::hoist_inline_composites(
        &spec,
        csilgen_common::HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
    let (types_ml, mli) = generate_types(&spec);
    assert!(types_ml.contains("api_error_error_type"));
    assert!(!types_ml.contains("Csil_cbor.t"));
    assert!(mli.contains("api_error_error_type"));

    let codec = generate_codec(&spec).expect("codec emitted");
    assert!(codec.contains("encode_api_error_error_type"));
    assert!(codec.contains("decode_api_error_error_type"));
    assert!(!codec.contains("no codec for"));
}

/// The same nominal-type problem, but for an inline `{ ... }` group rather than a
/// choice (found sweeping examples/multi-file/mixed/standalone.csil's
/// `StandaloneType.metadata` while verifying this fix) — `csilgen_common::hoist_inline_composites`
/// covers both shapes with the same mechanism.
#[test]
fn inline_group_field_hoists_to_synthesized_record() {
    let spec = CsilSpecSerialized {
        rules: vec![group_rule(
            "StandaloneType",
            vec![bare_entry(
                "metadata",
                CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![bare_entry("created", builtin("int"))],
                }),
            )],
        )],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    };
    let spec = csilgen_common::hoist_inline_composites(
        &spec,
        csilgen_common::HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
    let (types_ml, _) = generate_types(&spec);
    assert!(types_ml.contains("standalone_type_metadata"));
    assert!(!types_ml.contains("Csil_cbor.t"));

    let codec = generate_codec(&spec).expect("codec emitted");
    assert!(codec.contains("encode_standalone_type_metadata"));
    assert!(codec.contains("decode_standalone_type_metadata"));
    assert!(!codec.contains("no codec for"));
}

/// Regression test for the `.default`-suffixed literal choice arm bug found while
/// sweeping examples/build-integration/npm-project/api.csil: CSIL's grammar
/// attaches a trailing control operator to the immediately preceding primary
/// type, so the last arm of `text / "low" / "normal" / "high" .default "normal"`
/// parses as `Constrained { base_type: Literal("high"), .. }`, not a bare
/// `Literal`. Before `choice_arm_literal`, that arm fell out of the literal-enum
/// classification entirely (dropping it into the general union path with no
/// codec for its still-constrained payload) instead of becoming its own
/// `High` constructor alongside the other literals.
#[test]
fn choice_literal_arm_survives_trailing_default() {
    let choices = vec![
        builtin("text"),
        text_literal("low"),
        text_literal("normal"),
        CsilTypeExpression::Constrained {
            base_type: Box::new(text_literal("high")),
            constraints: vec![csilgen_common::CsilControlOperator::Default(
                CsilLiteralValue::Text("normal".to_string()),
            )],
        },
    ];
    let decl = generate_type_choice("Priority", &choices);
    assert_eq!(
        decl,
        "type priority = Low | Normal | High | Other of string"
    );

    let records = std::collections::HashSet::new();
    let choice_set = std::collections::HashSet::new();
    let aliases = std::collections::HashMap::new();
    let (enc, dec) = emit_choice_codec("Priority", &choices, &records, &choice_set, &aliases);
    assert!(!enc.contains("no codec for"));
    assert!(!dec.contains("no codec for"));
    assert!(enc.contains("High -> Cbor.Array [Cbor.int64 3L; Cbor.Text \"high\"]"));
}

/// A spec with an inline (unnamed) *mixed* choice field — a named-record arm plus
/// a literal arm, so it has a non-literal member and must code as the tagged sum
/// `[variant_index, value]`, matching the contract already proven for named
/// choices by `mixed_union_encodes_tagged_sum_not_bare_text`.
fn inline_mixed_choice_spec() -> CsilSpecSerialized {
    let person = group_rule("Person", vec![bare_entry("name", builtin("text"))]);
    let ticket = group_rule(
        "Ticket",
        vec![
            bare_entry("id", builtin("text")),
            optional_entry(
                "assignee",
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Person".to_string()),
                    text_literal("unassigned"),
                ]),
            ),
        ],
    );
    CsilSpecSerialized {
        rules: vec![person, ticket],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

/// Compiles and round-trips an inline mixed-choice field's synthesized codec with
/// dune, proving the actual wire bytes (tagged sum) rather than just the
/// generated source text. Skips cleanly when dune is not on PATH.
#[test]
fn inline_choice_mixed_field_round_trips_through_dune() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = inline_mixed_choice_spec();
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir =
        std::env::temp_dir().join(format!("csilgen-ocaml-inline-mixed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("dune-project"), "(lang dune 3.0)\n").unwrap();
    std::fs::write(
        dir.join("dune"),
        "(executable (name test) (modules types csil_cbor codec test))\n",
    )
    .unwrap();
    std::fs::write(dir.join("test.ml"), INLINE_MIXED_CHOICE_DRIVER_OCAML).unwrap();

    let run = std::process::Command::new("dune")
        .arg("exec")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .arg("./test.exe")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ocaml inline mixed-choice check failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const INLINE_MIXED_CHOICE_DRIVER_OCAML: &str = r#"let () =
  let alice : Types.person = { name = "Alice" } in
  let t1 : Types.ticket = { id = "T-1"; assignee = Some (Types.Person alice) } in
  let t1_back = Codec.decode_ticket_bytes (Codec.encode_ticket_bytes t1) in
  (match t1_back.assignee with
   | Some (Types.Person p) -> assert (p.name = "Alice")
   | _ -> failwith "expected Some (Person alice)");
  let t2 : Types.ticket = { id = "T-2"; assignee = Some Types.Unassigned } in
  let t2_back = Codec.decode_ticket_bytes (Codec.encode_ticket_bytes t2) in
  assert (t2_back.assignee = Some Types.Unassigned);
  let t3 : Types.ticket = { id = "T-3"; assignee = None } in
  let t3_back = Codec.decode_ticket_bytes (Codec.encode_ticket_bytes t3) in
  assert (t3_back.assignee = None);
  (* wire proof: the tagged sum, not a bare literal or an opaque blob, since the
     choice has a non-literal (Person) arm. *)
  (match Codec.encode_ticket_assignee Types.Unassigned with
   | Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "unassigned" ] -> ()
   | _ -> failwith "expected [1; \"unassigned\"]");
  (match Codec.encode_ticket_assignee (Types.Person alice) with
   | Csil_cbor.Array [ Csil_cbor.Uint 0L; Csil_cbor.Map _ ] -> ()
   | _ -> failwith "expected [0; <person map>]");
  print_string "ok\n"
"#;

/// A spec with an inline (unnamed) *all-literal* choice field — the closed
/// literal-enum shape, so it must code as the bare CBOR literal (its own
/// discriminant), never the tagged sum.
fn inline_all_literal_choice_spec() -> CsilSpecSerialized {
    let order = group_rule(
        "Order",
        vec![
            bare_entry("id", builtin("text")),
            bare_entry(
                "status",
                CsilTypeExpression::Choice(vec![
                    text_literal("pending"),
                    text_literal("shipped"),
                    text_literal("delivered"),
                ]),
            ),
        ],
    );
    CsilSpecSerialized {
        rules: vec![order],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

/// Compiles and round-trips an inline all-literal-choice field's synthesized
/// codec with dune, proving the actual wire bytes (bare literal, not a tagged
/// sum) rather than just the generated source text. Skips cleanly when dune is
/// not on PATH.
#[test]
fn inline_choice_all_literal_field_round_trips_through_dune() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = inline_all_literal_choice_spec();
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir = std::env::temp_dir().join(format!(
        "csilgen-ocaml-inline-literal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("dune-project"), "(lang dune 3.0)\n").unwrap();
    std::fs::write(
        dir.join("dune"),
        "(executable (name test) (modules types csil_cbor codec test))\n",
    )
    .unwrap();
    std::fs::write(dir.join("test.ml"), INLINE_ALL_LITERAL_CHOICE_DRIVER_OCAML).unwrap();

    let run = std::process::Command::new("dune")
        .arg("exec")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .arg("./test.exe")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ocaml inline all-literal-choice check failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const INLINE_ALL_LITERAL_CHOICE_DRIVER_OCAML: &str = r#"let () =
  let o1 : Types.order = { id = "O-1"; status = Types.Pending } in
  let o1_back = Codec.decode_order_bytes (Codec.encode_order_bytes o1) in
  assert (o1_back.status = Types.Pending);
  (* wire proof: an ALL-literal choice is the bare CBOR text, never a tagged
     sum. *)
  (match Codec.encode_order_status Types.Shipped with
   | Csil_cbor.Text "shipped" -> ()
   | _ -> failwith "expected bare CBOR text \"shipped\"");
  assert (Codec.decode_order_status (Csil_cbor.Text "delivered") = Types.Delivered);
  (* An unknown literal must be rejected, not silently accepted. *)
  (try
     let (_ : Types.order_status) = Codec.decode_order_status (Csil_cbor.Text "bogus") in
     failwith "expected failure for unknown literal"
   with Failure _ -> ());
  print_string "ok\n"
"#;

// --- generalized hoisting: array/map/tuple positions, nested -----------------

/// A "torture" spec exercising an inline choice (mixed, and OCaml's own
/// all-literal-must-still-be-named case) in every container position — an array
/// element, a map key, a map value, and a tuple slot — plus two shapes nested at
/// least two levels deep: an array of arrays, and a group nested inside an array
/// nested inside a field's own inline group. `csilgen_common::hoist_inline_composites`
/// must reach every one of these, not just a direct record field.
fn torture_spec() -> CsilSpecSerialized {
    let widget = group_rule("Widget", vec![bare_entry("name", builtin("text"))]);
    let mixed_choice = |literal: &str| {
        CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Reference("Widget".to_string()),
            text_literal(literal),
        ])
    };
    let tuple_entry = |value_type: CsilTypeExpression| CsilGroupEntry {
        key: None,
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: vec![],
    };
    let torture = group_rule(
        "Torture",
        vec![
            // Array element: a mixed inline choice (tagged-sum shape).
            bare_entry(
                "tags",
                CsilTypeExpression::Array {
                    element_type: Box::new(mixed_choice("unassigned")),
                    occurrence: None,
                },
            ),
            // Array element: an all-literal inline choice — OCaml still hoists this
            // (unlike TS/Java/C#/Kotlin) because a field/element type must be named;
            // see hoist.rs's module doc.
            bare_entry(
                "codes",
                CsilTypeExpression::Array {
                    element_type: Box::new(CsilTypeExpression::Choice(vec![
                        text_literal("red"),
                        text_literal("green"),
                        text_literal("blue"),
                    ])),
                    occurrence: None,
                },
            ),
            // Map value: a mixed inline choice.
            bare_entry(
                "labels",
                CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(mixed_choice("missing")),
                    occurrence: None,
                },
            ),
            // Map key: an all-literal inline choice.
            bare_entry(
                "flags",
                CsilTypeExpression::Map {
                    key: Box::new(CsilTypeExpression::Choice(vec![
                        text_literal("on"),
                        text_literal("off"),
                    ])),
                    value: Box::new(builtin("bool")),
                    occurrence: None,
                },
            ),
            // Tuple slot: a mixed inline choice at index 1 (index 0 stays a plain int).
            bare_entry(
                "coord",
                CsilTypeExpression::Tuple(CsilGroupExpression {
                    entries: vec![
                        tuple_entry(builtin("int")),
                        tuple_entry(mixed_choice("origin")),
                    ],
                }),
            ),
            // Nested two levels: an array of arrays of a mixed inline choice.
            bare_entry(
                "matrix",
                CsilTypeExpression::Array {
                    element_type: Box::new(CsilTypeExpression::Array {
                        element_type: Box::new(mixed_choice("empty_cell")),
                        occurrence: None,
                    }),
                    occurrence: None,
                },
            ),
            // Nested three levels: the field's own inline group, containing an array
            // of an inline group, containing a mixed inline choice.
            bare_entry(
                "nested",
                CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![bare_entry(
                        "inner",
                        CsilTypeExpression::Array {
                            element_type: Box::new(CsilTypeExpression::Group(
                                CsilGroupExpression {
                                    entries: vec![bare_entry("deep", mixed_choice("leaf"))],
                                },
                            )),
                            occurrence: None,
                        },
                    )],
                }),
            ),
        ],
    );
    CsilSpecSerialized {
        rules: vec![widget, torture],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

/// Every container position (array element, map key, map value, tuple slot) — and
/// every nesting depth among them, up to three levels for the field's own inline
/// group — resolves to its own named rule with a full codec, matching the
/// `<Owner>_<field>`/`_item`/`_key`/`_value`/`_<index>` naming
/// `wasm/csilgen-typescript-generator/src/hoist.rs` establishes. No position falls
/// through to the opaque `Csil_cbor.t` escape hatch or its `no codec for` stub.
#[test]
fn torture_spec_hoists_every_container_position_recursively() {
    let spec = csilgen_common::hoist_inline_composites(
        &torture_spec(),
        csilgen_common::HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
    let (types_ml, mli) = generate_types(&spec);
    for name in [
        "torture_tags_item",              // array element (mixed choice)
        "torture_codes_item",             // array element (all-literal choice)
        "torture_labels_value",           // map value (mixed choice)
        "torture_flags_key",              // map key (all-literal choice)
        "torture_coord_1",                // tuple slot (mixed choice)
        "torture_matrix_item_item",       // array-of-array, 2 levels deep
        "torture_nested",                 // the field's own inline group
        "torture_nested_inner_item",      // array-of-group inside it, 2 levels deep
        "torture_nested_inner_item_deep", // choice inside that group, 3 levels deep
    ] {
        assert!(
            types_ml.contains(name),
            "missing synthesized type `{name}`:\n{types_ml}"
        );
        assert!(mli.contains(name), "missing from .mli `{name}`:\n{mli}");
    }
    assert!(
        !types_ml.contains("Csil_cbor.t"),
        "an inline composite fell through to the opaque escape hatch:\n{types_ml}"
    );

    let codec = generate_codec(&spec).expect("codec emitted");
    for f in [
        "encode_torture_tags_item",
        "decode_torture_tags_item",
        "encode_torture_codes_item",
        "decode_torture_codes_item",
        "encode_torture_labels_value",
        "decode_torture_labels_value",
        "encode_torture_flags_key",
        "decode_torture_flags_key",
        "encode_torture_coord_1",
        "decode_torture_coord_1",
        "encode_torture_matrix_item_item",
        "decode_torture_matrix_item_item",
        "encode_torture_nested",
        "decode_torture_nested",
        "encode_torture_nested_inner_item",
        "decode_torture_nested_inner_item",
        "encode_torture_nested_inner_item_deep",
        "decode_torture_nested_inner_item_deep",
    ] {
        assert!(codec.contains(f), "missing hoisted codec `{f}`:\n{codec}");
    }
    assert!(
        !codec.contains("no codec for"),
        "unsupported field shape:\n{codec}"
    );

    // The mixed choices code as the tagged sum (a record arm plus a literal arm);
    // the all-literal ones (codes/flags) stay the closed bare-literal enum, exactly
    // like a named choice — hoisting doesn't change which shape a choice gets.
    assert!(codec.contains(
        "match v with Widget csil_x -> Cbor.Array [Cbor.int64 0L; (encode_widget csil_x)] | Unassigned -> Cbor.Array [Cbor.int64 1L; Cbor.Text \"unassigned\"]"
    ));
    assert!(codec.contains("match v with Red -> Cbor.Text \"red\" | Green -> Cbor.Text \"green\" | Blue -> Cbor.Text \"blue\""));
}

/// Compiles and round-trips the torture spec's codec with dune, proving actual wire
/// bytes for two of the generalized positions: an array-of-inline-choice element
/// (`tags`) and a map-value inline choice (`labels`). Both assert the tagged-sum
/// `[variant_index, value]` shape at the correct 0-based index, and that decode
/// validates a literal arm's payload against its expected text rather than trusting
/// the index alone (a corrupted payload at a literal arm's index must fail, not
/// silently decode as that literal). Skips cleanly when dune is not on PATH.
#[test]
fn torture_array_and_map_positions_round_trip_through_dune() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = torture_spec();
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-ocaml-torture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("dune-project"), "(lang dune 3.0)\n").unwrap();
    std::fs::write(
        dir.join("dune"),
        "(executable (name test) (modules types csil_cbor codec test))\n",
    )
    .unwrap();
    std::fs::write(dir.join("test.ml"), TORTURE_DRIVER_OCAML).unwrap();

    let run = std::process::Command::new("dune")
        .arg("exec")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .arg("./test.exe")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ocaml torture-spec check failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const TORTURE_DRIVER_OCAML: &str = r#"let () =
  let widget : Types.widget = { name = "spinner" } in
  let t : Types.torture = {
    tags = [ Types.Widget widget; Types.Unassigned ];
    codes = [ Types.Red; Types.Green; Types.Blue ];
    labels = [ ("k1", Types.Widget widget); ("k2", Types.Missing) ];
    flags = [ (Types.On, true); (Types.Off, false) ];
    coord = (7L, Types.Widget widget);
    matrix = [ [ Types.Widget widget; Types.Empty_cell ] ];
    nested = { inner = [ { deep = Types.Widget widget }; { deep = Types.Leaf } ] };
  } in
  let t_back = Codec.decode_torture_bytes (Codec.encode_torture_bytes t) in
  assert (t_back.tags = t.tags);
  assert (t_back.codes = t.codes);
  assert (t_back.labels = t.labels);
  assert (t_back.flags = t.flags);
  assert (t_back.coord = t.coord);
  assert (t_back.matrix = t.matrix);
  assert (t_back.nested = t.nested);

  (* Array-of-inline-choice element (`tags`): wire proof of the tagged sum at its
     declared 0-based index, for both the record arm and the literal arm. *)
  (match Codec.encode_torture_tags_item Types.Unassigned with
   | Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "unassigned" ] -> ()
   | _ -> failwith "expected [1; \"unassigned\"]");
  (match Codec.encode_torture_tags_item (Types.Widget widget) with
   | Csil_cbor.Array [ Csil_cbor.Uint 0L; Csil_cbor.Map _ ] -> ()
   | _ -> failwith "expected [0; <widget map>]");
  (* Literal validation: a payload that doesn't match the literal arm's own text at
     its own index must be rejected, not silently accepted as that arm. *)
  (try
     let (_ : Types.torture_tags_item) =
       Codec.decode_torture_tags_item
         (Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "wrong" ])
     in
     failwith "expected failure for a mismatched literal payload"
   with Failure _ -> ());

  (* Map-value inline choice (`labels`): the same tagged-sum + literal-validation
     proof, one container position over. *)
  (match Codec.encode_torture_labels_value Types.Missing with
   | Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "missing" ] -> ()
   | _ -> failwith "expected [1; \"missing\"]");
  (match Codec.encode_torture_labels_value (Types.Widget widget) with
   | Csil_cbor.Array [ Csil_cbor.Uint 0L; Csil_cbor.Map _ ] -> ()
   | _ -> failwith "expected [0; <widget map>]");
  (try
     let (_ : Types.torture_labels_value) =
       Codec.decode_torture_labels_value
         (Csil_cbor.Array [ Csil_cbor.Uint 1L; Csil_cbor.Text "wrong" ])
     in
     failwith "expected failure for a mismatched literal payload"
   with Failure _ -> ());

  print_string "ok\n"
"#;

// --- self-contained package mode --------------------------------------------

/// A `GeneratorConfig` for `target` whose `emit_packages` option is the given JSON
/// array (or absent when `None`), so a test can request — or not — a package.
fn package_config(target: &str, emit_packages: Option<serde_json::Value>) -> GeneratorConfig {
    let mut options = HashMap::new();
    if let Some(langs) = emit_packages {
        options.insert("emit_packages".to_string(), langs);
    }
    GeneratorConfig {
        target: target.into(),
        output_dir: "/tmp".into(),
        options,
    }
}

#[test]
fn package_files_absent_without_request() {
    // No `emit_packages`: the output is the unchanged flat surface (no scaffolding,
    // modules at the root rather than under `lib/`).
    let spec = corndogs_spec();
    let cfg = package_config("ocaml-client", None);
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"types.ml"));
    assert!(paths.contains(&"client.ml"));
    assert!(!paths.contains(&"dune-project"));
    assert!(!paths.iter().any(|p| p.ends_with(".opam")));
    assert!(!paths.contains(&"genquickstart.md"));
    assert!(!paths.iter().any(|p| p.starts_with("lib/")));
}

// --- package README + CSIL-RPC Quickstart -----------------------------------

/// `Name = { .. }` parses to `TypeDef(Group(..))` (not `GroupDef`), so build the
/// pingpong records that way to guard the README path against the real parser.
fn record_typedef(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
            entries,
        })),
        position: pos(),
        doc_comments: vec![],
    }
}

/// A minimal `ping: Ping -> Pong` service over two single-field records — the canonical
/// Quickstart shape, with records as `TypeDef(Group)` per the real parser.
fn pingpong_spec() -> CsilSpecSerialized {
    CsilSpecSerialized {
        rules: vec![
            record_typedef("Ping", vec![bare_entry("msg", builtin("text"))]),
            record_typedef("Pong", vec![bare_entry("msg", builtin("text"))]),
            CsilRule {
                name: "Echo".into(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "ping".into(),
                        input_type: CsilTypeExpression::Reference("Ping".into()),
                        output_type: CsilTypeExpression::Reference("Pong".into()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: pos(),
                        doc_comments: vec![],
                        wire_id: None,
                    }],
                    wire_id: None,
                }),
                position: pos(),
                doc_comments: vec![],
            },
        ],
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

/// An `Echo` service with a `->` op (`ping`) AND a record `<->` op (`watch`), both over
/// the single-field `Ping`/`Pong` records — so all three genquickstart sections render
/// their full library-based examples.
fn transports_spec() -> CsilSpecSerialized {
    CsilSpecSerialized {
        rules: vec![
            record_typedef("Ping", vec![bare_entry("msg", builtin("text"))]),
            record_typedef("Pong", vec![bare_entry("msg", builtin("text"))]),
            CsilRule {
                name: "Echo".into(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![
                        CsilServiceOperation {
                            name: "ping".into(),
                            input_type: CsilTypeExpression::Reference("Ping".into()),
                            output_type: CsilTypeExpression::Reference("Pong".into()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: pos(),
                            doc_comments: vec![],
                            wire_id: Some(7),
                        },
                        CsilServiceOperation {
                            name: "watch".into(),
                            input_type: CsilTypeExpression::Reference("Ping".into()),
                            output_type: CsilTypeExpression::Reference("Pong".into()),
                            direction: CsilServiceDirection::Bidirectional,
                            position: pos(),
                            doc_comments: vec![],
                            wire_id: Some(3),
                        },
                    ],
                    wire_id: Some(1),
                }),
                position: pos(),
                doc_comments: vec![],
            },
        ],
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

/// These assert the structural contract of the emitted genquickstart; the companion
/// `genquickstart_carriers_compile_against_lib` proves the same carriers actually
/// compile against the real package + transport library when a toolchain is present.
#[test]
fn genquickstart_intro_credits_transport_lib() {
    let spec = transports_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let c = &readme_of(&files);

    // The intro credits the library for the envelope/framing/lifecycle and names the
    // carrier-only contribution; the Consume section adds the transport dep with a
    // "not yet published" caveat.
    assert!(c.contains("`csilgen-transport` library owns the"));
    assert!(c.contains("*carrier*"));
    assert!(c.contains("(libraries echo csilgen-transport unix)"));
    assert!(c.contains("not yet published"));
}

#[test]
fn genquickstart_rpc_section_uses_lib_envelope_over_http() {
    let spec = transports_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let c = &readme_of(&files);

    assert!(c.contains("## CSIL-RPC (HTTP)"));
    // The carrier implements the generated transport seam (the `client.call` record).
    assert!(c.contains("Client.make_client ~call"));
    // The envelope is the library's RPC request/response — never hand-rolled.
    assert!(c.contains("Rpc.new_request service op payload"));
    assert!(c.contains("Rpc.encode_request req"));
    assert!(c.contains("Rpc.decode_response resp_bytes"));
    assert!(c.contains("Rpc.as_transport_error resp"));
    // It POSTs the canonical mount over the stdlib HTTP carrier.
    assert!(c.contains("/csil/v1/rpc"));
    assert!(c.contains("POST %s HTTP/1.1"));
    // The typed ServiceError application arm is surfaced distinctly.
    assert!(c.contains("\"ServiceError\""));
    // The typed client is constructed over the carrier with a base URL, and the example
    // call passes a generated sample request literal.
    assert!(c.contains("make_rpc_client \"http://localhost:5080\""));
    assert!(c.contains("Client.Echo.ping client req"));
    assert!(c.contains("let req : Types.ping = { msg = \"example\" }"));
}

#[test]
fn genquickstart_events_section_handshake_and_router_dispatch() {
    let spec = transports_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let c = &readme_of(&files);

    assert!(c.contains("## CSIL-Events (TLS)"));
    // A frame carrier built on the library's length-prefix framing over a stream.
    assert!(c.contains("Carrier.stream_carrier ic oc"));
    assert!(c.contains("TLS swap point"));
    // The $hello / $hello-ack handshake via the library control plane.
    assert!(c.contains("Events.encode_hello hello"));
    assert!(c.contains("Events.decode_hello_ack frame"));
    assert!(c.contains("Events.parse_profile ack.ack_profile"));
    // The $ping / $pong heartbeat answered with the library Heartbeat.
    assert!(c.contains("Events.ping_name"));
    assert!(c.contains("Events.encode_heartbeat hb"));
    // Dispatch into the generated server router + one outbound event via the codec.
    assert!(c.contains("Events.new_verbose_event (Some \"Echo\") \"watch\""));
    assert!(c.contains("Codec.encode_pong_bytes value"));
    assert!(c.contains("Services.Echo.route handler ~op:name ~payload:ev.payload"));
    // The handler record decodes the inbound channel payload with the generated codec.
    assert!(c.contains("Services.Echo.handler"));
    assert!(c.contains("Codec.decode_ping_bytes payload"));
}

#[test]
fn genquickstart_datagrams_section_send_and_late_response() {
    let spec = transports_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let c = &readme_of(&files);

    assert!(c.contains("## CSIL-Datagrams (UDP)"));
    // The UDP datagram carrier comes from the library, over a connected unix socket.
    assert!(c.contains("Udp.udp_datagram_carrier sock"));
    assert!(c.contains("Unix.SOCK_DGRAM"));
    // Encode the `->` request via the generated codec, wrap in the library's Datagram.
    assert!(c.contains("op_ord = 7L"));
    assert!(c.contains("let req : Types.ping = { msg = \"example\" }"));
    assert!(c.contains("Datagrams.new_datagram op_ord 0L (Codec.encode_ping_bytes req)"));
    assert!(c.contains("Datagrams.encode_datagram dg"));
    // The recv path decodes a late datagram into the RESPONSE type, with the caveat.
    assert!(c.contains("Datagrams.decode_datagram bytes"));
    assert!(c.contains("Codec.decode_pong_bytes dg.payload"));
    assert!(c.contains("MAY arrive later"));
    assert!(c.contains("synchronous response"));
}

#[test]
fn genquickstart_transports_option_selects_sections() {
    // A subset names only events: the RPC and Datagrams sections are suppressed.
    let spec = transports_spec();
    let mut cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    cfg.options.insert(
        "genquickstart_transports".to_string(),
        serde_json::json!(["events"]),
    );
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let c = &readme_of(&files);

    assert!(c.contains("## CSIL-Events (TLS)"));
    assert!(!c.contains("## CSIL-RPC (HTTP)"));
    assert!(!c.contains("## CSIL-Datagrams (UDP)"));
}

#[test]
fn genquickstart_no_channel_op_shows_handshake_note() {
    // The pingpong spec has no `<->` op: the Events section still shows the handshake +
    // heartbeat, with a note where the dispatch would go.
    let spec = pingpong_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let c = &readme_of(&files);

    assert!(c.contains("## CSIL-Events (TLS)"));
    assert!(c.contains("Events.encode_hello hello"));
    assert!(c.contains("no generated channel router"));
    // No handler/router dispatch is wired without a channel op.
    assert!(!c.contains("Services.Echo.route"));
}

/// The `genquickstart.md` body, or a panic naming the missing file.
fn readme_of(files: &[GeneratedFile]) -> String {
    files
        .iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md emitted at the package root")
        .content
        .clone()
}

#[test]
fn package_readme_absent_without_request() {
    let spec = pingpong_spec();
    let cfg = package_config("ocaml-client", None);
    let files = generate_ocaml(&spec, &cfg).unwrap();
    assert!(!files.iter().any(|f| f.path == "genquickstart.md"));
}

#[test]
fn package_readme_opt_out_suppresses_only_readme() {
    let spec = pingpong_spec();

    // By default the README is emitted alongside the package scaffolding.
    let default_cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let default_files = generate_ocaml(&spec, &default_cfg).unwrap();
    assert!(default_files.iter().any(|f| f.path == "genquickstart.md"));

    // An explicit `emit_readme: false` suppresses only the README; the dune/opam
    // scaffolding and the relocated sources are unchanged.
    let mut cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    cfg.options
        .insert("emit_readme".to_string(), serde_json::json!(false));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(!paths.contains(&"genquickstart.md"));
    assert!(paths.contains(&"dune-project"));
    assert!(paths.iter().any(|p| p.ends_with(".opam")));
    assert!(paths.contains(&"lib/types.ml"));
}

#[test]
fn package_files_absent_when_other_language_requested() {
    // A shared `emit_packages` naming only other languages must leave OCaml output
    // byte-identical to the no-option case.
    let spec = corndogs_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["go", "rust"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(!paths.contains(&"dune-project"));
    assert!(!paths.iter().any(|p| p.starts_with("lib/")));
}

#[test]
fn package_files_emitted_when_ocaml_requested() {
    let spec = corndogs_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["go", "ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    // The package name derives from the lone service (`CorndogsService`), sanitized
    // to a valid OCaml library name.
    assert!(paths.contains(&"dune-project"));
    assert!(paths.contains(&"corndogs_service.opam"));
    assert!(paths.contains(&"genquickstart.md"));
    assert!(paths.contains(&"lib/dune"));
    // The generated modules are relocated under `lib/`, none left at the root.
    assert!(paths.contains(&"lib/types.ml"));
    assert!(paths.contains(&"lib/codec.ml"));
    assert!(paths.contains(&"lib/client.ml"));
    assert!(!paths.contains(&"types.ml"));

    let lib_dune = files.iter().find(|f| f.path == "lib/dune").unwrap();
    assert!(lib_dune.content.contains("(name corndogs_service)"));
    assert!(lib_dune.content.contains("(public_name corndogs_service)"));
    let project = files.iter().find(|f| f.path == "dune-project").unwrap();
    assert!(project.content.contains("(lang dune 3.0)"));
    assert!(project.content.contains("(name corndogs_service)"));
}

#[test]
fn package_name_and_version_overrides_apply() {
    let spec = corndogs_spec();
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["ocaml"]));
    // A non-identifier override is sanitized to a valid OCaml lib name.
    options.insert("package_name".to_string(), serde_json::json!("My-Cool Lib"));
    options.insert("package_version".to_string(), serde_json::json!("2.3.4"));
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options,
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"my_cool_lib.opam"));
    assert!(paths.contains(&"lib/types.ml"));
    let opam = files.iter().find(|f| f.path == "my_cool_lib.opam").unwrap();
    assert!(opam.content.contains("version: \"2.3.4\""));
}

#[test]
fn sanitize_lib_name_produces_valid_names() {
    assert_eq!(sanitize_lib_name("CorndogsService"), "corndogs_service");
    assert_eq!(sanitize_lib_name("My-Cool Lib"), "my_cool_lib");
    // A leading digit is illegal for an OCaml lib name, so it gets a letter prefix.
    assert_eq!(sanitize_lib_name("3things"), "csil_3_things");
}

/// Generate an `ocaml-client` package into a temp dir and run `dune build` to prove
/// the emitted dune/opam scaffolding plus relocated modules form a buildable
/// package. Skips cleanly when dune is absent so the suite stays portable.
#[test]
fn package_builds_with_dune() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = corndogs_spec();
    let cfg = package_config("ocaml-client", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-ocaml-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &f.content).unwrap();
    }

    // The release profile mirrors the codec round-trip test: the generated source is
    // warning-clean there, so the build proves the package shape, not lint policy.
    let run = std::process::Command::new("dune")
        .arg("build")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dune build of generated package failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- genquickstart carrier verification -------------------------------------

/// Whether a tool answers `--version` on PATH (so the test skips cleanly off a CI
/// box without the OCaml toolchain rather than failing).
fn tool_on_path(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .output()
        .is_ok()
}

/// Pull every fenced ```ocaml block out of the genquickstart, in document order.
/// Each block is a complete, copy-paste carrier example, so each becomes its own
/// executable for the compile-check.
fn ocaml_code_blocks(md: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = md;
    while let Some(start) = rest.find("```ocaml\n") {
        let after = &rest[start + "```ocaml\n".len()..];
        let Some(end) = after.find("```") else { break };
        blocks.push(after[..end].to_string());
        rest = &after[end + 3..];
    }
    blocks
}

/// Compile-check the three transport carriers the genquickstart actually emits
/// against the REAL generated package and the reference `transports/ocaml` library.
///
/// The carriers use Unix sockets/TLS, so a `dune build` (typecheck + compile) is the
/// right bar — a full network run is not hermetic. The package is generated with both
/// surfaces (the package mode carries `Client` and `Services`), so all three sections
/// — CSIL-RPC (over `Client`), CSIL-Events (over `Services`), CSIL-Datagrams (over
/// `Client`) — resolve against one staged project. Skips cleanly when the OCaml
/// toolchain or the reference library is absent.
#[test]
fn genquickstart_carriers_compile_against_lib() {
    if !tool_on_path("dune") || !tool_on_path("ocaml") {
        eprintln!("skipping: no dune/ocaml on PATH");
        return;
    }
    // The reference transport library lives outside this crate, in the repo.
    let lib_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../transports/ocaml/lib")
        .canonicalize();
    let Ok(lib_src) = lib_src else {
        eprintln!("skipping: transports/ocaml/lib not found");
        return;
    };

    // A spec with a `->` op (drives RPC + Datagrams) AND a record `<->` op (drives the
    // Events router dispatch), so every section emits its full library-based example.
    let spec = transports_spec();
    let cfg = package_config("ocaml", Some(serde_json::json!(["ocaml"])));
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let md = readme_of(&files);
    let blocks = ocaml_code_blocks(&md);
    assert_eq!(
        blocks.len(),
        3,
        "expected RPC + Events + Datagrams carrier blocks, got {}",
        blocks.len()
    );

    let dir = std::env::temp_dir().join(format!(
        "csilgen-ocaml-genquickstart-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    // Stage the generated package as-is (its own dune-project is the build root); the
    // genquickstart.md doc itself is not a build input.
    for f in &files {
        if f.path == "genquickstart.md" {
            continue;
        }
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &f.content).unwrap();
    }

    // Vendor the reference transport library next to the package, as the library name
    // (`csilgen_transport`) the carriers `open Csilgen_transport`. A private
    // (public_name-less) copy needs no opam file; warnings are silenced so the
    // compile-check fails only on real type errors.
    let tdir = dir.join("transport");
    std::fs::create_dir_all(&tdir).unwrap();
    for entry in std::fs::read_dir(&lib_src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".ml") || name.ends_with(".mli") {
            std::fs::copy(entry.path(), tdir.join(name.as_ref())).unwrap();
        }
    }
    std::fs::write(
        tdir.join("dune"),
        "(library\n (name csilgen_transport)\n (libraries unix)\n (flags (:standard -w -a)))\n",
    )
    .unwrap();

    // Each carrier block becomes its own executable over the generated package
    // (`echo`) and the vendored transport library.
    for (i, block) in blocks.iter().enumerate() {
        let cdir = dir.join(format!("carrier_{i}"));
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::write(cdir.join("main.ml"), block).unwrap();
        std::fs::write(
            cdir.join("dune"),
            "(executable\n (name main)\n (libraries echo csilgen_transport unix)\n (flags (:standard -w -a)))\n",
        )
        .unwrap();
    }

    let run = std::process::Command::new("dune")
        .arg("build")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dune build of emitted genquickstart carriers failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- mixed-kind literal enum (the shared classify_choice migration fix) -----

fn int_literal(value: i64) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Integer(value))
}

/// A spec with an inline choice mixing a text literal and an integer literal
/// (`"a" / 1`). Per the shared `csilgen_common::classify_choice` contract this is
/// STILL an all-literal `Enum` (ANY kind mix), not a `Union` — before this
/// generator's migration to the shared classifier it required a uniform
/// text-only or int-only vocabulary to recognize an enum at all, so a choice
/// like this misclassified as a `Union` (a `[variant_index, value]` tagged sum)
/// instead of the bare-literal wire every other all-literal choice gets.
fn mixed_kind_literal_choice_spec() -> CsilSpecSerialized {
    let record = group_rule(
        "MixedEnumRecord",
        vec![bare_entry(
            "code",
            CsilTypeExpression::Choice(vec![text_literal("a"), int_literal(1)]),
        )],
    );
    CsilSpecSerialized {
        rules: vec![record],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

/// The generated `types.ml` declares a real OCaml variant (one nullary
/// constructor per literal) for a mixed-kind enum, not a fallback to the opaque
/// `Csil_cbor.t` a generic `Union` arm without a payload type would otherwise
/// need.
#[test]
fn mixed_kind_literal_choice_emits_nullary_variant() {
    let cfg = GeneratorConfig {
        target: "ocaml-typesonly".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&mixed_kind_literal_choice_spec(), &cfg).unwrap();
    let types_ml = &files.iter().find(|f| f.path == "types.ml").unwrap().content;
    assert!(
        types_ml.contains("mixed_enum_record_code = A | V_1"),
        "expected a nullary-constructor variant for the mixed-kind choice:\n{types_ml}"
    );
    assert!(
        !types_ml.contains("Csil_cbor.t"),
        "a mixed-kind literal enum must be a real variant, not an opaque payload:\n{types_ml}"
    );
}

/// Compiles and round-trips the mixed-kind enum's generated codec with dune,
/// proving the actual wire bytes: a bare CBOR literal (never a tagged-sum array)
/// for BOTH the text member and the integer member, and that decode rejects an
/// unknown literal. Skips cleanly when dune is not on PATH.
#[test]
fn mixed_kind_literal_choice_round_trips_through_dune() {
    let have = std::process::Command::new("dune")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dune on PATH");
        return;
    }
    let spec = mixed_kind_literal_choice_spec();
    let cfg = GeneratorConfig {
        target: "ocaml-client".into(),
        output_dir: "/tmp".into(),
        options: HashMap::new(),
    };
    let files = generate_ocaml(&spec, &cfg).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-ocaml-mixedenum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("dune-project"), "(lang dune 3.0)\n").unwrap();
    std::fs::write(
        dir.join("dune"),
        "(executable (name test) (modules types csil_cbor codec test))\n",
    )
    .unwrap();
    std::fs::write(dir.join("test.ml"), MIXED_KIND_LITERAL_CHOICE_DRIVER_OCAML).unwrap();

    let run = std::process::Command::new("dune")
        .arg("exec")
        .arg("--profile")
        .arg("release")
        .arg("--root")
        .arg(&dir)
        .arg("./test.exe")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ocaml mixed-kind literal choice check failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_KIND_LITERAL_CHOICE_DRIVER_OCAML: &str = r#"let () =
  (* Both kinds round-trip through the full record codec. *)
  let text_rec : Types.mixed_enum_record = { code = Types.A } in
  let text_back = Codec.decode_mixed_enum_record_bytes (Codec.encode_mixed_enum_record_bytes text_rec) in
  assert (text_back.code = Types.A);
  let int_rec : Types.mixed_enum_record = { code = Types.V_1 } in
  let int_back = Codec.decode_mixed_enum_record_bytes (Codec.encode_mixed_enum_record_bytes int_rec) in
  assert (int_back.code = Types.V_1);
  (* Wire proof: a mixed-kind ALL-literal choice is still the bare CBOR literal,
     never a tagged-sum array — the same enum contract every all-literal choice
     gets, regardless of the mix of literal kinds. *)
  (match Codec.encode_mixed_enum_record_code Types.A with
   | Csil_cbor.Text "a" -> ()
   | _ -> failwith "expected bare CBOR text \"a\"");
  (match Codec.encode_mixed_enum_record_code Types.V_1 with
   | Csil_cbor.Uint 1L -> ()
   | _ -> failwith "expected bare CBOR uint 1");
  (* An unknown literal (right general CBOR shape, wrong value) must be rejected. *)
  (try
     let (_ : Types.mixed_enum_record_code) =
       Codec.decode_mixed_enum_record_code (Csil_cbor.Text "z")
     in
     failwith "expected failure for unknown text literal"
   with Failure _ -> ());
  (try
     let (_ : Types.mixed_enum_record_code) =
       Codec.decode_mixed_enum_record_code (Csil_cbor.Uint 99L)
     in
     failwith "expected failure for unknown int literal"
   with Failure _ -> ());
  print_string "ok\n"
"#;
