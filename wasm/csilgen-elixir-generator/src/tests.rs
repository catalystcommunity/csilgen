use super::*;
use csilgen_common::{
    CsilPosition, CsilRule, CsilServiceDefinition, CsilServiceOperation, CsilSpecSerialized,
    GeneratorConfig,
};
use std::collections::HashMap;

fn opts(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn bare_entry(name: &str, ty: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type: ty,
        occurrence: None,
        metadata: vec![],
        doc_comments: vec![],
    }
}

fn optional_entry(name: &str, ty: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type: ty,
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: vec![],
    }
}

fn group_input(
    type_name: &str,
    entries: Vec<CsilGroupEntry>,
    options: HashMap<String, serde_json::Value>,
) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: type_name.to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "elixir".to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: meta(),
    }
}

fn meta() -> GeneratorMetadata {
    GeneratorMetadata {
        name: "elixir".to_string(),
        version: "0.1.0".to_string(),
        description: String::new(),
        target: "elixir".to_string(),
        capabilities: vec![],
        author: None,
        homepage: None,
    }
}

fn make_op(
    name: &str,
    input: &str,
    output: CsilTypeExpression,
    direction: CsilServiceDirection,
    wire_id: Option<u64>,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: CsilTypeExpression::Reference(input.to_string()),
        output_type: output,
        direction,
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
        wire_id,
    }
}

fn service_input(
    name: &str,
    ops: Vec<CsilServiceOperation>,
    wire_id: Option<u64>,
    target: &str,
) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: name.to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: ops,
                    wire_id,
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
            }],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: meta(),
    }
}

fn file<'a>(out: &'a WasmGeneratorOutput, path: &str) -> &'a str {
    out.files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.content.as_str())
        .unwrap_or_else(|| panic!("file {path} not emitted"))
}

#[test]
fn test_pascal_and_snake_case() {
    assert_eq!(pascal_case("deposit-claim"), "DepositClaim");
    assert_eq!(pascal_case("user_name"), "UserName");
    assert_eq!(snake_case("deposit-claim"), "deposit_claim");
    assert_eq!(snake_case("DepositClaimRequest"), "deposit_claim_request");
    assert_eq!(snake_case("already_snake"), "already_snake");
}

#[test]
fn test_struct_emission() {
    let input = group_input(
        "DepositClaimRequest",
        vec![
            bare_entry("subject", CsilTypeExpression::Builtin("text".to_string())),
            bare_entry("claim", CsilTypeExpression::Builtin("bytes".to_string())),
            optional_entry("note", CsilTypeExpression::Builtin("text".to_string())),
        ],
        HashMap::new(),
    );
    let out = process_generation(input).unwrap();
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("defmodule Csilgen.Generated.DepositClaimRequest do"));
    // Required fields are enforced; the optional one is not.
    assert!(types.contains("@enforce_keys [:subject, :claim]"));
    assert!(types.contains("defstruct [:subject, :claim, :note]"));
    assert!(types.contains("subject: String.t()"));
    assert!(types.contains("claim: binary()"));
    assert!(types.contains("note: String.t() | nil"));
    // Verbatim wire keys are preserved, never atomized on the wire.
    assert!(types.contains("@wire_keys [subject: \"subject\", claim: \"claim\", note: \"note\"]"));
}

#[test]
fn test_wire_keys_stay_verbatim_snake_case() {
    let input = group_input(
        "Task",
        vec![bare_entry(
            "current_state",
            CsilTypeExpression::Builtin("text".to_string()),
        )],
        HashMap::new(),
    );
    let out = process_generation(input).unwrap();
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("current_state: \"current_state\""));
}

#[test]
fn test_custom_module_root() {
    let input = group_input(
        "Thing",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        opts(&[("elixir_module", serde_json::json!("MyApp"))]),
    );
    let out = process_generation(input).unwrap();
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("defmodule MyApp.Thing do"));
}

#[test]
fn test_client_target() {
    let op = make_op(
        "deposit-claim",
        "DepositClaimRequest",
        CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Reference("DepositClaimResponse".to_string()),
            CsilTypeExpression::Reference("ServiceError".to_string()),
        ]),
        CsilServiceDirection::Unidirectional,
        None,
    );
    let input = service_input("Attestation", vec![op], None, "elixir-client");
    let out = process_generation(input).unwrap();
    let client = file(&out, "client.gen.ex");
    assert!(client.contains("defmodule Csilgen.Generated.Transport do"));
    assert!(client.contains("defmodule Csilgen.Generated.AttestationClient do"));
    // ServiceError half of the union is stripped from the success type.
    assert!(
        client.contains(":: {:ok, Csilgen.Generated.DepositClaimResponse.t()} | {:error, term()}")
    );
    // Wire service is lowercased base; wire method is PascalCase verbatim.
    assert!(client.contains("\"attestation\", \"DepositClaim\", req"));
    // Server file must not be emitted for the client target.
    assert!(!out.files.iter().any(|f| f.path == "server.gen.ex"));
}

#[test]
fn test_server_target_handlers_and_routers() {
    let unary = make_op(
        "list-events",
        "User",
        CsilTypeExpression::Reference("User".to_string()),
        CsilServiceDirection::Unidirectional,
        Some(1),
    );
    let bidi = make_op(
        "play",
        "User",
        CsilTypeExpression::Reference("User".to_string()),
        CsilServiceDirection::Bidirectional,
        Some(2),
    );
    let input = service_input("Match", vec![unary, bidi], Some(7), "elixir");
    let out = process_generation(input).unwrap();
    let server = file(&out, "server.gen.ex");
    // Codec seam emitted because there is a channel op.
    assert!(server.contains("defmodule Csilgen.Generated.Codec do"));
    assert!(server.contains("defmodule Csilgen.Generated.MatchServer do"));
    // Handler callbacks: unary returns Output; bidi inbound is fire-and-forget.
    assert!(
        server.contains("@callback list_events(req :: Csilgen.Generated.User.t(), ctx :: map())")
    );
    assert!(
        server.contains("@callback play(msg :: Csilgen.Generated.User.t(), ctx :: map()) :: :ok")
    );
    // Verbose router dispatches by wire method name.
    assert!(server.contains("def route(handler, codec, \"Play\" = _method, data, ctx) do"));
    // Compact router (wire-id present) dispatches by ordinal.
    assert!(server.contains("def route_compact(handler, codec, 2 = _op, data, ctx) do"));
    // Wire-id accessors exposed.
    assert!(server.contains("def wire_id, do: 7"));
    assert!(server.contains("def wire_id(:play), do: 2"));
    // Outbound encoder for the bidi op.
    assert!(server.contains("def encode_play(codec, msg), do: {\"Play\", codec.encode(msg)}"));
}

#[test]
fn test_compact_router_absent_without_wire_ids() {
    let bidi = make_op(
        "play",
        "User",
        CsilTypeExpression::Reference("User".to_string()),
        CsilServiceDirection::Bidirectional,
        None,
    );
    let input = service_input("Match", vec![bidi], None, "elixir");
    let out = process_generation(input).unwrap();
    let server = file(&out, "server.gen.ex");
    assert!(server.contains("def route("));
    assert!(!server.contains("def route_compact("));
}

#[test]
fn test_unknown_subtarget_errors() {
    let input = service_input(
        "S",
        vec![make_op(
            "x",
            "User",
            CsilTypeExpression::Reference("User".to_string()),
            CsilServiceDirection::Unidirectional,
            None,
        )],
        None,
        "elixir-bogus",
    );
    assert!(process_generation(input).is_err());
}

#[test]
fn test_bad_decimal_mapping_errors() {
    let input = group_input(
        "T",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        opts(&[("decimal_mapping", serde_json::json!("nope"))]),
    );
    assert!(process_generation(input).is_err());
}

#[test]
fn test_validation_emission() {
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("name".to_string())),
        value_type: CsilTypeExpression::Builtin("text".to_string()),
        occurrence: None,
        metadata: vec![
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(1)),
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(100)),
        ],
        doc_comments: vec![],
    };
    let input = group_input("CreateUser", vec![entry], HashMap::new());
    let out = process_generation(input).unwrap();
    let v = file(&out, "validation.gen.ex");
    assert!(v.contains("defmodule Csilgen.Generated.Validation do"));
    assert!(v.contains("def validate_create_user(%Csilgen.Generated.CreateUser{} = v) do"));
    assert!(v.contains("String.length(v.name) >= 1"));
    assert!(v.contains("String.length(v.name) <= 100"));
}

#[test]
fn test_optional_validation_skips_nil() {
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("note".to_string())),
        value_type: CsilTypeExpression::Builtin("text".to_string()),
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinLength(3),
        )],
        doc_comments: vec![],
    };
    let input = group_input("T", vec![entry], HashMap::new());
    let out = process_generation(input).unwrap();
    let v = file(&out, "validation.gen.ex");
    assert!(v.contains("is_nil(v.note)"));
}

#[test]
fn test_constructors_emission() {
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("retries".to_string())),
        value_type: CsilTypeExpression::Builtin("int".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::Custom {
                name: "default".to_string(),
                value: CsilLiteralValue::Integer(3),
            },
        )],
        doc_comments: vec![],
    };
    let input = group_input(
        "Config",
        vec![entry],
        opts(&[("generate_constructors", serde_json::json!(true))]),
    );
    let out = process_generation(input).unwrap();
    let c = file(&out, "constructors.gen.ex");
    assert!(c.contains("def new_config() do"));
    assert!(c.contains("retries: 3"));
    // A defaulted field is part of defstruct with its default, not enforced.
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("defstruct [retries: 3]"));
    assert!(!types.contains("@enforce_keys"));
}

#[test]
fn test_formatter_always_emitted() {
    let input = group_input(
        "T",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        HashMap::new(),
    );
    let out = process_generation(input).unwrap();
    assert!(out.files.iter().any(|f| f.path == ".formatter.exs"));
}

#[test]
fn test_message_escaping_neutralizes_interpolation() {
    // A `#` in a constraint message must not open an Elixir interpolation.
    assert_eq!(escape_msg("a#{b}"), "a\\#{b}");
    assert_eq!(escape_msg("say \"hi\""), "say \\\"hi\\\"");
}

#[test]
fn test_choice_of_text_literals_dedups_union() {
    // `text / "a" / "b"` previously expanded to `String.t() | String.t() | String.t()`;
    // identical arms collapse to a single type so the spec reads cleanly.
    let cfg = ElixirConfig::from_options(&HashMap::new()).unwrap();
    let choice = CsilTypeExpression::Choice(vec![
        CsilTypeExpression::Builtin("text".into()),
        CsilTypeExpression::Literal(CsilLiteralValue::Text("a".into())),
        CsilTypeExpression::Literal(CsilLiteralValue::Text("b".into())),
    ]);
    assert_eq!(map_type(&choice, &cfg), "String.t()");
}

#[test]
fn test_validation_size_fn_matches_field_shape() {
    // `length/1` raises on a binary or map, so each shape must get its own BIF:
    // text → String.length, bytes → byte_size, map → map_size, list → length.
    let entries = vec![
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("blob".to_string())),
            value_type: CsilTypeExpression::Builtin("bytes".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MaxLength(8),
            )],
            doc_comments: vec![],
        },
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("tags".to_string())),
            value_type: CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                occurrence: None,
            },
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MaxItems(3),
            )],
            doc_comments: vec![],
        },
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("meta".to_string())),
            value_type: CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Builtin("any".to_string())),
                occurrence: None,
            },
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MaxItems(2),
            )],
            doc_comments: vec![],
        },
    ];
    let input = group_input("Shapes", entries, HashMap::new());
    let out = process_generation(input).unwrap();
    let v = file(&out, "validation.gen.ex");
    assert!(v.contains("byte_size(v.blob) <= 8"));
    assert!(v.contains("length(v.tags) <= 3"));
    assert!(v.contains("map_size(v.meta) <= 2"));
}

#[test]
fn test_channel_router_decodes_with_module_not_typespec() {
    // The codec's decode/2 takes a runtime module, never the `.t()` typespec —
    // emitting `Mod.t()` would call an undefined `t/0` at dispatch time.
    let bidi = make_op(
        "play",
        "MoveRequest",
        CsilTypeExpression::Reference("MoveResponse".to_string()),
        CsilServiceDirection::Bidirectional,
        None,
    );
    let input = service_input("Match", vec![bidi], None, "elixir");
    let out = process_generation(input).unwrap();
    let server = file(&out, "server.gen.ex");
    assert!(server.contains("codec.decode(data, Csilgen.Generated.MoveRequest)"));
    assert!(!server.contains("codec.decode(data, Csilgen.Generated.MoveRequest.t())"));
}

#[test]
fn test_type_mapping() {
    let cfg = ElixirConfig::from_options(&HashMap::new()).unwrap();
    assert_eq!(
        map_type(&CsilTypeExpression::Builtin("text".into()), &cfg),
        "String.t()"
    );
    assert_eq!(
        map_type(&CsilTypeExpression::Builtin("bstr".into()), &cfg),
        "binary()"
    );
    assert_eq!(
        map_type(&CsilTypeExpression::Builtin("uint".into()), &cfg),
        "integer()"
    );
    assert_eq!(
        map_type(&CsilTypeExpression::Builtin("timestamp".into()), &cfg),
        "DateTime.t()"
    );
    assert_eq!(
        map_type(
            &CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("int".into())),
                occurrence: None
            },
            &cfg
        ),
        "[integer()]"
    );
}
