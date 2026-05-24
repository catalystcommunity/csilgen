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
    assert_eq!(files.len(), 1);
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
    assert_eq!(paths, vec!["types.gen.ts", "client.gen.ts"]);

    let client = file(&files, "client.gen.ts");
    // type-only import from the companion types module
    assert!(client.contains("import type {"));
    assert!(client.contains("} from \"./types.gen\";"));
    // transport interface present
    assert!(client.contains("export interface ServiceTransport {"));
    // per-service classes
    assert!(client.contains("export class AuthClient {"));
    assert!(client.contains("export class MemberClient {"));
    // camelCase method + wire strings (lowercase service, PascalCase method)
    assert!(client.contains(
        "login(req: LoginRequest, opts?: { signal?: AbortSignal }): Promise<LoginResponse>"
    ));
    assert!(
        client.contains("this.t.call<LoginRequest, LoginResponse>(\"auth\", \"Login\", req, opts)")
    );
    assert!(client.contains("this.t.call<ListMembersRequest, ListMembersResponse>(\"member\", \"ListMembers\", req, opts)"));
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
    assert_eq!(paths, vec!["types.gen.ts", "server.gen.ts"]);

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
        vec!["types.gen.ts", "client.gen.ts", "server.gen.ts"]
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

    assert!(client.contains("Promise<LoginResponse>"));
    assert!(!client.contains("Promise<LoginResponse | ServiceError>"));
    // ServiceError is thrown, not returned, so it is not imported
    let import_line = client
        .lines()
        .find(|l| l.contains("import type"))
        .unwrap_or("");
    assert!(!import_line.contains("ServiceError"));
}

#[test]
fn domain_error_in_union_is_preserved() {
    // A non-ServiceError member of the union is a returned value and stays.
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
    assert!(client.contains("Promise<LoginResponse | LoginError>"));
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

    // <-> gets both check + send.
    assert!(
        client
            .contains("sendPlay(req: PlayerInput, opts?: { signal?: AbortSignal }): Promise<void>")
    );
    assert!(client.contains("\"PlaySend\""));
    assert!(client.contains("checkPlay(opts?: { signal?: AbortSignal }): Promise<GameState[]>"));
    assert!(client.contains("\"PlayCheck\""));

    // <- gets check only (no send — server pushes).
    assert!(
        client.contains("checkNotify(opts?: { signal?: AbortSignal }): Promise<Acknowledgment[]>")
    );
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
