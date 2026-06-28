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

// ---- codec ----------------------------------------------------------------

/// A corndogs-shaped spec exercising the codec's full surface: text, bytes, an
/// optional int, an inline map, a list, a nested record, and a service whose
/// output is a `Res / ServiceError` choice.
fn corndogs_rules() -> Vec<CsilRule> {
    let text = || builtin("text");
    let optional_int = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("priority".to_string())),
        value_type: builtin("int"),
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: vec![],
    };
    let task = group_rule(
        "Task",
        vec![
            bare_entry("uuid", text()),
            bare_entry("current_state", text()),
            bare_entry("payload", builtin("bytes")),
            optional_int,
            bare_entry(
                "labels",
                CsilTypeExpression::Map {
                    key: Box::new(text()),
                    value: Box::new(builtin("int")),
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
            // A field typed as a transparent scalar alias (`Uuid = text`): it must
            // round-trip via the underlying text codec, not the dropped-data stub.
            bare_entry("owner", CsilTypeExpression::Reference("Uuid".to_string())),
        ],
    );
    let req = group_rule(
        "SubmitTaskRequest",
        vec![
            bare_entry("task", CsilTypeExpression::Reference("Task".to_string())),
            bare_entry("queue", text()),
        ],
    );
    let err = group_rule(
        "ServiceError",
        vec![
            bare_entry("code", builtin("int")),
            bare_entry("message", text()),
        ],
    );
    let svc = service_rule(
        "CorndogsService",
        vec![op(
            "submit-task",
            CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
            CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Reference("Task".to_string()),
                CsilTypeExpression::Reference("ServiceError".to_string()),
            ]),
            CsilServiceDirection::Unidirectional,
            None,
        )],
        None,
    );
    // A record used as a named map alias's value type, to prove map-of-record aliases
    // recurse into the value record's own codec.
    let some_record = group_rule(
        "SomeRecord",
        vec![
            bare_entry("name", text()),
            bare_entry("value", builtin("int")),
        ],
    );
    // Three named aggregate aliases, each used as a field below: a map of ints, a map
    // of records, and a list of text. These must round-trip every entry, not collapse
    // to the old data-less `void *` / count-less pointer.
    let string_int64_map = alias_rule(
        "StringInt64Map",
        CsilTypeExpression::Map {
            key: Box::new(text()),
            value: Box::new(builtin("int")),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        },
    );
    let record_map = alias_rule(
        "M",
        CsilTypeExpression::Map {
            key: Box::new(text()),
            value: Box::new(CsilTypeExpression::Reference("SomeRecord".to_string())),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        },
    );
    let tags = alias_rule(
        "Tags",
        CsilTypeExpression::Array {
            element_type: Box::new(text()),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        },
    );
    let metrics = group_rule(
        "Metrics",
        vec![
            bare_entry(
                "queue_counts",
                CsilTypeExpression::Reference("StringInt64Map".to_string()),
            ),
            bare_entry("detailed", CsilTypeExpression::Reference("M".to_string())),
            bare_entry(
                "tag_list",
                CsilTypeExpression::Reference("Tags".to_string()),
            ),
        ],
    );
    vec![
        alias_rule("Uuid", builtin("text")),
        string_int64_map,
        record_map,
        tags,
        some_record,
        task,
        req,
        err,
        metrics,
        svc,
    ]
}

#[test]
fn codec_emitted_with_typed_client() {
    let input = input_with_rules(corndogs_rules(), "c-client", HashMap::new());
    let out = process_generation(input).unwrap();
    let codec = out
        .files
        .iter()
        .find(|f| f.path == "codec.gen.h")
        .expect("codec.gen.h emitted");
    // Public per-type wrappers and the internal canonical-map encoder are present.
    assert!(codec.content.contains("csil_encode_SubmitTaskRequest("));
    assert!(codec.content.contains("csil_decode_Task("));
    assert!(
        codec
            .content
            .contains("static inline int csilc_enc_Task(csilc_buf *b, const Task *v)")
    );
    // bytes -> CBOR byte string (major type 2), via the runtime's csilc_w_bytes.
    assert!(
        codec
            .content
            .contains("csilc_w_bytes(b, (v->payload).data, (v->payload).len)")
    );
    // Keys are emitted verbatim, in canonical (encoded-key) order: the shorter
    // encoded key sorts first, so within Task `tags`/`uuid` (len 4) precede longer
    // keys and `current_state` (len 13) comes last; `tags` < `uuid` on content.
    let body_start = codec
        .content
        .find("csilc_enc_Task(csilc_buf *b, const Task *v) {")
        .unwrap();
    let enc = &codec.content[body_start..];
    let pos_tags = enc.find("\"tags\"").unwrap();
    let pos_uuid = enc.find("\"uuid\"").unwrap();
    let pos_state = enc.find("\"current_state\"").unwrap();
    assert!(
        pos_tags < pos_uuid && pos_uuid < pos_state,
        "fields not in canonical key order"
    );

    let client = out
        .files
        .iter()
        .find(|f| f.path == "client.gen.h")
        .expect("client.gen.h emitted");
    // Typed seam: a SubmitTaskRequest in, a Task out, codec called internally.
    assert!(client.content.contains(
        "csil_corndogs_submit_task(const CsilgenTransport *t, const SubmitTaskRequest *req,"
    ));
    assert!(
        client
            .content
            .contains("Task *resp, CsilCodecArena **resp_owner)")
    );
    assert!(
        client
            .content
            .contains("csil_encode_SubmitTaskRequest(req,")
    );
    assert!(client.content.contains("csil_decode_Task(csil_respb,"));
}

/// Compile the generated C and round-trip a typed request/response with gcc. Skips
/// cleanly when no C compiler is on PATH so the suite stays portable; run here with
/// a toolchain present, this is the real proof the output is usable as generated.
#[test]
fn codec_round_trips_through_gcc() {
    let cc = ["cc", "gcc", "clang"].into_iter().find(|c| {
        std::process::Command::new(c)
            .arg("--version")
            .output()
            .is_ok()
    });
    let Some(cc) = cc else {
        eprintln!("skipping: no C compiler on PATH");
        return;
    };

    let input = input_with_rules(corndogs_rules(), "c-client", HashMap::new());
    let out = process_generation(input).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-c-codec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &out.files {
        std::fs::write(dir.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(dir.join("driver.c"), CODEC_DRIVER_C).unwrap();

    let bin = dir.join("driver");
    let compile = std::process::Command::new(cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-O1"])
        .arg("-I")
        .arg(&dir)
        .arg(dir.join("driver.c"))
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "compile failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    // The generated codec must compile without warnings under -Wall -Wextra.
    let warnings = String::from_utf8_lossy(&compile.stderr);
    assert!(warnings.is_empty(), "compiler warnings:\n{warnings}");

    let run = std::process::Command::new(&bin).output().unwrap();
    assert!(
        run.status.success(),
        "round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const CODEC_DRIVER_C: &str = r#"#include "client.gen.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/* A loopback "server": decode the typed request, then encode its task as the
 * typed response, exercising decode and encode on the far side of the seam. */
static int stub_call(void *self, const char *service, const char *op,
                     const uint8_t *req, size_t req_len,
                     uint8_t **resp, size_t *resp_len) {
    (void)self;
    assert(strcmp(service, "corndogs") == 0);
    assert(strcmp(op, "SubmitTask") == 0);
    SubmitTaskRequest in;
    CsilCodecArena *owner;
    if (csil_decode_SubmitTaskRequest(req, req_len, &in, &owner)) return 1;
    int rc = csil_encode_Task(&in.task, resp, resp_len);
    csil_codec_arena_free(owner);
    return rc;
}

int main(void) {
    int64_t prio = 7;
    char *lk[2] = {"a", "b"};
    int64_t lv[2] = {1, 2};
    char *tags[2] = {"x", "y"};
    uint8_t pay[3] = {0xde, 0xad, 0xbe};

    Task t;
    memset(&t, 0, sizeof(t));
    t.uuid = "u-123";
    t.current_state = "PENDING";
    t.payload.data = pay;
    t.payload.len = 3;
    t.priority = &prio;
    t.labels_keys = lk;
    t.labels_values = lv;
    t.labels_count = 2;
    t.tags = tags;
    t.tags_count = 2;
    t.owner = "owner-7";

    SubmitTaskRequest req;
    memset(&req, 0, sizeof(req));
    req.task = t;
    req.queue = "default";

    /* direct codec round-trip through the nested record */
    uint8_t *bytes = NULL;
    size_t n = 0;
    assert(csil_encode_SubmitTaskRequest(&req, &bytes, &n) == 0);
    SubmitTaskRequest back;
    CsilCodecArena *owner = NULL;
    assert(csil_decode_SubmitTaskRequest(bytes, n, &back, &owner) == 0);
    assert(strcmp(back.task.uuid, "u-123") == 0);
    assert(strcmp(back.task.current_state, "PENDING") == 0);
    assert(back.task.payload.len == 3 && memcmp(back.task.payload.data, pay, 3) == 0);
    assert(back.task.priority && *back.task.priority == 7);
    assert(back.task.labels_count == 2);
    assert(strcmp(back.task.labels_keys[0], "a") == 0 && back.task.labels_values[0] == 1);
    assert(strcmp(back.task.labels_keys[1], "b") == 0 && back.task.labels_values[1] == 2);
    assert(back.task.tags_count == 2);
    assert(strcmp(back.task.tags[0], "x") == 0 && strcmp(back.task.tags[1], "y") == 0);
    /* the scalar-alias field (Uuid = text) must survive the round-trip, not stub out */
    assert(strcmp(back.task.owner, "owner-7") == 0);
    assert(strcmp(back.queue, "default") == 0);
    free(bytes);
    csil_codec_arena_free(owner);

    /* an absent optional must round-trip to NULL, not a zero value */
    Task t2 = t;
    t2.priority = NULL;
    SubmitTaskRequest req2 = req;
    req2.task = t2;
    uint8_t *b2 = NULL;
    size_t n2 = 0;
    assert(csil_encode_SubmitTaskRequest(&req2, &b2, &n2) == 0);
    SubmitTaskRequest back2;
    CsilCodecArena *owner2 = NULL;
    assert(csil_decode_SubmitTaskRequest(b2, n2, &back2, &owner2) == 0);
    assert(back2.task.priority == NULL);
    free(b2);
    csil_codec_arena_free(owner2);

    /* typed client round-trip over the loopback carrier */
    CsilgenTransport tr;
    tr.call = stub_call;
    tr.self = NULL;
    Task resp;
    CsilCodecArena *rowner = NULL;
    assert(csil_corndogs_submit_task(&tr, &req, &resp, &rowner) == 0);
    assert(strcmp(resp.uuid, "u-123") == 0);
    assert(resp.payload.len == 3 && memcmp(resp.payload.data, pay, 3) == 0);
    assert(resp.priority && *resp.priority == 7);
    assert(resp.tags_count == 2 && strcmp(resp.tags[1], "y") == 0);
    assert(strcmp(resp.owner, "owner-7") == 0);
    csil_codec_arena_free(rowner);

    /* named map/list aliases must carry every entry through a round-trip: a map of
     * ints, a map of records, and a list of text, each populated as a field. */
    SomeRecord dv[2] = {{"alpha", 10}, {"beta", 20}};
    char *mk[2] = {"q1", "q2"};
    int64_t mv[2] = {3, 1};
    char *dk[2] = {"d1", "d2"};
    char *tl[3] = {"red", "green", "blue"};

    Metrics met;
    memset(&met, 0, sizeof(met));
    met.queue_counts.keys = mk;
    met.queue_counts.values = mv;
    met.queue_counts.count = 2;
    met.detailed.keys = dk;
    met.detailed.values = dv;
    met.detailed.count = 2;
    met.tag_list.items = tl;
    met.tag_list.count = 3;

    uint8_t *mb = NULL;
    size_t mn = 0;
    assert(csil_encode_Metrics(&met, &mb, &mn) == 0);
    Metrics mback;
    CsilCodecArena *mowner = NULL;
    assert(csil_decode_Metrics(mb, mn, &mback, &mowner) == 0);
    assert(mback.queue_counts.count == 2);
    assert(strcmp(mback.queue_counts.keys[0], "q1") == 0 && mback.queue_counts.values[0] == 3);
    assert(strcmp(mback.queue_counts.keys[1], "q2") == 0 && mback.queue_counts.values[1] == 1);
    assert(mback.detailed.count == 2);
    assert(strcmp(mback.detailed.keys[0], "d1") == 0 && strcmp(mback.detailed.keys[1], "d2") == 0);
    assert(strcmp(mback.detailed.values[0].name, "alpha") == 0 && mback.detailed.values[0].value == 10);
    assert(strcmp(mback.detailed.values[1].name, "beta") == 0 && mback.detailed.values[1].value == 20);
    assert(mback.tag_list.count == 3);
    assert(strcmp(mback.tag_list.items[0], "red") == 0);
    assert(strcmp(mback.tag_list.items[1], "green") == 0);
    assert(strcmp(mback.tag_list.items[2], "blue") == 0);
    free(mb);
    csil_codec_arena_free(mowner);

    printf("ok\n");
    return 0;
}
"#;

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

fn alias_rule(name: &str, target: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(target),
        position: pos(),
        doc_comments: vec![],
    }
}

/// A field typed as a transparent scalar alias (`Uuid = text`) must encode/decode as
/// its underlying type, not fall through to the warned `null`/zero stub that silently
/// drops the value (the regression this fix addresses).
#[test]
fn alias_reference_resolves_through_to_underlying_codec() {
    let rules = vec![
        alias_rule("Uuid", builtin("text")),
        group_rule(
            "Doc",
            vec![bare_entry(
                "id",
                CsilTypeExpression::Reference("Uuid".to_string()),
            )],
        ),
    ];
    let input = input_with_rules(rules, "c-client", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();
    let mut warnings = Vec::new();
    let codec = generate_codec(&input, &config, &mut warnings).expect("codec emitted");

    // The `id` field resolves through the alias to `text`, so its encode is the text
    // writer keyed on the struct member — not the `csilc_w_null` stub.
    assert!(
        codec.contains("if (csilc_w_text(b, (v->id), (v->id) ? strlen(v->id) : 0))"),
        "alias field did not resolve to the underlying text encoder:\n{codec}"
    );
    assert!(
        codec.contains("if (!csilc_get_text(csilc_f, &(out->id)))"),
        "alias field did not resolve to the underlying text decoder:\n{codec}"
    );
    // No "has no generated codec" degradation should be reported for the alias.
    assert!(
        !warnings.iter().any(|w| w.message.contains("Uuid")),
        "alias reference was treated as an un-codec'd type: {warnings:?}"
    );
}

/// A named map alias (`StringInt64Map = {* text => int}`) and a named list alias
/// (`Tags = [* text]`) now emit faithful struct types that carry their entries —
/// parallel `keys`/`values`/`count` for a map, `items`/`count` for a list — and each
/// is a codec'd type with a real map/array encode, not the old data-less `void *` /
/// count-less pointer that could not round-trip.
#[test]
fn named_map_and_list_aliases_are_faithful_codecd_structs() {
    let rules = vec![
        alias_rule(
            "StringInt64Map",
            CsilTypeExpression::Map {
                key: Box::new(builtin("text")),
                value: Box::new(builtin("int")),
                occurrence: Some(CsilOccurrence::ZeroOrMore),
            },
        ),
        alias_rule(
            "Tags",
            CsilTypeExpression::Array {
                element_type: Box::new(builtin("text")),
                occurrence: Some(CsilOccurrence::ZeroOrMore),
            },
        ),
    ];
    let input = input_with_rules(rules, "c", HashMap::new());
    let config = CConfig::from_options(&input.config.options).unwrap();

    // The map alias is a real struct (keys/values/count), never the opaque void*.
    let types = generate_types(&input, &config).unwrap();
    assert!(
        types.contains("typedef struct StringInt64Map {")
            && types.contains("char **keys;")
            && types.contains("int64_t *values;"),
        "named map alias is not a faithful keys/values/count struct:\n{types}"
    );
    assert!(
        types.contains("typedef struct Tags {") && types.contains("char **items;"),
        "named list alias is not a faithful items/count struct:\n{types}"
    );
    assert!(
        !types.contains("typedef void *StringInt64Map;"),
        "named map alias still falls back to the opaque void* limitation:\n{types}"
    );

    // Each alias is codec'd: a real CBOR map / array walk over the struct members, not
    // a null/void stub.
    let mut warnings = Vec::new();
    let codec = generate_codec(&input, &config, &mut warnings).expect("codec emitted");
    assert!(
        codec.contains(
            "static inline int csilc_enc_StringInt64Map(csilc_buf *b, const StringInt64Map *v)"
        ) && codec.contains("if (csilc_w_map_head(b, v->count)) return -1;"),
        "named map alias did not get a real map encoder:\n{codec}"
    );
    assert!(
        codec.contains(
            "static inline int csilc_dec_StringInt64Map(const csilc_value *m, CsilCodecArena *a, StringInt64Map *out)"
        ),
        "named map alias did not get a decoder:\n{codec}"
    );
    assert!(
        codec.contains("static inline int csilc_enc_Tags(csilc_buf *b, const Tags *v)")
            && codec.contains("if (csilc_w_array_head(b, v->count)) return -1;"),
        "named list alias did not get a real array encoder:\n{codec}"
    );
    // No "has no generated codec" degradation for the now-codec'd aliases.
    assert!(
        !warnings
            .iter()
            .any(|w| w.message.contains("StringInt64Map") || w.message.contains("Tags")),
        "map/list alias was treated as un-codec'd: {warnings:?}"
    );
}

// ---- self-contained package (README + Quickstart) -------------------------

// `Name = { ... }` parses to `TypeDef(Group(..))` (not `GroupDef`), so build the
// records that way here to guard the README/example path against the real parser.
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

fn pingpong_rules() -> Vec<CsilRule> {
    vec![
        record_typedef("Ping", vec![bare_entry("msg", builtin("text"))]),
        record_typedef("Pong", vec![bare_entry("msg", builtin("text"))]),
        service_rule(
            "Echo",
            vec![op(
                "ping",
                CsilTypeExpression::Reference("Ping".to_string()),
                CsilTypeExpression::Reference("Pong".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            None,
        ),
    ]
}

fn file_named<'a>(files: &'a [GeneratedFile], path: &str) -> &'a str {
    files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.content.as_str())
        .unwrap_or_else(|| panic!("missing emitted file: {path}"))
}

#[test]
fn readme_absent_without_emit_packages() {
    let input = input_with_rules(pingpong_rules(), "c-client", HashMap::new());
    let files = process_generation(input).expect("generate").files;
    assert!(
        !files.iter().any(|f| f.path == "README.md"),
        "README.md must not be emitted without emit_packages"
    );
}

#[test]
fn package_readme_has_quickstart_carrier_and_example() {
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    let input = input_with_rules(pingpong_rules(), "c-client", options);
    let files = process_generation(input).expect("generate").files;
    let readme = file_named(&files, "README.md");

    // The carrier (the must-have) implements the generated transport seam.
    assert!(
        readme
            .contains("static int csil_rpc_call(void *self, const char *service, const char *op,"),
        "README must embed the CSIL-RPC carrier seam:\n{readme}"
    );
    assert!(
        readme.contains("CsilgenTransport transport = { .call = csil_rpc_call, .self = &carrier }"),
        "example must wire the carrier into the transport seam:\n{readme}"
    );
    // The canonical mount + method.
    assert!(
        readme.contains("POST /csil/v1/rpc HTTP/1.1"),
        "carrier must POST the canonical mount:\n{readme}"
    );
    // The request payload is tag-24 wrapped, built with the package's own codec.
    assert!(
        readme.contains("csilc_w_tag(&env, 24)")
            && readme.contains("csilc_w_bytes(&env, req, req_len)"),
        "request payload must be a tag-24 byte string built with the generated codec:\n{readme}"
    );
    // Both transport-status and typed ServiceError arms are handled.
    assert!(
        readme.contains("status != 0") && readme.contains("\"ServiceError\""),
        "carrier must handle the status and ServiceError arms:\n{readme}"
    );
    // The example call hits the first op with a generated sample literal and prints
    // the typed response field.
    assert!(
        readme.contains("Ping req = { .msg = \"example\" }"),
        "example must pass a generated sample request literal:\n{readme}"
    );
    assert!(
        readme.contains("csil_echo_ping(&transport, &req, &resp, &owner)"),
        "example must call the first service op over the carrier:\n{readme}"
    );
    assert!(
        readme.contains("resp.msg"),
        "example must print the typed response field:\n{readme}"
    );
}

#[test]
fn serviceless_package_readme_is_types_only() {
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    let input = input_with_rules(
        vec![record_typedef(
            "Thing",
            vec![bare_entry("id", builtin("uint"))],
        )],
        "c-typesonly",
        options,
    );
    let files = process_generation(input).expect("generate").files;
    let readme = file_named(&files, "README.md");
    assert!(
        readme.contains("has no service operations"),
        "a serviceless package must get the types-only section:\n{readme}"
    );
    assert!(
        !readme.contains("csil_rpc_call"),
        "a serviceless package must not embed a client carrier:\n{readme}"
    );
}

/// End-to-end proof the README Quickstart carrier compiles against the real generated
/// package: emit the ping/pong client package, write every file, extract the README's
/// `c` block as `main.c`, and compile it with `cc`. A clean compile proves the carrier
/// is valid against the generated codec + client. The sandbox kills cross-process
/// loopback sockets, so this compiles (does not run) the carrier; the standalone
/// `tools/csil-rpc-echo-mock.py` verifies the same carrier against real HTTP normally.
/// Skips when no C compiler is on PATH so the suite stays portable.
#[test]
fn readme_quickstart_carrier_compiles_with_cc() {
    let cc = ["cc", "gcc", "clang"].into_iter().find(|bin| {
        std::process::Command::new(bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });
    let Some(cc) = cc else {
        eprintln!("skipping: no C compiler on PATH");
        return;
    };

    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    let input = input_with_rules(pingpong_rules(), "c-client", options);
    let files = process_generation(input).expect("generate").files;

    let dir = std::env::temp_dir().join(format!("csilgen-c-readme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }

    let readme = file_named(&files, "README.md");
    let block = extract_c_block(readme).expect("README must contain a c code block");
    std::fs::write(dir.join("main.c"), &block).unwrap();

    let out = std::process::Command::new(cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-c", "main.c", "-o"])
        .arg(dir.join("main.o"))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{cc} failed on the README carrier:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The contents of the first ```` ```c ```` fenced block in `md`.
fn extract_c_block(md: &str) -> Option<String> {
    let start = md.find("```c\n")? + "```c\n".len();
    let end = md[start..].find("\n```")? + start;
    Some(md[start..end].to_string())
}
