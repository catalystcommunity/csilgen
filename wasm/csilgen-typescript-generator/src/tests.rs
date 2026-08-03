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

/// An optional `bytes` field carries three distinct states — absent, present-and-empty,
/// present-and-non-empty — and the codec must decide presence by whether the value is
/// set, never by whether it is non-empty (cbor-wire-contract.md "Optional fields"). A
/// truthy `if (v.payload)` would treat a zero-length `Uint8Array` as absent and silently
/// lose a caller's "replace this with nothing".
#[test]
fn optional_bytes_encodes_on_presence_not_emptiness() {
    let mut input = input_for("typescript-typesonly");
    input.csil_spec = spec_of(vec![group_rule(
        "UpdateRequest",
        vec![
            field("id", builtin("text"), false),
            field("payload", builtin("bytes"), true),
        ],
        vec![],
    )]);
    let files = generate_files(&input).expect("generate");
    let types = file(&files, "types.gen.ts");
    let codec = file(&files, "codec.gen.ts");

    // The `?` marker distinguishes undefined (absent) from an empty Uint8Array
    // (present-and-empty).
    assert!(
        types.contains("payload?: Uint8Array;"),
        "optional bytes needs a presence-carrying type:\n{types}"
    );
    // Encode gates on `!== undefined`, never on truthiness.
    assert!(
        codec.contains("if (v.payload !== undefined) csilMap.set(\"payload\", v.payload);"),
        "encode must gate on presence, not emptiness:\n{codec}"
    );
    assert!(
        !codec.contains("if (v.payload) csilMap.set"),
        "encode must not gate on truthiness -- an empty Uint8Array is present:\n{codec}"
    );
    // Decode maps a missing key to undefined but keeps a present zero-length byte
    // string, so the three states stay distinct.
    assert!(
        codec.contains("csilV === undefined ? undefined : asBytes(csilV)"),
        "decode must gate on key presence:\n{codec}"
    );
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
    // `client_style` defaults to `both`, so the async twin rides alongside the sync
    // client. The blocking `client.gen.ts` below is unchanged from sync-only output.
    assert_eq!(
        paths,
        vec![
            "types.gen.ts",
            "codec.gen.ts",
            "client.gen.ts",
            "client.async.gen.ts"
        ]
    );

    let client = file(&files, "client.gen.ts");
    // type-only import from the companion types module
    assert!(client.contains("import type {"));
    assert!(client.contains("} from \"./types.gen.ts\";"));
    // the typed methods pull their codec helpers from the codec module
    assert!(client.contains("} from \"./codec.gen.ts\";"));
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
    assert!(client.contains(
        "const csilResp = this.t.call(\"AuthService\", \"Login\", toLoginRequestCbor(req));"
    ));
    assert!(client.contains("return fromLoginResponseCbor(csilResp);"));
    // wire strings: verbatim CSIL service and operation names
    assert!(client.contains(
        "this.t.call(\"MemberService\", \"list-members\", toListMembersRequestCbor(req));"
    ));
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
fn async_twin_emitted_by_default_with_marked_symbols() {
    // Default `client_style` is `both`: the async twin lives at `client.async.gen.ts`
    // and carries an `Async` marker on every exported symbol so it coexists with the
    // sync client in one package/barrel.
    let files = generate_files(&input_for("typescript-client")).expect("generate");
    let twin = file(&files, "client.async.gen.ts");

    // Transport seam returns a Promise; its interface name is marked.
    assert!(twin.contains("export interface AsyncServiceTransport {"));
    assert!(
        twin.contains("call(service: string, op: string, req: Uint8Array): Promise<Uint8Array>;")
    );
    // Marked per-service + aggregate class names.
    assert!(twin.contains("export class AuthAsyncClient {"));
    assert!(twin.contains("export class MemberAsyncClient {"));
    assert!(twin.contains("export class AsyncApiClient {"));
    assert!(twin.contains("constructor(private readonly t: AsyncServiceTransport) {}"));
    // Methods are async and Promise-returning, awaiting the byte seam.
    assert!(twin.contains("async login(req: LoginRequest): Promise<LoginResponse> {"));
    assert!(twin.contains(
        "const csilResp = await this.t.call(\"AuthService\", \"Login\", toLoginRequestCbor(req));"
    ));
    assert!(twin.contains("return fromLoginResponseCbor(csilResp);"));
    // The aggregate wires the marked per-service clients.
    assert!(twin.contains("this.auth = new AuthAsyncClient(t);"));

    // The sync client is untouched (no Promise, no await, original names).
    let sync = file(&files, "client.gen.ts");
    assert!(sync.contains("login(req: LoginRequest): LoginResponse {"));
    assert!(!sync.contains("Promise<"));
    assert!(!sync.contains("await "));
}

#[test]
fn client_style_async_is_drop_in_at_canonical_path() {
    // `client_style: async` yields a single async client at the canonical path with
    // the original symbol names — a drop-in replacement for a sync consumer.
    let mut input = input_for("typescript-client");
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("async"));
    let files = generate_files(&input).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["types.gen.ts", "codec.gen.ts", "client.gen.ts"],
        "async drop-in emits no separate twin"
    );

    let client = file(&files, "client.gen.ts");
    // Canonical (unmarked) names, but async + Promise.
    assert!(client.contains("export interface ServiceTransport {"));
    assert!(
        client.contains("call(service: string, op: string, req: Uint8Array): Promise<Uint8Array>;")
    );
    assert!(client.contains("export class AuthClient {"));
    assert!(client.contains("export class ApiClient {"));
    assert!(client.contains("async login(req: LoginRequest): Promise<LoginResponse> {"));
    assert!(client.contains("const csilResp = await this.t.call("));
}

#[test]
fn client_style_sync_suppresses_the_twin() {
    let mut input = input_for("typescript-client");
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("sync"));
    let files = generate_files(&input).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts", "client.gen.ts"]);
    let client = file(&files, "client.gen.ts");
    assert!(!client.contains("Promise<"));
    assert!(!client.contains("await "));
}

#[test]
fn client_style_invalid_value_is_rejected() {
    let mut input = input_for("typescript-client");
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("blocking"));
    let err = generate_files(&input).expect_err("invalid client_style must fail generation");
    assert!(
        err.contains("client_style"),
        "error should name the offending option: {err}"
    );
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
    // dispatch routing keys: verbatim CSIL service and operation names
    assert!(server.contains("case \"AuthService\": {"));
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
            "client.async.gen.ts",
            "server.gen.ts"
        ]
    );
}

// A service-less spec (records/types only, no `ServiceDef`) has nothing for
// `client.gen.ts`/`server.gen.ts` to route to: both files unconditionally
// reference the synthetic `ServiceError`, which `types.gen.ts` only exports when
// the spec declares services. Emitting either file for a service-less spec would
// therefore always fail to typecheck (`ServiceError` imported but never
// exported). Go and Python already skip client/server emission for service-less
// specs; the aggregate/client/server TypeScript targets do the same.
#[test]
fn serviceless_aggregate_target_omits_client_and_server() {
    let files = generate_files(&input_with_spec("typescript", money_spec())).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts"]);
    // The synthetic error type is only ever emitted for service-bearing specs, so
    // a service-less types module never declares it either.
    assert!(!file(&files, "types.gen.ts").contains("ServiceError"));
}

#[test]
fn serviceless_client_target_omits_client_file() {
    let files =
        generate_files(&input_with_spec("typescript-client", money_spec())).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts"]);
}

#[test]
fn serviceless_server_target_omits_server_file() {
    let files =
        generate_files(&input_with_spec("typescript-server", money_spec())).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts"]);
}

// A spec that *does* declare services must keep emitting a working server (and
// client) — the service-less gate above must not regress the common case.
#[test]
fn spec_with_services_still_emits_a_working_server() {
    let files = generate_files(&input_for("typescript-server")).expect("generate");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.gen.ts", "codec.gen.ts", "server.gen.ts"]);
    assert!(file(&files, "types.gen.ts").contains("export interface ServiceError {"));
    assert!(file(&files, "server.gen.ts").contains("ServiceError"));
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

// `Name = { ... }` parses to `TypeDef(Group(..))` (not `GroupDef`), so build the
// records that way here to guard the sample-literal path against the real parser.
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

fn pingpong_spec() -> CsilSpecSerialized {
    spec_of(vec![
        record_typedef("Ping", vec![field("msg", builtin("text"), false)]),
        record_typedef("Pong", vec![field("msg", builtin("text"), false)]),
        CsilRule {
            name: "Echo".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![op("ping", "Ping", "Pong", vec![])],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: vec![],
        },
    ])
}

/// A spec with both a `->` op (`ping`) and a `<->` op (`pulse`), records built as
/// `TypeDef(Group)` so the codec, client, and channel router all render against real
/// ops — the canonical verification spec for the 3-transport genquickstart.
fn transports_spec() -> CsilSpecSerialized {
    let mut spec = spec_of(vec![
        record_typedef("Ping", vec![field("msg", builtin("text"), false)]),
        record_typedef("Pong", vec![field("msg", builtin("text"), false)]),
        CsilRule {
            name: "Echo".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    op("ping", "Ping", "Pong", vec![]),
                    op_with_direction(
                        "pulse",
                        "Ping",
                        "Pong",
                        CsilServiceDirection::Bidirectional,
                        vec![],
                    ),
                ],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: vec![],
        },
    ]);
    spec.service_count = 1;
    spec
}

// ---------------------------------------------------------------------------
// 3-transport genquickstart: structure + per-section unit assertions
// ---------------------------------------------------------------------------

fn transports_readme(spec: CsilSpecSerialized) -> String {
    let mut input = input_with_spec("typescript", spec);
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    let files = generate_files(&input).expect("generate");
    file(&files, "genquickstart.md").to_string()
}

#[test]
fn genquickstart_has_all_three_sections_by_default() {
    let readme = transports_readme(transports_spec());
    for heading in [
        "## CSIL-RPC (HTTP)",
        "## CSIL-Events (TLS)",
        "## CSIL-Datagrams (UDP)",
    ] {
        assert!(
            readme.contains(heading),
            "default genquickstart must contain {heading}:\n{readme}"
        );
    }
    // Install line pulls in the transport library alongside the package.
    assert!(readme.contains("npm install echo-client csilgen-transport"));
}

#[test]
fn genquickstart_transports_subset_emits_only_listed_sections() {
    let mut input = input_with_spec("typescript", transports_spec());
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    input.config.options.insert(
        "genquickstart_transports".to_string(),
        serde_json::json!(["rpc"]),
    );
    let files = generate_files(&input).expect("generate");
    let readme = file(&files, "genquickstart.md");
    assert!(readme.contains("## CSIL-RPC (HTTP)"));
    assert!(
        !readme.contains("## CSIL-Events (TLS)"),
        "events section must be suppressed:\n{readme}"
    );
    assert!(
        !readme.contains("## CSIL-Datagrams (UDP)"),
        "datagrams section must be suppressed:\n{readme}"
    );
}

#[test]
fn genquickstart_transports_unknown_or_empty_falls_back_to_all() {
    // An empty array, or one naming only unknown transports, falls back to all three
    // rather than producing an empty document.
    for opt in [serde_json::json!([]), serde_json::json!(["bogus"])] {
        let mut input = input_with_spec("typescript", transports_spec());
        input.config.options.insert(
            "emit_packages".to_string(),
            serde_json::json!(["typescript"]),
        );
        input
            .config
            .options
            .insert("genquickstart_transports".to_string(), opt.clone());
        let files = generate_files(&input).expect("generate");
        let readme = file(&files, "genquickstart.md");
        assert!(
            readme.contains("## CSIL-RPC (HTTP)")
                && readme.contains("## CSIL-Events (TLS)")
                && readme.contains("## CSIL-Datagrams (UDP)"),
            "{opt} must fall back to all three sections:\n{readme}"
        );
    }

    // A mix of known + unknown keeps only the known one.
    let mut input = input_with_spec("typescript", transports_spec());
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    input.config.options.insert(
        "genquickstart_transports".to_string(),
        serde_json::json!(["datagrams", "bogus"]),
    );
    let files = generate_files(&input).expect("generate");
    let readme = file(&files, "genquickstart.md");
    assert!(readme.contains("## CSIL-Datagrams (UDP)"));
    assert!(!readme.contains("## CSIL-RPC (HTTP)"));
    assert!(!readme.contains("## CSIL-Events (TLS)"));
}

#[test]
fn each_section_names_its_library_imports_and_seam() {
    let readme = transports_readme(transports_spec());
    let rpc = section(&readme, "## CSIL-RPC (HTTP)");
    let events = section(&readme, "## CSIL-Events (TLS)");
    let datagrams = section(&readme, "## CSIL-Datagrams (UDP)");

    // RPC: the library envelope types + the canonical HTTP mount, no hand-rolled map.
    assert!(rpc.contains("import { RpcRequest, RpcResponse } from \"csilgen-transport\";"));
    assert!(rpc.contains("/csil/v1/rpc"));
    assert!(rpc.contains("implements AsyncServiceTransport"));
    assert!(rpc.contains("new AsyncApiClient(new HttpRpcCarrier("));
    assert!(rpc.contains("client.echo.ping({ msg: \"example\" })"));

    // Events: the lib's handshake/framing surface + the generated channel router.
    assert!(events.contains("from \"csilgen-transport\";"));
    assert!(events.contains("$hello"));
    assert!(events.contains("frameLengthPrefixed"));
    assert!(events.contains("LengthPrefixedDeframer"));
    assert!(events.contains("routeEchoChannel(handlers, codec, ev.event!, ev.payload)"));
    assert!(events.contains("encodeEchoPulse(codec,"));

    // Datagrams: the lib's Datagram + carrier seam, and the no-sync-response warning.
    assert!(
        datagrams.contains("import { Datagram, type DatagramCarrier } from \"csilgen-transport\";")
    );
    assert!(datagrams.contains("sendDatagram"));
    assert!(datagrams.contains("NO synchronous response"));
    assert!(datagrams.contains("new Datagram(OP_ORD, 0, toPingCbor(req)).encode()"));
}

#[test]
fn events_section_without_channel_ops_emits_a_note() {
    // pingpong_spec has only a `->` op, so the Events section keeps the handshake but
    // replaces the dispatch wiring with a note (no generated router import).
    let readme = transports_readme(pingpong_spec());
    let events = section(&readme, "## CSIL-Events (TLS)");
    assert!(events.contains("$hello"));
    assert!(
        events.contains("no <->/<- operations"),
        "must note the absence of channel ops:\n{events}"
    );
    assert!(
        !events.contains("routeEchoChannel"),
        "no channel router import when there are no channel ops:\n{events}"
    );
}

/// In package mode the README is emitted by default, and an explicit
/// `emit_readme: false` suppresses only the README — the rest of the package
/// (barrel, `package.json`, `tsconfig.json`) is unaffected.
#[test]
fn package_readme_opt_out_suppresses_only_readme() {
    let mut input = input_with_spec("typescript", transports_spec());
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );

    // Default: README present alongside the rest of the package scaffolding.
    let files = generate_files(&input).expect("generate");
    assert!(
        files.iter().any(|f| f.path == "genquickstart.md"),
        "README must be emitted by default in package mode"
    );

    // Explicit opt-out: README gone, everything else still present.
    input
        .config
        .options
        .insert("emit_readme".to_string(), serde_json::json!(false));
    let files = generate_files(&input).expect("generate");
    assert!(
        !files.iter().any(|f| f.path == "genquickstart.md"),
        "emit_readme: false must suppress the README"
    );
    for path in ["index.ts", "package.json", "tsconfig.json"] {
        assert!(
            files.iter().any(|f| f.path == path),
            "emit_readme: false must leave {path} untouched"
        );
    }
}

// ---------------------------------------------------------------------------
// Hermetic execution of the genquickstart examples (node, in-process loopback)
// ---------------------------------------------------------------------------

fn have_node_npx() -> bool {
    let have = |bin: &str| {
        std::process::Command::new(bin)
            .arg("--version")
            .output()
            .is_ok()
    };
    have("node") && have("npx")
}

/// The `ts` block under the given `## ` heading (the section's first fenced block).
fn section_ts_block(md: &str, heading: &str) -> String {
    let sec = section(md, heading);
    let start = sec.find("```ts\n").expect("section has a ts block") + "```ts\n".len();
    let rest = &sec[start..];
    let end = rest.find("\n```").expect("ts block is closed");
    rest[..end].to_string()
}

/// The slice of `md` from `heading` up to the next `## ` heading (or end).
fn section<'a>(md: &'a str, heading: &str) -> &'a str {
    let start = md.find(heading).expect("section heading present");
    let rest = &md[start..];
    match rest[heading.len()..].find("\n## ") {
        Some(off) => &rest[..heading.len() + off],
        None => rest,
    }
}

/// Copy the in-repo `csilgen-transport` library into `dir/lib`, stripping the
/// `.ts` import extensions so it compiles as CommonJS alongside the generated
/// package (the lib's own ESM `.ts`-suffixed specifiers are not valid under
/// CommonJS module resolution without `allowImportingTsExtensions` + `noEmit`).
fn copy_transport_lib(dir: &std::path::Path) {
    let src =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/typescript/src");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("ts") {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .unwrap()
            .replace(".ts\"", "\"");
        std::fs::write(lib.join(path.file_name().unwrap()), content).unwrap();
    }
}

/// Build the genquickstart package for `transports_spec`, write it + the copied
/// library + a `node:tls`/`node:dgram` shim into a fresh temp dir, and return the
/// dir. The example specifiers `csilgen-transport`/`echo-client` are repointed at
/// the local `./lib/index`/`./index` so everything compiles as one program.
fn stage_transports_package(label: &str) -> (std::path::PathBuf, String) {
    let mut input = input_with_spec("typescript", transports_spec());
    input.config.options.insert(
        "emit_packages".to_string(),
        serde_json::json!(["typescript"]),
    );
    let files = generate_files(&input).expect("generate");
    let readme = file(&files, "genquickstart.md").to_string();

    let dir = std::env::temp_dir().join(format!("csilgen-ts-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    copy_transport_lib(&dir);
    std::fs::write(dir.join("node-shims.d.ts"), NODE_SHIMS_DTS).unwrap();
    (dir, readme)
}

/// Repoint an example block's package specifiers at the locally-staged copies.
fn local_specifiers(block: &str) -> String {
    block
        .replace("from \"csilgen-transport\"", "from \"./lib/index\"")
        .replace("from \"echo-client\"", "from \"./index\"")
}

fn run_tsc(dir: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut args = vec!["-y", "-p", "typescript@5", "tsc", "-p", "tsconfig.json"];
    args.extend_from_slice(extra_args);
    std::process::Command::new("npx")
        .args(&args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// CSIL-RPC: run the emitted HTTP-carrier example under node with `globalThis.fetch`
/// stubbed by an in-process CSIL-RPC echo built on the library's `RpcRequest`/
/// `RpcResponse`. A green run proves the carrier builds the envelope via the lib,
/// drives `fetch`, and the typed client decodes the reply round-trip. Hermetic (no
/// sockets). Skips when node/npx are unavailable.
#[test]
fn genquickstart_rpc_section_round_trips_under_node() {
    if !have_node_npx() {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }
    let (dir, readme) = stage_transports_package("rpc");
    let block = local_specifiers(&section_ts_block(&readme, "## CSIL-RPC (HTTP)"));
    let driver = format!("{RPC_ECHO_STUB_TS}\n{block}");
    std::fs::write(dir.join("driver.ts"), driver).unwrap();
    std::fs::write(dir.join("tsconfig.json"), TRANSPORTS_TSCONFIG).unwrap();

    let build = run_tsc(&dir, &[]);
    assert!(
        build.status.success(),
        "tsc failed on the RPC example:\n{}{}",
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
        "node run of the RPC example failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("example"),
        "RPC round-trip did not return the sent field: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// CSIL-Datagrams: run the emitted UDP example under node with the real carrier
/// swapped for the library's in-process `LoopbackDatagramCarrier`, seeded with one
/// response datagram. Proves the example `Datagram`-encodes the request via the
/// generated codec, `sendDatagram`s it, and decodes an inbound response datagram back
/// into the typed response. Hermetic (no sockets). Skips when node/npx unavailable.
#[test]
fn genquickstart_datagrams_section_round_trips_under_node() {
    if !have_node_npx() {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }
    let (dir, readme) = stage_transports_package("dgram");
    let block = local_specifiers(&section_ts_block(&readme, "## CSIL-Datagrams (UDP)"))
        // Swap the real UDP carrier for the seeded loopback (sockets are killed in the
        // sandbox; the lib loopback exercises the same send/recv codec path in-process).
        .replace(
            "openUdpCarrier(\"localhost\", 9000)",
            "((globalThis as any).__loopback as DatagramCarrier)",
        );
    let driver = format!("{DATAGRAM_LOOPBACK_PREAMBLE_TS}\n{block}");
    std::fs::write(dir.join("driver.ts"), driver).unwrap();
    std::fs::write(dir.join("tsconfig.json"), TRANSPORTS_TSCONFIG).unwrap();

    let build = run_tsc(&dir, &[]);
    assert!(
        build.status.success(),
        "tsc failed on the datagrams example:\n{}{}",
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
        "node run of the datagrams example failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("late response"),
        "datagram recv path did not decode the seeded response: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// CSIL-Events: the full TLS session is an interactive, socket-driven loop, so it is
/// verified compile-only (`tsc --noEmit`) against the generated package + library —
/// proving the handshake, heartbeat, Codec, and `routeEchoChannel` dispatch wiring all
/// type-check. The RPC + datagrams examples above are additionally *run*. Skips when
/// node/npx are unavailable.
#[test]
fn genquickstart_events_section_type_checks() {
    if !have_node_npx() {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }
    let (dir, readme) = stage_transports_package("events");
    let block = local_specifiers(&section_ts_block(&readme, "## CSIL-Events (TLS)"));
    std::fs::write(dir.join("driver.ts"), block).unwrap();
    std::fs::write(dir.join("tsconfig.json"), TRANSPORTS_TSCONFIG_NOEMIT).unwrap();

    let build = run_tsc(&dir, &["--noEmit"]);
    assert!(
        build.status.success(),
        "events example failed to type-check:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An in-process CSIL-RPC echo installed as `globalThis.fetch`, built on the library's
/// `RpcRequest`/`RpcResponse` (aliased so they don't collide with the example's own
/// imports). It decodes the request envelope and replies with a status-0 `Pong` reply
/// echoing the request payload.
const RPC_ECHO_STUB_TS: &str = r#"import { RpcRequest as _RReq, RpcResponse as _RResp } from "./lib/index";
(globalThis as any).fetch = async (_url: string, init: { body: Uint8Array }): Promise<Response> => {
  const req = _RReq.decode(new Uint8Array(init.body));
  const resp = _RResp.ok("Pong", req.payload).encode();
  return new Response(resp as BodyInit, { status: 200 });
};
"#;

/// Seeds a library `LoopbackDatagramCarrier` with one response datagram and exposes
/// it as `globalThis.__loopback` so the datagrams example (carrier line swapped) sends
/// to and receives from it in-process. Imports are aliased to avoid colliding with the
/// example's own `Datagram`/codec imports.
const DATAGRAM_LOOPBACK_PREAMBLE_TS: &str = r#"import { LoopbackDatagramCarrier as _LDC, Datagram as _DG } from "./lib/index";
import { toPongCbor as _toPong } from "./index";
const _lb = new _LDC();
_lb.pushInbound(new _DG(1, 0, _toPong({ msg: "example" })).encode());
(globalThis as any).__loopback = _lb;
"#;

/// Minimal ambient declarations for the node modules the Events/Datagrams carrier
/// examples import, so they type-check without `@types/node`. The hermetic tests swap
/// the real sockets for library loopbacks, so these are never executed.
const NODE_SHIMS_DTS: &str = r#"declare module "node:tls" {
  export interface TLSSocket {
    on(event: string, cb: (chunk: Uint8Array) => void): void;
    write(data: Uint8Array): void;
  }
  export function connect(options: { host: string; port: number }): TLSSocket;
}
declare module "node:dgram" {
  export interface Socket {
    on(event: string, cb: (msg: Uint8Array) => void): void;
    send(msg: Uint8Array, port: number, host: string): void;
  }
  export function createSocket(type: string): Socket;
}
"#;

/// tsconfig including both the generated package (`*.ts`) and the staged library
/// (`lib/*.ts`), emitting CommonJS into `out/` so the driver runs under node.
const TRANSPORTS_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "rewriteRelativeImportExtensions": true,
    "lib": ["es2020", "dom"],
    "outDir": "out"
  },
  "include": ["*.ts", "lib/*.ts"]
}
"#;

/// As `TRANSPORTS_TSCONFIG`, but `noEmit` for a type-check-only verification.
const TRANSPORTS_TSCONFIG_NOEMIT: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "rewriteRelativeImportExtensions": true,
    "lib": ["es2020", "dom"],
    "noEmit": true
  },
  "include": ["*.ts", "lib/*.ts"]
}
"#;

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

    // Router decodes by method name (verbatim CSIL op names as wire keys).
    assert!(client.contains("export function routeMatchChannel("));
    assert!(client.contains("case \"play\":"));
    assert!(client.contains("handlers.play(codec.decode<GameState>(bytes));"));
    assert!(client.contains("case \"notify\":"));

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
    assert!(server.contains("case \"list-events\":"));

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
    assert!(server.contains("case \"play\":"));
    assert!(
        !server.contains("case \"notify\":"),
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
    // list-events -> Unidirectional, routed via dispatch.
    assert!(server.contains("case \"list-events\":"));
    // <-> and <- ops live in routeMatchChannel, NOT in dispatch.
    let dispatch_block_start = server.find("export async function dispatch").unwrap();
    let dispatch_block = &server[dispatch_block_start..];
    assert!(
        !dispatch_block.contains("case \"play\":"),
        "bidi <-> must not be dispatched in connection mode"
    );
    assert!(
        !dispatch_block.contains("case \"notify\":"),
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
    assert!(
        client.contains("this.t.call(\"MatchService\", \"playSend\", toPlayerInputCbor(req));")
    );
    assert!(client.contains("checkPlay(): GameState[] {"));
    assert!(client.contains("this.t.call(\"MatchService\", \"playCheck\", new Uint8Array());"));
    assert!(client.contains(
        "return asArray(decode(csilResp)).map((csilE) => fromGameStateCborValue(csilE));"
    ));

    // <- gets check only (no send — server pushes).
    assert!(client.contains("checkNotify(): Acknowledgment[] {"));
    assert!(client.contains("\"notifyCheck\""));
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
    assert!(server.contains("case \"playSend\":"));
    assert!(server.contains("await handlers.match.sendPlay(req, ctx);"));
    assert!(server.contains("case \"playCheck\":"));
    assert!(server.contains("await handlers.match.checkPlay(ctx)"));
    assert!(server.contains("case \"notifyCheck\":"));
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
        server.contains("import { CsilDecimal } from \"./types.gen.ts\";"),
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
        client.contains("this.t.call(\"FeedService\", \"poll-event\", new Uint8Array());"),
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

/// Compile and run the generated **async** client against an async loopback
/// transport (one that returns a `Promise<Uint8Array>`), proving the Promise-
/// returning surface type-checks and round-trips a typed request/response under
/// node. Skips when node/npx is unavailable so the suite stays portable.
#[test]
fn async_client_round_trips_through_typescript() {
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

    // Default style is `both`, so the corndogs package carries `client.async.gen.ts`.
    let files = generate_files(&input_with_spec("typescript-client", corndogs_spec())).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-async-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.ts"), ASYNC_CLIENT_DRIVER_TS).unwrap();
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
        "node async round-trip failed:\n{}{}",
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

/// The mixed-union regression: `OrderStatus = text / "pending" / ... / "refunded"`
/// (examples/real-world-api/e-commerce-api.csil) has a general `text` arm alongside
/// several literal arms of the same base type. Before the literal-first ordering
/// fix, the general arm's `typeof v === "string"` predicate — checked first because
/// it was declared first — matched every string, making every literal's own declared
/// index unreachable on encode.
fn mixed_union_spec() -> CsilSpecSerialized {
    let literal = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));
    let order_status = CsilTypeExpression::Choice(vec![
        builtin("text"),
        literal("pending"),
        literal("confirmed"),
        literal("processing"),
        literal("shipped"),
        literal("delivered"),
        literal("cancelled"),
        literal("refunded"),
    ]);
    spec_of(vec![
        alias_rule("OrderStatus", order_status),
        group_rule(
            "Order",
            vec![
                field("id", builtin("text"), false),
                field("status", reference("OrderStatus"), false),
            ],
            vec![],
        ),
    ])
}

/// The `OrderStatus`-shaped literal arms, in declaration order, alongside their
/// 0-based declared index (`text` is index 0; see `mixed_union_spec`).
fn mixed_union_literals() -> [(&'static str, usize); 7] {
    [
        ("pending", 1),
        ("confirmed", 2),
        ("processing", 3),
        ("shipped", 4),
        ("delivered", 5),
        ("cancelled", 6),
        ("refunded", 7),
    ]
}

fn union_dispatch_spec() -> CsilSpecSerialized {
    let union_rule = |name: &str, arms: Vec<CsilTypeExpression>| CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(arms)),
        position: pos(),
        doc_comments: vec![],
    };

    spec_of(vec![
        record_typedef("EventPayload", vec![field("name", builtin("text"), false)]),
        record_typedef(
            "PageViewPayload",
            vec![field("route", builtin("text"), false)],
        ),
        record_typedef(
            "ErrorPayload",
            vec![
                field("error_type", builtin("text"), false),
                field("message", builtin("text"), false),
                field("handled", builtin("bool"), false),
            ],
        ),
        union_rule(
            "TypedValue",
            vec![
                builtin("null"),
                builtin("bool"),
                builtin("int"),
                builtin("uint"),
                builtin("float"),
                builtin("decimal"),
                builtin("text"),
                builtin("bytes"),
            ],
        ),
        union_rule(
            "Payload",
            vec![
                reference("EventPayload"),
                reference("PageViewPayload"),
                reference("ErrorPayload"),
            ],
        ),
        record_typedef(
            "TelemetryItem",
            vec![field("payload", reference("Payload"), false)],
        ),
    ])
}

#[test]
fn union_decimal_guard_does_not_shadow_text_or_bytes() {
    let files =
        generate_files(&input_with_spec("typescript-client", union_dispatch_spec())).unwrap();
    let codec = file(&files, "codec.gen.ts");
    let encoder = codec
        .split("export function toTypedValueCborValue")
        .nth(1)
        .expect("encoder emitted");

    assert!(encoder.contains(
        "if (v instanceof CsilDecimal) { const csilV = v as CsilDecimal; return [5, { tag: 4, value: csilV.toTag4() }]; }"
    ));
    assert!(encoder.contains(
        "if (typeof v === \"string\") { const csilV = v as string; return [6, csilV]; }"
    ));
    assert!(encoder.contains(
        "if (v instanceof Uint8Array) { const csilV = v as Uint8Array; return [7, csilV]; }"
    ));
    assert!(!encoder.contains("if (true)"));
}

#[test]
fn union_record_guards_check_each_arms_required_properties() {
    let files =
        generate_files(&input_with_spec("typescript-client", union_dispatch_spec())).unwrap();
    let codec = file(&files, "codec.gen.ts");
    let encoder = codec
        .split("export function toPayloadCborValue")
        .nth(1)
        .expect("encoder emitted");

    for (member, index, ty) in [
        ("name", 0, "EventPayload"),
        ("route", 1, "PageViewPayload"),
        ("errorType", 2, "ErrorPayload"),
    ] {
        assert!(
            encoder.contains(&format!(
                "Object.prototype.hasOwnProperty.call(v, \"{member}\")"
            )),
            "record arm {ty} does not check its required property:\n{codec}"
        );
        assert!(
            encoder.contains(&format!("const csilV = v as {ty}; return [{index},")),
            "record arm {ty} lost its declared index:\n{codec}"
        );
    }
}

#[test]
fn union_runtime_dispatch_reaches_decimal_text_bytes_and_each_record_arm() {
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
        generate_files(&input_with_spec("typescript-client", union_dispatch_spec())).unwrap();
    let dir =
        std::env::temp_dir().join(format!("csilgen-ts-union-dispatch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &files {
        std::fs::write(dir.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(
        dir.join("driver.ts"),
        r#"import { toPayloadCborValue, toTypedValueCborValue } from "./codec.gen";
import { CsilDecimal } from "./types.gen";

function index(value: unknown): number | bigint {
  return (value as [number | bigint, unknown])[0];
}

if (index(toTypedValueCborValue(new CsilDecimal(-2, 123n))) !== 5) throw new Error("decimal arm");
if (index(toTypedValueCborValue("us-west2")) !== 6) throw new Error("text arm");
if (index(toTypedValueCborValue(new Uint8Array([1]))) !== 7) throw new Error("bytes arm");
if (index(toPayloadCborValue({ name: "event" })) !== 0) throw new Error("event arm");
if (index(toPayloadCborValue({ route: "/docs" })) !== 1) throw new Error("page arm");
if (index(toPayloadCborValue({ errorType: "io", message: "failed", handled: false })) !== 2) throw new Error("error arm");
console.log("ok");
"#,
    )
    .unwrap();
    std::fs::write(dir.join("tsconfig.json"), CODEC_TSCONFIG).unwrap();

    let build = std::process::Command::new("npx")
        .args(["-y", "-p", "typescript@5", "tsc", "-p", "tsconfig.json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "tsc failed:\n{}{}",
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
        "node failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mixed_union_encode_checks_every_literal_before_the_general_arm() {
    let files = generate_files(&input_with_spec("typescript-client", mixed_union_spec())).unwrap();
    let codec = file(&files, "codec.gen.ts");
    let encoder = codec
        .split("export function toOrderStatusCborValue")
        .nth(1)
        .expect("encoder emitted");

    // Every literal arm keeps its own declared index...
    for (lit, idx) in mixed_union_literals() {
        assert!(
            encoder.contains(&format!(
                "if (v === \"{lit}\") {{ const csilV = v as \"{lit}\"; return [{idx}, csilV]; }}"
            )),
            "literal arm for {lit} not emitted with its declared index {idx}:\n{codec}"
        );
    }
    // ...and every literal arm is checked before the general `text` arm, so the
    // general arm's broader predicate never shadows a literal's declared index.
    let general_pos = encoder
        .find("typeof v === \"string\"")
        .expect("general arm present");
    let last_literal_pos = encoder
        .find("if (v === \"refunded\")")
        .expect("last-declared literal arm present");
    assert!(
        last_literal_pos < general_pos,
        "the general arm's predicate is checked before a literal arm, which shadows it on encode:\n{codec}"
    );
    // The general arm still returns its own declared index 0, as the fallback for
    // any string that is none of the declared literals.
    assert!(
        encoder.contains(
            "if (typeof v === \"string\") { const csilV = v as string; return [0, csilV]; }"
        ),
        "general arm should return its own declared index 0 as the fallback:\n{codec}"
    );
}

#[test]
fn mixed_union_decode_dispatches_every_index_and_validates_literals() {
    let files = generate_files(&input_with_spec("typescript-client", mixed_union_spec())).unwrap();
    let codec = file(&files, "codec.gen.ts");
    let decoder = codec
        .split("export function fromOrderStatusCborValue")
        .nth(1)
        .expect("decoder emitted");

    // Index 0 (the general arm) decodes permissively, like any other `text` field.
    assert!(
        decoder.contains("case 0: return asString(csilArr[1]);"),
        "general-arm index 0 not dispatched:\n{codec}"
    );
    // Every literal index validates the payload equals the declared literal via the
    // shared `asLiteral` runtime helper, rather than merely casting it.
    for (lit, idx) in mixed_union_literals() {
        assert!(
            decoder.contains(&format!(
                "case {idx}: return asLiteral<\"{lit}\">(csilArr[1], \"{lit}\");"
            )),
            "literal index {idx} ({lit}) does not validate via asLiteral:\n{codec}"
        );
    }
}

/// End-to-end proof under node: encode dispatches literal-over-general, every
/// declared index round-trips, and a literal-index payload that does not equal the
/// declared literal is rejected rather than silently accepted. Skips when node/npx
/// is unavailable so the suite stays portable.
#[test]
fn mixed_union_round_trips_and_rejects_literal_mismatch_under_node() {
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

    let files = generate_files(&input_with_spec("typescript-client", mixed_union_spec())).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-mixedunion-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.ts"), MIXED_UNION_DRIVER_TS).unwrap();
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
    // The package build depends on this to compile the `.ts`-extensioned
    // relative specifiers the modules use.
    assert_eq!(
        tsconfig["compilerOptions"]["rewriteRelativeImportExtensions"],
        true
    );

    // The barrel re-exports the generated modules and is the package `main` source.
    let index = file(&files, "index.ts");
    assert!(index.contains("from \"./types.gen.ts\""));
    assert!(index.contains("from \"./codec.gen.ts\""));
    assert!(index.contains("from \"./client.gen.ts\""));
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
    assert!(index.contains("export * from \"./client.gen.ts\";"));
    assert!(
        !index.contains("export * from \"./server.gen.ts\";"),
        "server must not be star-exported alongside client, got:\n{index}"
    );
    // Server still contributes its unique surface explicitly.
    assert!(index.contains("from \"./server.gen.ts\";"));
    assert!(index.contains("dispatch"));
    assert!(index.contains("ServerHandlers"));
}

/// The fixture set every `import_extension` coverage test below runs over: every
/// sub-target (typesonly/client/server/aggregate), a spec that exercises
/// bidirectional channel ops (`channel_spec`), a spec that exercises every
/// transport section (`transports_spec`), and the package barrel — the one
/// emitter of relative *re-exports* rather than plain imports.
fn specifier_coverage_inputs() -> Vec<WasmGeneratorInput> {
    vec![
        input_with_spec("typescript-typesonly", channel_spec()),
        input_with_spec("typescript-client", channel_spec()),
        input_with_spec("typescript-server", channel_spec()),
        input_with_spec("typescript", channel_spec()),
        input_with_spec("typescript", transports_spec()),
        package_input_with_spec("typescript", channel_spec()),
    ]
}

/// Every relative (`./`/`../`) specifier appearing in a `from "..."` position
/// across the emitted `.ts` files, paired with the file that carries it.
fn collect_relative_specifiers(files: &[GeneratedFile]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for f in files {
        if !f.path.ends_with(".ts") {
            continue;
        }
        for line in f.content.lines() {
            let Some(idx) = line.find("from \"") else {
                continue;
            };
            let spec = &line[idx + "from \"".len()..];
            let Some(end) = spec.find('"') else { continue };
            let spec = &spec[..end];
            if spec.starts_with("./") || spec.starts_with("../") {
                out.push((f.path.clone(), spec.to_string()));
            }
        }
    }
    out
}

/// Default `import_extension` (absent option) must reproduce the behavior
/// requested in docs/csilgen-requests/typescript-codec-import-missing-extension.md:
/// every relative specifier the generator emits carries the `.ts` extension, so
/// Node's ESM loader (and `nodenext` typechecking) resolve it without a workaround.
#[test]
fn import_extension_default_ts_matches_previous_behavior() {
    for input in specifier_coverage_inputs() {
        let files = generate_files(&input).expect("generate");
        let specs = collect_relative_specifiers(&files);
        assert!(
            !specs.is_empty(),
            "fixture produced no relative specifiers to check"
        );
        for (path, spec) in specs {
            assert!(
                spec.ends_with(".ts"),
                "{path} ({}) emits a relative specifier without .ts: {spec}",
                input.config.target,
            );
        }
    }
}

/// `import_extension: "js"` — the specifier a `tsc` build actually emits on disk —
/// must rewrite every relative specifier consistently (types/codec/client/server/
/// index barrel), matching plain `nodenext` resolution on any TypeScript version
/// with no 5.7 extension-rewriting flag required. This is the pre-diff-compatible
/// path for a bare (non-package) consumer: see
/// `import_extension_js_compiles_clean_under_plain_nodenext_without_57_flags`
/// below for the real `tsc` proof that TS5097 is gone.
#[test]
fn import_extension_js_rewrites_every_relative_specifier() {
    for mut input in specifier_coverage_inputs() {
        input
            .config
            .options
            .insert("import_extension".to_string(), serde_json::json!("js"));
        let files = generate_files(&input).expect("generate");
        let specs = collect_relative_specifiers(&files);
        assert!(
            !specs.is_empty(),
            "fixture produced no relative specifiers to check"
        );
        for (path, spec) in specs {
            assert!(
                spec.ends_with(".js"),
                "{path} ({}) import_extension=js did not rewrite: {spec}",
                input.config.target,
            );
        }
    }
}

/// `import_extension: "none"` — the generator's pre-existing (extension-less)
/// behavior — must rewrite every relative specifier consistently, with no
/// trailing `.ts`/`.js` anywhere.
#[test]
fn import_extension_none_rewrites_every_relative_specifier() {
    for mut input in specifier_coverage_inputs() {
        input
            .config
            .options
            .insert("import_extension".to_string(), serde_json::json!("none"));
        let files = generate_files(&input).expect("generate");
        let specs = collect_relative_specifiers(&files);
        assert!(
            !specs.is_empty(),
            "fixture produced no relative specifiers to check"
        );
        for (path, spec) in specs {
            assert!(
                !spec.ends_with(".ts") && !spec.ends_with(".js"),
                "{path} ({}) import_extension=none left an extension: {spec}",
                input.config.target,
            );
        }
    }
}

/// Mirrors `invalid_bidirectional_transport_value_fails_generation` /
/// `invalid_decimal_mapping_fails_generation`: an unrecognized `import_extension`
/// value is a hard generation error, not a silent fallback.
#[test]
fn import_extension_invalid_value_is_rejected() {
    let mut input = input_with_spec("typescript-client", channel_spec());
    input.config.options.insert(
        "import_extension".to_string(),
        serde_json::Value::String("mjs".to_string()),
    );
    let err = generate_files(&input).expect_err("invalid import_extension must fail generation");
    assert!(
        err.contains("import_extension") && err.contains("mjs"),
        "error must name the option and bad value, got {err:?}"
    );
}

/// An explicit `*_module` option (`client_types_module`, `client_codec_module`,
/// `codec_types_module`) is used verbatim regardless of `import_extension` — only
/// the *default* specifier follows the option.
#[test]
fn import_extension_does_not_override_explicit_module_option() {
    let mut input = input_for("typescript-client");
    input
        .config
        .options
        .insert("import_extension".to_string(), serde_json::json!("js"));
    input.config.options.insert(
        "client_types_module".to_string(),
        serde_json::Value::String("../generated/types".to_string()),
    );
    let client = file(&generate_files(&input).expect("generate"), "client.gen.ts").to_string();
    assert!(client.contains("} from \"../generated/types\";"));
}

// ---------------------------------------------------------------------------
// `import_extension` real-compile proofs — bare (non-package) output, the
// consumer this option exists for: `csilgen generate --target typescript`
// dropped into an existing project with no emitted tsconfig/package.json.
// ---------------------------------------------------------------------------

/// A driver exercising the typed client + codec against the generated modules,
/// importing them with the given specifier suffix (`.ts`, `.js`, or empty).
fn corndogs_driver_ts(specifier_suffix: &str) -> String {
    format!(
        r#"import {{ CorndogsClient, type ServiceTransport }} from "./client.gen{specifier_suffix}";
import {{
  toSubmitTaskRequestCbor,
  fromSubmitTaskRequestCbor,
  toTaskCbor,
}} from "./codec.gen{specifier_suffix}";
import type {{ Task, SubmitTaskRequest }} from "./types.gen{specifier_suffix}";

class Loopback implements ServiceTransport {{
  call(_service: string, _op: string, req: Uint8Array): Uint8Array {{
    const reqObj = fromSubmitTaskRequestCbor(req);
    return toTaskCbor(reqObj.task);
  }}
}}

function check(ok: boolean, what: string): void {{
  if (!ok) throw new Error("check failed: " + what);
}}

const task: Task = {{
  uuid: "u-123",
  currentState: "PENDING",
  payload: new Uint8Array([1, 2, 3]),
  priority: 7,
  labels: {{ a: 1 }},
  tags: ["x"],
}};
const req: SubmitTaskRequest = {{ task, queue: "default" }};

// Direct codec round-trip, independent of the client.
const reqBytes = toSubmitTaskRequestCbor(req);
check(reqBytes.length > 0, "encoded request has bytes");

const client = new CorndogsClient(new Loopback());
const resp = client.submitTask(req);
check(resp.uuid === "u-123", "resp uuid");

console.log("ok");
"#
    )
}

/// Stage `generate_files(input)` plus `driver.ts` and `tsconfig`, in a fresh temp
/// dir named `label`. Returns the dir for the caller to run `tsc`/`node` against.
fn stage_import_extension_fixture(
    label: &str,
    input: &WasmGeneratorInput,
    driver_suffix: &str,
    tsconfig: &str,
) -> std::path::PathBuf {
    let files = generate_files(input).expect("generate");
    let dir = std::env::temp_dir().join(format!(
        "csilgen-ts-import-ext-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.ts"), corndogs_driver_ts(driver_suffix)).unwrap();
    std::fs::write(dir.join("tsconfig.json"), tsconfig).unwrap();
    dir
}

/// `import_extension: "ts"` (default) under `nodenext` with `allowImportingTsExtensions`
/// and `noEmit` set — the consumer-requested behavior from
/// docs/csilgen-requests/typescript-codec-import-missing-extension.md must still
/// typecheck clean with no workaround flags beyond the ones that request already
/// required.
const IMPORT_EXTENSION_TS_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "allowImportingTsExtensions": true,
    "noEmit": true
  },
  "include": ["*.ts"]
}
"#;

#[test]
fn import_extension_ts_compiles_clean_under_nodenext_with_allow_ts_extensions() {
    if !have_node_npx() {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }
    let input = input_with_spec("typescript-client", corndogs_spec());
    let dir =
        stage_import_extension_fixture("ts-nodenext", &input, ".ts", IMPORT_EXTENSION_TS_TSCONFIG);
    let build = run_tsc(&dir, &["--noEmit"]);
    assert!(
        build.status.success(),
        "default import_extension=ts must typecheck clean under nodenext+allowImportingTsExtensions:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );
}

/// `import_extension: "js"` under plain `nodenext` with NO TypeScript 5.7 flags —
/// the pre-diff-compatible path for a bare consumer stuck on an older TypeScript
/// (or one that hasn't opted into `allowImportingTsExtensions`/
/// `rewriteRelativeImportExtensions`). Proves TS5097 ("An import path can only end
/// with a '.ts' extension when 'allowImportingTsExtensions' is enabled") is gone:
/// the generator now emits `.js`-suffixed specifiers pointing at the sibling `.ts`
/// sources, which `nodenext` resolves without any extra flag. Also runs the
/// compiled output under node to prove it is not just syntactically clean but
/// actually executes.
const IMPORT_EXTENSION_JS_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "dist"
  },
  "include": ["*.ts"]
}
"#;

#[test]
fn import_extension_js_compiles_clean_under_plain_nodenext_without_57_flags() {
    if !have_node_npx() {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }
    let mut input = input_with_spec("typescript-client", corndogs_spec());
    input
        .config
        .options
        .insert("import_extension".to_string(), serde_json::json!("js"));
    let dir =
        stage_import_extension_fixture("js-nodenext", &input, ".js", IMPORT_EXTENSION_JS_TSCONFIG);
    let build = run_tsc(&dir, &[]);
    assert!(
        build.status.success(),
        "import_extension=js must compile clean under plain nodenext (no 5.7 flags) — TS5097 must be gone:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );
    let run = std::process::Command::new("node")
        .arg(dir.join("dist").join("driver.js"))
        .output()
        .unwrap();
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).contains("ok"),
        "compiled driver.js must run clean:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}

/// `import_extension: "none"` under `moduleResolution: "bundler"` — the shape a
/// consumer feeding the raw `.ts` sources to a bundler (esbuild/webpack/Vite) or a
/// tsc-only-for-types setup expects: bundler resolution does not require an
/// extension on a relative specifier at all.
const IMPORT_EXTENSION_NONE_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["*.ts"]
}
"#;

#[test]
fn import_extension_none_compiles_clean_under_bundler_resolution() {
    if !have_node_npx() {
        eprintln!("skipping: node/npx not on PATH");
        return;
    }
    let mut input = input_with_spec("typescript-client", corndogs_spec());
    input
        .config
        .options
        .insert("import_extension".to_string(), serde_json::json!("none"));
    let dir =
        stage_import_extension_fixture("none-bundler", &input, "", IMPORT_EXTENSION_NONE_TSCONFIG);
    let build = run_tsc(&dir, &["--noEmit"]);
    assert!(
        build.status.success(),
        "import_extension=none must typecheck clean under moduleResolution bundler:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );
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

#[test]
fn serviceless_package_type_checks_without_client_or_server() {
    // Regression coverage for the service-less `server.gen.ts` importing a
    // `ServiceError` that a service-less `types.gen.ts` never exports: this used
    // to fail `tsc --noEmit` (TS2305, no exported member `ServiceError`) for any
    // service-less spec built under the aggregate target. `money_spec` declares
    // records but no `ServiceDef`, so client/server must be absent and the
    // resulting package (types + codec + barrel) must still type-check clean.
    let input = package_input_with_spec("typescript", money_spec());
    let files = generate_files(&input).expect("generate");
    for path in ["client.gen.ts", "client.async.gen.ts", "server.gen.ts"] {
        assert!(
            !files.iter().any(|f| f.path == path),
            "{path} must not be emitted for a service-less spec"
        );
    }
    typecheck_emitted_package("serviceless", &input);
}

const CODEC_TSCONFIG_NOEMIT: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "rewriteRelativeImportExtensions": true,
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
    "rewriteRelativeImportExtensions": true,
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

const MIXED_UNION_DRIVER_TS: &str = r#"import {
  toOrderStatusCbor,
  fromOrderStatusCbor,
  toOrderStatusCborValue,
  fromOrderStatusCborValue,
} from "./codec.gen";
import type { OrderStatus } from "./types.gen";

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error("check failed: " + what);
}

function sameArray(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

// Literal-over-general precedence on encode: a value equal to a declared literal
// takes that literal's own declared index, not the general `text` arm's index 0.
check(sameArray(toOrderStatusCborValue("pending"), [1, "pending"]), "encode(pending) -> index 1");
check(sameArray(toOrderStatusCborValue("refunded"), [7, "refunded"]), "encode(refunded) -> index 7");
// A string that is not one of the declared literals falls through to the general
// arm and keeps ITS declared index 0.
check(sameArray(toOrderStatusCborValue("on-hold"), [0, "on-hold"]), "encode(on-hold) -> index 0");

// Every declared index round-trips through the byte-level codec.
const statuses: OrderStatus[] = [
  "on-hold",
  "pending",
  "confirmed",
  "processing",
  "shipped",
  "delivered",
  "cancelled",
  "refunded",
];
for (const s of statuses) {
  check(fromOrderStatusCbor(toOrderStatusCbor(s)) === s, `round-trip(${s})`);
}

// A literal-index payload that does not equal the declared literal is rejected
// rather than silently returned.
let threw = false;
try {
  fromOrderStatusCborValue([1, "confirmed"]);
} catch {
  threw = true;
}
check(threw, "decode([1, \"confirmed\"]) must reject a literal mismatch");

console.log("ok");
"#;

const ASYNC_CLIENT_DRIVER_TS: &str = r#"import { CorndogsAsyncClient, type AsyncServiceTransport } from "./client.async.gen";
import { fromSubmitTaskRequestCbor, toTaskCbor } from "./codec.gen";
import type { Task, SubmitTaskRequest } from "./types.gen";

// An async carrier: returns a Promise of the response bytes, exactly the shape a
// browser `fetch` adapter would. The microtask hop proves `await` really threads
// through the generated client.
class AsyncLoopback implements AsyncServiceTransport {
  async call(_service: string, _op: string, req: Uint8Array): Promise<Uint8Array> {
    await Promise.resolve();
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

async function main(): Promise<void> {
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

  const client = new CorndogsAsyncClient(new AsyncLoopback());
  const resp = await client.submitTask(req);
  check(resp.uuid === "u-123", "resp uuid");
  check(bytesEq(resp.payload, payload), "resp payload");
  check(resp.priority === 7, "resp priority");

  console.log("ok");
}

// A rejected top-level promise exits node non-zero, which the test treats as failure.
void main();
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

// ---------------------------------------------------------------------------
// Inline (anonymous) composite field hoisting
//
// A mixed choice or inline group written directly in a field / array element /
// map value / tuple element must behave exactly like a reference to a named
// choice/group with the same arms: mixed choices ride the wire as tagged sums,
// inline groups as CBOR maps, and a closed all-literal choice (even one whose
// last arm carries a trailing `.default`, which the parser attaches as a
// `Constrained` wrapper) as its bare literal with decode-side membership
// validation. These mirror the torture spec shared with the java/csharp/kotlin
// agents and the OCaml byte oracle.
// ---------------------------------------------------------------------------

fn choice(arms: Vec<CsilTypeExpression>) -> CsilTypeExpression {
    CsilTypeExpression::Choice(arms)
}

fn lit_text(s: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
}

fn lit_int(i: i64) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Integer(i))
}

fn array_of(element: CsilTypeExpression) -> CsilTypeExpression {
    CsilTypeExpression::Array {
        element_type: Box::new(element),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    }
}

fn map_of(value: CsilTypeExpression) -> CsilTypeExpression {
    CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(value),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    }
}

fn tuple_of(elements: Vec<CsilTypeExpression>) -> CsilTypeExpression {
    CsilTypeExpression::Tuple(CsilGroupExpression {
        entries: elements
            .into_iter()
            .map(|t| CsilGroupEntry {
                key: None,
                value_type: t,
                occurrence: None,
                metadata: vec![],
                doc_comments: vec![],
            })
            .collect(),
    })
}

fn inline_group_ty(entries: Vec<CsilGroupEntry>) -> CsilTypeExpression {
    CsilTypeExpression::Group(CsilGroupExpression { entries })
}

/// A trailing `.default` on the last choice arm, exactly as the parser attaches
/// it: a `Constrained { base_type: Literal, .. }` wrapping that one arm.
fn default_arm(s: &str, default: &str) -> CsilTypeExpression {
    constrained(
        lit_text(s),
        vec![CsilControlOperator::Default(CsilLiteralValue::Text(
            default.to_string(),
        ))],
    )
}

fn inline_choice_spec() -> CsilSpecSerialized {
    let mut spec = spec_of(vec![
        record_typedef(
            "InlineChoicePayload",
            vec![field("detail", builtin("text"), false)],
        ),
        record_typedef(
            "InlineChoiceRecord",
            vec![
                field(
                    "status",
                    choice(vec![
                        builtin("text"),
                        lit_text("pending"),
                        lit_text("active"),
                        lit_text("closed"),
                    ]),
                    false,
                ),
                field(
                    "priority",
                    choice(vec![
                        builtin("text"),
                        lit_text("low"),
                        lit_text("normal"),
                        default_arm("high", "normal"),
                    ]),
                    true,
                ),
                field(
                    "size",
                    choice(vec![
                        lit_text("small"),
                        lit_text("medium"),
                        default_arm("large", "medium"),
                    ]),
                    true,
                ),
                field(
                    "payload",
                    choice(vec![
                        lit_text("none"),
                        lit_int(42),
                        reference("InlineChoicePayload"),
                    ]),
                    false,
                ),
                field(
                    "tags",
                    array_of(choice(vec![
                        builtin("text"),
                        lit_text("red"),
                        lit_text("green"),
                        lit_text("blue"),
                        builtin("int"),
                    ])),
                    false,
                ),
                field(
                    "labels",
                    map_of(choice(vec![
                        builtin("text"),
                        lit_text("yes"),
                        lit_text("no"),
                        builtin("bool"),
                    ])),
                    false,
                ),
                field(
                    "coord",
                    tuple_of(vec![
                        builtin("int"),
                        choice(vec![
                            builtin("text"),
                            lit_text("x"),
                            lit_text("y"),
                            lit_text("z"),
                        ]),
                    ]),
                    false,
                ),
                field(
                    "nested",
                    inline_group_ty(vec![field(
                        "kind",
                        choice(vec![
                            builtin("text"),
                            lit_text("a"),
                            lit_text("b"),
                            builtin("int"),
                        ]),
                        false,
                    )]),
                    false,
                ),
            ],
        ),
        CsilRule {
            name: "InlineChoiceService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![op(
                    "round-trip",
                    "InlineChoiceRecord",
                    "InlineChoiceRecord",
                    vec![],
                )],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: vec![],
        },
    ]);
    spec.service_count = 1;
    spec
}

#[test]
fn inline_choice_hoists_mixed_positions_and_leaves_literal_enums_inline() {
    let files = generate_files(&input_with_spec("typescript", inline_choice_spec())).unwrap();
    let types = file(&files, "types.gen.ts");
    let codec = file(&files, "codec.gen.ts");

    // Every mixed inline position is hoisted to a synthesized named type, so the
    // owning field references it (a direct field, an array element, a map value, a
    // tuple element, and an inline group's own field all get names).
    for decl in [
        "export type InlineChoiceRecordStatus = string | \"pending\" | \"active\" | \"closed\";",
        "export type InlineChoiceRecordPayload = \"none\" | 42 | InlineChoicePayload;",
        "export type InlineChoiceRecordTagsItem = string | \"red\" | \"green\" | \"blue\" | number;",
        "export type InlineChoiceRecordLabelsValue = string | \"yes\" | \"no\" | boolean;",
        "export type InlineChoiceRecordCoord1 = string | \"x\" | \"y\" | \"z\";",
        "export interface InlineChoiceRecordNested {",
        "export type InlineChoiceRecordNestedKind = string | \"a\" | \"b\" | number;",
    ] {
        assert!(
            types.contains(decl),
            "missing synthesized type `{decl}`:\n{types}"
        );
    }
    // The array element type is `Item[]` (the whole element is the choice), not the
    // precedence-broken `... | number[]` an un-hoisted inline choice produced.
    assert!(
        types.contains("tags: InlineChoiceRecordTagsItem[];"),
        "array element not hoisted to a named item type:\n{types}"
    );

    // A closed all-literal choice keeps its inline form (no synthesized union codec);
    // the field type is the bare union.
    assert!(
        types.contains("size?: \"small\" | \"medium\" | \"large\";"),
        "closed literal enum should stay an inline union type:\n{types}"
    );
    assert!(
        !codec.contains("toInlineChoiceRecordSizeCborValue"),
        "closed literal enum must NOT get a tagged-sum union codec:\n{codec}"
    );

    // Each hoisted mixed choice routes through the reused union codec machinery.
    for f in [
        "export function toInlineChoiceRecordStatusCborValue",
        "export function toInlineChoiceRecordPayloadCborValue",
        "export function toInlineChoiceRecordTagsItemCborValue",
        "export function toInlineChoiceRecordLabelsValueCborValue",
        "export function toInlineChoiceRecordCoord1CborValue",
        "export function toInlineChoiceRecordNestedCborValue",
        "export function toInlineChoiceRecordNestedKindCborValue",
    ] {
        assert!(codec.contains(f), "missing hoisted codec `{f}`:\n{codec}");
    }

    // The owning record routes each position through the synthesized codec rather
    // than passing the raw value through.
    assert!(
        codec.contains("csilMap.set(\"nested\", toInlineChoiceRecordNestedCborValue(v.nested));"),
        "inline group field not routed through its record codec:\n{codec}"
    );
    assert!(
        codec.contains(
            "v.tags.map((csilE): CborValue => toInlineChoiceRecordTagsItemCborValue(csilE))"
        ),
        "array element not routed through its item codec:\n{codec}"
    );

    // The payload union's record arm routes through the record codec at its declared
    // index 2, and the literal arms keep indices 0/1.
    assert!(
        codec.contains("case 2: return fromInlineChoicePayloadCborValue(csilArr[1]);"),
        "record union arm not dispatched at its declared index:\n{codec}"
    );
}

#[test]
fn inline_closed_enum_default_arm_validates_membership_and_stays_bare() {
    let codec = file(
        &generate_files(&input_with_spec("typescript", inline_choice_spec())).unwrap(),
        "codec.gen.ts",
    )
    .to_string();

    // The closed enum's decode validates the value is one of the declared literals —
    // and crucially the `.default`-wrapped final arm ("large") is included, proving the
    // `Constrained` wrapper is seen through rather than dropping that arm.
    assert!(
        codec.contains(
            "asEnumMember(asString(csilV), [\"small\", \"medium\", \"large\"]) as \"small\" | \"medium\" | \"large\""
        ),
        "closed enum decode must validate membership against all three literals:\n{codec}"
    );
    // It stays a bare literal on the wire (the record sets the value directly).
    assert!(
        codec.contains("if (v.size !== undefined) csilMap.set(\"size\", v.size);"),
        "closed enum must encode as the bare literal value:\n{codec}"
    );
}

#[test]
fn inline_mixed_choice_default_arm_keeps_its_declared_index_before_general() {
    let codec = file(
        &generate_files(&input_with_spec("typescript", inline_choice_spec())).unwrap(),
        "codec.gen.ts",
    )
    .to_string();
    // `priority`'s final arm ("high") carries the trailing `.default`, so the parser
    // wraps it in `Constrained`. It must still be recognized as a literal: it keeps
    // its declared index 3 and is checked BEFORE the general `text` arm, or the general
    // arm's `typeof v === "string"` predicate would shadow it and miswrite [0, "high"].
    let encoder = codec
        .split("export function toInlineChoiceRecordPriorityCborValue")
        .nth(1)
        .expect("priority union encoder emitted");
    assert!(
        encoder.contains("if (v === \"high\") { const csilV = v as \"high\"; return [3, csilV]; }"),
        "the `.default`-wrapped literal arm lost its declared index:\n{codec}"
    );
    let high_pos = encoder.find("v === \"high\"").expect("high arm present");
    let general_pos = encoder
        .find("typeof v === \"string\"")
        .expect("general arm present");
    assert!(
        high_pos < general_pos,
        "the general arm is checked before the `.default` literal arm, shadowing it:\n{codec}"
    );
}

/// End-to-end proof under node: the hoisted inline choices/groups produce bytes
/// identical to the OCaml oracle for the record-field-level positions, a full record
/// round-trips through every position (array/map/tuple included — positions OCaml has
/// no oracle for), and the closed enum rejects an unknown value. Skips when node/npx
/// is unavailable so the suite stays portable.
#[test]
fn inline_choice_matches_cross_language_bytes_and_round_trips_under_node() {
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

    let files = generate_files(&input_with_spec("typescript", inline_choice_spec())).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-inlinechoice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.ts"), INLINE_CHOICE_DRIVER_TS).unwrap();
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

const INLINE_CHOICE_DRIVER_TS: &str = r#"import {
  toInlineChoiceRecordStatusCbor,
  toInlineChoiceRecordPriorityCbor,
  toInlineChoiceRecordPayloadCbor,
  toInlineChoiceRecordNestedKindCbor,
  toInlineChoiceRecordCbor,
  fromInlineChoiceRecordCbor,
  fromInlineChoiceRecordCborValue,
} from "./codec.gen";
import type { InlineChoiceRecord } from "./types.gen";

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error("check failed: " + what);
}
const hex = (b: Uint8Array): string =>
  Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

// Byte cross-check against the OCaml oracle (record-field-level inline choices).
check(hex(toInlineChoiceRecordStatusCbor("pending")) === "82016770656e64696e67", "status(pending) = [1,\"pending\"]");
check(hex(toInlineChoiceRecordStatusCbor("free")) === "82006466726565", "status(free) = [0,\"free\"]");
check(hex(toInlineChoiceRecordPriorityCbor("high")) === "82036468696768", "priority(high) = [3,\"high\"]");
check(hex(toInlineChoiceRecordPayloadCbor("none")) === "8200646e6f6e65", "payload(none) = [0,\"none\"]");
check(hex(toInlineChoiceRecordPayloadCbor({ detail: "hi" })) === "8202a16664657461696c626869", "payload(inline) = [2,{detail:\"hi\"}]");
check(hex(toInlineChoiceRecordNestedKindCbor("a")) === "82016161", "kind(a) = [1,\"a\"]");
check(hex(toInlineChoiceRecordNestedKindCbor("free")) === "82006466726565", "kind(free) = [0,\"free\"]");
check(hex(toInlineChoiceRecordNestedKindCbor(7)) === "820307", "kind(7) = [3,7]");

// A closed all-literal enum rides the wire as its bare literal, and a full record
// round-trips through every hoisted position (array, map, tuple, nested group).
const rec: InlineChoiceRecord = {
  status: "pending",
  size: "medium",
  payload: { detail: "x" },
  tags: ["red", 5],
  labels: { a: "yes", b: true },
  coord: [1, "x"],
  nested: { kind: 7 },
};
const bytes = toInlineChoiceRecordCbor(rec);
check(hex(bytes).includes("6473697a65666d656469756d"), "size rides wire as bare text \"medium\"");
const back = fromInlineChoiceRecordCbor(bytes);
check(JSON.stringify(back) === JSON.stringify(rec), "record round-trip: " + JSON.stringify(back));

// Decode validates the closed enum's membership: an unknown value is rejected.
let threw = false;
try {
  fromInlineChoiceRecordCborValue(new Map<unknown, unknown>([
    ["status", [0, "x"]],
    ["payload", [0, "none"]],
    ["tags", []],
    ["labels", new Map()],
    ["coord", [1, [0, "x"]]],
    ["nested", new Map([["kind", [1, "a"]]])],
    ["size", "huge"],
  ]) as never);
} catch {
  threw = true;
}
check(threw, "decode rejects an unknown closed-enum value");

console.log("ok");
"#;

// `Color`/`Priority` are named (`Name = "a" / "b"`) enums, referenced from a
// record field rather than declared inline — the parity gap fixed in
// `codec::aliases`: a `Reference` to a literal-only choice used to fall through
// to a blind, unvalidated cast (`aliases()` excluded ALL choices, not just
// non-literal unions), while the identical inline spelling already validated
// membership via `asEnumMember`.
fn named_enum_spec() -> CsilSpecSerialized {
    spec_of(vec![
        CsilRule {
            name: "Color".to_string(),
            rule_type: CsilRuleType::TypeDef(choice(vec![
                lit_text("red"),
                lit_text("green"),
                lit_text("blue"),
            ])),
            position: pos(),
            doc_comments: vec![],
        },
        CsilRule {
            name: "Priority".to_string(),
            rule_type: CsilRuleType::TypeDef(choice(vec![lit_int(1), lit_int(2), lit_int(3)])),
            position: pos(),
            doc_comments: vec![],
        },
        record_typedef(
            "NamedEnumRecord",
            vec![
                field("color", reference("Color"), false),
                field("priority", reference("Priority"), false),
            ],
        ),
    ])
}

/// Empirical proof (via `node`) that a NAMED enum reference validates wire-value
/// membership on decode exactly like an inline enum does: a well-typed value
/// outside the declared literal set ("purple", 99) must raise, and every declared
/// member must still round-trip.
#[test]
fn named_enum_reference_validates_membership_under_node() {
    let have_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    if !have_node {
        eprintln!("skipping: node not on PATH");
        return;
    }

    let files = generate_files(&input_with_spec("typescript", named_enum_spec())).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-namedenum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("probe.mjs"), NAMED_ENUM_DRIVER_JS).unwrap();

    let run = std::process::Command::new("node")
        .arg("probe.mjs")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "node probe failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const NAMED_ENUM_DRIVER_JS: &str = r#"import { toNamedEnumRecordCbor, fromNamedEnumRecordCbor } from "./codec.gen.ts";

function check(ok, what) {
  if (!ok) throw new Error("check failed: " + what);
}

// Every declared member round-trips.
const good = { color: "green", priority: 2 };
const back = fromNamedEnumRecordCbor(toNamedEnumRecordCbor(good));
check(back.color === "green" && back.priority === 2, "valid named-enum members round-trip");

// A well-typed value outside the declared set must fail decode — the named-enum
// analog of the inline-choice membership check above.
let threwColor = false;
try {
  fromNamedEnumRecordCbor(toNamedEnumRecordCbor({ color: "purple", priority: 1 }));
} catch {
  threwColor = true;
}
check(threwColor, "decode rejects an out-of-set named text enum (Color) value");

let threwPriority = false;
try {
  fromNamedEnumRecordCbor(toNamedEnumRecordCbor({ color: "red", priority: 99 }));
} catch {
  threwPriority = true;
}
check(threwPriority, "decode rejects an out-of-set named int enum (Priority) value");

console.log("ok");
"#;

/// A record with an inline MIXED-kind literal choice field (`"a" / 1`, a text
/// literal and an integer literal in the same choice) — the shape the shared
/// `csilgen_common::classify_choice` contract fixed: ALL-literal (any kind mix)
/// classifies as an `Enum`, so this must stay inline (not hoisted) exactly like a
/// uniform-kind literal choice, and decode must validate membership.
fn mixed_kind_literal_choice_spec() -> CsilSpecSerialized {
    spec_of(vec![record_typedef(
        "MixedEnumRecord",
        vec![field(
            "code",
            choice(vec![lit_text("a"), lit_int(1)]),
            false,
        )],
    )])
}

/// Regression test for the pre-fix bug: TS's decode picked a single scalar
/// reader (`asNumber`/`asBool`/`asString`) by requiring a UNIFORM literal kind
/// across every arm, defaulting to `asString` for anything else — which threw at
/// runtime decoding the integer member of a mixed-kind choice like `"a" / 1`
/// (`asString` rejects a non-string CBOR item). Confirms the generated source
/// routes a mixed-kind choice through the new `asEnumScalar` generic reader.
#[test]
fn mixed_kind_literal_choice_decode_uses_generic_enum_scalar_reader() {
    let files = generate_files(&input_with_spec(
        "typescript",
        mixed_kind_literal_choice_spec(),
    ))
    .unwrap();
    let codec = &files
        .iter()
        .find(|f| f.path == "codec.gen.ts")
        .unwrap()
        .content;
    assert!(
        codec.contains("asEnumScalar"),
        "mixed-kind choice decode must route through the generic asEnumScalar reader:\n{codec}"
    );
}

/// Empirical proof (via `node`) that a mixed-kind literal choice (`"a" / 1`)
/// rides the wire as a bare literal (not a tagged-sum union — it is still an
/// ALL-literal `Enum` per the classification contract), every declared member of
/// EITHER kind round-trips, and an out-of-vocabulary value is rejected on decode.
#[test]
fn mixed_kind_literal_choice_round_trips_and_validates_membership_under_node() {
    let have_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    if !have_node {
        eprintln!("skipping: node not on PATH");
        return;
    }

    let files = generate_files(&input_with_spec(
        "typescript",
        mixed_kind_literal_choice_spec(),
    ))
    .unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-ts-mixedenum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("probe.mjs"), MIXED_ENUM_DRIVER_JS).unwrap();

    let run = std::process::Command::new("node")
        .arg("probe.mjs")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "node probe failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_ENUM_DRIVER_JS: &str = r#"import { toMixedEnumRecordCbor, fromMixedEnumRecordCbor } from "./codec.gen.ts";

function check(ok, what) {
  if (!ok) throw new Error("check failed: " + what);
}

// Both declared members — a text literal AND an integer literal in the same
// choice — round-trip.
const textBack = fromMixedEnumRecordCbor(toMixedEnumRecordCbor({ code: "a" }));
check(textBack.code === "a", "text member of mixed-kind enum round-trips");
const intBack = fromMixedEnumRecordCbor(toMixedEnumRecordCbor({ code: 1 }));
check(intBack.code === 1, "int member of mixed-kind enum round-trips");

// A well-typed value (a string, matching the "a" arm's runtime type) outside the
// declared vocabulary must still be rejected — a membership check, not merely a
// type check.
let threwString = false;
try {
  fromMixedEnumRecordCbor(toMixedEnumRecordCbor({ code: "z" }));
} catch {
  threwString = true;
}
check(threwString, "decode rejects an out-of-set text value for a mixed-kind enum");

let threwInt = false;
try {
  fromMixedEnumRecordCbor(toMixedEnumRecordCbor({ code: 99 }));
} catch {
  threwInt = true;
}
check(threwInt, "decode rejects an out-of-set int value for a mixed-kind enum");

console.log("ok");
"#;
