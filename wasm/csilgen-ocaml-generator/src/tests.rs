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

#[test]
fn wire_service_strips_service_suffix() {
    assert_eq!(wire_service_name("CorndogsService"), "Corndogs");
    assert_eq!(wire_service_name("Attestation"), "Attestation");
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
    // The wire op string is verbatim; the OCaml fn name is snake_case.
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
    assert_eq!(paths, vec!["types.ml", "types.mli"]);
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
