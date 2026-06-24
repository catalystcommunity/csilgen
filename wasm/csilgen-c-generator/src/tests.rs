//! In-module codegen tests: these call the private generation functions directly
//! (the Go generator's in-lib test style) and assert on the emitted C substrings.

use super::*;
use csilgen_common::{
    CsilGroupKey, CsilPosition, CsilRule, CsilServiceOperation, CsilSpecSerialized,
    GeneratorConfig, GeneratorMetadata,
};

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn metadata() -> GeneratorMetadata {
    GeneratorMetadata {
        name: "c-generator".to_string(),
        version: "0.1.0".to_string(),
        description: String::new(),
        target: "c".to_string(),
        capabilities: vec![],
        author: None,
        homepage: None,
    }
}

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
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

fn input_with_rules(
    rules: Vec<CsilRule>,
    target: &str,
    options: HashMap<String, serde_json::Value>,
) -> WasmGeneratorInput {
    let service_count = rules
        .iter()
        .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
        .count();
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules,
            source_content: None,
            service_count,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: metadata(),
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

fn op(
    name: &str,
    input: CsilTypeExpression,
    output: CsilTypeExpression,
    direction: CsilServiceDirection,
    wire_id: Option<u64>,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: input,
        output_type: output,
        direction,
        position: pos(),
        doc_comments: vec![],
        wire_id,
    }
}

fn service_rule(
    name: &str,
    operations: Vec<CsilServiceOperation>,
    wire_id: Option<u64>,
) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations,
            wire_id,
        }),
        position: pos(),
        doc_comments: vec![],
    }
}

#[test]
fn basic_struct_emits_typedef_and_fields() {
    let input = input_with_rules(
        vec![group_rule(
            "User",
            vec![
                bare_entry("name", builtin("text")),
                bare_entry("issued_at", builtin("uint")),
            ],
        )],
        "c",
        HashMap::new(),
    );
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).expect("types emitted");
    assert!(types.contains("typedef struct User {"));
    assert!(types.contains("} User;"));
    // snake_case field names map verbatim (wire keys stay verbatim).
    assert!(types.contains("char *name;"));
    assert!(types.contains("uint64_t issued_at;"));
    assert!(types.contains("#ifndef CSILGEN_TYPES_GEN_H"));
}

#[test]
fn optional_scalar_becomes_pointer() {
    let mut entry = bare_entry("count", builtin("uint"));
    entry.occurrence = Some(CsilOccurrence::Optional);
    let input = input_with_rules(vec![group_rule("Bag", vec![entry])], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).unwrap();
    assert!(types.contains("uint64_t *count;"));
}

#[test]
fn array_field_expands_to_pointer_and_count() {
    let entry = bare_entry(
        "tags",
        CsilTypeExpression::Array {
            element_type: Box::new(builtin("text")),
            occurrence: None,
        },
    );
    let input = input_with_rules(vec![group_rule("Post", vec![entry])], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).unwrap();
    assert!(types.contains("char **tags;"));
    assert!(types.contains("size_t tags_count;"));
}

#[test]
fn bytes_decimal_timestamp_map_to_helpers() {
    let input = input_with_rules(
        vec![group_rule(
            "Rec",
            vec![
                bare_entry("blob", builtin("bytes")),
                bare_entry("amount", builtin("decimal")),
                bare_entry("when", builtin("timestamp")),
            ],
        )],
        "c",
        HashMap::new(),
    );
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).unwrap();
    assert!(types.contains("CsilBytes blob;"));
    assert!(types.contains("CsilDecimal amount;"));
    assert!(types.contains("CsilTimestamp when;"));
    // The conditional helper headers are pulled in.
    assert!(types.contains("#include \"csil_decimal.gen.h\""));
    assert!(types.contains("#include \"csil_timestamp.gen.h\""));
}

#[test]
fn decimal_helper_file_emitted_only_under_csil_mapping() {
    let rules = vec![group_rule(
        "Rec",
        vec![bare_entry("amount", builtin("decimal"))],
    )];
    let csil = process_generation(input_with_rules(rules.clone(), "c", HashMap::new())).unwrap();
    assert!(csil.files.iter().any(|f| f.path == "csil_decimal.gen.h"));

    let mut opts = HashMap::new();
    opts.insert("decimal_mapping".to_string(), serde_json::json!("library"));
    let lib = process_generation(input_with_rules(rules, "c", opts)).unwrap();
    assert!(!lib.files.iter().any(|f| f.path == "csil_decimal.gen.h"));
}

#[test]
fn type_alias_emitted() {
    let rule = CsilRule {
        name: "UserId".to_string(),
        rule_type: CsilRuleType::TypeDef(builtin("uint")),
        position: pos(),
        doc_comments: vec![],
    };
    let input = input_with_rules(vec![rule], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).unwrap();
    assert!(types.contains("typedef uint64_t UserId;"));
}

#[test]
fn text_literal_choice_is_an_enum() {
    let rule = CsilRule {
        name: "Color".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            CsilTypeExpression::Literal(CsilLiteralValue::Text("red".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("green".to_string())),
        ]),
        position: pos(),
        doc_comments: vec![],
    };
    let input = input_with_rules(vec![rule], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).unwrap();
    assert!(types.contains("typedef enum Color {"));
    assert!(types.contains("COLOR_RED,"));
    assert!(types.contains("COLOR_GREEN,"));
}

#[test]
fn reference_choice_is_a_tagged_union() {
    let rule = CsilRule {
        name: "DepositResult".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            CsilTypeExpression::Reference("ClaimResponse".to_string()),
            CsilTypeExpression::Reference("ServiceError".to_string()),
        ]),
        position: pos(),
        doc_comments: vec![],
    };
    let input = input_with_rules(vec![rule], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let types = generate_types(&input, &config).unwrap();
    assert!(types.contains("typedef enum DepositResultTag {"));
    assert!(types.contains("DEPOSIT_RESULT_CLAIM_RESPONSE,"));
    assert!(types.contains("ClaimResponse claim_response;"));
    assert!(types.contains("DepositResultTag tag;"));
}

#[test]
fn client_emits_prefixed_calls_with_verbatim_wire_names() {
    let svc = service_rule(
        "AttestationService",
        vec![op(
            "deposit-claim",
            CsilTypeExpression::Reference("DepositClaimRequest".to_string()),
            CsilTypeExpression::Reference("DepositClaimResponse".to_string()),
            CsilServiceDirection::Unidirectional,
            None,
        )],
        None,
    );
    let input = input_with_rules(vec![svc], "c-client", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let client = generate_client(&input, &config).expect("client emitted");
    // kebab -> snake for the C symbol; service base lowercased and op PascalCased
    // for the wire strings.
    assert!(client.contains("csil_attestation_deposit_claim("));
    assert!(client.contains("\"attestation\", \"DepositClaim\""));
    assert!(client.contains("CsilgenTransport"));
}

#[test]
fn server_emits_handlers_wire_ids_and_compact_router() {
    let svc = service_rule(
        "ChatService",
        vec![
            op(
                "say",
                CsilTypeExpression::Reference("SayMessage".to_string()),
                builtin("nil"),
                CsilServiceDirection::Bidirectional,
                Some(0),
            ),
            op(
                "ping",
                builtin("nil"),
                CsilTypeExpression::Reference("Pong".to_string()),
                CsilServiceDirection::Unidirectional,
                Some(1),
            ),
        ],
        Some(7),
    );
    let input = input_with_rules(vec![svc], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let mut warnings = Vec::new();
    let server = generate_server(&input, &config, &mut warnings).expect("server emitted");
    assert!(server.contains("typedef struct ChatHandlers {"));
    // Bidirectional handler is fire-and-forget; unidirectional has a response out.
    assert!(server.contains("int (*say)(void *ctx, const SayMessage *msg);"));
    assert!(server.contains("int (*ping)(void *ctx, Pong *resp);"));
    assert!(server.contains("#define CHAT_SERVICE_WIRE_ID 7u"));
    assert!(server.contains("#define CHAT_OP_SAY_WIRE_ID 0u"));
    // Verbose + compact router twins.
    assert!(server.contains("route_chat_channel("));
    assert!(server.contains("route_chat_channel_compact("));
    assert!(server.contains("\"Say\""));
    assert!(server.contains("case 0u:"));
}

#[test]
fn compact_router_absent_without_wire_id() {
    let svc = service_rule(
        "ChatService",
        vec![op(
            "say",
            CsilTypeExpression::Reference("SayMessage".to_string()),
            builtin("nil"),
            CsilServiceDirection::Bidirectional,
            None,
        )],
        None,
    );
    let input = input_with_rules(vec![svc], "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let mut warnings = Vec::new();
    let server = generate_server(&input, &config, &mut warnings).unwrap();
    assert!(server.contains("route_chat_channel("));
    assert!(!server.contains("route_chat_channel_compact("));
    assert!(!server.contains("WIRE_ID"));
}

#[test]
fn validation_emits_predicate_for_constraints() {
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("name".to_string())),
        value_type: builtin("text"),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinLength(3),
        )],
        doc_comments: vec![],
    };
    let input = input_with_rules(vec![group_rule("User", vec![entry])], "c", HashMap::new());
    let validation = generate_validation(&input).expect("validation emitted");
    assert!(validation.contains("static inline bool User_validate(const User *v)"));
    assert!(validation.contains("strlen(v->name) < 3u"));
}

#[test]
fn validation_omitted_without_checks() {
    let input = input_with_rules(
        vec![group_rule(
            "User",
            vec![bare_entry("name", builtin("text"))],
        )],
        "c",
        HashMap::new(),
    );
    assert!(generate_validation(&input).is_none());
}

#[test]
fn typesonly_omits_service_surface() {
    let svc = service_rule(
        "ChatService",
        vec![op(
            "ping",
            builtin("nil"),
            CsilTypeExpression::Reference("Pong".to_string()),
            CsilServiceDirection::Unidirectional,
            None,
        )],
        None,
    );
    let out =
        process_generation(input_with_rules(vec![svc], "c-typesonly", HashMap::new())).unwrap();
    assert!(!out.files.iter().any(|f| f.path == "server.gen.h"));
    assert!(!out.files.iter().any(|f| f.path == "client.gen.h"));
}

#[test]
fn unknown_subtarget_is_a_hard_error() {
    let input = input_with_rules(
        vec![group_rule(
            "User",
            vec![bare_entry("name", builtin("text"))],
        )],
        "c-bogus",
        HashMap::new(),
    );
    assert!(process_generation(input).is_err());
}

#[test]
fn unknown_decimal_mapping_is_a_hard_error() {
    let mut opts = HashMap::new();
    opts.insert("decimal_mapping".to_string(), serde_json::json!("nonsense"));
    let input = input_with_rules(
        vec![group_rule(
            "User",
            vec![bare_entry("name", builtin("text"))],
        )],
        "c",
        opts,
    );
    assert!(process_generation(input).is_err());
}

#[test]
fn metadata_advertises_c_target() {
    let ptr = get_metadata();
    assert!(!ptr.is_null());
    // The first four bytes are the JSON length; decode and parse it.
    let len = unsafe { std::ptr::read(ptr as *const u32) } as usize;
    let json = unsafe { std::slice::from_raw_parts(ptr.add(4), len) };
    let meta: GeneratorMetadata = serde_json::from_slice(json).unwrap();
    assert_eq!(meta.target, "c");
}
