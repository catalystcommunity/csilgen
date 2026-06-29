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
        !files.iter().any(|f| f.path == "genquickstart.md"),
        "genquickstart.md must not be emitted without emit_packages"
    );
}

#[test]
fn package_readme_present_by_default_in_package_mode() {
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    let input = input_with_rules(pingpong_rules(), "c-client", options);
    let files = process_generation(input).expect("generate").files;
    assert!(
        files.iter().any(|f| f.path == "genquickstart.md"),
        "genquickstart.md must be emitted by default in package mode"
    );
}

#[test]
fn emit_readme_false_suppresses_only_the_readme() {
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    options.insert("emit_readme".to_string(), serde_json::json!(false));
    let input = input_with_rules(pingpong_rules(), "c-client", options);
    let files = process_generation(input).expect("generate").files;
    assert!(
        !files.iter().any(|f| f.path == "genquickstart.md"),
        "explicit emit_readme:false must suppress genquickstart.md"
    );
    // Suppressing the README must not disturb any other package output.
    assert!(
        files.iter().any(|f| f.path != "genquickstart.md"),
        "package output other than the README must still be emitted"
    );
}

/// The verification spec: a `->` unary op AND a record-typed `<->` channel op on one
/// service, so every section has a generated surface to wire to (per the contract).
fn transports_rules() -> Vec<CsilRule> {
    vec![
        record_typedef("Ping", vec![bare_entry("msg", builtin("text"))]),
        record_typedef("Pong", vec![bare_entry("msg", builtin("text"))]),
        record_typedef("ChatMsg", vec![bare_entry("body", builtin("text"))]),
        record_typedef("ChatEvent", vec![bare_entry("body", builtin("text"))]),
        service_rule(
            "Echo",
            vec![
                op(
                    "ping",
                    CsilTypeExpression::Reference("Ping".to_string()),
                    CsilTypeExpression::Reference("Pong".to_string()),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                op(
                    "chat",
                    CsilTypeExpression::Reference("ChatMsg".to_string()),
                    CsilTypeExpression::Reference("ChatEvent".to_string()),
                    CsilServiceDirection::Bidirectional,
                    None,
                ),
            ],
            Some(7),
        ),
    ]
}

fn render_transports_readme(extra: Vec<(&str, serde_json::Value)>) -> String {
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    for (k, v) in extra {
        options.insert(k.to_string(), v);
    }
    let input = input_with_rules(transports_rules(), "c-client", options);
    let files = process_generation(input).expect("generate").files;
    file_named(&files, "genquickstart.md").to_string()
}

#[test]
fn readme_intro_credits_the_transport_library() {
    let readme = render_transports_readme(vec![]);
    assert!(readme.contains("the `csil` transport library (`transports/c`) owns the"));
    assert!(readme.contains("supply only a *carrier* that"));
    // The Install section adds the transport-lib dependency with the not-yet-published note.
    assert!(readme.contains("not yet"));
    assert!(readme.contains("vendor `transports/c`"));
}

#[test]
fn readme_rpc_section_uses_lib_envelope_and_example_call() {
    let readme = render_transports_readme(vec![]);
    assert!(readme.contains("## CSIL-RPC (HTTP)"));
    assert!(readme.contains("#include <csil/csil.h>"));
    // The carrier implements the generated byte seam.
    assert!(
        readme
            .contains("static int csil_rpc_call(void *self, const char *service, const char *op,")
    );
    assert!(
        readme.contains("CsilgenTransport transport = { .call = csil_rpc_call, .self = &carrier }")
    );
    // The envelope is built and parsed with the LIBRARY, not hand-rolled.
    assert!(readme.contains("csil_rpc_request_init(&rq, service, op, req, req_len)"));
    assert!(readme.contains("csil_rpc_request_encode(&rq, &env, &env_len)"));
    assert!(readme.contains("csil_rpc_response_decode(body, body_len, &rsp)"));
    assert!(readme.contains("csil_rpc_response_is_transport_error(&rsp)"));
    assert!(readme.contains("rsp.variant && strcmp(rsp.variant, \"ServiceError\")"));
    assert!(!readme.contains("csilc_w_tag(&env, 24)")); // no hand-rolled envelope
    // The canonical mount + the example call with a generated sample literal.
    assert!(readme.contains("POST /csil/v1/rpc HTTP/1.1"));
    assert!(readme.contains("Ping req = { .msg = \"example\" }"));
    assert!(readme.contains("csil_echo_ping(&transport, &req, &resp, &owner)"));
    assert!(readme.contains("resp.msg"));
}

#[test]
fn readme_events_section_handshake_heartbeat_and_router_dispatch() {
    let readme = render_transports_readme(vec![]);
    assert!(readme.contains("## CSIL-Events (TLS)"));
    // A frame carrier over a TLS byte stream via the lib's stream-carrier framing.
    assert!(readme.contains("csil_stream stream = { .read = tls_read, .write = tls_write"));
    assert!(readme.contains("csil_frame_carrier carrier = csil_stream_carrier(&stream, 0)"));
    // The $hello / $hello-ack handshake with the lib's Hello/HelloAck.
    assert!(readme.contains("csil_hello hello = {"));
    assert!(readme.contains("csil_hello_encode(&hello, &hb, &hbn)"));
    assert!(readme.contains("csil_hello_ack_decode(ackf, ackn, &ack)"));
    assert!(readme.contains("csil_profile_parse(ack.profile, &profile)"));
    // The $ping / $pong heartbeat via the lib's heartbeat + control names.
    assert!(readme.contains("strcmp(ev.event, CSIL_PING_NAME) == 0"));
    assert!(readme.contains("CSIL_PONG_NAME"));
    assert!(readme.contains("csil_heartbeat pong = { .nonce = ping.nonce }"));
    // One outbound event via the generated encoder, dispatch into the generated router.
    assert!(readme.contains("EchoHandlers handlers = { .chat = on_chat }"));
    assert!(readme.contains("encode_echo_chat(&codec, &out_msg, &outb, &outn)"));
    assert!(readme.contains(
        "route_echo_channel(&handlers, NULL, &codec, ev.event, ev.payload, ev.payload_len)"
    ));
}

#[test]
fn readme_datagrams_section_send_and_late_arrival_note() {
    let readme = render_transports_readme(vec![]);
    assert!(readme.contains("## CSIL-Datagrams (UDP)"));
    assert!(readme.contains("csil_datagram_carrier carrier = { .send_datagram = udp_send"));
    // Encode a `->` request via the generated codec, wrap in the lib Datagram, fire-and-forget.
    assert!(readme.contains("csil_encode_Ping(&req, &payload, &payload_len)"));
    assert!(readme.contains("csil_datagram_init(&dg, OP_ORD, 0, payload, payload_len)"));
    // The op's datagram ordinal (ping op has no @wire-id → placeholder 1).
    assert!(readme.contains("#define OP_ORD 1u"));
    // Recv path decodes an inbound Datagram into the RESPONSE type, with the loss note.
    assert!(readme.contains("csil_datagram_decode(inb, inn, &in_dg)"));
    assert!(readme.contains("csil_decode_Pong(in_dg.payload, in_dg.payload_len, &resp, &owner)"));
    assert!(readme.contains("MAY arrive later — or never"));
}

#[test]
fn readme_no_channel_op_emits_events_note() {
    // pingpong has only a `->` op: Events still shows handshake + heartbeat, but notes the
    // absence of a generated channel router rather than wiring dispatch.
    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    let input = input_with_rules(pingpong_rules(), "c-client", options);
    let files = process_generation(input).expect("generate").files;
    let readme = file_named(&files, "genquickstart.md");
    assert!(readme.contains("## CSIL-Events (TLS)"));
    assert!(readme.contains("csil_hello_encode(&hello, &hb, &hbn)"));
    assert!(readme.contains("no generated channel router"));
    assert!(!readme.contains("route_echo_channel"));
}

#[test]
fn readme_transport_subset_renders_only_listed_sections() {
    // `genquickstart_transports: ["rpc"]` renders only the RPC section.
    let only_rpc = render_transports_readme(vec![(
        "genquickstart_transports",
        serde_json::json!(["rpc"]),
    )]);
    assert!(only_rpc.contains("## CSIL-RPC (HTTP)"));
    assert!(!only_rpc.contains("## CSIL-Events (TLS)"));
    assert!(!only_rpc.contains("## CSIL-Datagrams (UDP)"));

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
fn serviceless_package_readme_has_no_call_sections() {
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
    let readme = file_named(&files, "genquickstart.md");
    // No `->` op → the RPC and Datagrams sections fall back to the "no operations" note.
    assert!(
        readme.contains("no `->` operations"),
        "a serviceless package must note the absence of operations:\n{readme}"
    );
    assert!(
        !readme.contains("csil_rpc_call"),
        "a serviceless package must not embed a client carrier:\n{readme}"
    );
}

/// End-to-end proof every emitted section compiles against the ONE generated package AND
/// the real transport library. Generate a single package (`--target c`, the surface a user
/// publishes), write every file, and confirm that one package carries both the RPC client
/// (`client.gen.h`) and the Events router (`server.gen.h`) — package mode emits all
/// surfaces when services exist, so the three quickstart sections (RPC client, Events
/// router, Datagrams codec) all resolve against this single directory. Extract each
/// section's `c` block and `cc -c` it with `-I <transports/c/include>`. A clean compile
/// proves each carrier is valid against the generated codec/client/router and the library's
/// envelope APIs. The sandbox kills cross-process loopback sockets, so this compiles (does
/// not run) the carriers. Skips when no C compiler is on PATH so the suite stays portable.
#[test]
fn genquickstart_sections_compile_with_cc() {
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

    // The transport library headers live at the repo root, beside this crate's grandparent.
    let lib_include =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/c/include");
    if !lib_include.exists() {
        eprintln!("skipping: transport lib headers not found at {lib_include:?}");
        return;
    }

    let mut options = HashMap::new();
    options.insert("emit_packages".to_string(), serde_json::json!(["c"]));
    let files = process_generation(input_with_rules(transports_rules(), "c", options))
        .expect("generate")
        .files;

    // The single package must be self-contained: the Events router section needs
    // server.gen.h and the RPC section needs client.gen.h, both from THIS package.
    assert!(
        files.iter().any(|f| f.path == "client.gen.h"),
        "package mode must emit client.gen.h (the RPC section depends on it)"
    );
    assert!(
        files.iter().any(|f| f.path == "server.gen.h"),
        "package mode must emit server.gen.h (the Events router section depends on it)"
    );

    let dir = std::env::temp_dir().join(format!("csilgen-c-3transport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }

    let readme = file_named(&files, "genquickstart.md");
    let blocks = extract_all_c_blocks(readme);
    assert_eq!(blocks.len(), 3, "expected three transport code blocks");

    for (i, block) in blocks.iter().enumerate() {
        let src = format!("section{i}.c");
        std::fs::write(dir.join(&src), block).unwrap();
        let out = std::process::Command::new(cc)
            .args(["-std=c11", "-Wall", "-Wextra", "-I"])
            .arg(&lib_include)
            .args(["-c", &src, "-o"])
            .arg(dir.join(format!("section{i}.o")))
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{cc} failed on genquickstart section {i}:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The contents of every ```` ```c ```` fenced block in `md`, in order.
fn extract_all_c_blocks(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(s) = rest.find("```c\n") {
        let body = &rest[s + "```c\n".len()..];
        let end = body.find("\n```").expect("unterminated c block");
        out.push(body[..end].to_string());
        rest = &body[end + "\n```".len()..];
    }
    out
}

// ---- non-record op boundaries (scalar aliases, bare arrays) ----------------

/// A service whose ops cross non-record boundaries: `get-house` takes a scalar
/// transparent alias (`HouseID = text`) and `list-houses` returns a bare `[* House]`.
/// Before the fix the client called a `csil_encode_HouseID`/`csil_decode_<list>` that
/// no record codec ever defined, so the package failed to compile.
fn house_service_rules() -> Vec<CsilRule> {
    let house = group_rule(
        "House",
        vec![
            bare_entry(
                "house_id",
                CsilTypeExpression::Reference("HouseID".to_string()),
            ),
            bare_entry("name", builtin("text")),
        ],
    );
    let list_req = group_rule("ListReq", vec![bare_entry("limit", builtin("int"))]);
    let svc = service_rule(
        "HouseService",
        vec![
            op(
                "get-house",
                CsilTypeExpression::Reference("HouseID".to_string()),
                CsilTypeExpression::Reference("House".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            ),
            op(
                "list-houses",
                CsilTypeExpression::Reference("ListReq".to_string()),
                CsilTypeExpression::Array {
                    element_type: Box::new(CsilTypeExpression::Reference("House".to_string())),
                    occurrence: Some(CsilOccurrence::ZeroOrMore),
                },
                CsilServiceDirection::Unidirectional,
                None,
            ),
        ],
        None,
    );
    vec![alias_rule("HouseID", builtin("text")), house, list_req, svc]
}

#[test]
fn nonrecord_op_boundaries_emit_synthetic_codecs() {
    let out = process_generation(input_with_rules(
        house_service_rules(),
        "c-client",
        HashMap::new(),
    ))
    .unwrap();
    let codec = file_named(&out.files, "codec.gen.h");
    let client = file_named(&out.files, "client.gen.h");

    // The scalar-alias request boundary gets its own bare-value codec + public wrappers.
    assert!(
        codec.contains("static inline int csilc_enc_HouseID(csilc_buf *b, const HouseID *v)"),
        "missing synthetic scalar-alias encoder:\n{codec}"
    );
    assert!(codec.contains("csil_encode_HouseID("));
    assert!(codec.contains("csil_decode_HouseID("));
    // The bare-array response boundary reuses the named-list machinery: an items+count
    // struct plus its array codec, so the wire form stays a plain CBOR array.
    assert!(
        codec.contains("} CsilHouseList;"),
        "missing synthetic list struct for the bare array boundary:\n{codec}"
    );
    assert!(
        codec.contains(
            "static inline int csilc_enc_CsilHouseList(csilc_buf *b, const CsilHouseList *v)"
        ),
        "missing synthetic list encoder:\n{codec}"
    );
    assert!(codec.contains("csil_decode_CsilHouseList("));

    // The client declares the boundary C types and calls the synthetic wrappers.
    assert!(client.contains("const HouseID *req"));
    assert!(client.contains("if (csil_encode_HouseID(req,"));
    assert!(client.contains("CsilHouseList *resp"));
    assert!(client.contains("csil_decode_CsilHouseList(csil_respb,"));
}

const NONRECORD_DRIVER_C: &str = r#"#include "client.gen.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/* Loopback "server": echo the decoded scalar id back inside a House, and answer a
 * list request with a two-element House array, exercising every boundary codec. */
static int stub_call(void *self, const char *service, const char *op,
                     const uint8_t *req, size_t req_len,
                     uint8_t **resp, size_t *resp_len) {
    (void)self;
    assert(strcmp(service, "house") == 0);
    if (strcmp(op, "GetHouse") == 0) {
        HouseID id;
        CsilCodecArena *owner;
        if (csil_decode_HouseID(req, req_len, &id, &owner)) return 1;
        House h;
        memset(&h, 0, sizeof(h));
        h.house_id = id; /* echo the scalar request value back */
        h.name = "Maple";
        int rc = csil_encode_House(&h, resp, resp_len);
        csil_codec_arena_free(owner);
        return rc;
    }
    if (strcmp(op, "ListHouses") == 0) {
        ListReq lr;
        CsilCodecArena *owner;
        if (csil_decode_ListReq(req, req_len, &lr, &owner)) return 1;
        House hs[2];
        memset(hs, 0, sizeof(hs));
        hs[0].house_id = "h1"; hs[0].name = "A";
        hs[1].house_id = "h2"; hs[1].name = "B";
        CsilHouseList list;
        list.items = hs;
        list.count = 2;
        int rc = csil_encode_CsilHouseList(&list, resp, resp_len);
        csil_codec_arena_free(owner);
        return rc;
    }
    return 1;
}

int main(void) {
    CsilgenTransport tr;
    tr.call = stub_call;
    tr.self = NULL;

    /* scalar-alias request boundary: a bare CBOR text carries the id over the wire */
    HouseID qid = "house-42";
    House resp;
    CsilCodecArena *ro = NULL;
    assert(csil_house_get_house(&tr, &qid, &resp, &ro) == 0);
    assert(strcmp(resp.name, "Maple") == 0);
    assert(strcmp(resp.house_id, "house-42") == 0);
    csil_codec_arena_free(ro);

    /* bare-array response boundary: a CBOR array decodes into items + count */
    ListReq lr;
    lr.limit = 10;
    CsilHouseList houses;
    CsilCodecArena *lo = NULL;
    assert(csil_house_list_houses(&tr, &lr, &houses, &lo) == 0);
    assert(houses.count == 2);
    assert(strcmp(houses.items[0].house_id, "h1") == 0 && strcmp(houses.items[0].name, "A") == 0);
    assert(strcmp(houses.items[1].house_id, "h2") == 0 && strcmp(houses.items[1].name, "B") == 0);
    csil_codec_arena_free(lo);

    printf("ok\n");
    return 0;
}
"#;

/// Compile and round-trip a scalar-alias request and a bare-array response through
/// gcc. Skips cleanly when no C compiler is present so the suite stays portable.
#[test]
fn nonrecord_boundary_client_round_trips_through_gcc() {
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

    let out = process_generation(input_with_rules(
        house_service_rules(),
        "c-client",
        HashMap::new(),
    ))
    .unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-c-nonrec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &out.files {
        std::fs::write(dir.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(dir.join("driver.c"), NONRECORD_DRIVER_C).unwrap();

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
