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
        vec![
            group_rule(
                "CreateUserRequest",
                vec![bare_entry("name", builtin("text"))],
            ),
            group_rule("User", vec![bare_entry("id", builtin("int"))]),
            service_rule("UserService", ops, None),
        ],
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
    assert!(services.contains("case \"Play\":"));
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
fn client_target_emits_typed_client_with_canonical_wire_strings() {
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
        vec![
            group_rule(
                "DepositClaimRequest",
                vec![bare_entry("subject", builtin("text"))],
            ),
            group_rule(
                "DepositClaimResponse",
                vec![bare_entry("ok", builtin("bool"))],
            ),
            service_rule("Attestation", ops, None),
        ],
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
    // Typed seam: request serializes itself, the carrier moves bytes.
    assert!(client.content.contains("request: request.toCbor()"));
    assert!(
        client
            .content
            .contains("DepositClaimResponse.fromCbor(csilResp)")
    );
    // Canonical wire strings (the wire contract): service lowercased + `Service`
    // stripped, op PascalCased — so a Swift client reaches the same endpoint as its
    // peers (this was the kotlin/dart/swift routing bug).
    assert!(
        client
            .content
            .contains("service: \"attestation\", op: \"DepositClaim\"")
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
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule("RoomDelta", vec![bare_entry("seq", builtin("int"))]),
            service_rule("World", ops, None),
        ],
        1,
    );
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
    // A null-input op sends an empty request body.
    assert!(client.content.contains("request: [])"));
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

// --- codec ------------------------------------------------------------------

/// A corndogs-shaped spec: text, bytes, an optional int, a map, a list, a nested
/// record, and a service whose output is a `Res / ServiceError` choice.
fn corndogs_rules() -> Vec<CsilRule> {
    let map_ty = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("int")),
        occurrence: None,
    };
    let list_ty = CsilTypeExpression::Array {
        element_type: Box::new(builtin("text")),
        occurrence: None,
    };
    vec![
        group_rule(
            "Task",
            vec![
                bare_entry("uuid", builtin("text")),
                bare_entry("current_state", builtin("text")),
                bare_entry("payload", builtin("bytes")),
                opt_entry("priority", builtin("int")),
                bare_entry("labels", map_ty),
                bare_entry("tags", list_ty),
            ],
        ),
        group_rule(
            "SubmitTaskRequest",
            vec![
                bare_entry("task", reference("Task")),
                bare_entry("queue", builtin("text")),
            ],
        ),
        group_rule("ServiceError", vec![bare_entry("code", builtin("int"))]),
        service_rule(
            "CorndogsService",
            vec![make_op(
                "submit-task",
                reference("SubmitTaskRequest"),
                CsilTypeExpression::Choice(vec![reference("Task"), reference("ServiceError")]),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]
}

#[test]
fn codec_emitted_with_typed_client() {
    let input = input_from_rules("swift-client", corndogs_rules(), 1);
    let files = build_files(&input).expect("ok");
    let codec = files
        .iter()
        .find(|f| f.path == "Codec.swift")
        .expect("Codec.swift emitted");
    assert!(codec.content.contains("public enum CsilCbor"));
    assert!(codec.content.contains("public extension Task {"));
    assert!(codec.content.contains("func toCbor() -> [UInt8]"));
    assert!(
        codec
            .content
            .contains("static func fromCbor(_ bytes: [UInt8]) throws -> SubmitTaskRequest")
    );
    // bytes -> CBOR byte string (.bytes, major type 2); text -> .text; nested record
    // recurses via toCborValue.
    assert!(codec.content.contains(".bytes(self.payload)"));
    assert!(codec.content.contains(".text(self.uuid)"));
    assert!(codec.content.contains("self.task.toCborValue()"));
    // Canonical key order within Task: `tags`/`uuid` (len 4) precede longer keys,
    // `current_state` (len 13) is last.
    let body = codec.content.split("extension Task").nth(1).unwrap();
    let pos_tags = body.find("\"tags\"").unwrap();
    let pos_uuid = body.find("\"uuid\"").unwrap();
    let pos_state = body.find("\"current_state\"").unwrap();
    assert!(pos_tags < pos_uuid && pos_uuid < pos_state);

    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("Client.swift emitted");
    assert!(
        client
            .content
            .contains("public func submitTask(_ request: SubmitTaskRequest) throws -> Task")
    );
    assert!(client.content.contains("request: request.toCbor()"));
    assert!(client.content.contains("Task.fromCbor(csilResp)"));
    // The carrier seam is raw bytes.
    assert!(
        client
            .content
            .contains("func call(service: String, op: String, request: [UInt8]) throws -> [UInt8]")
    );
}

#[test]
fn async_twin_emitted_by_default_with_marked_symbols() {
    // Default client_style is `both`: an `async` twin at ClientAsync.swift whose symbols
    // carry an `Async` marker so it coexists with the blocking client in one module.
    let input = input_from_rules("swift-client", corndogs_rules(), 1);
    let files = build_files(&input).expect("ok");
    let twin = files
        .iter()
        .find(|f| f.path == "ClientAsync.swift")
        .expect("ClientAsync.swift emitted");

    // `async` transport seam, marked protocol name.
    assert!(
        twin.content
            .contains("public protocol AsyncCsilTransport {")
    );
    assert!(twin.content.contains(
        "func call(service: String, op: String, request: [UInt8]) async throws -> [UInt8]"
    ));
    // Marked per-service client over the marked seam.
    assert!(twin.content.contains("public struct CorndogsAsyncClient {"));
    assert!(
        twin.content
            .contains("public let transport: AsyncCsilTransport")
    );
    // Methods are `async throws` and `await` the byte seam.
    assert!(
        twin.content
            .contains("public func submitTask(_ request: SubmitTaskRequest) async throws -> Task")
    );
    assert!(twin.content.contains("try await transport.call("));
    assert!(twin.content.contains("Task.fromCbor(csilResp)"));
    // The twin reuses the module's CsilClientError (declared in Client.swift).
    assert!(!twin.content.contains("public struct CsilClientError"));

    // The sync client is untouched: blocking seam + canonical names, no `async`.
    let sync = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("Client.swift emitted");
    assert!(sync.content.contains("public struct CorndogsClient {"));
    assert!(
        sync.content
            .contains("public func submitTask(_ request: SubmitTaskRequest) throws -> Task")
    );
    assert!(!sync.content.contains("async"));
    assert!(!sync.content.contains("await"));
    assert!(sync.content.contains("public struct CsilClientError"));
}

#[test]
fn client_style_async_is_drop_in_at_canonical_path() {
    // `client_style: async` yields a single `async` client at the canonical path with
    // the canonical symbol names — a drop-in for a blocking consumer.
    let mut input = input_from_rules("swift-client", corndogs_rules(), 1);
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("async"));
    let files = build_files(&input).expect("ok");
    assert!(files.iter().any(|f| f.path == "Client.swift"));
    assert!(
        !files.iter().any(|f| f.path == "ClientAsync.swift"),
        "async drop-in emits no separate twin"
    );

    let client = files.iter().find(|f| f.path == "Client.swift").unwrap();
    // Canonical (unmarked) names, but `async`.
    assert!(client.content.contains("public protocol CsilTransport {"));
    assert!(client.content.contains(
        "func call(service: String, op: String, request: [UInt8]) async throws -> [UInt8]"
    ));
    assert!(client.content.contains("public struct CorndogsClient {"));
    assert!(
        client
            .content
            .contains("public let transport: CsilTransport")
    );
    assert!(
        client
            .content
            .contains("public func submitTask(_ request: SubmitTaskRequest) async throws -> Task")
    );
    assert!(client.content.contains("try await transport.call("));
    // The drop-in is the primary file, so it still declares the shared error.
    assert!(client.content.contains("public struct CsilClientError"));
}

#[test]
fn client_style_sync_suppresses_the_twin() {
    let mut input = input_from_rules("swift-client", corndogs_rules(), 1);
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("sync"));
    let files = build_files(&input).expect("ok");
    assert!(files.iter().any(|f| f.path == "Client.swift"));
    assert!(!files.iter().any(|f| f.path == "ClientAsync.swift"));
    let client = files.iter().find(|f| f.path == "Client.swift").unwrap();
    assert!(!client.content.contains("async"));
    assert!(!client.content.contains("await"));
    assert!(!client.content.contains("AsyncCsilTransport"));
}

#[test]
fn client_style_invalid_value_is_rejected() {
    // A bad value fails the whole run regardless of surface.
    let mut input = input_from_rules("swift-client", corndogs_rules(), 1);
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("blocking"));
    assert!(build_files(&input).is_err());

    // The validator names the offending option so the failure is actionable.
    let mut opts = HashMap::new();
    opts.insert("client_style".to_string(), serde_json::json!("blocking"));
    let err = client_style(&opts).expect_err("invalid client_style must be rejected");
    assert!(
        err.contains("client_style"),
        "error should name the option: {err}"
    );
}

fn typedef_rule(name: &str, ty: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(ty),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

#[test]
fn named_map_alias_field_roundtrips_as_a_map_not_a_stub() {
    // Regression: a field typed as a transparent named map alias
    // (`StringInt64Map = {* text => int}`) used to fall through the record check to
    // the `.null` / `asText` stub, dropping the entire dictionary on the wire.
    let map_ty = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("int")),
        occurrence: None,
    };
    let rules = vec![
        typedef_rule("StringInt64Map", map_ty),
        group_rule(
            "Holder",
            vec![bare_entry("counts", reference("StringInt64Map"))],
        ),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    // Encode must build a CBOR map; decode must read one back via asMap.
    assert!(codec.contains("CsilCborValue.map(self.counts.map {"));
    assert!(codec.contains("try CsilCbor.asMap"));
    assert!(codec.contains("reduce(into: [String: Int64]()"));
    // The stub must be gone for this field.
    let body = codec.split("extension Holder").nth(1).unwrap();
    assert!(!body.contains("\"counts\", .null"));
    assert!(!body.contains("CsilCbor.asText((try CsilCbor.require(cborValue, \"counts\"))"));
}

#[test]
fn named_map_of_record_alias_recurses_into_the_record_codec() {
    // `M = {* text => SomeRecord}`: the alias resolves to a map whose values are a
    // record, so the value handler must recurse to the record's
    // `toCborValue` / `init(cborValue:)` rather than stubbing.
    let map_of_record = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(reference("SomeRecord")),
        occurrence: None,
    };
    let rules = vec![
        typedef_rule("M", map_of_record),
        group_rule("SomeRecord", vec![bare_entry("id", builtin("text"))]),
        group_rule("Holder", vec![bare_entry("items", reference("M"))]),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    let body = codec.split("extension Holder").nth(1).unwrap();
    // Encode recurses to the record value's codec inside a map builder.
    assert!(body.contains("CsilCborValue.map(self.items.map {"));
    assert!(body.contains("$0.value.toCborValue()"));
    // Decode reads a map and reconstructs each record value.
    assert!(body.contains("try CsilCbor.asMap"));
    assert!(body.contains("try SomeRecord(cborValue: $1.1)"));
    assert!(!body.contains("\"items\", .null"));
}

#[test]
fn wire_service_strips_service_and_lowercases() {
    // The Swift-specific bug: `CorndogsService` previously hit the wire as
    // "CorndogsService" (Service not stripped, not lowercased). It must be
    // "corndogs", and `submit-task` must be "SubmitTask".
    let ops = vec![make_op(
        "submit-task",
        reference("SubmitTaskRequest"),
        reference("Task"),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule(
                "SubmitTaskRequest",
                vec![bare_entry("queue", builtin("text"))],
            ),
            group_rule("Task", vec![bare_entry("uuid", builtin("text"))]),
            service_rule("CorndogsService", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files.iter().find(|f| f.path == "Client.swift").unwrap();
    assert!(
        client
            .content
            .contains("service: \"corndogs\", op: \"SubmitTask\"")
    );
    assert!(!client.content.contains("CorndogsService\""));
    assert!(!client.content.contains("submit-task"));
}

// ---------------------------------------------------------------------------
// Self-contained SwiftPM package mode
// ---------------------------------------------------------------------------

/// Build an input whose generator options are pre-populated, so package-mode
/// triggers (`emit_packages`, `package_name`, `package_version`) can be exercised.
fn input_with_options(
    target: &str,
    rules: Vec<CsilRule>,
    service_count: usize,
    options: Vec<(&str, serde_json::Value)>,
) -> WasmGeneratorInput {
    let mut input = input_from_rules(target, rules, service_count);
    input.config.options = options
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    input
}

fn one_record_rules() -> Vec<CsilRule> {
    vec![group_rule(
        "Task",
        vec![bare_entry("uuid", builtin("text"))],
    )]
}

#[test]
fn package_mode_emits_manifest_and_relocates_sources() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![("emit_packages", serde_json::json!(["swift"]))],
    );
    let files = build_files(&input).expect("ok");

    let manifest = files
        .iter()
        .find(|f| f.path == "Package.swift")
        .expect("Package.swift emitted in package mode");
    assert!(manifest.content.contains("// swift-tools-version:5.9"));
    assert!(manifest.content.contains("import PackageDescription"));
    // Default coordinates: package + product + target all "CsilgenClient".
    assert!(manifest.content.contains("name: \"CsilgenClient\""));
    assert!(
        manifest
            .content
            .contains(".library(name: \"CsilgenClient\", targets: [\"CsilgenClient\"])")
    );
    assert!(
        manifest
            .content
            .contains(".target(name: \"CsilgenClient\")")
    );

    // The package README rides at the root, beside the manifest (never under Sources/).
    assert!(files.iter().any(|f| f.path == "genquickstart.md"));

    // Generated sources live under SwiftPM's required Sources/<Target>/ layout, and
    // none remain at the package root.
    assert!(
        files
            .iter()
            .any(|f| f.path == "Sources/CsilgenClient/Types.swift")
    );
    assert!(
        !files
            .iter()
            .any(|f| f.path == "Types.swift" || f.path == "Codec.swift")
    );
}

#[test]
fn package_name_and_version_drive_manifest_coordinates() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            // A kebab package name keeps its raw spelling for the package/product, but
            // the target identifier is PascalCased so it is a valid Swift identifier.
            ("package_name", serde_json::json!("my-client")),
            ("package_version", serde_json::json!("2.4.0")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let manifest = files
        .iter()
        .find(|f| f.path == "Package.swift")
        .expect("Package.swift emitted");
    assert!(manifest.content.contains("name: \"my-client\""));
    assert!(
        manifest
            .content
            .contains(".library(name: \"my-client\", targets: [\"MyClient\"])")
    );
    assert!(manifest.content.contains(".target(name: \"MyClient\")"));
    // Version informs the manifest comment only (SwiftPM uses git tags).
    assert!(manifest.content.contains("my-client 2.4.0"));
    // Sources relocate under the PascalCased target directory.
    assert!(
        files
            .iter()
            .any(|f| f.path == "Sources/MyClient/Types.swift")
    );
}

#[test]
fn non_package_mode_leaves_output_unchanged() {
    let input = input_from_rules("swift", one_record_rules(), 0);
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Types.swift"));
    assert!(!files.iter().any(|f| f.path.starts_with("Sources/")));
}

#[test]
fn emit_packages_without_swift_stays_off() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        // Other languages requested, but not Swift.
        vec![("emit_packages", serde_json::json!(["go", "rust"]))],
    );
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Types.swift"));
}

#[test]
fn emit_packages_non_array_is_parsed_defensively() {
    // A malformed (non-array) value must not error and must not enable package mode.
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![("emit_packages", serde_json::json!("swift"))],
    );
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Types.swift"));
}

// --- package README: 3-transport Quickstart ---------------------------------
//
// No Swift toolchain is available here (no swiftc), so the emitted sections cannot be
// compiled or run; these tests assert the structural contract instead (assert-only).

/// A minimal `ping: Ping -> Pong` unary service over two single-field records — used for
/// the RPC + Datagrams sections and the no-channel Events note.
fn pingpong_rules() -> Vec<CsilRule> {
    vec![
        group_rule("Ping", vec![bare_entry("msg", builtin("text"))]),
        group_rule("Pong", vec![bare_entry("msg", builtin("text"))]),
        service_rule(
            "Echo",
            vec![make_op(
                "ping",
                reference("Ping"),
                reference("Pong"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]
}

/// The verification spec: a `->` unary op AND a record-typed `<->` channel op on one
/// service, so every section has a generated surface to wire to (per the contract).
fn transports_rules() -> Vec<CsilRule> {
    vec![
        group_rule("Ping", vec![bare_entry("msg", builtin("text"))]),
        group_rule("Pong", vec![bare_entry("msg", builtin("text"))]),
        group_rule("ChatMsg", vec![bare_entry("body", builtin("text"))]),
        group_rule("ChatEvent", vec![bare_entry("body", builtin("text"))]),
        service_rule(
            "Echo",
            vec![
                make_op(
                    "ping",
                    reference("Ping"),
                    reference("Pong"),
                    CsilServiceDirection::Unidirectional,
                ),
                make_op(
                    "chat",
                    reference("ChatMsg"),
                    reference("ChatEvent"),
                    CsilServiceDirection::Bidirectional,
                ),
            ],
            Some(7),
        ),
    ]
}

fn render_transports_readme(options: Vec<(&str, serde_json::Value)>) -> String {
    let mut opts = vec![
        ("emit_packages", serde_json::json!(["swift"])),
        ("package_name", serde_json::json!("Echo")),
    ];
    opts.extend(options);
    let input = input_with_options("swift-client", transports_rules(), 1, opts);
    let files = build_files(&input).expect("ok");
    files
        .into_iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md emitted at the package root")
        .content
}

#[test]
fn readme_intro_credits_the_transport_library() {
    let c = render_transports_readme(vec![]);
    // The intro credits the lib for the envelope/framing/lifecycle and the carrier model.
    assert!(c.contains("`CsilgenTransport` library owns the envelope"));
    assert!(c.contains("supply only a *carrier* that moves bytes"));
    // The Install section adds the transport-lib dependency with the not-yet-published note.
    assert!(c.contains(".package(path: \"../csilgen/transports/swift\")"));
    assert!(c.contains("not yet published"));
}

#[test]
fn readme_rpc_section_uses_lib_envelope_and_example_call() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-RPC (HTTP)"));
    assert!(c.contains("import CsilgenTransport"));
    // The carrier implements the generated byte seam and uses the lib's RpcRequest envelope.
    assert!(c.contains("struct HttpRpcCarrier: CsilTransport"));
    assert!(
        c.contains("func call(service: String, op: String, request: [UInt8]) throws -> [UInt8]")
    );
    assert!(c.contains("RpcRequest(service: service, op: op, payload: request).encode()"));
    // It POSTs the canonical mount over URLSession.
    assert!(c.contains("/csil/v1/rpc"));
    assert!(c.contains("httpMethod = \"POST\""));
    assert!(c.contains("URLSession.shared.dataTask"));
    // Decode + both error arms via the lib: non-zero transport status and the ServiceError arm.
    assert!(c.contains("RpcResponse.decode(try outcome.get())"));
    assert!(c.contains("resp.asTransportError()"));
    assert!(c.contains("resp.variant == \"ServiceError\""));
    // The typed client is built over the carrier and the example calls the first `->` op.
    assert!(
        c.contains("EchoClient(transport: HttpRpcCarrier(baseURL: \"http://localhost:5080\"))")
    );
    assert!(c.contains("try client.ping(Ping(msg: \"example\"))"));
    assert!(c.contains("import Echo"));
}

#[test]
fn readme_events_section_handshake_heartbeat_and_router_dispatch() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-Events (TLS)"));
    // A frame carrier over a TLS byte stream via the lib's StreamCarrier framing.
    assert!(c.contains("StreamCarrier(stream: TlsByteStream(host: \"localhost\", port: 7443))"));
    assert!(c.contains("final class TlsByteStream: ByteStream"));
    // The $hello / $hello-ack handshake with the lib Hello/HelloAck.
    assert!(
        c.contains("Hello(versions: [csilVersion], profiles: [\"verbose\"], service: \"echo\")")
    );
    assert!(c.contains("HelloAck.decode(ackFrame)"));
    assert!(c.contains("Profile.parse(ack.profile)"));
    // The $ping / $pong heartbeat via the lib Heartbeat + control names.
    assert!(c.contains("ev.event == Control.pingName"));
    assert!(c.contains("Control.pongName"));
    assert!(c.contains("Heartbeat(nonce: ping.nonce)"));
    // One outbound event via the generated encoder, dispatch into the generated router.
    assert!(c.contains("struct Handler: Echo"));
    assert!(c.contains("encodeEchoChat(codec: codec, msg: ChatEvent(body: \"example\"))"));
    assert!(
        c.contains("try routeEchoChannel(handler, codec: codec, op: ev.event!, data: ev.payload)")
    );
}

#[test]
fn readme_datagrams_section_send_and_late_arrival_note() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-Datagrams (UDP)"));
    assert!(c.contains("final class UdpDatagramCarrier: DatagramCarrier"));
    // Encode a `->` request via the generated codec, wrap in the lib Datagram, fire-and-forget.
    assert!(c.contains("let req: Ping = Ping(msg: \"example\")"));
    assert!(c.contains("Datagram(opOrd: opOrd, seq: 0, payload: req.toCbor()).encode()"));
    // The op's datagram ordinal (ping op has no @wire-id → placeholder 1).
    assert!(c.contains("let opOrd: UInt64 = 1"));
    // Recv path decodes an inbound Datagram into the RESPONSE type, with the loss note.
    assert!(c.contains("Datagram.decode(inbound)"));
    assert!(c.contains("Pong.fromCbor(dg.payload)"));
    assert!(c.contains("MAY arrive later — or never"));
}

#[test]
fn readme_no_channel_op_emits_events_note() {
    // pingpong has only a `->` op: Events still shows handshake + heartbeat, but notes the
    // absence of a generated channel router rather than wiring dispatch.
    let input = input_with_options(
        "swift-client",
        pingpong_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let c = &files
        .iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md")
        .content;
    assert!(c.contains("## CSIL-Events (TLS)"));
    assert!(c.contains("Hello(versions: [csilVersion], profiles: [\"verbose\"]).encode()"));
    assert!(c.contains("no generated channel router"));
    assert!(!c.contains("routeEchoChannel"));
}

#[test]
fn readme_transport_subset_renders_only_listed_sections() {
    // `genquickstart_transports: ["rpc"]` renders only the RPC section.
    let c = render_transports_readme(vec![(
        "genquickstart_transports",
        serde_json::json!(["rpc"]),
    )]);
    assert!(c.contains("## CSIL-RPC (HTTP)"));
    assert!(!c.contains("## CSIL-Events (TLS)"));
    assert!(!c.contains("## CSIL-Datagrams (UDP)"));

    // An absent option renders all three.
    let all = render_transports_readme(vec![]);
    assert!(all.contains("## CSIL-RPC (HTTP)"));
    assert!(all.contains("## CSIL-Events (TLS)"));
    assert!(all.contains("## CSIL-Datagrams (UDP)"));

    // An all-unknown array falls back to all three rather than an empty doc.
    let unknown = render_transports_readme(vec![(
        "genquickstart_transports",
        serde_json::json!(["bogus"]),
    )]);
    assert!(unknown.contains("## CSIL-RPC (HTTP)"));
    assert!(unknown.contains("## CSIL-Events (TLS)"));
    assert!(unknown.contains("## CSIL-Datagrams (UDP)"));
}

#[test]
fn package_readme_absent_without_package_mode() {
    let input = input_from_rules("swift-client", pingpong_rules(), 1);
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "genquickstart.md"));
}

#[test]
fn package_readme_opt_out_suppresses_only_readme() {
    // By default the README rides at the package root in package mode.
    let default_input = input_with_options(
        "swift-client",
        pingpong_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
        ],
    );
    let default_files = build_files(&default_input).expect("ok");
    assert!(default_files.iter().any(|f| f.path == "genquickstart.md"));

    // An explicit `emit_readme: false` suppresses only the README; the manifest and the
    // relocated sources are unchanged.
    let input = input_with_options(
        "swift-client",
        pingpong_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
            ("emit_readme", serde_json::json!(false)),
        ],
    );
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "genquickstart.md"));
    assert!(files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Sources/Echo/Types.swift"));
}

#[test]
fn empty_package_name_falls_back_to_default() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            // A blank name must not yield an empty target; fall back to the default.
            ("package_name", serde_json::json!("   ")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let manifest = files
        .iter()
        .find(|f| f.path == "Package.swift")
        .expect("Package.swift emitted");
    assert!(
        manifest
            .content
            .contains(".target(name: \"CsilgenClient\")")
    );
}

/// A package must be SELF-CONTAINED: its genquickstart references the typed client
/// (CSIL-RPC/Datagrams), the channel router + handler protocol (CSIL-Events), and the
/// codec (all sections), so the emitted file set must carry ALL THREE surfaces together —
/// Client.swift (client), Services.swift (router/protocol), and Codec.swift (codec) — even
/// from a single-surface (sub-)target like `swift-client`, which flat-mode would emit
/// without the router. No swiftc here, so this is assert-only: the file set proves the
/// quickstart's symbols all resolve against the single package. Mirrors the OCaml generator.
#[test]
fn package_mode_is_self_contained_with_client_router_and_codec() {
    let input = input_with_options(
        "swift-client",
        transports_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let has = |name: &str| files.iter().any(|f| f.path.ends_with(name));

    // The package carries the RPC client AND the channel router AND the codec together.
    assert!(
        has("/Client.swift"),
        "package missing the RPC client surface"
    );
    assert!(
        has("/Services.swift"),
        "package missing the channel-router surface"
    );
    assert!(has("/Codec.swift"), "package missing the codec");

    // Services.swift actually defines the router + handler protocol the Events section calls.
    let services = files
        .iter()
        .find(|f| f.path.ends_with("/Services.swift"))
        .unwrap();
    assert!(services.content.contains("public protocol Echo"));
    assert!(services.content.contains(
        "public func routeEchoChannel(_ handler: Echo, codec: CsilCodec, op: String, data: [UInt8]) throws"
    ));
    // Client.swift defines the typed client the RPC/Datagrams sections call.
    let client = files
        .iter()
        .find(|f| f.path.ends_with("/Client.swift"))
        .unwrap();
    assert!(client.content.contains("EchoClient"));

    // The Events section dispatches via the generated router + encoder (not codec-direct).
    let quickstart = files.iter().find(|f| f.path == "genquickstart.md").unwrap();
    assert!(
        quickstart.content.contains(
            "try routeEchoChannel(handler, codec: codec, op: ev.event!, data: ev.payload)"
        )
    );
    assert!(
        quickstart
            .content
            .contains("encodeEchoChat(codec: codec, msg: ChatEvent(body: \"example\"))")
    );

    // A flat (non-package) client build stays surface-only: Client.swift but no Services.swift.
    let flat = build_files(&input_from_rules("swift-client", transports_rules(), 1)).expect("ok");
    assert!(flat.iter().any(|f| f.path == "Client.swift"));
    assert!(
        !flat.iter().any(|f| f.path == "Services.swift"),
        "flat client build must stay surface-only"
    );
}
