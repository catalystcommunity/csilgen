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
    // The @spec line overflows 98 cols, so mix wraps after `::` onto its own line.
    assert!(client.contains(
        "@spec deposit_claim(t(), Csilgen.Generated.DepositClaimRequest.t()) ::\n          Csilgen.Generated.DepositClaimResponse.t()"
    ));
    // Request is encoded to bytes, the reply decoded from bytes; the wire service and
    // op strings are the CSIL names verbatim (csil-rpc-transport.md §1.1). The call
    // overflows 98 cols flat, so mix breaks each argument onto its own line.
    assert!(client.contains(concat!(
        "      Csilgen.Generated.Transport.call(\n",
        "        transport,\n",
        "        \"Attestation\",\n",
        "        \"deposit-claim\",\n",
        "        Csilgen.Generated.DepositClaimRequest.to_cbor(req)\n",
        "      )"
    )));
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
    // Verbose router dispatches by the verbatim wire method name.
    assert!(server.contains("def route(handler, codec, \"play\" = _method, data, ctx) do"));
    // Compact router (wire-id present) dispatches by ordinal.
    assert!(server.contains("def route_compact(handler, codec, 2 = _op, data, ctx) do"));
    // Wire-id accessors exposed.
    assert!(server.contains("def wire_id, do: 7"));
    assert!(server.contains("def wire_id(:play), do: 2"));
    // Outbound encoder for the bidi op.
    assert!(server.contains("def encode_play(codec, msg), do: {\"play\", codec.encode(msg)}"));
}

#[test]
fn test_callback_success_union_breaks_like_mix() {
    // Regression: a `@callback`'s `{:ok, A | B}` return tuple was spliced in as a
    // raw string with no fit check, so it rode straight past mix's 98-column
    // width instead of breaking the same way any other tuple-holding-a-union
    // does. Pinned to the exact shape `mix format --check-formatted` accepts
    // (verified against examples/build-integration/npm-project/api.csil's
    // `get_notifications`/`delete_notification` callbacks): the tuple always
    // opens onto its own line once it doesn't fit, and the union inside either
    // stays flat at that deeper indent or itself breaks at `|`, independently.
    let breaks_further = make_op(
        "get-notifications",
        "GetNotificationsRequest",
        CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Reference("GetNotificationsResponse".to_string()),
            CsilTypeExpression::Reference("NotificationError".to_string()),
            CsilTypeExpression::Reference("ServiceError".to_string()),
        ]),
        CsilServiceDirection::Unidirectional,
        None,
    );
    let stays_flat = make_op(
        "delete-notification",
        "NotificationID",
        CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Reference("DeleteResponse".to_string()),
            CsilTypeExpression::Reference("NotificationError".to_string()),
            CsilTypeExpression::Reference("ServiceError".to_string()),
        ]),
        CsilServiceDirection::Unidirectional,
        None,
    );
    let input = service_input(
        "NotificationAPI",
        vec![breaks_further, stays_flat],
        None,
        "elixir",
    );
    let out = process_generation(input).unwrap();
    let server = file(&out, "server.gen.ex");
    assert!(server.contains(concat!(
        "  @callback get_notifications(req :: Csilgen.Generated.GetNotificationsRequest.t(), ctx :: map()) ::\n",
        "              {:ok,\n",
        "               Csilgen.Generated.GetNotificationsResponse.t()\n",
        "               | Csilgen.Generated.NotificationError.t()}\n",
        "              | {:error, Csilgen.Generated.ServiceError.t()}\n",
    )));
    assert!(server.contains(concat!(
        "  @callback delete_notification(req :: Csilgen.Generated.NotificationID.t(), ctx :: map()) ::\n",
        "              {:ok,\n",
        "               Csilgen.Generated.DeleteResponse.t() | Csilgen.Generated.NotificationError.t()}\n",
        "              | {:error, Csilgen.Generated.ServiceError.t()}\n",
    )));
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
fn test_optional_ordered_check_omits_redundant_parens() {
    // Regression: `or` binds looser than every comparison this module emits, so
    // mix strips parens around the right-hand condition as redundant. The
    // generator used to always wrap it (`is_nil(v.count) or (v.count >= 0)`),
    // which `mix format --check-formatted` rejects.
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("count".to_string())),
        value_type: CsilTypeExpression::Constrained {
            base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
            constraints: vec![CsilControlOperator::GreaterEqual(
                CsilLiteralValue::Integer(0),
            )],
        },
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: vec![],
    };
    let input = group_input("T", vec![entry], HashMap::new());
    let out = process_generation(input).unwrap();
    let v = file(&out, "validation.gen.ex");
    assert!(v.contains("is_nil(v.count) or v.count >= 0,"));
    assert!(!v.contains("or (v.count >= 0)"));
}

#[test]
fn test_large_bound_literal_gets_mix_underscore_grouping() {
    // Regression: mix normalizes any integer literal with 6+ digits by grouping
    // it in 3s from the right (`104857600` -> `104_857_600`); the generator used
    // to emit the bare digit run for a `MaxValue`/`MinValue`/size bound, which
    // `mix format --check-formatted` then rejected. The five-digit message text
    // is untouched — grouping only applies inside a real Elixir integer literal,
    // never inside a string.
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("size_bytes".to_string())),
        value_type: CsilTypeExpression::Constrained {
            base_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
            constraints: vec![CsilControlOperator::LessEqual(CsilLiteralValue::Integer(
                104_857_600,
            ))],
        },
        occurrence: None,
        metadata: vec![],
        doc_comments: vec![],
    };
    let input = group_input("T", vec![entry], HashMap::new());
    let out = process_generation(input).unwrap();
    let v = file(&out, "validation.gen.ex");
    assert!(v.contains("v.size_bytes <= 104_857_600,"));
    assert!(v.contains("must be at most 104857600"));
}

#[test]
fn test_long_regex_guard_breaks_through_the_call_not_past_the_width() {
    // Regression: a guard's `head` used to be spliced in as a raw string with no
    // fit check, so a long regex pattern rode past mix's 98-column width on the
    // `if(` line instead of breaking `Regex.match?`'s own arguments the way mix
    // does — pinned to the exact shape `mix format --check-formatted` accepts
    // (verified against examples/complex-metadata/advanced-api.csil). The error
    // message's own string literal is left as one long line: mix cannot break a
    // single string token, so it doesn't try, unlike the breakable call above it.
    // The stored pattern carries a literal `\/` (as CSIL source escapes a forward
    // slash inside `.regex "..."`, matching examples/complex-metadata/advanced-api.csil's
    // `content_type` field) so it round-trips unescaped into the `~r/.../` sigil.
    let pattern = "^[a-zA-Z0-9][a-zA-Z0-9!#$&\\-\\^_]*\\/[a-zA-Z0-9][a-zA-Z0-9!#$&\\-\\^_.]*$";
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("content_type".to_string())),
        value_type: CsilTypeExpression::Constrained {
            base_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
            constraints: vec![CsilControlOperator::Regex(pattern.to_string())],
        },
        occurrence: None,
        metadata: vec![],
        doc_comments: vec![],
    };
    let input = group_input("MediaAsset", vec![entry], HashMap::new());
    let out = process_generation(input).unwrap();
    let v = file(&out, "validation.gen.ex");
    assert!(v.contains(
        "           if(\n             Regex.match?(\n               ~r/^[a-zA-Z0-9][a-zA-Z0-9!#$&\\-\\^_]*\\/[a-zA-Z0-9][a-zA-Z0-9!#$&\\-\\^_.]*$/,\n               v.content_type\n             ),\n"
    ));
    assert!(v.contains("             do: :ok,\n             else:\n               {:error,\n"));
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
    // A defaulted field is part of defstruct with its default, not enforced. Every
    // field here has a default, so the whole list is a keyword list — mix elides the
    // brackets on a call's sole keyword-list argument (`defstruct(retries: 3)` and
    // `defstruct([retries: 3])` parse identically, and mix always prefers the former).
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("defstruct retries: 3"));
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
        "if(is_nil(v.priority), do: nil, else: {{:text, \"priority\"}, {:int, v.priority}}),"
    ));
    // map/list encode through the value tree; a nested record delegates.
    assert!(types.contains(
        "{:map, Enum.map(v.labels, fn {csil_k, csil_v} -> {{:text, csil_k}, {:int, csil_v}} end)}"
    ));
    assert!(types.contains("{:array, Enum.map(v.tags, fn csil_e -> {:text, csil_e} end)}"));
    assert!(types.contains("Csilgen.Generated.Task.to_cbor_value(v.task)"));
}

/// An entry carrying a literal default, modeled the way the parser records one: a
/// `default` custom constraint in the field metadata.
fn default_entry(name: &str, ty: CsilTypeExpression, value: CsilLiteralValue) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type: ty,
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::Custom {
                name: "default".to_string(),
                value,
            },
        )],
        doc_comments: vec![],
    }
}

#[test]
fn test_defstruct_keyword_defaults_come_last() {
    // A defaulted field declared before later bare fields must still emit last in the
    // defstruct list: Elixir rejects a keyword entry followed by a bare atom. This is
    // the longhouse `Project` shape that previously produced invalid syntax.
    let input = group_input(
        "Project",
        vec![
            bare_entry("name", CsilTypeExpression::Builtin("text".to_string())),
            default_entry(
                "status",
                CsilTypeExpression::Builtin("text".to_string()),
                CsilLiteralValue::Text("active".to_string()),
            ),
            bare_entry(
                "created_by",
                CsilTypeExpression::Builtin("text".to_string()),
            ),
        ],
        HashMap::new(),
    );
    let out = process_generation(input).unwrap();
    let types = file(&out, "types.gen.ex");
    // Bare atoms first, the keyword default last — the only ordering Elixir accepts.
    assert!(types.contains("defstruct [:name, :created_by, status: \"active\"]"));
}

#[test]
fn test_empty_record_codec_has_no_unused_bindings() {
    // A fieldless record reads neither the struct nor the decoded pairs, so both must
    // bind underscored to compile warning-clean.
    let input = group_input("EmptyRequest", vec![], HashMap::new());
    let out = process_generation(input).unwrap();
    let types = file(&out, "types.gen.ex");
    assert!(types.contains("def to_cbor_value(%__MODULE__{} = _v) do"));
    assert!(types.contains("def from_cbor_value({:map, _csil_kvs}) do"));
    assert!(!types.contains("csil_fields = Map.new"));
}

#[test]
fn test_optional_undecodable_field_underscores_bound_value() {
    // An optional field referencing a non-record type has no real decoder (it falls
    // back to a `raise`), so the decoded value is never read; the case binding must be
    // underscored to avoid an unused-variable warning. A decodable optional keeps it.
    let input = group_input(
        "Project",
        vec![
            optional_entry(
                "status",
                CsilTypeExpression::Reference("ProjectStatus".to_string()),
            ),
            optional_entry("note", CsilTypeExpression::Builtin("text".to_string())),
        ],
        HashMap::new(),
    );
    let out = process_generation(input).unwrap();
    let types = file(&out, "types.gen.ex");
    // The `case` clauses overflow 98 cols flat (the module-qualified raise message is
    // long), so mix breaks each clause body onto its own line instead of packing them
    // with `;`.
    assert!(types.contains(concat!(
        "        case Map.get(csil_fields, {:text, \"status\"}) do\n",
        "          nil -> nil\n",
        "          _csil_v -> raise(\"csilgen: no codec for type ProjectStatus\")\n",
        "        end"
    )));
    assert!(types.contains(concat!(
        "        case Map.get(csil_fields, {:text, \"note\"}) do\n",
        "          nil -> nil\n",
        "          csil_v -> Csilgen.Generated.Cbor.to_text(csil_v)\n",
        "        end"
    )));
}

#[test]
fn test_codec_runtime_pins_size_arg() {
    // The decoder reads a length into `arg` then matches a binary of that size; the size
    // expression must pin (`^arg`) or recent Elixir warns about an outer variable in a
    // bitstring size.
    let out = process_generation(corndogs_input("elixir-typesonly")).unwrap();
    let codec = file(&out, "codec.gen.ex");
    assert!(codec.contains("<<b::binary-size(^arg), r::binary>> = rest"));
    assert!(codec.contains("<<s::binary-size(^arg), r::binary>> = rest"));
    assert!(!codec.contains("binary-size(arg)"));
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

/// `OrderStatus = text / "pending" / "confirmed" / "processing" / "shipped" /
/// "delivered" / "cancelled" / "refunded"` (examples/real-world-api/e-commerce-api.csil
/// line 138) held by a single-field `Order` record: the mixed-union shape whose literal
/// arms used to be shadowed by the general `text` arm's blanket `is_binary` guard.
fn mixed_union_input() -> WasmGeneratorInput {
    let order_status = CsilRule {
        name: "OrderStatus".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Builtin("text".to_string()),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("pending".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("confirmed".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("processing".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("shipped".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("delivered".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("cancelled".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("refunded".to_string())),
        ])),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: vec![],
    };
    let order = CsilRule {
        name: "Order".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![bare_entry(
                "status",
                CsilTypeExpression::Reference("OrderStatus".to_string()),
            )],
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
            rules: vec![order_status, order],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "elixir-typesonly".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: meta(),
    }
}

/// Drives the mixed-union `OrderStatus` codec through real `elixir`: each literal
/// wins its own declared index over the general `text` arm, an unmatched string falls
/// back to the general arm's index 0, and decoding a literal index validates the
/// payload equals the declared literal rather than trusting the wire. Skips cleanly
/// when `elixir` is absent.
#[test]
fn mixed_union_round_trips_through_elixir() {
    let have = std::process::Command::new("elixir")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no elixir on PATH");
        return;
    }
    let out = process_generation(mixed_union_input()).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-elixir-union-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &out.files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.exs"), MIXED_UNION_DRIVER_ELIXIR).unwrap();

    let run = std::process::Command::new("elixir")
        .arg(dir.join("driver.exs"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "elixir mixed-union round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_UNION_DRIVER_ELIXIR: &str = r#"Code.require_file("codec.gen.ex", __DIR__)
Code.require_file("types.gen.ex", __DIR__)

alias Csilgen.Generated.Order
alias Csilgen.Generated.Cbor

# Declaration order: text=0, pending=1, confirmed=2, processing=3, shipped=4,
# delivered=5, cancelled=6, refunded=7.
literals = ["pending", "confirmed", "processing", "shipped", "delivered", "cancelled", "refunded"]

for {status, idx} <- Enum.with_index(literals, 1) do
  order = struct(Order, status: status)
  {:map, [{{:text, "status"}, value}]} = Order.to_cbor_value(order)
  true = value == {:array, [{:int, idx}, {:text, status}]}

  back = Order.from_cbor(Order.to_cbor(order))
  true = back.status == status
end

# A string matching no literal falls back to the general `text` arm at index 0.
order = struct(Order, status: "on-hold")
{:map, [{{:text, "status"}, value}]} = Order.to_cbor_value(order)
true = value == {:array, [{:int, 0}, {:text, "on-hold"}]}
back = Order.from_cbor(Order.to_cbor(order))
true = back.status == "on-hold"

# Decode validates a literal-index payload against the declared literal: index 1 is
# "pending", so a payload of "confirmed" at index 1 must raise rather than silently
# returning the wrong value.
bad_tree = {:map, [{{:text, "status"}, {:array, [{:int, 1}, {:text, "confirmed"}]}}]}

raised =
  try do
    Order.from_cbor_value(bad_tree)
    false
  rescue
    RuntimeError -> true
  end

true = raised

# Round-tripping every declared index through raw bytes recovers the same tree.
for idx <- 0..7 do
  inner = if idx == 0, do: {:text, "on-hold"}, else: {:text, Enum.at(literals, idx - 1)}
  bytes = Cbor.encode({:map, [{{:text, "status"}, {:array, [{:int, idx}, inner]}}]})
  order = Order.from_cbor(bytes)
  true = is_binary(order.status)
end

IO.puts("ok")
"#;

/// `Status = "pending" / "shipped" / 0 / 1` -- a named choice mixing text and
/// integer literal arms, held by a single-field `Ticket` record. Regression
/// fixture for the mixed-kind literal-enum defect: the old `enum_literal_kind`
/// derived its ONE scalar codec kind from `variants.first()` alone (here,
/// `Text("pending")`), so encode blindly wrapped every value -- including a `0`
/// or `1` -- as `{:text, v.status}`, and decode always read the wire value
/// through `Cbor.to_text`, both silently corrupting the integer arms.
fn mixed_literal_enum_input() -> WasmGeneratorInput {
    let pos = CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    };
    let status = CsilRule {
        name: "Status".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Literal(CsilLiteralValue::Text("pending".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("shipped".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Integer(0)),
            CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
        ])),
        position: pos.clone(),
        doc_comments: vec![],
    };
    let ticket = CsilRule {
        name: "Ticket".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![bare_entry(
                "status",
                CsilTypeExpression::Reference("Status".to_string()),
            )],
        }),
        position: pos,
        doc_comments: vec![],
    };
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![status, ticket],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "elixir-typesonly".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: meta(),
    }
}

/// Pins the shared `classify_choice` contract now flowing through this generator
/// (a mixed-kind-literal choice classifies as an `Enum`, not a `Union` -- no
/// `[index, value]` tagged-sum array here) AND the per-crate fix: encode
/// dispatches each literal to its OWN kind's wire wrap (`{:text, ...}` for the
/// text arms, `{:int, ...}` for the integer arms, not one kind blindly applied
/// to all four), and decode dispatches on the wire's own CBOR tag before
/// validating membership within that tag's declared literals.
#[test]
fn mixed_literal_enum_dispatches_encode_and_decode_per_kind() {
    let out = process_generation(mixed_literal_enum_input()).unwrap();
    let types = file(&out, "types.gen.ex");

    // Not a tagged-sum union: no `{:array, [{:int, i}, ...]}` wrapper for this
    // field at all -- the whole mixed vocabulary is one bare-literal enum.
    assert!(!types.contains(":array,"));

    // Encode: each literal's OWN kind, chosen by runtime equality against the
    // literal (not a blind wrap in one kind derived from the first arm).
    assert!(types.contains("v.status === \"pending\" -> {:text, v.status}"));
    assert!(types.contains("v.status === \"shipped\" -> {:text, v.status}"));
    assert!(types.contains("v.status === 0 -> {:int, v.status}"));
    assert!(types.contains("v.status === 1 -> {:int, v.status}"));
    assert!(types.contains("raise(\"csilgen: value does not match any Status variant\")"));

    // Decode: an outer dispatch on the wire's own CBOR tag, THEN membership
    // validation within that tag's declared literals only.
    assert!(types.contains("{:text, _} ->"));
    assert!(types.contains("{:int, _} ->"));
    assert!(
        types.contains(
            "Csilgen.Generated.Cbor.to_text(Map.fetch!(csil_fields, {:text, \"status\"}))"
        )
    );
    assert!(
        types.contains(
            "Csilgen.Generated.Cbor.to_int(Map.fetch!(csil_fields, {:text, \"status\"}))"
        )
    );
    assert!(types.contains("raise(\"csilgen: unknown Status literal #{inspect(csil_other)}\")"));
}

/// The mixed-kind literal enum's generated output is `mix format
/// --check-formatted` clean. Skips cleanly when `mix` is absent.
#[test]
fn mixed_literal_enum_output_is_mix_format_clean() {
    let have_mix = std::process::Command::new("mix")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_mix {
        eprintln!("skipping: no mix on PATH");
        return;
    }
    let out = process_generation(mixed_literal_enum_input()).unwrap();

    let dir = std::env::temp_dir().join(format!(
        "csilgen-elixir-mixed-enum-fmt-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &out.files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }

    let ex_files: Vec<String> = out
        .files
        .iter()
        .filter(|f| f.path.ends_with(".ex"))
        .map(|f| f.path.clone())
        .collect();
    let mut cmd = std::process::Command::new("mix");
    cmd.arg("format")
        .arg("--check-formatted")
        .args(&ex_files)
        .current_dir(&dir);
    let run = cmd.output().unwrap();
    assert!(
        run.status.success(),
        "mix format --check-formatted rejected generated output:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Drives the mixed-kind literal enum's codec through real `elixir`:
/// round-trips every declared literal of both kinds through raw CBOR bytes,
/// confirms each wire wrap uses its own literal's kind, and confirms decode
/// rejects (a) an out-of-vocabulary value of a declared kind (`2` when only
/// `0`/`1` are declared integers), (b) an out-of-vocabulary value of the other
/// declared kind (`"cancelled"` when only `"pending"`/`"shipped"` are declared
/// text), and (c) a wire kind with no declared literal at all (`bool`). Skips
/// cleanly when `elixir` is absent.
#[test]
fn mixed_literal_enum_round_trips_through_elixir() {
    let have = std::process::Command::new("elixir")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no elixir on PATH");
        return;
    }
    let out = process_generation(mixed_literal_enum_input()).unwrap();

    let dir =
        std::env::temp_dir().join(format!("csilgen-elixir-mixed-enum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &out.files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.exs"), MIXED_LITERAL_ENUM_DRIVER_ELIXIR).unwrap();

    let run = std::process::Command::new("elixir")
        .arg(dir.join("driver.exs"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "elixir mixed-literal-enum round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_LITERAL_ENUM_DRIVER_ELIXIR: &str = r#"Code.require_file("codec.gen.ex", __DIR__)
Code.require_file("types.gen.ex", __DIR__)

alias Csilgen.Generated.Ticket

# Every declared literal of both kinds round-trips through raw CBOR bytes.
for status <- ["pending", "shipped", 0, 1] do
  ticket = struct(Ticket, status: status)
  bytes = Ticket.to_cbor(ticket)
  back = Ticket.from_cbor(bytes)
  true = back.status == status
end

# Each literal's wire wrap uses its OWN kind, not one kind for the whole enum
# (the bug: encode used to blindly wrap every value as {:text, ...}).
{:map, [{{:text, "status"}, {:text, "pending"}}]} =
  Ticket.to_cbor_value(struct(Ticket, status: "pending"))

{:map, [{{:text, "status"}, {:int, 0}}]} = Ticket.to_cbor_value(struct(Ticket, status: 0))

# An out-of-vocabulary value of a declared kind (2 when only 0/1 are declared
# integers) must be rejected, not silently accepted.
bad_int_tree = {:map, [{{:text, "status"}, {:int, 2}}]}

raised_bad_int =
  try do
    Ticket.from_cbor_value(bad_int_tree)
    false
  rescue
    RuntimeError -> true
  end

true = raised_bad_int

# Likewise for the text kind ("cancelled" is not a declared literal).
bad_text_tree = {:map, [{{:text, "status"}, {:text, "cancelled"}}]}

raised_bad_text =
  try do
    Ticket.from_cbor_value(bad_text_tree)
    false
  rescue
    RuntimeError -> true
  end

true = raised_bad_text

# A wire kind with no declared literal at all (bool) must also be rejected.
bad_kind_tree = {:map, [{{:text, "status"}, {:bool, true}}]}

raised_bad_kind =
  try do
    Ticket.from_cbor_value(bad_kind_tree)
    false
  rescue
    RuntimeError -> true
  end

true = raised_bad_kind

IO.puts("ok")
"#;

/// Torture spec exercising: a named all-literal enum (`Grade`) and a named mixed
/// choice (`Level`) whose *last* arm carries a trailing `.default` control
/// operator -- the parser attaches it to that one arm (`Constrained { base_type:
/// Literal, .. }`), which used to fall out of literal classification entirely
/// (both crashed with "no codec for this field shape") -- plus inline (anonymous)
/// choice fields at the record's own field position, as an array element, as a map
/// value, and as a tuple element, mirroring `APIError.error_type` in
/// examples/real-world-api/e-commerce-api.csil.
fn constrained_arm_torture_input() -> WasmGeneratorInput {
    let default_high = |lit: &str| CsilTypeExpression::Constrained {
        base_type: Box::new(CsilTypeExpression::Literal(CsilLiteralValue::Text(
            lit.to_string(),
        ))),
        constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
            "normal".to_string(),
        ))],
    };
    let lit = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));
    let pos = CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    };

    let grade = CsilRule {
        name: "Grade".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            lit("low"),
            default_high("high"),
        ])),
        position: pos.clone(),
        doc_comments: vec![],
    };
    let level = CsilRule {
        name: "Level".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Builtin("text".to_string()),
            lit("low"),
            default_high("high"),
        ])),
        position: pos.clone(),
        doc_comments: vec![],
    };
    let inline_status = CsilTypeExpression::Choice(vec![
        CsilTypeExpression::Builtin("text".to_string()),
        lit("queued"),
        lit("shipped"),
        lit("delivered"),
    ]);
    let inline_priority = CsilTypeExpression::Choice(vec![lit("low"), lit("medium"), lit("high")]);
    let inline_color = CsilTypeExpression::Choice(vec![lit("red"), lit("green"), lit("blue")]);
    let inline_flag = CsilTypeExpression::Choice(vec![lit("on"), lit("off")]);
    let inline_yesno = CsilTypeExpression::Choice(vec![lit("yes"), lit("no")]);

    let torture = CsilRule {
        name: "Torture".to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![
                bare_entry("status", inline_status),
                bare_entry("priority", inline_priority),
                bare_entry(
                    "colors",
                    CsilTypeExpression::Array {
                        element_type: Box::new(inline_color),
                        occurrence: None,
                    },
                ),
                bare_entry(
                    "flags",
                    CsilTypeExpression::Map {
                        key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                        value: Box::new(inline_flag),
                        occurrence: None,
                    },
                ),
                bare_entry(
                    "pair",
                    CsilTypeExpression::Tuple(CsilGroupExpression {
                        entries: vec![
                            CsilGroupEntry {
                                key: None,
                                value_type: CsilTypeExpression::Builtin("text".to_string()),
                                occurrence: None,
                                metadata: vec![],
                                doc_comments: vec![],
                            },
                            CsilGroupEntry {
                                key: None,
                                value_type: inline_yesno,
                                occurrence: None,
                                metadata: vec![],
                                doc_comments: vec![],
                            },
                        ],
                    }),
                ),
                bare_entry("grade", CsilTypeExpression::Reference("Grade".to_string())),
                bare_entry("level", CsilTypeExpression::Reference("Level".to_string())),
            ],
        }),
        position: pos,
        doc_comments: vec![],
    };

    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![grade, level, torture],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "elixir-typesonly".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: meta(),
    }
}

/// Drives the constrained-last-arm and inline-choice-field contract through real
/// `elixir`. Skips cleanly when `elixir` is absent.
#[test]
fn constrained_arm_and_inline_choice_round_trip_through_elixir() {
    let have = std::process::Command::new("elixir")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no elixir on PATH");
        return;
    }
    let out = process_generation(constrained_arm_torture_input()).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-elixir-torture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &out.files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.exs"), TORTURE_DRIVER_ELIXIR).unwrap();

    let run = std::process::Command::new("elixir")
        .arg(dir.join("driver.exs"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "elixir torture round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const TORTURE_DRIVER_ELIXIR: &str = r#"Code.require_file("codec.gen.ex", __DIR__)
Code.require_file("types.gen.ex", __DIR__)

alias Csilgen.Generated.Torture
alias Csilgen.Generated.Cbor

sample =
  struct(Torture,
    status: "queued",
    priority: "medium",
    colors: ["red", "blue"],
    flags: %{"a" => "on", "b" => "off"},
    pair: {"hi", "yes"},
    grade: "low",
    level: "low"
  )

# Grade is a closed literal enum (with a `.default`-suffixed last arm): bare-text
# wire, not a tagged-sum array.
{:map, kvs} = Torture.to_cbor_value(sample)
fields = Map.new(kvs)
true = fields[{:text, "grade"}] == {:text, "low"}

back = Torture.from_cbor(Torture.to_cbor(sample))
true = back == sample

# Level's constrained last arm ("high" .default "normal") keeps its own declared
# index (2) with literal-equality validation, and the general `text` arm (index 0)
# stays reachable for values that aren't "low"/"high".
for {level, want_idx} <- [{"low", 1}, {"high", 2}, {"other", 0}] do
  v = %{sample | level: level}
  {:map, kvs} = Torture.to_cbor_value(v)
  fields = Map.new(kvs)
  true = fields[{:text, "level"}] == {:array, [{:int, want_idx}, {:text, level}]}
  back = Torture.from_cbor(Torture.to_cbor(v))
  true = back.level == level
end

# Decode rejects a wrong-literal payload at a literal's declared index.
bad = {:map, [
  {{:text, "status"}, {:array, [{:int, 1}, {:text, "queued"}]}},
  {{:text, "priority"}, {:text, "low"}},
  {{:text, "colors"}, {:array, []}},
  {{:text, "flags"}, {:map, []}},
  {{:text, "pair"}, {:array, [{:text, "hi"}, {:text, "yes"}]}},
  {{:text, "grade"}, {:text, "low"}},
  {{:text, "level"}, {:array, [{:int, 2}, {:text, "not-high"}]}}
]}

raised =
  try do
    Torture.from_cbor_value(bad)
    false
  rescue
    RuntimeError -> true
  end

true = raised

# Grade decode validates enum membership: an unknown literal must raise, not
# silently pass through.
bad_grade = {:map, [
  {{:text, "status"}, {:array, [{:int, 1}, {:text, "queued"}]}},
  {{:text, "priority"}, {:text, "low"}},
  {{:text, "colors"}, {:array, []}},
  {{:text, "flags"}, {:map, []}},
  {{:text, "pair"}, {:array, [{:text, "hi"}, {:text, "yes"}]}},
  {{:text, "grade"}, {:text, "unknown"}},
  {{:text, "level"}, {:array, [{:int, 1}, {:text, "low"}]}}
]}

raised_grade =
  try do
    Torture.from_cbor_value(bad_grade)
    false
  rescue
    RuntimeError -> true
  end

true = raised_grade

# Inline all-literal choices (priority / array element / map value / tuple
# element) ride the wire as the bare literal, same as a named enum, and reject an
# unknown literal on decode.
true = fields[{:text, "priority"}] == {:text, "medium"}
true = fields[{:text, "colors"}] == {:array, [{:text, "red"}, {:text, "blue"}]}
true = fields[{:text, "flags"}] ==
         {:map, [{{:text, "a"}, {:text, "on"}}, {{:text, "b"}, {:text, "off"}}]}
true = fields[{:text, "pair"}] == {:array, [{:text, "hi"}, {:text, "yes"}]}

bad_tree = {:map, [
  {{:text, "status"}, {:array, [{:int, 1}, {:text, "queued"}]}},
  {{:text, "priority"}, {:text, "unknown"}},
  {{:text, "colors"}, {:array, []}},
  {{:text, "flags"}, {:map, []}},
  {{:text, "pair"}, {:array, [{:text, "hi"}, {:text, "yes"}]}},
  {{:text, "grade"}, {:text, "low"}},
  {{:text, "level"}, {:array, [{:int, 1}, {:text, "low"}]}}
]}

raised_priority =
  try do
    Torture.from_cbor_value(bad_tree)
    false
  rescue
    RuntimeError -> true
  end

true = raised_priority

IO.puts("ok")
"#;

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
    assert!(on.files.iter().any(|f| f.path == "genquickstart.md"));

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
    assert!(!off.files.iter().any(|f| f.path == "genquickstart.md"));
    // The rest of the publishable package is unchanged.
    assert!(off.files.iter().any(|f| f.path == "mix.exs"));
    let on_without_readme: Vec<_> = on
        .files
        .iter()
        .filter(|f| f.path != "genquickstart.md")
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

/// A package-mode input with a `->` op AND a record-typed `<->` op, so all three
/// genquickstart sections render their full library-based examples (the unary op feeds
/// RPC + Datagrams; the channel op feeds Events dispatch).
fn transports_package_input() -> WasmGeneratorInput {
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
    let success = CsilTypeExpression::Choice(vec![
        CsilTypeExpression::Reference("user".to_string()),
        CsilTypeExpression::Reference("ServiceError".to_string()),
    ]);
    let service = CsilRule {
        name: "user_service".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![
                make_op(
                    "get-user",
                    "user",
                    success.clone(),
                    CsilServiceDirection::Unidirectional,
                    Some(7),
                ),
                make_op(
                    "watch-user",
                    "user",
                    success,
                    CsilServiceDirection::Bidirectional,
                    Some(3),
                ),
            ],
            wire_id: Some(1),
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

/// Hermetic verification that the `genquickstart.md` transport carriers are not just
/// well-formed strings but actually compile and run against the real `csilgen_transport`
/// library and the package's own generated codec. The genquickstart names both the
/// client surface (RPC) and the server surface (Events router), so this stages both.
///
/// What it proves, end to end, under the real BEAM:
/// - CSIL-RPC: the emitted `CsilRpcHttpCarrier` compiles; a typed request round-trips
///   through the generated codec + the library's RPC request/response envelope, with an
///   in-process echo standing in for the HTTP carrier.
/// - CSIL-Datagrams: the emitted `UdpDatagramCarrier` is run for real over a localhost
///   UDP echo — `:gen_udp` open/send/recv and `Datagrams` encode/decode included — and the
///   typed value round-trips through the generated codec.
/// - CSIL-Events: the interactive TLS session (`TlsFrameCarrier` + handshake + heartbeat +
///   router dispatch) is compile-checked rather than dialed (no live peer).
///
/// The transport library is loaded from its `lib/` sources (no `mix`/`_build` needed), so
/// the only external requirement is `elixir` on PATH; the test skips cleanly without it.
#[test]
fn genquickstart_carriers_run_and_compile() {
    use std::process::Command;
    if Command::new("elixir").arg("--version").output().is_err() {
        eprintln!("skipping: no elixir on PATH");
        return;
    }

    // The genquickstart RPC example drives the client surface; its Events example drives
    // the server router surface. Package mode emits BOTH surfaces into the one package, so
    // a SINGLE generation of the genquickstart's own target stages everything the three
    // sections reference — no second generation, mirroring the OCaml reference.
    let out = process_generation(transports_package_input()).unwrap();
    assert!(
        out.files.iter().any(|f| f.path == "lib/client.gen.ex"),
        "package mode must emit the client surface for the RPC section"
    );
    assert!(
        out.files.iter().any(|f| f.path == "lib/server.gen.ex"),
        "package mode must emit the server router surface for the Events section"
    );

    let dir = std::env::temp_dir().join(format!("csilgen-elixir-genqs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let pkg = dir.join("pkg");
    for f in &out.files {
        let p = pkg.join(&f.path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, &f.content).unwrap();
    }

    // The transport library lives at the repo root, two levels up from this crate.
    let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../transports/elixir/lib/csilgen");
    assert!(
        lib.join("transport/rpc.ex").exists(),
        "transport lib sources not found at {}",
        lib.display()
    );

    let harness = dir.join("verify.exs");
    std::fs::write(&harness, GENQUICKSTART_HARNESS_ELIXIR).unwrap();

    let run = Command::new("elixir")
        .arg(&harness)
        .arg(&pkg)
        .arg(&lib)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success()
            && stdout.contains("EVENTS_COMPILE_OK")
            && stdout.contains("RPC_RUN_OK")
            && stdout.contains("DG_RUN_OK"),
        "genquickstart carrier verification failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Elixir driver for `genquickstart_carriers_run_and_compile`. Argv is `[pkg, lib]`:
/// the staged package directory and the transport library's `lib/csilgen` source root.
/// It loads the library + generated modules, extracts the three transport code blocks
/// from `genquickstart.md`, runs RPC + Datagrams, and compile-checks Events. Structs are
/// built with `struct/2` (not `%Mod{}` literals) because the script is one compile unit
/// and the generated modules only exist after `Code.require_file` runs.
const GENQUICKSTART_HARNESS_ELIXIR: &str = r#"[pkg, lib] = System.argv()

for f <- ~w(transport/cbor.ex transport/status.ex transport/conventions.ex transport/carrier.ex transport/rpc.ex transport/datagrams.ex transport/events.ex transport.ex) do
  Code.require_file(Path.join(lib, f))
end

# Codec before the types whose to_cbor/from_cbor call it; then client + server.
Code.require_file(Path.join([pkg, "lib", "codec.gen.ex"]))
Code.require_file(Path.join([pkg, "lib", "types.gen.ex"]))
Code.require_file(Path.join([pkg, "lib", "client.gen.ex"]))
Code.require_file(Path.join([pkg, "lib", "server.gen.ex"]))

md = File.read!(Path.join(pkg, "genquickstart.md"))

blocks =
  Regex.scan(~r/```elixir\n(.*?)```/s, md)
  |> Enum.map(fn [_, b] -> b end)

rpc = Enum.find(blocks, &String.contains?(&1, "CsilRpcHttpCarrier"))
events = Enum.find(blocks, &String.contains?(&1, "TlsFrameCarrier"))
datagrams = Enum.find(blocks, &String.contains?(&1, "UdpDatagramCarrier"))

# --- Events: compile-check the carrier + session (no live peer to dial) ---
[events_mods, _] = String.split(events, "\nEventsSession.run(", parts: 2)
Code.compile_string(events_mods)
IO.puts("EVENTS_COMPILE_OK")

# --- RPC: compile the emitted carrier, then run the typed round-trip through an
#     in-process echo standing in for the HTTP carrier (lib RPC envelope included) ---
[rpc_carrier, _] = String.split(rpc, "\ntransport = CsilRpcHttpCarrier.new", parts: 2)
Code.compile_string(rpc_carrier)
IO.puts("RPC_CARRIER_COMPILE_OK")

defmodule EchoRpc do
  @behaviour Csilgen.Generated.Transport
  alias Csilgen.Transport.RPC
  defstruct []

  @impl true
  def call(%__MODULE__{}, service, op, req) when is_binary(req) do
    # Build and parse the request through the library envelope the emitted carrier uses,
    # then echo the request payload back as an ok response the typed client decodes.
    envelope = RPC.encode_request(RPC.new_request(service, op, req))
    {:ok, dreq} = RPC.decode_request(envelope)
    body = RPC.encode_response(RPC.new_response_ok("User", dreq.payload))
    {:ok, resp} = RPC.decode_response(body)
    :ok = RPC.as_transport_error(resp)
    resp.payload
  end
end

rpc_req = struct(Csilgen.Generated.User, name: "example", id: 7)
rpc_client = Csilgen.Generated.UserClient.new(struct(EchoRpc, []))
^rpc_req = Csilgen.Generated.UserClient.get_user(rpc_client, rpc_req)
IO.puts("RPC_RUN_OK")

# --- Datagrams: run the emitted :gen_udp carrier for real over a localhost UDP echo ---
[dg_carrier, _] = String.split(datagrams, "\nalias Csilgen.Transport.Datagrams", parts: 2)
Code.compile_string(dg_carrier)
IO.puts("DG_CARRIER_COMPILE_OK")

alias Csilgen.Transport.Datagrams

{:ok, srv} = :gen_udp.open(0, [:binary, active: false])
{:ok, srv_port} = :inet.port(srv)

spawn(fn ->
  {:ok, {addr, port, data}} = :gen_udp.recv(srv, 0, 5000)
  :gen_udp.send(srv, addr, port, data)
end)

{:ok, carrier} = UdpDatagramCarrier.open("localhost", srv_port)
dg_req = struct(Csilgen.Generated.User, name: "example", id: 9)
payload = Csilgen.Generated.User.to_cbor(dg_req)
datagram = Datagrams.encode_datagram(Datagrams.new_datagram(7, 0, payload))
{:ok, carrier} = UdpDatagramCarrier.send_datagram(carrier, datagram)

case UdpDatagramCarrier.recv_datagram(carrier) do
  {:ok, bytes, _carrier} ->
    {:ok, dg} = Datagrams.decode_datagram(bytes)
    ^dg_req = Csilgen.Generated.User.from_cbor(dg.payload)
    IO.puts("DG_RUN_OK")

  :empty ->
    raise "datagram echo did not arrive"
end
"#;

#[test]
fn genquickstart_intro_credits_transport_lib() {
    let out = process_generation(transports_package_input()).unwrap();
    let body = file(&out, "genquickstart.md");

    // Title + a deps install hint naming this package and the transport library.
    assert!(body.starts_with("# csilgen_client\n"));
    assert!(body.contains("{:csilgen_client,"));
    assert!(body.contains("csilgen_transport"));
    assert!(body.contains("not yet published"));
    // The intro credits the library for the envelope/framing/lifecycle and the
    // carrier-only contribution.
    assert!(body.contains("`csilgen_transport` library owns the"));
    assert!(body.contains("*carrier*"));
}

#[test]
fn genquickstart_rpc_section_uses_lib_envelope_over_http() {
    let out = process_generation(transports_package_input()).unwrap();
    let body = file(&out, "genquickstart.md");

    assert!(body.contains("## CSIL-RPC (HTTP)"));
    // The carrier implements the generated transport seam (the behaviour).
    assert!(body.contains("defmodule CsilRpcHttpCarrier do"));
    assert!(body.contains("@behaviour Csilgen.Generated.Transport"));
    assert!(body.contains("def call(%__MODULE__{rpc_url: url}, service, op, req)"));

    // The envelope is the library's RPC request/response — never hand-rolled.
    assert!(body.contains("alias Csilgen.Transport.RPC"));
    assert!(body.contains("RPC.encode_request(RPC.new_request(service, op, req))"));
    assert!(body.contains("RPC.decode_response(body)"));
    assert!(body.contains("RPC.as_transport_error(resp)"));

    // It POSTs to the CSIL-RPC endpoint over a plain :gen_tcp HTTP client (in
    // :kernel — runs under mix/releases with no extra_applications), not :inets/:httpc.
    assert!(body.contains("/csil/v1/rpc"));
    assert!(body.contains(":gen_tcp.connect("));
    assert!(body.contains("POST #{path} HTTP/1.1"));
    assert!(!body.contains(":httpc"));
    assert!(!body.contains(":inets"));

    // The typed ServiceError application arm is handled distinctly.
    assert!(body.contains("resp.variant == \"ServiceError\""));

    // Client construction over the carrier + the first unary call with a generated
    // sample struct literal (required fields only, struct field atoms).
    assert!(body.contains("transport = CsilRpcHttpCarrier.new(\"http://localhost:5080\")"));
    assert!(body.contains("client = Csilgen.Generated.UserClient.new(transport)"));
    assert!(body.contains(
        "resp = Csilgen.Generated.UserClient.get_user(client, %Csilgen.Generated.User{name: \"example\", id: 0})"
    ));
}

#[test]
fn genquickstart_events_section_handshake_and_router_dispatch() {
    let out = process_generation(transports_package_input()).unwrap();
    let body = file(&out, "genquickstart.md");

    assert!(body.contains("## CSIL-Events (TLS)"));
    // A TLS frame carrier built on the library's length-prefix framing.
    assert!(body.contains("defmodule TlsFrameCarrier do"));
    assert!(body.contains("@behaviour Csilgen.Transport.Carrier"));
    assert!(body.contains("Carrier.frame_length_prefixed(frame)"));
    assert!(body.contains("Carrier.read_length_prefixed(c.buffer)"));
    assert!(body.contains(":ssl.connect("));

    // The $hello / $hello-ack handshake via the library control plane.
    assert!(body.contains("Events.encode_hello(%Hello{versions: [1], profiles: [\"verbose\"]"));
    assert!(body.contains("Events.parse_profile(profile_name)"));
    // The $ping / $pong heartbeat answered with the library Heartbeat.
    assert!(body.contains("event: \"$ping\""));
    assert!(body.contains("Events.encode_heartbeat(%Heartbeat{nonce: nonce})"));

    // Dispatch into the generated server router + one outbound event via the encoder.
    assert!(body.contains("Csilgen.Generated.UserServer.encode_watch_user(ExampleCodec,"));
    assert!(
        body.contains("Csilgen.Generated.UserServer.route(ExampleHandler, ExampleCodec, ev.event")
    );
    assert!(body.contains("@behaviour Csilgen.Generated.UserServer"));
    assert!(body.contains("def watch_user(msg, _ctx)"));
    // The Codec seam is backed by the generated per-type helpers.
    assert!(body.contains("@behaviour Csilgen.Generated.Codec"));
    assert!(body.contains("def encode(value), do: value.__struct__.to_cbor(value)"));
    assert!(body.contains("def decode(data, type), do: type.from_cbor(data)"));
}

#[test]
fn genquickstart_datagrams_section_send_and_late_response() {
    let out = process_generation(transports_package_input()).unwrap();
    let body = file(&out, "genquickstart.md");

    assert!(body.contains("## CSIL-Datagrams (UDP)"));
    // A UDP datagram carrier over the stdlib :gen_udp.
    assert!(body.contains("defmodule UdpDatagramCarrier do"));
    assert!(body.contains(":gen_udp.open("));
    assert!(body.contains(":gen_udp.send("));

    // Encode the `->` request via the generated codec, wrap in the library's Datagram.
    assert!(body.contains("alias Csilgen.Transport.Datagrams"));
    assert!(body.contains("op_ord = 7"));
    assert!(body.contains("payload = Csilgen.Generated.User.to_cbor(req)"));
    assert!(body.contains("Datagrams.encode_datagram(Datagrams.new_datagram(op_ord, 0, payload))"));

    // The recv path decodes a late datagram into the RESPONSE type, with the "may
    // arrive later — or never" caveat.
    assert!(body.contains("Datagrams.decode_datagram(bytes)"));
    assert!(body.contains("resp = Csilgen.Generated.User.from_cbor(dg.payload)"));
    assert!(body.contains("MAY arrive later"));
    assert!(body.contains("synchronous response"));
}

#[test]
fn genquickstart_transports_option_selects_sections() {
    // A subset names only events: the RPC and Datagrams sections are suppressed.
    let mut input = transports_package_input();
    input.config.options.insert(
        "genquickstart_transports".to_string(),
        serde_json::json!(["events"]),
    );
    let out = process_generation(input).unwrap();
    let body = file(&out, "genquickstart.md");

    assert!(body.contains("## CSIL-Events (TLS)"));
    assert!(!body.contains("## CSIL-RPC (HTTP)"));
    assert!(!body.contains("## CSIL-Datagrams (UDP)"));
}

#[test]
fn genquickstart_no_channel_op_shows_handshake_note() {
    // The single-unary-op spec has no `<->` op: the Events section still shows the
    // handshake + heartbeat, with a note where the dispatch would go.
    let out = process_generation(readme_package_input()).unwrap();
    let body = file(&out, "genquickstart.md");

    assert!(body.contains("## CSIL-Events (TLS)"));
    assert!(body.contains("Events.encode_hello(%Hello{versions: [1], profiles: [\"verbose\"]})"));
    assert!(body.contains("no generated channel router"));
    // No router/handler dispatch is wired without a channel op.
    assert!(!body.contains("ExampleHandler"));
}

#[test]
fn package_readme_absent_without_package_mode() {
    // The flat (non-package) layout never ships a README.
    let mut input = transports_package_input();
    input.config.options = HashMap::new();
    let out = process_generation(input).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "genquickstart.md"));
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

#[test]
fn non_record_op_boundaries_get_client_methods() {
    let pos = || CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    };
    let alias = |name: &str| CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Builtin("text".to_string())),
        position: pos(),
        doc_comments: vec![],
    };
    let record = |name: &str, entries: Vec<CsilGroupEntry>| CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: vec![],
    };
    let arr = CsilTypeExpression::Array {
        element_type: Box::new(CsilTypeExpression::Reference("Member".to_string())),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    };
    let map = CsilTypeExpression::Map {
        key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
        value: Box::new(CsilTypeExpression::Builtin("text".to_string())),
        occurrence: None,
    };
    let ops = vec![
        // record -> record (the only shape the old filter kept)
        make_op(
            "create-member",
            "Member",
            CsilTypeExpression::Reference("Member".to_string()),
            CsilServiceDirection::Unidirectional,
            None,
        ),
        // scalar-id request -> record response
        make_op(
            "get-member",
            "MemberID",
            CsilTypeExpression::Reference("Member".to_string()),
            CsilServiceDirection::Unidirectional,
            None,
        ),
        // record request -> bare-array response
        make_op(
            "list-members",
            "ListMembersRequest",
            arr,
            CsilServiceDirection::Unidirectional,
            None,
        ),
        // scalar-id request -> scalar response
        make_op(
            "delete-task",
            "TaskID",
            CsilTypeExpression::Builtin("bool".to_string()),
            CsilServiceDirection::Unidirectional,
            None,
        ),
        // record request -> map response
        make_op(
            "member-names",
            "ListMembersRequest",
            map,
            CsilServiceDirection::Unidirectional,
            None,
        ),
    ];
    let mut input = service_input("MemberService", ops, None, "elixir-client");
    let rules = &mut input.csil_spec.rules;
    rules.insert(0, alias("MemberID"));
    rules.insert(1, alias("TaskID"));
    rules.insert(
        2,
        record(
            "Member",
            vec![
                bare_entry("id", CsilTypeExpression::Reference("MemberID".to_string())),
                bare_entry("name", CsilTypeExpression::Builtin("text".to_string())),
            ],
        ),
    );
    rules.insert(
        3,
        record(
            "ListMembersRequest",
            vec![optional_entry(
                "limit",
                CsilTypeExpression::Builtin("uint".to_string()),
            )],
        ),
    );

    let out = process_generation(input).unwrap();
    let client = file(&out, "client.gen.ex");

    // Every op gets a method now — scalar-id request, bare-array and scalar/map responses included.
    assert!(client.contains("def create_member(%__MODULE__{transport: transport}, req) do"));
    assert!(client.contains("def get_member(%__MODULE__{transport: transport}, req) do"));
    assert!(client.contains("def list_members(%__MODULE__{transport: transport}, req) do"));
    assert!(client.contains("def delete_task(%__MODULE__{transport: transport}, req) do"));
    assert!(client.contains("def member_names(%__MODULE__{transport: transport}, req) do"));
    // No op is dropped with a note anymore.
    assert!(!client.contains("handle it manually"));
    assert!(!client.contains("non-record payload"));
    // Record boundary keeps its module's to_cbor/from_cbor wrappers, byte-for-byte unchanged.
    assert!(client.contains("Csilgen.Generated.Member.to_cbor(req)"));
    assert!(client.contains("Csilgen.Generated.Member.from_cbor(resp)"));
    // Non-record boundaries ride per-op helpers over the shared value codec.
    assert!(client.contains(concat!(
        "  defp encode_get_member_request(req) do\n",
        "    Csilgen.Generated.Cbor.encode({:text, req})\n",
        "  end"
    )));
    assert!(client.contains("defp decode_list_members_response(csil_bytes) do"));
    assert!(client.contains("defp decode_delete_task_response(csil_bytes) do"));
    assert!(client.contains("decode_member_names_response(resp)"));
    // Non-record response @specs map to their real shapes, not a record `.t()`. The
    // list_members spec overflows 98 cols flat, so mix wraps after `::`.
    assert!(client.contains(concat!(
        "@spec list_members(t(), Csilgen.Generated.ListMembersRequest.t()) ::\n",
        "          [Csilgen.Generated.Member.t()]"
    )));
    assert!(client.contains(":: boolean()"));
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
