//! Unit tests for the Swift generator's emitter functions.

use super::*;
use csilgen_common::{
    CsilGroupExpression, CsilPosition, CsilRule, CsilServiceOperation, CsilSpecSerialized,
    GeneratorConfig,
};
use std::collections::HashMap;

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn input_from_rules(
    target: &str,
    rules: Vec<CsilRule>,
    service_count: usize,
) -> WasmGeneratorInput {
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
            options: HashMap::new(),
        },
        generator_metadata: GeneratorMetadata {
            name: "swift".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            target: "swift".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        },
    }
}

fn bare_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

fn opt_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    let mut e = bare_entry(name, value_type);
    e.occurrence = Some(CsilOccurrence::Optional);
    e
}

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn make_op(
    name: &str,
    input: CsilTypeExpression,
    output: CsilTypeExpression,
    direction: CsilServiceDirection,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: input,
        output_type: output,
        direction,
        position: pos(),
        doc_comments: Vec::new(),
        wire_id: None,
    }
}

fn service_rule(name: &str, ops: Vec<CsilServiceOperation>, wire_id: Option<u64>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: ops,
            wire_id,
        }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

#[test]
fn type_mapping_covers_builtins() {
    assert_eq!(map_type(&builtin("int"), false), "Int64");
    assert_eq!(map_type(&builtin("uint"), false), "UInt64");
    assert_eq!(map_type(&builtin("float"), false), "Double");
    assert_eq!(map_type(&builtin("text"), false), "String");
    assert_eq!(map_type(&builtin("tstr"), false), "String");
    assert_eq!(map_type(&builtin("bytes"), false), "[UInt8]");
    assert_eq!(map_type(&builtin("bstr"), false), "[UInt8]");
    assert_eq!(map_type(&builtin("bool"), false), "Bool");
    assert_eq!(map_type(&reference("User"), false), "User");
    assert_eq!(map_type(&builtin("text"), true), "String?");
}

#[test]
fn array_and_map_mapping() {
    let arr = CsilTypeExpression::Array {
        element_type: Box::new(builtin("int")),
        occurrence: None,
    };
    assert_eq!(map_type(&arr, false), "[Int64]");
    let map = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("bytes")),
        occurrence: None,
    };
    assert_eq!(map_type(&map, false), "[String: [UInt8]]");
}

#[test]
fn snake_case_field_becomes_camel_with_verbatim_wire_key() {
    let rule = group_rule(
        "Task",
        vec![
            bare_entry("current_state", builtin("text")),
            opt_entry("note", builtin("text")),
        ],
    );
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public struct Task: Equatable, Sendable"));
    // snake_case -> camelCase identifier...
    assert!(types.contains("public let currentState: String"));
    assert!(types.contains("public let note: String?"));
    // ...but the wire key stays the original snake_case verbatim.
    assert!(types.contains("\"currentState\": \"current_state\""));
    // Optional defaults to nil in the memberwise init.
    assert!(types.contains("note: String? = nil"));
}

#[test]
fn swift_keyword_field_is_backtick_escaped() {
    let rule = group_rule("Config", vec![bare_entry("protocol", builtin("text"))]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let `protocol`: String"));
}

#[test]
fn type_choice_becomes_enum_with_associated_values() {
    let rule = CsilRule {
        name: "DepositResult".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            reference("DepositOk"),
            reference("ServiceError"),
        ]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum DepositResult: Equatable, Sendable"));
    assert!(types.contains("case depositOk(DepositOk)"));
    assert!(types.contains("case serviceError(ServiceError)"));
}

fn text_literal(s: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
}

#[test]
fn closed_string_literal_choice_becomes_string_backed_enum() {
    let rule = CsilRule {
        name: "Color".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            text_literal("red"),
            text_literal("forest_green"),
            // a label that collides with a Swift keyword must still backtick-escape.
            text_literal("default"),
        ]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum Color: String, Equatable, Sendable, CaseIterable"));
    assert!(types.contains("case red = \"red\""));
    // camelCased case name, verbatim wire raw value.
    assert!(types.contains("case forestGreen = \"forest_green\""));
    assert!(types.contains("case `default` = \"default\""));
    // No opaque-byte fallback cases.
    assert!(!types.contains("AnyCsilValue"));
    assert!(!types.contains("case case1"));
}

#[test]
fn open_text_choice_collapses_to_string_typealias() {
    let rule = CsilRule {
        name: "OrderStatus".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            builtin("text"),
            text_literal("pending"),
            text_literal("shipped"),
        ])),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public typealias OrderStatus = String"));
    assert!(!types.contains("case case1"));
}

#[test]
fn error_suffixed_arms_are_stripped_from_the_returned_success_type() {
    // `create-user: CreateUserRequest -> User / UserError` returns just User; the error
    // half is thrown, never collapsed to opaque AnyCsilValue.
    let ops = vec![make_op(
        "create-user",
        reference("CreateUserRequest"),
        CsilTypeExpression::Choice(vec![reference("User"), reference("UserError")]),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![service_rule("UserService", ops, None)],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("client");
    assert!(
        client
            .content
            .contains("public func createUser(_ request: CreateUserRequest) throws -> User")
    );
    assert!(!client.content.contains("AnyCsilValue"));
}

#[test]
fn zero_minimum_length_check_is_not_emitted() {
    // `.size (0..200)` must not produce a dead `count < 0` guard.
    let entry = bare_entry(
        "description",
        CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                min: 0,
                max: 200,
            })],
        },
    );
    let rule = group_rule("Doc", vec![entry]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(!types.contains("< 0"));
    assert!(types.contains("self.description.count > 200"));
}

#[test]
fn any_value_typealias_emitted_without_any_validation() {
    // A field of core type `any` with no constraints still needs AnyCsilValue defined.
    let rule = group_rule("Envelope", vec![bare_entry("body", builtin("any"))]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public typealias AnyCsilValue = [UInt8]"));
    assert!(types.contains("public let body: AnyCsilValue"));
    // No validation machinery when there are no constraints.
    assert!(!types.contains("CsilValidationError"));
}

#[test]
fn validation_emits_throwing_checks() {
    let entry = bare_entry(
        "username",
        CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        },
    );
    let rule = group_rule("Account", vec![entry]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public func validate() throws"));
    assert!(types.contains("self.username.count < 3"));
    assert!(types.contains("throw CsilValidationError("));
    assert!(types.contains("struct CsilValidationError"));
}

#[test]
fn numeric_min_value_compares_as_scalar() {
    let mut entry = bare_entry("age", builtin("int"));
    entry.metadata.push(CsilFieldMetadata::Constraint(
        CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(18)),
    ));
    let rule = group_rule("Person", vec![entry]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("if self.age < 18"));
}

#[test]
fn server_target_emits_protocol_and_routers() {
    let ops = vec![
        make_op(
            "submit-task",
            reference("SubmitTaskRequest"),
            CsilTypeExpression::Choice(vec![
                reference("SubmitTaskResponse"),
                reference("ServiceError"),
            ]),
            CsilServiceDirection::Unidirectional,
        ),
        make_op(
            "play",
            reference("Move"),
            reference("Move"),
            CsilServiceDirection::Bidirectional,
        ),
    ];
    let input = input_from_rules("swift", vec![service_rule("Match", ops, Some(7))], 1);
    let services = generate_services(&input).expect("services emitted");
    // Handler protocol with verbatim-derived method names; ServiceError stripped from output.
    assert!(services.contains("public protocol Match {"));
    assert!(
        services
            .contains("func submitTask(_ request: SubmitTaskRequest) throws -> SubmitTaskResponse")
    );
    assert!(services.contains("func play(_ msg: Move) throws"));
    // Verbose router keyed by verbatim op name.
    assert!(services.contains("public func routeMatchChannel(_ handler: Match, codec: CsilCodec, op: String, data: [UInt8]) throws"));
    assert!(services.contains("case \"play\":"));
    assert!(services.contains("try handler.play(msg)"));
    // Compact twin keyed by ordinal (service has a wire-id).
    assert!(services.contains("public func routeMatchChannelCompact"));
    // Wire-id ordinals.
    assert!(services.contains("public enum MatchWireID"));
    assert!(services.contains("public static let service: UInt64 = 7"));
    // Codec seam present because there is a channel op.
    assert!(services.contains("public protocol CsilCodec"));
}

#[test]
fn client_target_emits_typed_client_with_verbatim_wire_strings() {
    let ops = vec![make_op(
        "deposit-claim",
        reference("DepositClaimRequest"),
        CsilTypeExpression::Choice(vec![
            reference("DepositClaimResponse"),
            reference("ServiceError"),
        ]),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![service_rule("Attestation", ops, None)],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("client emitted");
    assert!(client.content.contains("public protocol CsilTransport"));
    assert!(client.content.contains("public struct AttestationClient"));
    assert!(client.content.contains(
        "public func depositClaim(_ request: DepositClaimRequest) throws -> DepositClaimResponse"
    ));
    // Wire service + op stay verbatim (camelCase must not leak onto the wire).
    assert!(
        client
            .content
            .contains("service: \"Attestation\", op: \"deposit-claim\"")
    );
    // No server protocol for the client sub-target.
    assert!(!files.iter().any(|f| f.path == "Services.swift"));
}

#[test]
fn null_input_op_takes_no_request() {
    let ops = vec![make_op(
        "room-delta",
        builtin("null"),
        reference("RoomDelta"),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules("swift-client", vec![service_rule("World", ops, None)], 1);
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("client emitted");
    assert!(
        client
            .content
            .contains("public func roomDelta() throws -> RoomDelta")
    );
    assert!(client.content.contains("request: nil as String?"));
}

#[test]
fn typesonly_target_skips_services() {
    let ops = vec![make_op(
        "ping",
        builtin("null"),
        reference("Pong"),
        CsilServiceDirection::Unidirectional,
    )];
    let rules = vec![
        group_rule("Pong", vec![bare_entry("ok", builtin("bool"))]),
        service_rule("Health", ops, None),
    ];
    let input = input_from_rules("swift-typesonly", rules, 1);
    let files = build_files(&input).expect("ok");
    assert!(files.iter().any(|f| f.path == "Types.swift"));
    assert!(!files.iter().any(|f| f.path == "Services.swift"));
    assert!(!files.iter().any(|f| f.path == "Client.swift"));
}

#[test]
fn unknown_subtarget_is_an_error() {
    let input = input_from_rules("swift-bogus", vec![], 0);
    assert!(build_files(&input).is_err());
}

#[test]
fn wire_id_free_service_omits_compact_router() {
    let ops = vec![make_op(
        "play",
        reference("Move"),
        reference("Move"),
        CsilServiceDirection::Bidirectional,
    )];
    let input = input_from_rules("swift", vec![service_rule("Match", ops, None)], 1);
    let services = generate_services(&input).expect("services emitted");
    assert!(services.contains("public func routeMatchChannel"));
    assert!(!services.contains("routeMatchChannelCompact"));
    assert!(!services.contains("MatchWireID"));
}
