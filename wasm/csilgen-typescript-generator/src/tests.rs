//! Output-shape tests for the TypeScript emitters.

use crate::{common, generate_files};
use csilgen_common::*;
use std::collections::HashMap;

fn bare(name: &str) -> Option<CsilGroupKey> {
    Some(CsilGroupKey::Bare(name.to_string()))
}

fn field(name: &str, ty: CsilTypeExpression, optional: bool) -> CsilGroupEntry {
    CsilGroupEntry {
        key: bare(name),
        value_type: ty,
        occurrence: optional.then_some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: vec![],
    }
}

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

fn constrained(
    base: CsilTypeExpression,
    constraints: Vec<CsilControlOperator>,
) -> CsilTypeExpression {
    CsilTypeExpression::Constrained {
        base_type: Box::new(base),
        constraints,
    }
}

fn field_meta(
    name: &str,
    ty: CsilTypeExpression,
    optional: bool,
    metadata: Vec<CsilFieldMetadata>,
) -> CsilGroupEntry {
    CsilGroupEntry {
        key: bare(name),
        value_type: ty,
        occurrence: optional.then_some(CsilOccurrence::Optional),
        metadata,
        doc_comments: vec![],
    }
}

fn spec_of(rules: Vec<CsilRule>) -> CsilSpecSerialized {
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>, docs: Vec<String>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: docs,
    }
}

fn op_with_direction(
    name: &str,
    input: &str,
    output: &str,
    direction: CsilServiceDirection,
    docs: Vec<String>,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: reference(input),
        output_type: reference(output),
        direction,
        position: pos(),
        doc_comments: docs,
        wire_id: None,
    }
}

fn op(name: &str, input: &str, output: &str, docs: Vec<String>) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: reference(input),
        output_type: reference(output),
        direction: CsilServiceDirection::Unidirectional,
        position: pos(),
        doc_comments: docs,
        wire_id: None,
    }
}

/// A spec with two type aliases/groups and two services (deliberately declared
/// out of alphabetical order to exercise deterministic sorting).
fn sample_spec() -> CsilSpecSerialized {
    let rules = vec![
        CsilRule {
            name: "HouseID".to_string(),
            rule_type: CsilRuleType::TypeDef(builtin("text")),
            position: pos(),
            doc_comments: vec!["A house identifier.".to_string()],
        },
        group_rule(
            "LoginRequest",
            vec![field("signed_assertion", builtin("text"), false)],
            vec![],
        ),
        group_rule(
            "LoginResponse",
            vec![field("token", builtin("text"), false)],
            vec![],
        ),
        group_rule(
            "ListMembersRequest",
            vec![field("house_id", reference("HouseID"), false)],
            vec![],
        ),
        group_rule(
            "ListMembersResponse",
            vec![field(
                "members",
                CsilTypeExpression::Array {
                    element_type: Box::new(builtin("text")),
                    occurrence: Some(CsilOccurrence::ZeroOrMore),
                },
                true,
            )],
            vec![],
        ),
        CsilRule {
            name: "MemberService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![op(
                    "list-members",
                    "ListMembersRequest",
                    "ListMembersResponse",
                    vec!["List all members of a house.".to_string()],
                )],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: vec![],
        },
        CsilRule {
            name: "AuthService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![op(
                    "Login",
                    "LoginRequest",
                    "LoginResponse",
                    vec!["Authenticate a caller.".to_string()],
                )],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: vec!["The auth service.".to_string()],
        },
    ];

    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 2,
        fields_with_metadata_count: 0,
    }
}

fn input_for(target: &str) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: sample_spec(),
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: GeneratorMetadata {
            name: "ts".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            target: "typescript".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        },
    }
}

fn file<'a>(files: &'a [GeneratedFile], path: &str) -> &'a str {
    files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.content.as_str())
        .unwrap_or_else(|| panic!("missing file {path}"))
}

#[test]
fn typesonly_emits_only_types_with_service_error() {
    let files = generate_files(&input_for("typescript-typesonly")).expect("generate");
    // The record codec rides alongside the types for every surface.
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts"]);
    let types = file(&files, "types.gen.ts");

    assert!(types.contains("export interface ServiceError {"));
    assert!(types.contains("export type HouseID = string;"));
    assert!(types.contains("export interface LoginRequest {"));
    assert!(types.contains("signedAssertion: string;"));
    // optional + array mapping
    assert!(types.contains("members?: string[];"));
    // doc comment becomes JSDoc
    assert!(types.contains("* A house identifier."));
    // the DO NOT EDIT banner
    assert!(types.contains("// Code generated by csilgen. DO NOT EDIT."));
}

#[test]
fn client_emits_types_and_client() {
    let files = generate_files(&input_for("typescript-client")).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts", "client.gen.ts"]);

    let client = file(&files, "client.gen.ts");
    // type-only import from the companion types module
    assert!(client.contains("import type {"));
    assert!(client.contains("} from \"./types.gen\";"));
    // the typed methods pull their codec helpers from the codec module
    assert!(client.contains("} from \"./codec.gen\";"));
    assert!(client.contains("fromLoginResponseCbor"));
    assert!(client.contains("toLoginRequestCbor"));
    // byte-seam transport interface present
    assert!(client.contains("export interface ServiceTransport {"));
    assert!(client.contains("call(service: string, op: string, req: Uint8Array): Uint8Array;"));
    // per-service classes
    assert!(client.contains("export class AuthClient {"));
    assert!(client.contains("export class MemberClient {"));
    // camelCase method (sync byte seam): encode -> call -> decode
    assert!(client.contains("login(req: LoginRequest): LoginResponse {"));
    assert!(
        client.contains(
            "const csilResp = this.t.call(\"auth\", \"Login\", toLoginRequestCbor(req));"
        )
    );
    assert!(client.contains("return fromLoginResponseCbor(csilResp);"));
    // wire strings: lowercase service, PascalCase method
    assert!(
        client.contains("this.t.call(\"member\", \"ListMembers\", toListMembersRequestCbor(req));")
    );
    // aggregate class with default name
    assert!(client.contains("export class ApiClient {"));
    assert!(client.contains("this.auth = new AuthClient(t);"));
    // JSDoc with throws + op doc
    assert!(client.contains("* Authenticate a caller."));
    assert!(client.contains("@throws {ServiceError}"));
    // alphabetical ordering: AuthClient appears before MemberClient
    assert!(client.find("AuthClient").unwrap() < client.find("MemberClient").unwrap());
}

#[test]
fn server_emits_types_and_server() {
    let files = generate_files(&input_for("typescript-server")).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts", "server.gen.ts"]);

    let server = file(&files, "server.gen.ts");
    assert!(server.contains("import type {"));
    assert!(server.contains("ServiceError"));
    assert!(server.contains("export interface RequestContext {"));
    assert!(server.contains("export interface Codec {"));
    assert!(server.contains("export interface AuthHandlers {"));
    assert!(
        server.contains("login(req: LoginRequest, ctx: RequestContext): Promise<LoginResponse>;")
    );
    assert!(server.contains("export interface ServerHandlers {"));
    assert!(server.contains("export async function dispatch("));
    // dispatch routing keys
    assert!(server.contains("case \"auth\": {"));
    assert!(server.contains("case \"Login\": {"));
    assert!(server.contains("const res = await handlers.auth.login(req, ctx);"));
}

#[test]
fn aggregate_target_emits_all_three() {
    let files = generate_files(&input_for("typescript")).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "types.gen.ts",
            "codec.gen.ts",
            "client.gen.ts",
            "server.gen.ts"
        ]
    );
}

#[test]
fn output_is_deterministic() {
    let a = generate_files(&input_for("typescript")).expect("generate");
    let b = generate_files(&input_for("typescript")).expect("generate");
    assert_eq!(a.len(), b.len());
    for (fa, fb) in a.iter().zip(b.iter()) {
        assert_eq!(fa.path, fb.path);
        assert_eq!(fa.content, fb.content);
    }
}

#[test]
fn aggregate_class_name_option_overrides_and_disables() {
    let mut input = input_for("typescript-client");
    input.config.options.insert(
        "aggregate_class_name".to_string(),
        serde_json::Value::String("LonghouseClient".to_string()),
    );
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();
    assert!(client.contains("export class LonghouseClient {"));
    assert!(!client.contains("export class ApiClient {"));

    // empty string disables aggregate emission entirely
    let mut input = input_for("typescript-client");
    input.config.options.insert(
        "aggregate_class_name".to_string(),
        serde_json::Value::String(String::new()),
    );
    let files = generate_files(&input).expect("generate");
    let client = file(&files, "client.gen.ts");
    assert!(!client.contains("ApiClient"));
    assert!(client.contains("export class AuthClient {"));
}

#[test]
fn service_error_is_stripped_from_return_union() {
    // An operation written as `-> LoginResponse / ServiceError` should return
    // only the success type and must not import the thrown ServiceError.
    let mut spec = sample_spec();
    if let CsilRuleType::ServiceDef(def) = &mut spec.rules[6].rule_type {
        def.operations[0].output_type =
            CsilTypeExpression::Choice(vec![reference("LoginResponse"), reference("ServiceError")]);
    } else {
        panic!("expected AuthService at index 6");
    }

    let mut input = input_for("typescript-client");
    input.csil_spec = spec;
    let files = generate_files(&input).expect("generate");
    let client = file(&files, "client.gen.ts");

    // The success type is the sole record; the method returns it (sync byte seam).
    assert!(client.contains("login(req: LoginRequest): LoginResponse {"));
    assert!(client.contains("return fromLoginResponseCbor(csilResp);"));
    assert!(!client.contains("ServiceError | "));
    // ServiceError is thrown, not returned, so it is not imported
    let import_line = client
        .lines()
        .find(|l| l.contains("import type"))
        .unwrap_or("");
    assert!(!import_line.contains("ServiceError"));
}

#[test]
fn domain_error_union_success_is_a_non_record_payload() {
    // Under the typed-codec byte seam a method's success must be a single record so
    // it can call `from<T>Cbor`. A `Res / DomainError` union success is not a record
    // reference, so the op is skipped with a note for the consumer to handle.
    let mut spec = sample_spec();
    if let CsilRuleType::ServiceDef(def) = &mut spec.rules[6].rule_type {
        def.operations[0].output_type =
            CsilTypeExpression::Choice(vec![reference("LoginResponse"), reference("LoginError")]);
    }
    // LoginError must be declarable for import; add it as a rule
    spec.rules.push(group_rule(
        "LoginError",
        vec![field("reason", builtin("text"), false)],
        vec![],
    ));
    let mut input = input_for("typescript-client");
    input.csil_spec = spec;
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();
    assert!(
        client.contains("// operation 'Login' has a non-record payload; (de)serialize it manually")
    );
    assert!(!client.contains("fromLoginResponseCbor"));
}

#[test]
fn client_types_module_option_changes_import_path() {
    let mut input = input_for("typescript-client");
    input.config.options.insert(
        "client_types_module".to_string(),
        serde_json::Value::String("../generated/types".to_string()),
    );
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();
    assert!(client.contains("} from \"../generated/types\";"));
}

// ---------------------------------------------------------------------------
// Bidirectional / reverse emission
// ---------------------------------------------------------------------------

/// Spec with a service that has one of each direction so we can assert the
/// per-direction emissions in isolation. `play` is bidirectional (client sends
/// PlayerInput, receives GameState); `notify` is reverse (server-pushed Event).
fn channel_spec() -> CsilSpecSerialized {
    let mut rules = vec![
        group_rule(
            "PlayerInput",
            vec![field("action", builtin("text"), false)],
            vec![],
        ),
        group_rule(
            "GameState",
            vec![field("tick", builtin("uint"), false)],
            vec![],
        ),
        group_rule("Event", vec![field("kind", builtin("text"), false)], vec![]),
        group_rule(
            "Acknowledgment",
            vec![field("ok", builtin("bool"), false)],
            vec![],
        ),
        group_rule(
            "ListRequest",
            vec![field("limit", builtin("uint"), true)],
            vec![],
        ),
        group_rule(
            "ListResponse",
            vec![field(
                "items",
                CsilTypeExpression::Array {
                    element_type: Box::new(reference("Event")),
                    occurrence: Some(CsilOccurrence::ZeroOrMore),
                },
                false,
            )],
            vec![],
        ),
    ];
    rules.push(CsilRule {
        name: "MatchService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![
                op_with_direction(
                    "list-events",
                    "ListRequest",
                    "ListResponse",
                    CsilServiceDirection::Unidirectional,
                    vec![],
                ),
                op_with_direction(
                    "play",
                    "PlayerInput",
                    "GameState",
                    CsilServiceDirection::Bidirectional,
                    vec!["Open a play channel.".to_string()],
                ),
                op_with_direction(
                    "notify",
                    "Event",
                    "Acknowledgment",
                    CsilServiceDirection::Reverse,
                    vec![],
                ),
            ],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: vec![],
    });

    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

fn input_with_spec(target: &str, spec: CsilSpecSerialized) -> WasmGeneratorInput {
    let mut input = input_for(target);
    input.csil_spec = spec;
    input
}

#[test]
fn invalid_bidirectional_transport_value_fails_generation() {
    let mut input = input_with_spec("typescript-client", channel_spec());
    input.config.options.insert(
        "ts_bidirectional_transport".to_string(),
        serde_json::Value::String("magic".to_string()),
    );
    let err = generate_files(&input).expect_err("invalid value must fail");
    assert!(
        err.contains("ts_bidirectional_transport") && err.contains("magic"),
        "error must name the option and bad value, got {err:?}"
    );
}

#[test]
fn connection_mode_client_emits_channel_handler_router_and_encoder() {
    let client = file(
        &generate_files(&input_with_spec("typescript-client", channel_spec())).expect("generate"),
        "client.gen.ts",
    )
    .to_string();

    // Unary op stays on the per-service class.
    assert!(client.contains("export class MatchClient"));
    assert!(client.contains("listEvents(req: ListRequest"));

    // Channel block: handler interface includes both <-> (output=GameState
    // on the client side) and <- (output=Acknowledgment pushed by server).
    assert!(client.contains("export interface MatchChannelHandlers"));
    assert!(client.contains("play(msg: GameState): void;"));
    assert!(client.contains("notify(msg: Acknowledgment): void;"));

    // Router decodes by method name (PascalCase wire keys).
    assert!(client.contains("export function routeMatchChannel("));
    assert!(client.contains("case \"Play\":"));
    assert!(client.contains("handlers.play(codec.decode<GameState>(bytes));"));
    assert!(client.contains("case \"Notify\":"));

    // Outbound encoder exists for <-> (client sends PlayerInput) but not for
    // reverse (server-pushed only).
    assert!(client.contains("export function encodeMatchPlay(codec: Codec, msg: PlayerInput)"));
    assert!(!client.contains("encodeMatchNotify"));

    // Codec is emitted alongside the channel router so the file is self-contained.
    assert!(client.contains("export interface Codec"));
    // ServiceError gets imported for the router's default-case throw.
    let imports = client
        .lines()
        .find(|l| l.starts_with("import type"))
        .expect("import line present");
    assert!(imports.contains("ServiceError"));
}

#[test]
fn connection_mode_server_emits_channel_handler_router_and_encoders() {
    let server = file(
        &generate_files(&input_with_spec("typescript-server", channel_spec())).expect("generate"),
        "server.gen.ts",
    )
    .to_string();

    // Unary op stays on the handlers interface + dispatched.
    assert!(server.contains("listEvents(req: ListRequest, ctx: RequestContext)"));
    assert!(server.contains("case \"ListEvents\":"));

    // Channel handler interface: server inbound is <-> input_type only.
    // Reverse contributes no inbound handler (server pushes, doesn't receive).
    assert!(server.contains("export interface MatchChannelHandlers"));
    assert!(server.contains("play(msg: PlayerInput, ctx: RequestContext): void;"));
    assert!(
        !server.contains("notify(msg:"),
        "reverse must not produce a server-side inbound handler"
    );

    // Router has the <-> case but not reverse.
    assert!(server.contains("export function routeMatchChannel("));
    assert!(server.contains("case \"Play\":"));
    assert!(
        !server.contains("case \"Notify\":"),
        "no reverse inbound on server side"
    );

    // Outbound encoders for both <-> (GameState) and <- (Acknowledgment).
    assert!(server.contains("export function encodeMatchPlay(codec: Codec, msg: GameState)"));
    assert!(
        server.contains("export function encodeMatchNotify(codec: Codec, msg: Acknowledgment)")
    );
}

#[test]
fn connection_mode_dispatch_does_not_route_channel_ops() {
    // Channel-mode bidi/reverse must NOT show up in the unary `dispatch` switch.
    let server = file(
        &generate_files(&input_with_spec("typescript-server", channel_spec())).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    // ListEvents -> Unidirectional, routed via dispatch.
    assert!(server.contains("case \"ListEvents\":"));
    // <-> and <- ops live in routeMatchChannel, NOT in dispatch.
    let dispatch_block_start = server.find("export async function dispatch").unwrap();
    let dispatch_block = &server[dispatch_block_start..];
    assert!(
        !dispatch_block.contains("case \"Play\":"),
        "bidi <-> must not be dispatched in connection mode"
    );
    assert!(
        !dispatch_block.contains("case \"Notify\":"),
        "reverse <- must not be dispatched in connection mode"
    );
}

#[test]
fn rpc_mode_emits_check_and_send_on_client() {
    let mut input = input_with_spec("typescript-client", channel_spec());
    input.config.options.insert(
        "ts_bidirectional_transport".to_string(),
        serde_json::Value::String("rpc".to_string()),
    );
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();

    // No channel/router/encoder emissions in rpc mode.
    assert!(!client.contains("MatchChannelHandlers"));
    assert!(!client.contains("routeMatchChannel"));
    assert!(!client.contains("encodeMatchPlay"));

    // <-> gets both check + send over the byte seam (sync, codec-encoded).
    assert!(client.contains("sendPlay(req: PlayerInput): void {"));
    assert!(client.contains("this.t.call(\"match\", \"PlaySend\", toPlayerInputCbor(req));"));
    assert!(client.contains("checkPlay(): GameState[] {"));
    assert!(client.contains("this.t.call(\"match\", \"PlayCheck\", new Uint8Array());"));
    assert!(client.contains(
        "return asArray(decode(csilResp)).map((csilE) => fromGameStateCborValue(csilE));"
    ));

    // <- gets check only (no send — server pushes).
    assert!(client.contains("checkNotify(): Acknowledgment[] {"));
    assert!(client.contains("\"NotifyCheck\""));
    assert!(!client.contains("sendNotify"));
}

#[test]
fn rpc_mode_server_dispatches_check_and_send_methods() {
    let mut input = input_with_spec("typescript-server", channel_spec());
    input.config.options.insert(
        "ts_bidirectional_transport".to_string(),
        serde_json::Value::String("rpc".to_string()),
    );
    let server = file(&generate_files(&input).expect("generate"), "server.gen.ts").to_string();

    // Handler interface gets sendPlay/checkPlay/checkNotify; no channel iface.
    assert!(!server.contains("MatchChannelHandlers"));
    assert!(server.contains("sendPlay(req: PlayerInput, ctx: RequestContext): Promise<void>;"));
    assert!(server.contains("checkPlay(ctx: RequestContext): Promise<GameState[]>;"));
    assert!(server.contains("checkNotify(ctx: RequestContext): Promise<Acknowledgment[]>;"));
    assert!(!server.contains("sendNotify"));

    // dispatch routes the synthetic Send/Check methods through call().
    assert!(server.contains("case \"PlaySend\":"));
    assert!(server.contains("await handlers.match.sendPlay(req, ctx);"));
    assert!(server.contains("case \"PlayCheck\":"));
    assert!(server.contains("await handlers.match.checkPlay(ctx)"));
    assert!(server.contains("case \"NotifyCheck\":"));
}

#[test]
fn ts_ws_base_url_emits_hint_constant() {
    let mut input = input_with_spec("typescript-client", channel_spec());
    input.config.options.insert(
        "ts_ws_base_url".to_string(),
        serde_json::Value::String("wss://api.example.com/v1".to_string()),
    );
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();
    assert!(client.contains("export const WS_BASE_URL = \"wss://api.example.com/v1\";"));
}

#[test]
fn reverse_only_service_emits_router_with_no_inbound_cases_on_server() {
    // A service whose channel ops are all `<-` has no server-side inbound;
    // the router emits and the switch is exhaustive on the empty set.
    let mut spec = channel_spec();
    if let CsilRuleType::ServiceDef(ref mut def) = spec.rules.last_mut().unwrap().rule_type {
        def.operations.retain(|op| !common::is_bidirectional(op));
    }
    let server = file(
        &generate_files(&input_with_spec("typescript-server", spec)).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    assert!(server.contains("export interface MatchChannelHandlers"));
    assert!(server.contains("export function routeMatchChannel("));
    // The handler interface is empty (no methods) since reverse has no server inbound.
    let iface_start = server
        .find("export interface MatchChannelHandlers")
        .unwrap();
    let iface_end = server[iface_start..].find("}\n").unwrap() + iface_start;
    let iface_body = &server[iface_start..=iface_end];
    assert!(
        !iface_body.contains("(msg:"),
        "reverse-only service must yield an empty server channel handlers interface, got {iface_body:?}"
    );
}

// ---------------------------------------------------------------------------
// Tagged core types: timestamp + decimal
// ---------------------------------------------------------------------------

/// A spec whose single record carries a `decimal` and a `timestamp` field.
fn money_spec() -> CsilSpecSerialized {
    spec_of(vec![group_rule(
        "Money",
        vec![
            field("amount", builtin("decimal"), false),
            field("captured_at", builtin("timestamp"), false),
        ],
        vec![],
    )])
}

#[test]
fn timestamp_maps_to_date() {
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", money_spec())).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(types.contains("capturedAt: Date;"));
}

#[test]
fn decimal_csil_default_injects_helper_and_no_decimal_js() {
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", money_spec())).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    // Field maps to the generated helper.
    assert!(types.contains("amount: CsilDecimal;"));
    // The helper is injected once, self-contained, with the tag-4 contract.
    assert!(types.contains("export class CsilDecimal {"));
    assert!(types.contains("static readonly CBOR_TAG = 4;"));
    assert!(types.contains("toTag4(): [number, bigint]"));
    assert!(types.contains("static fromString(text: string): CsilDecimal"));
    // Default mode must NOT import decimal.js (the doc comment may still name it
    // as the bridge target, so assert specifically on the import statement).
    assert!(!types.contains("from \"decimal.js\""));
    assert!(!types.contains("import Decimal"));
}

#[test]
fn decimal_library_mode_uses_decimal_js_and_no_helper() {
    let mut input = input_with_spec("typescript-typesonly", money_spec());
    input.config.options.insert(
        "decimal_mapping".to_string(),
        serde_json::Value::String("library".to_string()),
    );
    let types = file(&generate_files(&input).expect("generate"), "types.gen.ts").to_string();
    assert!(types.contains("import Decimal from \"decimal.js\";"));
    assert!(types.contains("amount: Decimal;"));
    // No self-contained helper when the library type is selected.
    assert!(!types.contains("export class CsilDecimal"));
}

#[test]
fn csil_decimal_not_injected_when_decimal_unused() {
    // The sample spec has no `decimal`, so neither the helper nor the import appears.
    let types = file(
        &generate_files(&input_for("typescript-typesonly")).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(!types.contains("CsilDecimal"));
    assert!(!types.contains("decimal.js"));
}

#[test]
fn invalid_decimal_mapping_fails_generation() {
    let mut input = input_with_spec("typescript-typesonly", money_spec());
    input.config.options.insert(
        "decimal_mapping".to_string(),
        serde_json::Value::String("bignum".to_string()),
    );
    let err = generate_files(&input).expect_err("invalid value must fail");
    assert!(
        err.contains("decimal_mapping") && err.contains("bignum"),
        "error must name the option and bad value, got {err:?}"
    );
}

#[test]
fn bare_decimal_op_is_a_non_record_payload_on_client() {
    // The byte-seam client can only (de)serialize record payloads. An op over a bare
    // inline `decimal` is therefore skipped with a note rather than emitting a method
    // that references an undefined codec helper.
    let spec = spec_of(vec![CsilRule {
        name: "PriceService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "Quote".to_string(),
                input_type: builtin("decimal"),
                output_type: builtin("decimal"),
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: vec![],
    }]);
    let mut input = input_with_spec("typescript-client", spec);
    input.csil_spec.service_count = 1;
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();
    assert!(client.contains("// operation 'Quote' has a non-record payload"));
    // No inline decimal reference leaks into the client, so no decimal import is needed.
    assert!(!client.contains("CsilDecimal"));
    assert!(!client.contains("decimal.js"));
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A record exercising both constraint systems: a `.size`-constrained string, a
/// `@MinValue` numeric, an optional `@MaxItems` array, and a `.regex`.
fn constrained_spec() -> CsilSpecSerialized {
    spec_of(vec![group_rule(
        "SignupRequest",
        vec![
            field(
                "username",
                constrained(
                    builtin("text"),
                    vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                        min: 3,
                        max: 20,
                    })],
                ),
                false,
            ),
            field(
                "slug",
                constrained(
                    builtin("text"),
                    vec![CsilControlOperator::Regex("^[a-z]+$".to_string())],
                ),
                false,
            ),
            field_meta(
                "age",
                builtin("int"),
                false,
                vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(18)),
                )],
            ),
            field_meta(
                "tags",
                CsilTypeExpression::Array {
                    element_type: Box::new(builtin("text")),
                    occurrence: Some(CsilOccurrence::ZeroOrMore),
                },
                true,
                vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::MaxItems(5),
                )],
            ),
        ],
        vec![],
    )])
}

#[test]
fn validation_emits_checks_for_both_constraint_systems() {
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", constrained_spec()))
            .expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    assert!(
        types.contains("export function validateSignupRequest(value: SignupRequest): string[] {")
    );
    assert!(types.contains("const errors: string[] = [];"));
    assert!(types.contains("return errors;"));

    // .size (control operator) → length range check.
    assert!(types.contains(
        "if (value.username.length < 3 || value.username.length > 20) errors.push(\"username: length must be between 3 and 20\");"
    ));
    // .regex (control operator) → module-level compiled RegExp + test.
    assert!(types.contains("const signupRequestSlugRe = new RegExp(\"^[a-z]+$\");"));
    assert!(types.contains("if (!signupRequestSlugRe.test(value.slug)) errors.push(\"slug: must match the required pattern\");"));
    // @MinValue (metadata) → numeric guard.
    assert!(types.contains("if (value.age < 18) errors.push(\"age: must be >= 18\");"));
    // @MaxItems on an optional field → guarded by presence test.
    assert!(types.contains("if (value.tags !== undefined) {"));
    assert!(
        types.contains("if (value.tags.length > 5) errors.push(\"tags: must have <= 5 items\");")
    );
}

#[test]
fn constrained_type_alias_emits_value_validator() {
    let spec = spec_of(vec![CsilRule {
        name: "HouseID".to_string(),
        rule_type: CsilRuleType::TypeDef(constrained(
            builtin("text"),
            vec![CsilControlOperator::Size(CsilSizeConstraint::Min(1))],
        )),
        position: pos(),
        doc_comments: vec![],
    }]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(types.contains("export function validateHouseID(value: HouseID): string[] {"));
    assert!(types.contains("if (value.length < 1) errors.push(\"value: length must be >= 1\");"));
}

#[test]
fn encoding_only_constraints_emit_no_validator() {
    // `.cbor` describes wire framing, not validity — no validator should appear.
    let spec = spec_of(vec![CsilRule {
        name: "Payload".to_string(),
        rule_type: CsilRuleType::TypeDef(constrained(
            builtin("bytes"),
            vec![CsilControlOperator::Cbor],
        )),
        position: pos(),
        doc_comments: vec![],
    }]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(!types.contains("validatePayload"));
}

#[test]
fn types_without_constraints_emit_no_validator() {
    // The constraint-free sample spec must produce no validator functions.
    let types = file(
        &generate_files(&input_for("typescript-typesonly")).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(!types.contains("export function validate"));
}

/// The exact spec from the bug report:
/// `user = { balance: decimal .ge "0.00", created_at: timestamp .ge "1970-..." }`
fn tagged_bound_spec() -> CsilSpecSerialized {
    spec_of(vec![group_rule(
        "user",
        vec![
            field(
                "balance",
                constrained(
                    builtin("decimal"),
                    vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                        "0.00".to_string(),
                    ))],
                ),
                false,
            ),
            field(
                "created_at",
                constrained(
                    builtin("timestamp"),
                    vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                        "1970-01-01T00:00:00Z".to_string(),
                    ))],
                ),
                false,
            ),
        ],
        vec![],
    )])
}

/// Every `errors.push(...)` argument must be a single, well-formed double-quoted
/// string literal — i.e. an even number of unescaped `"` after the opening one.
/// This is the syntactic guard against the original defect, which interpolated a
/// raw `"0.00"` and produced `errors.push("...>= "0.00"")`.
fn push_args_are_valid_string_literals(source: &str) {
    for line in source.lines() {
        let Some(rest) = line.split_once("errors.push(").map(|(_, r)| r) else {
            continue;
        };
        let arg = rest.trim_end().trim_end_matches(';').trim_end_matches(')');
        assert!(
            arg.starts_with('"') && arg.ends_with('"') && arg.len() >= 2,
            "push argument is not a quoted string: {arg:?}"
        );
        // Count quotes that are not backslash-escaped; the body between the outer
        // quotes must contain none, or the literal is broken.
        let body = &arg[1..arg.len() - 1];
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next(); // skip the escaped character
            } else {
                assert_ne!(c, '"', "unescaped quote inside string literal: {arg:?}");
            }
        }
    }
}

#[test]
fn tagged_bounds_escape_literals_and_compare_via_in_memory_types() {
    let types = file(
        &generate_files(&input_with_spec(
            "typescript-typesonly",
            tagged_bound_spec(),
        ))
        .expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    // No generated `errors.push` may carry an unescaped quote.
    push_args_are_valid_string_literals(&types);
    // The specific shape of the original defect must be gone.
    assert!(
        !types.contains("0.00\"\""),
        "the unescaped-literal defect is still present"
    );

    // `decimal` bound is reconstructed as CsilDecimal and compared by ordering;
    // the message keeps the (now escaped) bound text.
    assert!(types.contains(
        "if (value.balance.compare(CsilDecimal.fromString(\"0.00\")) < 0) errors.push(\"balance: must be >= \\\"0.00\\\"\");"
    ));

    // `timestamp` bound is reconstructed as a Date and compared chronologically.
    assert!(types.contains(
        "if (value.createdAt.getTime() < new Date(\"1970-01-01T00:00:00Z\").getTime()) errors.push(\"createdAt: must be >= \\\"1970-01-01T00:00:00Z\\\"\");"
    ));

    // The CsilDecimal helper now carries the ordering used by the guard.
    assert!(types.contains("compare(other: CsilDecimal): number"));
}

#[test]
fn decimal_bound_in_library_mode_uses_decimal_js_comparison() {
    let mut input = input_with_spec("typescript-typesonly", tagged_bound_spec());
    input.config.options.insert(
        "decimal_mapping".to_string(),
        serde_json::Value::String("library".to_string()),
    );
    let types = file(&generate_files(&input).expect("generate"), "types.gen.ts").to_string();

    push_args_are_valid_string_literals(&types);
    assert!(types.contains(
        "if (value.balance.cmp(new Decimal(\"0.00\")) < 0) errors.push(\"balance: must be >= \\\"0.00\\\"\");"
    ));
}

#[test]
fn numeric_bounds_keep_direct_comparison() {
    // A regression guard: numeric fields must NOT be routed through decimal/date
    // reconstruction — the bound stays a bare number literal.
    let spec = spec_of(vec![group_rule(
        "Account",
        vec![field(
            "balance",
            constrained(
                builtin("int"),
                vec![CsilControlOperator::GreaterEqual(
                    CsilLiteralValue::Integer(0),
                )],
            ),
            false,
        )],
        vec![],
    )]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(types.contains("if (value.balance < 0) errors.push(\"balance: must be >= 0\");"));
}

#[test]
fn regex_is_hoisted_to_module_level_const() {
    // The compiled RegExp must live at module scope (before the validator
    // function) so it is built once, not on every validate call.
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", constrained_spec()))
            .expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    let const_at = types
        .find("const signupRequestSlugRe = new RegExp(\"^[a-z]+$\");")
        .expect("hoisted regex const present");
    let fn_at = types
        .find("export function validateSignupRequest")
        .expect("validator present");
    assert!(
        const_at < fn_at,
        "regex const must be declared before the validator that uses it"
    );
    // The validator references the const, never an inline RegExp.
    assert!(!types.contains("new RegExp(\"^[a-z]+$\").test"));
    // The const sits at module scope (no leading indentation).
    assert!(
        types
            .lines()
            .any(|l| l == "const signupRequestSlugRe = new RegExp(\"^[a-z]+$\");")
    );
}

/// A service whose single op takes and returns a bare inline `decimal`, so
/// `ts_type` prints `CsilDecimal`/`Decimal` directly into client/server.
fn decimal_op_spec() -> CsilSpecSerialized {
    let mut spec = spec_of(vec![CsilRule {
        name: "PriceService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "Quote".to_string(),
                input_type: builtin("decimal"),
                output_type: builtin("decimal"),
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: vec![],
    }]);
    spec.service_count = 1;
    spec
}

#[test]
fn inline_decimal_in_op_signature_injects_import_in_server() {
    // The server still prints an inline `decimal` straight into a signature, so it
    // injects the value import. The byte-seam client skips such an op (non-record
    // payload), so it references no inline decimal and imports none.
    // csil (default): `CsilDecimal` is a value pulled from the types module.
    let server = file(
        &generate_files(&input_with_spec("typescript-server", decimal_op_spec()))
            .expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    assert!(
        server.contains("import { CsilDecimal } from \"./types.gen\";"),
        "server must import CsilDecimal, got:\n{server}"
    );
    let client = file(
        &generate_files(&input_with_spec("typescript-client", decimal_op_spec()))
            .expect("generate"),
        "client.gen.ts",
    )
    .to_string();
    assert!(
        !client.contains("CsilDecimal"),
        "client imports no inline decimal: {client}"
    );

    // library mode: the server's inline `decimal` maps to `Decimal` from decimal.js.
    let mut input = input_with_spec("typescript-server", decimal_op_spec());
    input.config.options.insert(
        "decimal_mapping".to_string(),
        serde_json::Value::String("library".to_string()),
    );
    let server = file(&generate_files(&input).expect("generate"), "server.gen.ts").to_string();
    assert!(
        server.contains("import Decimal from \"decimal.js\";"),
        "server library mode must import Decimal, got:\n{server}"
    );
}

#[test]
fn text_bound_with_control_char_uses_ts_escapes_not_debug() {
    // A `.eq` text bound carrying a control char must use TS `\uNNNN` escapes,
    // never Rust debug's `\u{NN}` brace form (invalid TypeScript).
    let spec = spec_of(vec![group_rule(
        "Token",
        vec![field(
            "code",
            constrained(
                builtin("text"),
                vec![CsilControlOperator::Equal(CsilLiteralValue::Text(
                    "a\u{1}b".to_string(),
                ))],
            ),
            false,
        )],
        vec![],
    )]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(
        types.contains("value.code !== \"a\\u0001b\""),
        "expected TS unicode escape in comparison, got:\n{types}"
    );
    assert!(
        !types.contains("\\u{1}"),
        "must not emit Rust debug brace escape"
    );
}

#[test]
fn integer_decimal_bound_is_quoted_for_constructor() {
    // The core may hand a `decimal` bound as an Integer (`decimal .ge 0`); both
    // `fromString` and `new Decimal` take a string, so it must be quoted.
    let spec = spec_of(vec![group_rule(
        "Wallet",
        vec![field(
            "balance",
            constrained(
                builtin("decimal"),
                vec![CsilControlOperator::GreaterEqual(
                    CsilLiteralValue::Integer(0),
                )],
            ),
            false,
        )],
        vec![],
    )]);

    // csil (default) mode quotes the integer bound for fromString.
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec.clone())).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    push_args_are_valid_string_literals(&types);
    assert!(types.contains(
        "if (value.balance.compare(CsilDecimal.fromString(\"0\")) < 0) errors.push(\"balance: must be >= 0\");"
    ));
    assert!(
        !types.contains("fromString(0)"),
        "integer bound must not be passed unquoted"
    );

    // library mode uses the string form too, for consistency.
    let mut input = input_with_spec("typescript-typesonly", spec);
    input.config.options.insert(
        "decimal_mapping".to_string(),
        serde_json::Value::String("library".to_string()),
    );
    let types = file(&generate_files(&input).expect("generate"), "types.gen.ts").to_string();
    assert!(types.contains(
        "if (value.balance.cmp(new Decimal(\"0\")) < 0) errors.push(\"balance: must be >= 0\");"
    ));
    assert!(
        !types.contains("new Decimal(0)"),
        "integer bound must not be passed unquoted"
    );
}

// ---------------------------------------------------------------------------
// Tuples, boolean @depends-on, and push-only (`<- Event`) operations
// ---------------------------------------------------------------------------

/// A single tuple slot: `key` is `None` for a positional element, `Some(Bare)`
/// for a labeled one.
fn tuple_entry(
    key: Option<CsilGroupKey>,
    ty: CsilTypeExpression,
    optional: bool,
) -> CsilGroupEntry {
    CsilGroupEntry {
        key,
        value_type: ty,
        occurrence: optional.then_some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: vec![],
    }
}

fn alias_rule(name: &str, ty: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(ty),
        position: pos(),
        doc_comments: vec![],
    }
}

#[test]
fn tuple_maps_to_positional_and_labeled_ts_tuple() {
    // `[text, int, bool]` is positional; an optional trailing slot becomes `?`.
    let positional = CsilTypeExpression::Tuple(CsilGroupExpression {
        entries: vec![
            tuple_entry(None, builtin("text"), false),
            tuple_entry(None, builtin("int"), false),
            tuple_entry(None, builtin("bool"), true),
        ],
    });
    // `[tag: text, value: any]` is fully keyed, so it is a labeled tuple.
    let labeled = CsilTypeExpression::Tuple(CsilGroupExpression {
        entries: vec![
            tuple_entry(bare("tag"), builtin("text"), false),
            tuple_entry(bare("value"), builtin("any"), false),
        ],
    });
    let spec = spec_of(vec![
        alias_rule("Row", positional),
        alias_rule("Tagged", labeled),
    ]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    assert!(
        types.contains("export type Row = [string, number, boolean?];"),
        "positional tuple, got: {types}"
    );
    assert!(
        types.contains("export type Tagged = [tag: string, value: any];"),
        "labeled tuple, got: {types}"
    );
}

#[test]
fn depends_on_expr_renders_as_jsdoc_note() {
    // country != "US" && (shipping_method == "express" || has_phone)
    let cond = CsilDependsCondition::All(vec![
        CsilDependsCondition::Compare {
            field: "country".to_string(),
            op: Some(CsilDependsCompareOp::Ne),
            value: Some(CsilLiteralValue::Text("US".to_string())),
        },
        CsilDependsCondition::Any(vec![
            CsilDependsCondition::Compare {
                field: "shipping_method".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("express".to_string())),
            },
            // No operator: a bare presence check.
            CsilDependsCondition::Compare {
                field: "has_phone".to_string(),
                op: None,
                value: None,
            },
        ]),
    ]);
    let spec = spec_of(vec![group_rule(
        "ShippingForm",
        vec![
            field("country", builtin("text"), false),
            field_meta(
                "postal_code",
                builtin("text"),
                true,
                vec![CsilFieldMetadata::DependsOnExpr(cond)],
            ),
        ],
        vec![],
    )]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    // Field names are camelCased; `&&`/`||` join with nested groups parenthesized.
    assert!(
        types.contains(
            "@depends-on country !== \"US\" && (shippingMethod === \"express\" || hasPhone)"
        ),
        "got: {types}"
    );
    assert!(types.contains("postalCode?: string;"));
}

#[test]
fn labeled_tuple_optional_before_required_stays_valid_ts() {
    // `[note?: text, id: int]`: an optional element precedes a required one. A `?`
    // suffix here is TS1257; the optional slot must instead admit `undefined`.
    let tuple = CsilTypeExpression::Tuple(CsilGroupExpression {
        entries: vec![
            tuple_entry(bare("note"), builtin("text"), true),
            tuple_entry(bare("id"), builtin("int"), false),
        ],
    });
    let spec = spec_of(vec![alias_rule("Entry", tuple)]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    assert!(
        types.contains("export type Entry = [note: string | undefined, id: number];"),
        "non-trailing optional must become `T | undefined`, got: {types}"
    );
    // The defective `?`-before-required shape must be gone.
    assert!(
        !types.contains("note?: string"),
        "optional `?` must not precede a required element, got: {types}"
    );
}

#[test]
fn positional_tuple_optional_before_required_stays_valid_ts() {
    // The positional twin of the above: `[text?, int]` → `[string | undefined, number]`.
    let tuple = CsilTypeExpression::Tuple(CsilGroupExpression {
        entries: vec![
            tuple_entry(None, builtin("text"), true),
            tuple_entry(None, builtin("int"), false),
        ],
    });
    let spec = spec_of(vec![alias_rule("Row", tuple)]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();
    assert!(
        types.contains("export type Row = [string | undefined, number];"),
        "got: {types}"
    );
    assert!(!types.contains("string?, number"), "got: {types}");
}

#[test]
fn depends_on_value_with_comment_terminator_is_sanitized() {
    // A `@depends-on` value carrying `*/` would close the JSDoc block early; the
    // emitted comment must neutralize it.
    let cond = CsilDependsCondition::Compare {
        field: "note".to_string(),
        op: Some(CsilDependsCompareOp::Eq),
        value: Some(CsilLiteralValue::Text("x*/y".to_string())),
    };
    let spec = spec_of(vec![group_rule(
        "Form",
        vec![field_meta(
            "field",
            builtin("text"),
            true,
            vec![CsilFieldMetadata::DependsOnExpr(cond)],
        )],
        vec![],
    )]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    // The note line carries the sanitized form, never a raw `*/` inside the value.
    let note = types
        .lines()
        .find(|l| l.contains("@depends-on"))
        .expect("depends-on note present");
    assert!(
        !note.contains("*/"),
        "raw comment terminator must be neutralized, got: {note:?}"
    );
    assert!(
        note.contains("x*\\/y"),
        "value must be present in escaped form, got: {note:?}"
    );
}

#[test]
fn both_depends_on_forms_render_as_jsdoc_notes() {
    // The simple equality form stays `DependsOn`; the `!=` form becomes
    // `DependsOnExpr`. Both must surface a note.
    let spec = spec_of(vec![group_rule(
        "Order",
        vec![
            field_meta(
                "shipped_at",
                builtin("timestamp"),
                true,
                vec![CsilFieldMetadata::DependsOn {
                    field: "status".to_string(),
                    value: Some(CsilLiteralValue::Text("active".to_string())),
                }],
            ),
            field_meta(
                "cancelled_at",
                builtin("timestamp"),
                true,
                vec![CsilFieldMetadata::DependsOnExpr(
                    CsilDependsCondition::Compare {
                        field: "status".to_string(),
                        op: Some(CsilDependsCompareOp::Ne),
                        value: Some(CsilLiteralValue::Text("active".to_string())),
                    },
                )],
            ),
        ],
        vec![],
    )]);
    let types = file(
        &generate_files(&input_with_spec("typescript-typesonly", spec)).expect("generate"),
        "types.gen.ts",
    )
    .to_string();

    assert!(
        types.contains("@depends-on status === \"active\""),
        "simple form must render, got: {types}"
    );
    assert!(
        types.contains("@depends-on status !== \"active\""),
        "expr form must render, got: {types}"
    );
}

#[test]
fn unidirectional_op_with_null_input_omits_request_param() {
    // A push op `-> Event` reaches the generator as a Unidirectional op whose
    // input is the `null` builtin; the unary client method must drop the request
    // parameter instead of emitting `req: null`.
    let mut rules = vec![group_rule(
        "Event",
        vec![field("kind", builtin("text"), false)],
        vec![],
    )];
    rules.push(CsilRule {
        name: "FeedService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "poll-event".to_string(),
                input_type: builtin("null"),
                output_type: reference("Event"),
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: vec![],
    });
    let spec = CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    };

    let client = file(
        &generate_files(&input_with_spec("typescript-client", spec)).expect("generate"),
        "client.gen.ts",
    )
    .to_string();

    assert!(
        client.contains("pollEvent(): Event {"),
        "null input must drop the request param, got: {client}"
    );
    assert!(
        !client.contains("req: null"),
        "null input must not surface as a request param, got: {client}"
    );
    assert!(
        client.contains("this.t.call(\"feed\", \"PollEvent\", new Uint8Array());"),
        "call must send an empty payload for the null request, got: {client}"
    );
    assert!(
        client.contains("return fromEventCbor(csilResp);"),
        "null-input op still decodes its record response, got: {client}"
    );
}

#[test]
fn push_only_reverse_op_with_null_input_emits_cleanly() {
    // `new-event: <- Event` reaches the generator as a Reverse op whose
    // input_type is the `null` builtin (the server pushes; there is no request).
    let mut rules = vec![group_rule(
        "Event",
        vec![field("kind", builtin("text"), false)],
        vec![],
    )];
    rules.push(CsilRule {
        name: "FeedService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "new-event".to_string(),
                input_type: builtin("null"),
                output_type: reference("Event"),
                direction: CsilServiceDirection::Reverse,
                position: pos(),
                doc_comments: vec![],
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: vec![],
    });
    let spec = CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    };

    // Connection mode (default): client receives via a handler, server pushes via
    // an encoder, and `null` never surfaces as a request parameter anywhere.
    let client = file(
        &generate_files(&input_with_spec("typescript-client", spec.clone())).expect("generate"),
        "client.gen.ts",
    )
    .to_string();
    assert!(
        client.contains("newEvent(msg: Event): void;"),
        "got: {client}"
    );
    assert!(
        !client.contains("req: null"),
        "null input must not surface as a request param"
    );

    let server = file(
        &generate_files(&input_with_spec("typescript-server", spec.clone())).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    assert!(
        server.contains("export function encodeFeedNewEvent(codec: Codec, msg: Event)"),
        "got: {server}"
    );
    assert!(!server.contains("req: null"));

    // RPC mode degrades reverse to a `check<Op>` poll; still no request param.
    let mut input = input_with_spec("typescript-server", spec);
    input.config.options.insert(
        "ts_bidirectional_transport".to_string(),
        serde_json::Value::String("rpc".to_string()),
    );
    let server_rpc = file(&generate_files(&input).expect("generate"), "server.gen.ts").to_string();
    assert!(
        server_rpc.contains("checkNewEvent(ctx: RequestContext): Promise<Event[]>;"),
        "got: {server_rpc}"
    );
    assert!(!server_rpc.contains("req: null"));
}

fn wire_id_spec() -> CsilSpecSerialized {
    let mut rules = vec![
        group_rule("Order", vec![field("id", builtin("text"), false)], vec![]),
        group_rule("Receipt", vec![field("id", builtin("text"), false)], vec![]),
    ];
    let mut place = op("place-order", "Order", "Receipt", vec![]);
    place.wire_id = Some(7);
    let cancel = op("cancel-order", "Order", "Receipt", vec![]);
    rules.push(CsilRule {
        name: "OrderService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![place, cancel],
            wire_id: Some(3),
        }),
        position: pos(),
        doc_comments: vec![],
    });
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

#[test]
fn wire_ids_const_emitted_when_present() {
    let server = file(
        &generate_files(&input_with_spec("typescript-server", wire_id_spec())).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    assert!(
        server.contains("export const OrderWireIds = {"),
        "expected wire-ids const, got: {server}"
    );
    assert!(
        server.contains("service: 3,"),
        "expected service ordinal, got: {server}"
    );
    assert!(
        server.contains("ops: {"),
        "expected nested ops object, got: {server}"
    );
    assert!(
        server.contains("placeOrder: 7,"),
        "expected operation ordinal, got: {server}"
    );
    assert!(
        server.contains("} as const;"),
        "expected `as const`, got: {server}"
    );
    // Operation without a wire-id contributes no key.
    assert!(
        !server.contains("cancelOrder:"),
        "operation without wire-id must not appear, got: {server}"
    );
}

#[test]
fn wire_ids_op_named_service_does_not_collide() {
    let mut spec = wire_id_spec();
    if let CsilRuleType::ServiceDef(service) = &mut spec.rules.last_mut().unwrap().rule_type {
        service.operations[0].name = "service".to_string();
    }
    let server = file(
        &generate_files(&input_with_spec("typescript-server", spec)).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    // The op named `service` nests under `ops`, so the top-level `service`
    // ordinal key is never overwritten.
    assert!(
        server.contains("service: 3,"),
        "expected service ordinal, got: {server}"
    );
    assert!(
        server.contains("ops: {"),
        "expected nested ops object, got: {server}"
    );
    assert!(
        server.contains("service: 7,"),
        "expected nested op ordinal, got: {server}"
    );
}

#[test]
fn wire_ids_const_absent_when_unset() {
    let server = file(
        &generate_files(&input_for("typescript-server")).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    assert!(
        !server.contains("WireIds"),
        "no wire-id output when services have no wire-id, got: {server}"
    );
}

#[test]
fn compact_dispatch_emitted_for_wire_id_spec() {
    let server = file(
        &generate_files(&input_with_spec("typescript-server", wire_id_spec())).expect("generate"),
        "server.gen.ts",
    )
    .to_string();

    // Verbose dispatch stays byte-identical alongside the compact twin.
    assert!(
        server.contains("export async function dispatch("),
        "verbose dispatch expected, got: {server}"
    );
    // Compact twin routes on numeric service + op ordinals.
    assert!(
        server.contains("export async function dispatchCompact("),
        "compact dispatch expected, got: {server}"
    );
    assert!(
        server.contains("service: number,"),
        "compact dispatch keys on numeric ordinals, got: {server}"
    );
    assert!(
        server.contains("case 3: {"),
        "compact dispatch matches the service ordinal, got: {server}"
    );
    assert!(
        server.contains("case 7: {"),
        "compact dispatch matches the op ordinal, got: {server}"
    );
    assert!(
        server.contains("const res = await handlers.order.placeOrder(req, ctx);"),
        "compact dispatch routes to the handler, got: {server}"
    );
    assert!(
        server.contains("unknown service ordinal"),
        "compact dispatch has an ordinal fallthrough, got: {server}"
    );
}

// channel_spec() with `@wire-id` ordinals added so the compact channel router
// twin has a bidirectional op to dispatch on.
fn wire_id_channel_spec() -> CsilSpecSerialized {
    let mut spec = channel_spec();
    if let CsilRuleType::ServiceDef(service) = &mut spec.rules.last_mut().unwrap().rule_type {
        service.wire_id = Some(1);
        for op in &mut service.operations {
            if matches!(op.direction, CsilServiceDirection::Bidirectional) {
                op.wire_id = Some(5);
            }
        }
    }
    spec
}

#[test]
fn compact_channel_router_emitted_for_wire_id_channel_spec() {
    let server = file(
        &generate_files(&input_with_spec(
            "typescript-server",
            wire_id_channel_spec(),
        ))
        .expect("generate"),
        "server.gen.ts",
    )
    .to_string();

    // Verbose router stays byte-identical alongside the compact twin.
    assert!(
        server.contains("export function routeMatchChannel("),
        "verbose router expected, got: {server}"
    );
    // Compact twin dispatches on the op ordinal, not the wire name.
    assert!(
        server.contains("export function routeMatchChannelCompact("),
        "compact router expected, got: {server}"
    );
    assert!(
        server.contains("op: number,"),
        "compact router keys on a numeric ordinal, got: {server}"
    );
    assert!(
        server.contains("case 5:"),
        "compact router matches the op ordinal, got: {server}"
    );
    assert!(
        server.contains("unknown channel ordinal"),
        "compact router has an ordinal fallthrough, got: {server}"
    );
}

#[test]
fn compact_dispatch_absent_when_unset() {
    let server = file(
        &generate_files(&input_for("typescript-server")).expect("generate"),
        "server.gen.ts",
    )
    .to_string();
    // The verbose dispatch survives; the compact twin must not appear.
    assert!(
        server.contains("export async function dispatch("),
        "verbose dispatch expected, got: {server}"
    );
    assert!(
        !server.contains("Compact"),
        "no compact routing without wire-ids, got: {server}"
    );
}

// ---------------------------------------------------------------------------
// Per-type CBOR codec + typed byte-seam client
// ---------------------------------------------------------------------------

fn map_ty(key: &str, value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Map {
        key: Box::new(builtin(key)),
        value: Box::new(builtin(value)),
        occurrence: None,
    }
}

fn list_ty(elem: &str) -> CsilTypeExpression {
    CsilTypeExpression::Array {
        element_type: Box::new(builtin(elem)),
        occurrence: None,
    }
}

/// A corndogs-shaped spec exercising the codec: text, bytes, an optional int, a
/// map, a list, a nested record, and a service whose output is a `Res / Error`
/// choice.
fn corndogs_spec() -> CsilSpecSerialized {
    let mut spec = spec_of(vec![
        group_rule(
            "Task",
            vec![
                field("uuid", builtin("text"), false),
                field("current_state", builtin("text"), false),
                field("payload", builtin("bytes"), false),
                field("priority", builtin("int"), true),
                field("labels", map_ty("text", "int"), false),
                field("tags", list_ty("text"), false),
            ],
            vec![],
        ),
        group_rule(
            "SubmitTaskRequest",
            vec![
                field("task", reference("Task"), false),
                field("queue", builtin("text"), false),
            ],
            vec![],
        ),
        CsilRule {
            name: "CorndogsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "submit-task".to_string(),
                    input_type: reference("SubmitTaskRequest"),
                    output_type: CsilTypeExpression::Choice(vec![
                        reference("Task"),
                        reference("ServiceError"),
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
        },
    ]);
    spec.service_count = 1;
    spec
}

/// Reproduces the named-map-alias regression: a field whose type is a named map
/// alias (`StringInt64Map = {* text => int}`) and a map-of-record alias
/// (`QueueAndStateCountsMap = {* text => QueueAndStateCounts}`). Before the codec
/// resolved transparent aliases, such fields were stubbed and dropped every entry.
fn map_alias_spec() -> CsilSpecSerialized {
    spec_of(vec![
        alias_rule("StringInt64Map", map_ty("text", "int")),
        group_rule(
            "QueueAndStateCounts",
            vec![
                field("active", builtin("int"), false),
                field("paused", builtin("int"), false),
            ],
            vec![],
        ),
        alias_rule(
            "QueueAndStateCountsMap",
            CsilTypeExpression::Map {
                key: Box::new(builtin("text")),
                value: Box::new(reference("QueueAndStateCounts")),
                occurrence: None,
            },
        ),
        group_rule(
            "GetCountsResponse",
            vec![
                field("queue_counts", reference("StringInt64Map"), false),
                field("total_task_count", builtin("int"), false),
                field(
                    "queue_and_state_counts",
                    reference("QueueAndStateCountsMap"),
                    false,
                ),
            ],
            vec![],
        ),
    ])
}

#[test]
fn codec_file_emits_runtime_and_per_record_helpers() {
    let files = generate_files(&input_with_spec("typescript-client", corndogs_spec())).unwrap();
    let codec = file(&files, "codec.gen.ts");

    // Self-contained value model + runtime entry points.
    assert!(codec.contains("export type CborValue ="));
    assert!(codec.contains("export function encodeValue(value: CborValue): Uint8Array {"));
    assert!(codec.contains("export function decode(bytes: Uint8Array): CborValue {"));
    // Per-record byte-level + value-level helpers.
    assert!(codec.contains("export function toTaskCbor(v: Task): Uint8Array {"));
    assert!(codec.contains("export function fromTaskCbor(bytes: Uint8Array): Task {"));
    assert!(codec.contains("export function toTaskCborValue(v: Task): CborValue {"));
    assert!(codec.contains("export function fromTaskCborValue(value: CborValue): Task {"));
    // Wire keys are the verbatim CSIL field names, not the camelCase TS members.
    assert!(codec.contains("csilMap.set(\"current_state\", v.currentState);"));
    assert!(codec.contains("currentState: asString(requireKey(value, \"current_state\")),"));
    // Canonical RFC 8949 key order at generation time: among Task's keys, "tags"
    // (len 4, 't'<'u') precedes "uuid"; "current_state" (len 13) is last.
    let tags_at = codec.find("csilMap.set(\"tags\"").unwrap();
    let uuid_at = codec.find("csilMap.set(\"uuid\"").unwrap();
    let state_at = codec.find("csilMap.set(\"current_state\"").unwrap();
    assert!(
        tags_at < uuid_at && uuid_at < state_at,
        "non-canonical key order:\n{codec}"
    );
    // An absent optional is omitted from the wire map.
    assert!(codec.contains("if (v.priority !== undefined) csilMap.set(\"priority\", v.priority);"));
    // bytes stays a Uint8Array (CBOR byte string, major type 2).
    assert!(codec.contains("csilMap.set(\"payload\", v.payload);"));
    // A nested record recurses into its own codec.
    assert!(codec.contains("csilMap.set(\"task\", toTaskCborValue(v.task));"));
}

#[test]
fn codec_absent_without_records() {
    // A services-only spec (no record types) emits no codec file.
    let files = generate_files(&decimal_op_spec_input()).unwrap();
    assert!(
        !files.iter().any(|f| f.path == "codec.gen.ts"),
        "no codec without records"
    );
}

fn decimal_op_spec_input() -> WasmGeneratorInput {
    input_with_spec("typescript-client", decimal_op_spec())
}

/// Compile (type-check) and run the generated TypeScript, round-tripping a typed
/// request/response through both the codec and the typed client. Skips when node
/// or npx is unavailable so the suite stays portable.
#[test]
fn codec_round_trips_through_typescript() {
    let have_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    let have_npx = std::process::Command::new("npx")
        .arg("--version")
        .output()
        .is_ok();
    if !have_node || !have_npx {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }

    let files = generate_files(&input_with_spec("typescript-client", corndogs_spec())).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-codec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.ts"), CODEC_DRIVER_TS).unwrap();
    std::fs::write(dir.join("tsconfig.json"), CODEC_TSCONFIG).unwrap();

    // Type-check and transpile to CommonJS JS (there is no `tsc` binary, but a pinned
    // `typescript@5` via npx provides one), then run the emitted driver under node.
    let build = std::process::Command::new("npx")
        .args(["-y", "-p", "typescript@5", "tsc", "-p", "tsconfig.json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "tsc type-check/compile failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = std::process::Command::new("node")
        .arg(dir.join("out").join("driver.js"))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "node round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codec_walks_named_map_alias_fields() {
    // The regression: a field typed as a named map alias was stubbed. The codec must
    // instead resolve the alias to its underlying map and walk the entries.
    let files = generate_files(&input_with_spec("typescript-client", map_alias_spec())).unwrap();
    let codec = file(&files, "codec.gen.ts");

    // `queue_counts: StringInt64Map` (`{* text => int}`) encodes as a real CBOR map of
    // its entries, not a `null`/identity stub, and decodes back to an object.
    assert!(
        codec.contains(
            "csilMap.set(\"queue_counts\", new Map<CborValue, CborValue>(Object.entries(v.queueCounts)"
        ),
        "named map alias not walked on encode:\n{codec}"
    );
    assert!(
        codec.contains(
            "queueCounts: Object.fromEntries(Array.from(asMap(requireKey(value, \"queue_counts\"))"
        ),
        "named map alias not walked on decode:\n{codec}"
    );
    // A map-of-record alias recurses into the value record's own codec.
    assert!(
        codec.contains("toQueueAndStateCountsCborValue(csilV)"),
        "map-of-record alias does not recurse into the record codec:\n{codec}"
    );
    assert!(
        codec.contains("fromQueueAndStateCountsCborValue(csilV)"),
        "map-of-record alias does not recurse into the record decoder:\n{codec}"
    );
}

/// End-to-end proof the named-map-alias fix preserves data: type-check the
/// generated codec and round-trip a populated map-alias / map-of-record record
/// through `from*Cbor(to*Cbor(x))` under node, asserting every entry survives.
/// Skips when node/npx is unavailable so the suite stays portable.
#[test]
fn codec_round_trips_named_map_aliases() {
    let have_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    let have_npx = std::process::Command::new("npx")
        .arg("--version")
        .output()
        .is_ok();
    if !have_node || !have_npx {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }

    let files = generate_files(&input_with_spec("typescript-client", map_alias_spec())).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-mapalias-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.ts"), CODEC_MAP_ALIAS_DRIVER_TS).unwrap();
    std::fs::write(dir.join("tsconfig.json"), CODEC_TSCONFIG).unwrap();

    let build = std::process::Command::new("npx")
        .args(["-y", "-p", "typescript@5", "tsc", "-p", "tsconfig.json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "tsc type-check/compile failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = std::process::Command::new("node")
        .arg(dir.join("out").join("driver.js"))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "node round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The tagged core types (`timestamp` -> tag 0 `Date`, `decimal` -> tag 4
/// `CsilDecimal`) are not exercised by the corndogs round-trip, so type-check a
/// money record's emitted codec to confirm those paths compile. Compile-only (csil
/// mapping pulls in no third-party package). Skips when node/npx is unavailable.
#[test]
fn tagged_types_codec_type_checks() {
    let have_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    let have_npx = std::process::Command::new("npx")
        .arg("--version")
        .output()
        .is_ok();
    if !have_node || !have_npx {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }

    let files =
        generate_files(&input_with_spec("typescript-typesonly", money_spec())).expect("generate");
    let dir = std::env::temp_dir().join(format!("csilgen-ts-tagged-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("tsconfig.json"), CODEC_TSCONFIG_NOEMIT).unwrap();

    let build = std::process::Command::new("npx")
        .args(["-y", "-p", "typescript@5", "tsc", "-p", "tsconfig.json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "tagged-types codec failed to type-check:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Self-contained publishable package (emit_packages)
// ---------------------------------------------------------------------------

fn package_input(target: &str) -> WasmGeneratorInput {
    let mut input = input_for(target);
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    input
}

fn package_input_with_spec(target: &str, spec: CsilSpecSerialized) -> WasmGeneratorInput {
    let mut input = input_with_spec(target, spec);
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    input
}

#[test]
fn package_files_absent_by_default() {
    let files = generate_files(&input_for("typescript-client")).expect("generate");
    for path in ["package.json", "tsconfig.json", "index.ts"] {
        assert!(
            !files.iter().any(|f| f.path == path),
            "{path} must not be emitted without emit_packages"
        );
    }
}

#[test]
fn package_files_absent_when_emit_packages_excludes_typescript() {
    let mut input = input_for("typescript-client");
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["rust", "go"]),
    );
    let files = generate_files(&input).expect("generate");
    assert!(!files.iter().any(|f| f.path == "package.json"));
    assert!(!files.iter().any(|f| f.path == "tsconfig.json"));
}

#[test]
fn package_emit_tolerates_malformed_option() {
    // A non-array, non-"typescript" value must not trip the parse or emit a package.
    for bad in [
        serde_json::json!("rust"),
        serde_json::json!(42),
        serde_json::json!({ "typescript": true }),
    ] {
        let mut input = input_for("typescript-client");
        input
            .config
            .options
            .insert("emit_packages".to_string(), bad);
        let files = generate_files(&input).expect("generate");
        assert!(!files.iter().any(|f| f.path == "package.json"));
    }

    // A bare string "typescript" is accepted as a convenience.
    let mut input = input_for("typescript-client");
    input
        .config
        .options
        .insert("emit_packages".to_string(), serde_json::json!("typescript"));
    let files = generate_files(&input).expect("generate");
    assert!(files.iter().any(|f| f.path == "package.json"));
}

#[test]
fn package_json_has_derived_name_and_defaults() {
    let files = generate_files(&package_input("typescript-client")).expect("generate");
    let pkg: serde_json::Value =
        serde_json::from_str(file(&files, "package.json")).expect("valid package.json");

    // sample_spec's first service alphabetically is AuthService -> auth-client.
    assert_eq!(pkg["name"], "auth-client");
    assert_eq!(pkg["version"], "0.1.0");
    assert_eq!(pkg["type"], "commonjs");
    assert_eq!(pkg["main"], "dist/index.js");
    assert_eq!(pkg["types"], "dist/index.d.ts");
    assert_eq!(pkg["scripts"]["build"], "tsc");
    assert!(
        pkg["devDependencies"]["typescript"].is_string(),
        "typescript dev dependency present"
    );
    assert_eq!(pkg["exports"]["."]["types"], "./dist/index.d.ts");

    let tsconfig: serde_json::Value =
        serde_json::from_str(file(&files, "tsconfig.json")).expect("valid tsconfig.json");
    assert_eq!(tsconfig["compilerOptions"]["strict"], true);
    assert_eq!(tsconfig["compilerOptions"]["declaration"], true);
    assert_eq!(tsconfig["compilerOptions"]["outDir"], "dist");

    // The barrel re-exports the generated modules and is the package `main` source.
    let index = file(&files, "index.ts");
    assert!(index.contains("from \"./types.gen\""));
    assert!(index.contains("from \"./codec.gen\""));
    assert!(index.contains("from \"./client.gen\""));
}

#[test]
fn package_name_falls_back_to_csilgen_client_without_services() {
    // money_spec carries records but no service, so no base name can be derived.
    let mut input = input_with_spec("typescript-typesonly", money_spec());
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    let pkg: serde_json::Value = serde_json::from_str(file(
        &generate_files(&input).expect("generate"),
        "package.json",
    ))
    .expect("valid package.json");
    assert_eq!(pkg["name"], "csilgen-client");
}

#[test]
fn package_name_and_version_options_override() {
    let mut input = package_input("typescript-client");
    input.config.options.insert(
        "package_name".to_string(),
        serde_json::json!("@acme/longhouse"),
    );
    input
        .config
        .options
        .insert("package_version".to_string(), serde_json::json!("2.3.4"));
    let pkg: serde_json::Value = serde_json::from_str(file(
        &generate_files(&input).expect("generate"),
        "package.json",
    ))
    .expect("valid package.json");
    assert_eq!(pkg["name"], "@acme/longhouse");
    assert_eq!(pkg["version"], "2.3.4");
}

#[test]
fn barrel_dedupes_codec_collision_for_aggregate_target() {
    // A channel spec's aggregate target emits both client.gen and server.gen, which
    // both export `Codec`. The barrel must star-export the first and fall back to a
    // named re-export for the second to avoid TS2308.
    let files =
        generate_files(&package_input_with_spec("typescript", channel_spec())).expect("generate");
    let index = file(&files, "index.ts");
    assert!(index.contains("export * from \"./client.gen\";"));
    assert!(
        !index.contains("export * from \"./server.gen\";"),
        "server must not be star-exported alongside client, got:\n{index}"
    );
    // Server still contributes its unique surface explicitly.
    assert!(index.contains("from \"./server.gen\";"));
    assert!(index.contains("dispatch"));
    assert!(index.contains("ServerHandlers"));
}

/// Generate a package from `input`, write it to a temp dir, and type-check it with
/// the pinned `typescript@5` to prove the output directory is a valid npm package.
/// Skips when node/npx is unavailable so the suite stays portable.
fn typecheck_emitted_package(label: &str, input: &WasmGeneratorInput) {
    let have_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    let have_npx = std::process::Command::new("npx")
        .arg("--version")
        .output()
        .is_ok();
    if !have_node || !have_npx {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }

    let files = generate_files(input).expect("generate");
    // Sanity: the package scaffolding is present.
    for path in ["package.json", "tsconfig.json", "index.ts"] {
        assert!(
            files.iter().any(|f| f.path == path),
            "{path} missing from emitted package"
        );
    }

    let dir = std::env::temp_dir().join(format!("csilgen-ts-pkg-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }

    // `-p typescript@5` installs the compiler; `tsc -p tsconfig.json --noEmit`
    // type-checks the package exactly as a consumer's `npm run build` would, minus
    // the file emission.
    let build = std::process::Command::new("npx")
        .args([
            "-y",
            "-p",
            "typescript@5",
            "tsc",
            "-p",
            "tsconfig.json",
            "--noEmit",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "emitted {label} package failed to type-check:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emitted_client_package_type_checks() {
    typecheck_emitted_package("client", &package_input("typescript-client"));
}

#[test]
fn emitted_aggregate_package_type_checks() {
    // A channel spec exercises the barrel's collision handling end to end: client and
    // server both export `Codec`, so the package only type-checks if the barrel
    // deduped them.
    typecheck_emitted_package(
        "aggregate",
        &package_input_with_spec("typescript", channel_spec()),
    );
}

const CODEC_TSCONFIG_NOEMIT: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "lib": ["es2020", "dom"],
    "noEmit": true
  },
  "include": ["*.ts"]
}
"#;

const CODEC_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "lib": ["es2020", "dom"],
    "outDir": "out"
  },
  "include": ["*.ts"]
}
"#;

const CODEC_MAP_ALIAS_DRIVER_TS: &str = r#"import {
  toGetCountsResponseCbor,
  fromGetCountsResponseCbor,
} from "./codec.gen";
import type { GetCountsResponse } from "./types.gen";

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error("check failed: " + what);
}

const resp: GetCountsResponse = {
  queueCounts: { q1: 3, q2: 1 },
  totalTaskCount: 4,
  queueAndStateCounts: {
    q1: { active: 2, paused: 1 },
    q2: { active: 0, paused: 5 },
  },
};

const back = fromGetCountsResponseCbor(toGetCountsResponseCbor(resp));
check(back.totalTaskCount === 4, "total_task_count");
// A named map-alias field must survive the round-trip with every entry intact.
check(Object.keys(back.queueCounts).length === 2, "queue_counts size");
check(back.queueCounts.q1 === 3 && back.queueCounts.q2 === 1, "queue_counts entries");
// A map-of-record alias recurses into the record codec for each value.
check(Object.keys(back.queueAndStateCounts).length === 2, "nested size");
check(back.queueAndStateCounts.q1.active === 2, "nested q1 active");
check(back.queueAndStateCounts.q2.paused === 5, "nested q2 paused");

console.log("ok");
"#;

const CODEC_DRIVER_TS: &str = r#"import { CorndogsClient, type ServiceTransport } from "./client.gen";
import {
  toSubmitTaskRequestCbor,
  fromSubmitTaskRequestCbor,
  toTaskCbor,
} from "./codec.gen";
import type { Task, SubmitTaskRequest } from "./types.gen";

class Loopback implements ServiceTransport {
  call(_service: string, _op: string, req: Uint8Array): Uint8Array {
    // Decode the typed request, then echo its task back as the typed response.
    const reqObj = fromSubmitTaskRequestCbor(req);
    return toTaskCbor(reqObj.task);
  }
}

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error("check failed: " + what);
}

function bytesEq(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

const payload = new Uint8Array([0xde, 0xad, 0xbe]);
const task: Task = {
  uuid: "u-123",
  currentState: "PENDING",
  payload,
  priority: 7,
  labels: { a: 1, b: 2 },
  tags: ["x", "y"],
};
const req: SubmitTaskRequest = { task, queue: "default" };

// Direct codec round-trip through the nested record.
const back = fromSubmitTaskRequestCbor(toSubmitTaskRequestCbor(req));
check(back.task.uuid === "u-123", "uuid");
check(back.task.currentState === "PENDING", "current_state");
check(bytesEq(back.task.payload, payload), "payload");
check(back.task.priority === 7, "priority");
check(back.task.labels.a === 1 && back.task.labels.b === 2, "labels");
check(back.task.tags.length === 2 && back.task.tags[1] === "y", "tags");
check(back.queue === "default", "queue");

// An absent optional must round-trip to undefined.
const task2: Task = {
  uuid: "u",
  currentState: "S",
  payload: new Uint8Array(),
  labels: {},
  tags: [],
};
const back2 = fromSubmitTaskRequestCbor(
  toSubmitTaskRequestCbor({ task: task2, queue: "q" }),
);
check(back2.task.priority === undefined, "absent optional");

// Typed client over the loopback carrier.
const client = new CorndogsClient(new Loopback());
const resp = client.submitTask(req);
check(resp.uuid === "u-123", "resp uuid");
check(bytesEq(resp.payload, payload), "resp payload");
check(resp.priority === 7, "resp priority");

console.log("ok");
"#;
