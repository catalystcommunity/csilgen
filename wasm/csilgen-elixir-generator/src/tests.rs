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
    // The typed byte seam only emits a call when both ends are records the codec
    // covers, so the request/response records must be declared in the spec.
    let mut input = service_input("Attestation", vec![op], None, "elixir-client");
    input.csil_spec.rules.insert(
        0,
        CsilRule {
            name: "DepositClaimRequest".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry(
                    "subject",
                    CsilTypeExpression::Builtin("text".to_string()),
                )],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: vec![],
        },
    );
    input.csil_spec.rules.insert(
        1,
        CsilRule {
            name: "DepositClaimResponse".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare_entry(
                    "id",
                    CsilTypeExpression::Builtin("text".to_string()),
                )],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: vec![],
        },
    );
    let out = process_generation(input).unwrap();
    let client = file(&out, "client.gen.ex");
    assert!(client.contains("defmodule Csilgen.Generated.Transport do"));
    assert!(client.contains("defmodule Csilgen.Generated.AttestationClient do"));
    // The byte seam: the transport callback takes/returns bytes, not a term.
    assert!(client.contains(
        "@callback call(t(), service :: String.t(), method :: String.t(), req :: binary()) ::"
    ));
    // ServiceError half of the union is stripped; the success type is the response.
    assert!(client.contains(":: Csilgen.Generated.DepositClaimResponse.t()"));
    // Request is encoded to bytes, the reply decoded from bytes; wire service is the
    // lowercased base and the wire method is PascalCase verbatim.
    assert!(client.contains(
        "Csilgen.Generated.Transport.call(transport, \"attestation\", \"DepositClaim\", Csilgen.Generated.DepositClaimRequest.to_cbor(req))"
    ));
    assert!(client.contains("Csilgen.Generated.DepositClaimResponse.from_cbor(resp)"));
    // The codec rides alongside; per-struct to_cbor/from_cbor are emitted.
    let codec = file(&out, "codec.gen.ex");
    assert!(codec.contains("defmodule Csilgen.Generated.Cbor do"));
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("def to_cbor(v), do: Csilgen.Generated.Cbor.encode(to_cbor_value(v))"));
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

// --- codec ------------------------------------------------------------------

/// A corndogs-shaped spec: text uuid/current_state, bytes payload, an optional int
/// priority, a map<text,int>, a list<text>, a nested request record, an error
/// record, and a service whose output is a `Task / ServiceError` choice.
fn corndogs_spec() -> CsilSpecSerialized {
    let text = || CsilTypeExpression::Builtin("text".to_string());
    let int = || CsilTypeExpression::Builtin("int".to_string());
    let task = CsilRule {
        name: "Task".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![
                bare_entry("uuid", text()),
                bare_entry("current_state", text()),
                bare_entry("payload", CsilTypeExpression::Builtin("bytes".to_string())),
                optional_entry("priority", int()),
                bare_entry(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(text()),
                        value: Box::new(int()),
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
                // A field typed as a named map alias (a `TypeDef` carrying a `{* text
                // => int}` map): the regression stubbed these to nil, dropping data.
                bare_entry(
                    "metrics",
                    CsilTypeExpression::Reference("StringInt64Map".to_string()),
                ),
                // A named map alias whose values are a record: each entry must route
                // through the referenced record module's codec.
                bare_entry(
                    "notes",
                    CsilTypeExpression::Reference("NoteMap".to_string()),
                ),
            ],
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    let req = CsilRule {
        name: "SubmitTaskRequest".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![
                bare_entry("task", CsilTypeExpression::Reference("Task".to_string())),
                bare_entry("queue", text()),
            ],
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    let err = CsilRule {
        name: "ServiceError".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![bare_entry("code", int()), bare_entry("message", text())],
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    let svc = CsilRule {
        name: "CorndogsService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![make_op(
                "submit-task",
                "SubmitTaskRequest",
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Task".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    // `StringInt64Map = {* text => int}` — a transparent map alias (a `TypeDef`
    // carrying a `Map`, not a group), the exact shape the codec used to stub out.
    let string_int64_map = CsilRule {
        name: "StringInt64Map".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
            key: Box::new(text()),
            value: Box::new(int()),
            occurrence: None,
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    let note = CsilRule {
        name: "Note".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![bare_entry("body", text())],
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    // `NoteMap = {* text => Note}` — a named map alias of record values, which has no
    // struct module of its own; per-entry values route to `Note`'s codec.
    let note_map = CsilRule {
        name: "NoteMap".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
            key: Box::new(text()),
            value: Box::new(CsilTypeExpression::Reference("Note".to_string())),
            occurrence: None,
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    CsilSpecSerialized {
        rules: vec![task, req, err, note, string_int64_map, note_map, svc],
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

fn corndogs_input(target: &str) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: corndogs_spec(),
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: meta(),
    }
}

#[test]
fn test_codec_module_emitted_with_records() {
    let out = process_generation(corndogs_input("elixir-client")).unwrap();
    let codec = file(&out, "codec.gen.ex");
    // The shared value codec carries text vs bytes and exposes encode/decode.
    assert!(codec.contains("defmodule Csilgen.Generated.Cbor do"));
    assert!(codec.contains("def encode(value)"));
    assert!(codec.contains("def decode(bin)"));
    // bytes -> CBOR byte string (major type 2), text -> major type 3.
    assert!(codec.contains("defp enc({:bytes, b}), do: [head(2, byte_size(b)), b]"));
    assert!(codec.contains("defp enc({:text, s}), do: [head(3, byte_size(s)), s]"));
    // bool/null/float scalar heads per the wire contract.
    assert!(codec.contains("defp enc({:bool, false}), do: <<0xF4>>"));
    assert!(codec.contains("defp enc({:bool, true}), do: <<0xF5>>"));
    assert!(codec.contains("defp enc(:null), do: <<0xF6>>"));
    assert!(codec.contains("defp enc({:float, f}), do: <<0xFB, f::float-size(64)>>"));
}

#[test]
fn test_no_codec_without_records() {
    // A service-only spec (no record types) emits no codec file.
    let op = make_op(
        "ping",
        "Nothing",
        CsilTypeExpression::Reference("Nothing".to_string()),
        CsilServiceDirection::Unidirectional,
        None,
    );
    let input = service_input("S", vec![op], None, "elixir-client");
    let out = process_generation(input).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "codec.gen.ex"));
}

#[test]
fn test_struct_codec_canonical_order_and_shapes() {
    let out = process_generation(corndogs_input("elixir-typesonly")).unwrap();
    let types = file(&out, "types.gen.ex");
    // Per-struct codec surface lands on the struct module itself.
    assert!(types.contains("def to_cbor_value(%__MODULE__{} = v) do"));
    assert!(types.contains("def from_cbor_value({:map, csil_kvs}) do"));
    assert!(types.contains("def to_cbor(v), do: Csilgen.Generated.Cbor.encode(to_cbor_value(v))"));
    assert!(types.contains(
        "def from_cbor(bytes), do: from_cbor_value(Csilgen.Generated.Cbor.decode(bytes))"
    ));
    // Canonical RFC 8949 key order is shorter-key-first, so `tags`/`uuid` precede
    // `payload`/`current_state`; verify `current_state` (longest) sorts last.
    let to_v = types
        .split("def to_cbor_value")
        .nth(1)
        .expect("Task to_cbor_value present");
    let pos_tags = to_v.find("\"tags\"").unwrap();
    let pos_uuid = to_v.find("\"uuid\"").unwrap();
    let pos_labels = to_v.find("\"labels\"").unwrap();
    let pos_payload = to_v.find("\"payload\"").unwrap();
    let pos_priority = to_v.find("\"priority\"").unwrap();
    let pos_state = to_v.find("\"current_state\"").unwrap();
    // 4-byte keys (uuid, tags) < 6-byte (labels) < 7 (payload) < 8 (priority) < 13.
    assert!(pos_uuid < pos_labels);
    assert!(pos_tags < pos_labels);
    assert!(pos_labels < pos_payload);
    assert!(pos_payload < pos_priority);
    assert!(pos_priority < pos_state);
    // bytes field uses the byte-string item; the optional priority is omitted when nil.
    assert!(types.contains("{{:text, \"payload\"}, {:bytes, v.payload}}"));
    assert!(types.contains(
        "(if is_nil(v.priority), do: nil, else: {{:text, \"priority\"}, {:int, v.priority}}),"
    ));
    // map/list encode through the value tree; a nested record delegates.
    assert!(types.contains(
        "{:map, Enum.map(v.labels, fn {csil_k, csil_v} -> {{:text, csil_k}, {:int, csil_v}} end)}"
    ));
    assert!(types.contains("{:array, Enum.map(v.tags, fn csil_e -> {:text, csil_e} end)}"));
    assert!(types.contains("Csilgen.Generated.Task.to_cbor_value(v.task)"));
}

/// Generate the corndogs `elixir-client` spec, write a driver that loads the
/// generated modules plus a loopback transport, round-trip via to_cbor/from_cbor
/// and the typed client, and run it with `elixir`. Skips when elixir is absent.
#[test]
fn codec_round_trips_through_elixir() {
    let have = std::process::Command::new("elixir")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no elixir on PATH");
        return;
    }
    let out = process_generation(corndogs_input("elixir-client")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-elixir-codec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &out.files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.exs"), CODEC_DRIVER_ELIXIR).unwrap();

    let run = std::process::Command::new("elixir")
        .arg(dir.join("driver.exs"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "elixir round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- publishable Mix package mode ---------------------------------------------

#[test]
fn test_package_mode_off_by_default() {
    // Without `emit_packages`, nothing changes: flat layout, no mix.exs.
    let input = group_input(
        "Thing",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        HashMap::new(),
    );
    let out = process_generation(input).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "mix.exs"));
    assert!(out.files.iter().any(|f| f.path == "types.gen.ex"));
}

#[test]
fn test_package_mode_requires_elixir_member() {
    // An `emit_packages` array that omits "elixir" must not trigger the package.
    let input = group_input(
        "Thing",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        opts(&[("emit_packages", serde_json::json!(["go", "rust"]))]),
    );
    let out = process_generation(input).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "mix.exs"));
    assert!(out.files.iter().any(|f| f.path == "types.gen.ex"));
}

#[test]
fn test_package_mode_defensive_parse() {
    // A malformed `emit_packages` (not an array) degrades to no package, never errors.
    let input = group_input(
        "Thing",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        opts(&[("emit_packages", serde_json::json!("elixir"))]),
    );
    let out = process_generation(input).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "mix.exs"));
}

#[test]
fn test_package_mode_emits_mix_and_lib_layout() {
    let input = group_input(
        "Thing",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        opts(&[("emit_packages", serde_json::json!(["elixir"]))]),
    );
    let out = process_generation(input).unwrap();
    // mix.exs at root with the default app/version.
    let mix = file(&out, "mix.exs");
    assert!(mix.contains("defmodule CsilgenClient.MixProject do"));
    assert!(mix.contains("app: :csilgen_client"));
    assert!(mix.contains("version: \"0.1.0\""));
    assert!(mix.contains("elixir: \"~> 1.14\""));
    assert!(mix.contains("defp deps do\n    []\n  end"));
    // Modules move under lib/; the flat path is gone.
    assert!(out.files.iter().any(|f| f.path == "lib/types.gen.ex"));
    assert!(!out.files.iter().any(|f| f.path == "types.gen.ex"));
    // .formatter.exs stays at the root and reaches lib/.
    let fmt = file(&out, ".formatter.exs");
    assert!(fmt.contains("lib/**/*.ex"));
    assert!(out.files.iter().any(|f| f.path == ".formatter.exs"));
}

#[test]
fn emit_readme_false_suppresses_only_readme() {
    let entries = || {
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )]
    };
    // Default package mode: the README rides along.
    let on = process_generation(group_input(
        "Thing",
        entries(),
        opts(&[("emit_packages", serde_json::json!(["elixir"]))]),
    ))
    .unwrap();
    assert!(on.files.iter().any(|f| f.path == "README.md"));

    // An explicit `emit_readme: false` drops only the README.
    let off = process_generation(group_input(
        "Thing",
        entries(),
        opts(&[
            ("emit_packages", serde_json::json!(["elixir"])),
            ("emit_readme", serde_json::json!(false)),
        ]),
    ))
    .unwrap();
    assert!(!off.files.iter().any(|f| f.path == "README.md"));
    // The rest of the publishable package is unchanged.
    assert!(off.files.iter().any(|f| f.path == "mix.exs"));
    let on_without_readme: Vec<_> = on
        .files
        .iter()
        .filter(|f| f.path != "README.md")
        .map(|f| &f.path)
        .collect();
    let off_paths: Vec<_> = off.files.iter().map(|f| &f.path).collect();
    assert_eq!(on_without_readme, off_paths);
}

#[test]
fn test_package_mode_honors_name_and_version() {
    let input = group_input(
        "Thing",
        vec![bare_entry(
            "a",
            CsilTypeExpression::Builtin("int".to_string()),
        )],
        opts(&[
            ("emit_packages", serde_json::json!(["elixir"])),
            ("package_name", serde_json::json!("MyCool-App")),
            ("package_version", serde_json::json!("2.3.4")),
        ]),
    );
    let out = process_generation(input).unwrap();
    let mix = file(&out, "mix.exs");
    // package_name is normalized to a valid snake_case atom; module is PascalCased.
    assert!(mix.contains("app: :my_cool_app"));
    assert!(mix.contains("defmodule MyCoolApp.MixProject do"));
    assert!(mix.contains("version: \"2.3.4\""));
}

/// An input with a `user` record and a `user_service` with a unary `get-user` op, in
/// `elixir-client` package mode — so the README's full carrier Quickstart is exercised.
fn readme_package_input() -> WasmGeneratorInput {
    let user = CsilRule {
        name: "user".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![
                bare_entry("name", CsilTypeExpression::Builtin("text".to_string())),
                bare_entry("id", CsilTypeExpression::Builtin("int".to_string())),
            ],
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    let service = CsilRule {
        name: "user_service".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "get-user".to_string(),
                input_type: CsilTypeExpression::Reference("user".to_string()),
                output_type: CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("user".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                direction: CsilServiceDirection::Unidirectional,
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![user, service],
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "elixir-client".to_string(),
            output_dir: "/tmp".to_string(),
            options: opts(&[("emit_packages", serde_json::json!(["elixir"]))]),
        },
        generator_metadata: meta(),
    }
}

#[test]
fn package_readme_has_quickstart_carrier_and_example() {
    let out = process_generation(readme_package_input()).unwrap();
    // The README rides with the package, at the root.
    assert!(out.files.iter().any(|f| f.path == "README.md"));
    let body = file(&out, "README.md");

    // Title + a deps install hint naming this package.
    assert!(body.starts_with("# csilgen_client\n"));
    assert!(body.contains("{:csilgen_client,"));

    // The carrier implements the generated transport seam (the behaviour).
    assert!(body.contains("defmodule CsilRpcTransport do"));
    assert!(body.contains("@behaviour Csilgen.Generated.Transport"));
    assert!(body.contains("def call(%__MODULE__{rpc_url: url}, service, op, req)"));

    // It POSTs to the CSIL-RPC endpoint, wraps the payload in CBOR tag 24, and reuses
    // the generated codec (no third-party dep).
    assert!(body.contains("/csil/v1/rpc"));
    assert!(body.contains("POST "));
    assert!(body.contains("{{:text, \"payload\"}, {:tag, 24, {:bytes, req}}}"));
    assert!(body.contains("Cbor.encode(envelope)"));
    assert!(body.contains("Cbor.decode(body)"));

    // The status / typed ServiceError arms are handled.
    assert!(body.contains("transport status"));
    assert!(body.contains("{:text, \"ServiceError\"} ->"));
    assert!(body.contains("raise \"service error"));

    // Client construction over the carrier + the first unary call with a generated
    // sample struct literal (required fields only, struct field atoms).
    assert!(body.contains("transport = CsilRpcTransport.new(\"http://localhost:5080\")"));
    assert!(body.contains("client = Csilgen.Generated.UserClient.new(transport)"));
    assert!(body.contains(
        "resp = Csilgen.Generated.UserClient.get_user(client, %Csilgen.Generated.User{name: \"example\", id: 0})"
    ));
}

#[test]
fn package_readme_absent_without_package_mode() {
    // The flat (non-package) layout never ships a README.
    let mut input = readme_package_input();
    input.config.options = HashMap::new();
    let out = process_generation(input).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "README.md"));
}

/// Build the corndogs `elixir-client` spec in package mode and prove the emitted Mix
/// project compiles: `mix compile` when Mix is present (no network — deps are empty),
/// else `elixirc` over `lib/` plus an `elixir`-side parse of `mix.exs`. Skips when no
/// Elixir toolchain is on PATH.
#[test]
fn elixir_package_compiles() {
    use std::process::Command;
    if Command::new("elixir").arg("--version").output().is_err() {
        eprintln!("skipping: no elixir on PATH");
        return;
    }
    let input = WasmGeneratorInput {
        csil_spec: corndogs_spec(),
        config: GeneratorConfig {
            target: "elixir-client".to_string(),
            output_dir: "/tmp".to_string(),
            options: opts(&[("emit_packages", serde_json::json!(["elixir"]))]),
        },
        generator_metadata: meta(),
    };
    let out = process_generation(input).unwrap();
    assert!(out.files.iter().any(|f| f.path == "mix.exs"));
    assert!(out.files.iter().all(|f| {
        // Every Elixir module is under lib/ in package mode; config files stay at root.
        !f.path.ends_with(".ex") || f.path.starts_with("lib/")
    }));

    let dir = std::env::temp_dir().join(format!("csilgen-elixir-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &out.files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &f.content).unwrap();
    }

    let have_mix = Command::new("mix")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // Prefer the real publish path: a `mix compile` of the project. MIX_ENV=prod and
    // empty deps keep it offline; clearing MIX_HOME/HOME-derived caches is unnecessary
    // because nothing is fetched.
    let compiled_via_mix = have_mix
        && Command::new("mix")
            .arg("compile")
            .current_dir(&dir)
            .env("MIX_ENV", "prod")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    if !compiled_via_mix {
        // Fallback for hosts without Mix/Hex: compile the lib modules directly and
        // confirm mix.exs is at least syntactically valid Elixir.
        let beam = dir.join("_beam");
        std::fs::create_dir_all(&beam).unwrap();
        let mut ec = Command::new("elixirc");
        ec.arg("-o").arg(&beam);
        for f in &out.files {
            if f.path.ends_with(".ex") {
                ec.arg(dir.join(&f.path));
            }
        }
        let r = ec.output().unwrap();
        assert!(
            r.status.success(),
            "elixirc failed:\n{}{}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        let parse = Command::new("elixir")
            .arg("-e")
            .arg("File.read!(\"mix.exs\") |> Code.string_to_quoted!()")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            parse.status.success(),
            "mix.exs did not parse:\n{}",
            String::from_utf8_lossy(&parse.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// The generated structs are built with `struct/2` rather than `%Mod{}` literals:
// a script is compiled as one unit, so a `%Mod{}` literal would expand at compile
// time before `Code.require_file` has loaded the generated modules; `struct/2`
// resolves the module at runtime instead.
const CODEC_DRIVER_ELIXIR: &str = r#"# Load the codec before the types (whose to_cbor calls it) and the client.
Code.require_file("codec.gen.ex", __DIR__)
Code.require_file("types.gen.ex", __DIR__)
Code.require_file("client.gen.ex", __DIR__)

alias Csilgen.Generated.Task
alias Csilgen.Generated.SubmitTaskRequest
alias Csilgen.Generated.CorndogsClient
alias Csilgen.Generated.Note

defmodule Loopback do
  @moduledoc "A byte-loopback transport: decode the request, re-encode its task."
  defstruct []

  def call(%Loopback{}, _service, _op, req_bytes) do
    req = Csilgen.Generated.SubmitTaskRequest.from_cbor(req_bytes)
    Csilgen.Generated.Task.to_cbor(req.task)
  end
end

payload = <<0xDE, 0xAD, 0xBE>>

task =
  struct(Task,
    uuid: "u-123",
    current_state: "PENDING",
    payload: payload,
    priority: 7,
    labels: %{"a" => 1, "b" => 2},
    tags: ["x", "y"],
    metrics: %{"hits" => 10, "misses" => 2},
    notes: %{"n1" => struct(Note, body: "first"), "n2" => struct(Note, body: "second")}
  )

# Direct codec round-trip through the struct.
back = Task.from_cbor(Task.to_cbor(task))
true = back.uuid == "u-123"
true = back.current_state == "PENDING"
true = back.payload == payload
true = back.priority == 7
true = back.labels == %{"a" => 1, "b" => 2}
true = back.tags == ["x", "y"]

# A named map alias (StringInt64Map = {* text => int}) used to be stubbed to nil;
# its entries must survive the round-trip intact.
true = back.metrics == %{"hits" => 10, "misses" => 2}

# A map-of-record alias (NoteMap = {* text => Note}): each value rehydrates as a
# Note struct through the referenced record's codec.
true = back.notes["n1"].body == "first"
true = back.notes["n2"].body == "second"

# An absent optional must round-trip to nil (omitted from the wire map).
task2 = struct(task, priority: nil)
back2 = Task.from_cbor(Task.to_cbor(task2))
true = back2.priority == nil

# Nested record round-trip.
req = struct(SubmitTaskRequest, task: task, queue: "default")
rback = SubmitTaskRequest.from_cbor(SubmitTaskRequest.to_cbor(req))
true = rback.task.uuid == "u-123"
true = rback.queue == "default"

# Typed client over the loopback byte carrier.
client = CorndogsClient.new(struct(Loopback, []))
result = CorndogsClient.submit_task(client, req)
true = result.uuid == "u-123"
true = result.payload == payload
true = result.priority == 7

IO.puts("ok")
"#;
