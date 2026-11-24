//! Tests for the names the generator reserves, renames and puts on the wire.

use super::*;
use csilgen_common::GeneratorConfig;

fn at() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

fn entry(key: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(key.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type,
        position: at(),
        doc_comments: Vec::new(),
    }
}

fn record(name: &str, fields: Vec<(&str, CsilTypeExpression)>) -> CsilRule {
    rule(
        name,
        CsilRuleType::GroupDef(CsilGroupExpression {
            entries: fields.into_iter().map(|(k, t)| entry(k, t)).collect(),
        }),
    )
}

fn alias(name: &str, ty: CsilTypeExpression) -> CsilRule {
    rule(name, CsilRuleType::TypeDef(ty))
}

fn service(name: &str, ops: Vec<(&str, CsilTypeExpression, CsilTypeExpression)>) -> CsilRule {
    rule(
        name,
        CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: ops
                .into_iter()
                .map(|(op, input_type, output_type)| CsilServiceOperation {
                    name: op.to_string(),
                    input_type,
                    output_type,
                    direction: CsilServiceDirection::Unidirectional,
                    position: at(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                })
                .collect(),
            wire_id: None,
        }),
    )
}

fn input(target: &str, rules: Vec<CsilRule>) -> WasmGeneratorInput {
    let service_count = rules
        .iter()
        .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
        .count();
    let mut options = HashMap::new();
    options.insert(
        "module_root_filename".to_string(),
        serde_json::Value::String("lib.rs".to_string()),
    );
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules,
            source_content: None,
            service_count,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        },
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp/output".to_string(),
            options,
        },
        generator_metadata: GeneratorMetadata {
            name: "rust-code-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test Rust generator".to_string(),
            target: target.to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: None,
            homepage: None,
        },
    }
}

/// Generate `rules` for `target`, returning the warnings and every file joined.
fn generate(target: &str, rules: Vec<CsilRule>) -> (Vec<String>, String, Vec<GeneratedFile>) {
    let input = input(target, rules);
    let mut generator = RustCodeGenerator::new(&input);
    let files = generator.generate().expect("generation ok");
    let all = files.iter().map(|f| f.content.as_str()).collect();
    let warnings = generator
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();
    (warnings, all, files)
}

/// Build the generated files as a library crate with clippy at `-D warnings`.
/// Skipped when no cargo is on PATH.
fn assert_builds(label: &str, files: &[GeneratedFile]) {
    let probe = std::process::Command::new("cargo")
        .arg("--version")
        .output();
    if probe.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: no cargo toolchain on PATH");
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("csilgen-rust-names-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for file in files.iter().filter(|f| f.path.ends_with(".rs")) {
        std::fs::write(src.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"names-check\"\nedition = \"2024\"\nversion = \"0.0.0\"\n\n\
         [workspace]\n\n[dependencies]\n",
    )
    .unwrap();
    let run = std::process::Command::new("cargo")
        .args(["clippy", "--quiet", "--", "-D", "warnings"])
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{label} does not build:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn rp_schema() -> Vec<CsilRule> {
    vec![
        record(
            "RpIssueAttestationRequest",
            vec![("claim_type", builtin("text"))],
        ),
        record(
            "RpIssueAttestationResponse",
            vec![("deposited", builtin("bool"))],
        ),
        service(
            "Rp",
            vec![(
                "issue-attestation",
                reference("RpIssueAttestationRequest"),
                reference("RpIssueAttestationResponse"),
            )],
        ),
    ]
}

/// The linkkeys request: a record that IS its op's boundary uses its own codec, so
/// no per-op codec is emitted beside it and the record keeps its name.
#[test]
fn a_record_boundary_keeps_its_name() {
    for target in ["rust", "rust-client"] {
        let (warnings, all, files) = generate(target, rp_schema());
        assert!(warnings.is_empty(), "{target}: {warnings:?}");
        for item in [
            "pub struct RpIssueAttestationRequest {",
            "pub fn encode_rp_issue_attestation_request(",
            "pub fn decode_rp_issue_attestation_request(",
            "pub struct RpIssueAttestationResponse {",
            "pub fn encode_rp_issue_attestation_response(",
        ] {
            assert_eq!(all.matches(item).count(), 1, "{target}: {item}\n{all}");
        }
        assert_builds(&format!("rp-{target}"), &files);
    }
}

/// A non-record boundary gets a per-op codec, so a record with that codec's name is
/// still renamed and warned.
#[test]
fn a_rule_named_like_an_emitted_op_codec_is_renamed() {
    let rules = vec![
        alias("MemberID", builtin("text")),
        record("MemberGetMemberRequest", vec![("id", builtin("text"))]),
        service(
            "Member",
            vec![("get-member", reference("MemberID"), reference("MemberID"))],
        ),
    ];
    for target in ["rust", "rust-client"] {
        let (warnings, all, files) = generate(target, rules.clone());
        assert!(
            warnings.iter().any(|w| w.contains(
                "rule `MemberGetMemberRequest` is generated as `MemberGetMemberRequestType`"
            )),
            "{target}: {warnings:?}"
        );
        assert!(
            all.contains("pub struct MemberGetMemberRequestType {"),
            "{all}"
        );
        assert_eq!(
            all.matches("pub fn encode_member_get_member_request(")
                .count(),
            1,
            "{all}"
        );
        assert_builds(&format!("member-{target}"), &files);
    }
}

/// A client struct name is reserved only where a client is emitted.
#[test]
fn client_names_are_reserved_only_for_a_client() {
    let mut rules = rp_schema();
    rules.push(record("RpClient", vec![("x", builtin("text"))]));
    let (warnings, all, _) = generate("rust-typesonly", rules.clone());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(all.contains("pub struct RpClient {"), "{all}");
    let (warnings, all, files) = generate("rust-client", rules);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(all.contains("pub struct RpClientType {"), "{all}");
    assert_builds("rp-client-clash", &files);
}

/// Two services with one identifier: the later one is renamed for its generated
/// names, and both keep their declared names on the wire.
#[test]
fn clashing_services_keep_their_wire_names() {
    let rules = vec![
        record("Ping", vec![("n", builtin("int"))]),
        service(
            "UserSvc",
            vec![("ping", reference("Ping"), reference("Ping"))],
        ),
        service(
            "user-svc",
            vec![("ping", reference("Ping"), reference("Ping"))],
        ),
    ];
    for target in ["rust", "rust-client"] {
        let (warnings, all, files) = generate(target, rules.clone());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("service `user-svc` is generated as")),
            "{target}: {warnings:?}"
        );
        if target == "rust-client" {
            assert!(all.contains("\"UserSvc\", \"ping\""), "{all}");
            assert!(all.contains("\"user-svc\", \"ping\""), "{all}");
            assert!(all.contains("pub struct UserSvcClient<"), "{all}");
            assert!(all.contains("pub struct UserSvc2Client<"), "{all}");
        } else {
            assert!(all.contains("pub trait UserSvc {"), "{all}");
            assert!(all.contains("pub trait user_svc2 {"), "{all}");
        }
        assert_builds(&format!("services-{target}"), &files);
    }
}

/// A rule the hoister invents gets the same clash checks as a declared one, and the
/// invented rule is the one renamed.
#[test]
fn a_hoisted_name_that_clashes_is_renamed() {
    // `Lookup_find` with an inline record field `request` hoists `Lookup_find_request`,
    // whose `encode_lookup_find_request` is also the per-op codec of `find`.
    let rules = vec![
        record(
            "Lookup_find",
            vec![(
                "request",
                CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![entry("id", builtin("int"))],
                }),
            )],
        ),
        service("Lookup", vec![("find", builtin("text"), builtin("text"))]),
    ];
    let (warnings, all, files) = generate("rust-client", rules);
    assert!(
        warnings
            .iter()
            .any(|w| w
                .contains("rule `Lookup_find_request` is generated as `Lookup_find_requestType`")),
        "{warnings:?}"
    );
    assert!(all.contains("pub struct Lookup_find {"), "{all}");
    assert!(
        all.contains("pub struct Lookup_find_requestType {"),
        "{all}"
    );
    assert_eq!(
        all.matches("pub fn encode_lookup_find_request(").count(),
        1,
        "{all}"
    );
    assert_builds("hoisted-clash", &files);
}

/// A mixed op output becomes one named union with every arm, in client and server.
#[test]
fn a_mixed_op_output_is_one_named_union() {
    let rules = vec![
        record("Holder", vec![("n", builtin("int"))]),
        record("Err", vec![("message", builtin("text"))]),
        service(
            "Lookup",
            vec![(
                "read-counts",
                reference("Holder"),
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                    reference("Err"),
                ]),
            )],
        ),
    ];
    for target in ["rust", "rust-client"] {
        let (warnings, all, files) = generate(target, rules.clone());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(all.contains("pub enum LookupReadCountsOutput {"), "{all}");
        assert!(
            all.contains("Variant0(std::collections::HashMap<String, i64>),"),
            "{all}"
        );
        assert!(all.contains("Variant1(Err),"), "{all}");
        assert!(all.contains("Result<LookupReadCountsOutput, "), "{all}");
        assert_builds(&format!("mixed-{target}"), &files);
    }
}

fn text_map(key: &str, value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Map {
        key: Box::new(builtin(key)),
        value: Box::new(builtin(value)),
        occurrence: None,
    }
}

fn group(fields: Vec<(&str, &str)>) -> CsilTypeExpression {
    CsilTypeExpression::Group(CsilGroupExpression {
        entries: fields
            .into_iter()
            .map(|(k, t)| entry(k, builtin(t)))
            .collect(),
    })
}

/// Run the shared wire vectors (canonical map order, duplicate keys, tagged record
/// unions, the mixed op output) through the generated codec with the fixture's own
/// peer, on the resolved form of `tests/interop/wire-vectors/wire.csil`.
#[test]
fn codec_passes_shared_wire_vectors() {
    let probe = std::process::Command::new("cargo")
        .arg("--version")
        .output();
    if probe.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: no cargo toolchain on PATH");
        return;
    }
    let rules = vec![
        alias("Counts", text_map("text", "int")),
        alias("Labels", text_map("int", "text")),
        record(
            "Holder",
            vec![
                ("counts", reference("Counts")),
                ("labels", reference("Labels")),
            ],
        ),
        alias(
            "Contact",
            CsilTypeExpression::Choice(vec![
                group(vec![("id", "int"), ("name", "text")]),
                group(vec![("id", "int")]),
                group(vec![("email", "text")]),
            ]),
        ),
        record("Err", vec![("message", builtin("text"))]),
        service(
            "Lookup",
            vec![(
                "read-counts",
                reference("Holder"),
                CsilTypeExpression::Choice(vec![text_map("text", "int"), reference("Err")]),
            )],
        ),
    ];
    let (_, _, files) = generate("rust", rules);
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/interop/wire-vectors")
        .canonicalize()
        .expect("wire-vectors fixture exists");
    let dir = std::env::temp_dir().join(format!("csilgen-rust-wire-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for file in files.iter().filter(|f| f.path.ends_with(".rs")) {
        let path = if file.path == "lib.rs" {
            "mod.rs"
        } else {
            &file.path
        };
        std::fs::write(src.join(path), &file.content).unwrap();
    }
    std::fs::copy(fixture.join("peer.rs"), src.join("main.rs")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"wire-check\"\nedition = \"2024\"\nversion = \"0.0.0\"\n\n\
         [workspace]\n\n[dependencies]\n",
    )
    .unwrap();
    let run = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--"])
        .arg(fixture.join("vectors.tsv"))
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "wire vectors failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn literal_choice(name: &str, labels: &[&str]) -> CsilRule {
    alias(
        name,
        CsilTypeExpression::Choice(
            labels
                .iter()
                .map(|l| CsilTypeExpression::Literal(CsilLiteralValue::Text(l.to_string())))
                .collect(),
        ),
    )
}

/// The linkkeys formatter request and long names: no emitted code line passes
/// rustfmt's 100 columns, so rustfmt has no line to wrap. The formatting fixture
/// runs rustfmt itself on every repo spec.
#[test]
fn no_emitted_line_passes_the_rustfmt_width() {
    let long = "ConsentRowPolicyForApplicationsThatRequestLongNames";
    let rules = vec![
        literal_choice(
            "ClaimValueType",
            &[
                "text",
                "url",
                "email",
                "bool",
                "int",
                "float",
                "decimal",
                "date",
                "timestamp",
                "opaque",
            ],
        ),
        literal_choice(long, &["allow-once", "allow-always-for-this-application"]),
        record(
            "Holder",
            vec![
                ("kind", reference("ClaimValueType")),
                (
                    "policy_for_applications_that_request_long_names",
                    reference(long),
                ),
                ("counts_by_name_for_the_account", text_map("text", "int")),
            ],
        ),
        record("ErrorForLongOperations", vec![("message", builtin("text"))]),
        service(
            "ServiceWithALongName",
            vec![
                (
                    "call-something-with-a-long-name",
                    reference("Holder"),
                    CsilTypeExpression::Choice(vec![
                        reference("Holder"),
                        reference("ErrorForLongOperations"),
                    ]),
                ),
                (
                    "read-mixed-output-with-an-inline-map-arm",
                    reference("Holder"),
                    CsilTypeExpression::Choice(vec![
                        text_map("text", "int"),
                        reference("ErrorForLongOperations"),
                    ]),
                ),
            ],
        ),
    ];
    for target in ["rust", "rust-client"] {
        let (_, _, files) = generate(target, rules.clone());
        for file in files.iter().filter(|f| f.path.ends_with(".rs")) {
            for (n, line) in file.content.lines().enumerate() {
                // A comment, a string literal or one token alone on its line is a
                // line rustfmt cannot break either.
                let trimmed = line.trim_start();
                let code = !trimmed.starts_with("//")
                    && !trimmed.starts_with('"')
                    && trimmed.contains(' ');
                assert!(
                    !code || line.len() <= 100,
                    "{target} {}:{} has {} columns:\n{line}",
                    file.path,
                    n + 1,
                    line.len()
                );
            }
        }
    }
}

/// A named union has its own public codec too, so it clashes with a per-op codec
/// of the same name like a record does.
#[test]
fn a_union_named_like_an_emitted_op_codec_is_renamed() {
    let rules = vec![
        alias(
            "LookupFindRequest",
            CsilTypeExpression::Choice(vec![builtin("int"), builtin("text")]),
        ),
        service("Lookup", vec![("find", builtin("text"), builtin("text"))]),
    ];
    let (warnings, all, files) = generate("rust-client", rules);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("rule `LookupFindRequest` is generated as `LookupFindRequestType`")),
        "{warnings:?}"
    );
    assert_eq!(
        all.matches("pub fn encode_lookup_find_request(").count(),
        1,
        "{all}"
    );
    assert!(
        all.contains("pub fn encode_lookup_find_request_type("),
        "{all}"
    );
    assert_builds("union-clash", &files);
}

/// A map key with a longer head than its value needs is refused, in a dynamic map
/// and in a record, and the shortest form of the same key decodes.
#[test]
fn map_keys_must_use_the_shortest_head() {
    let probe = std::process::Command::new("cargo")
        .arg("--version")
        .output();
    if probe.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: no cargo toolchain on PATH");
        return;
    }
    let rules = vec![
        record(
            "Keyed",
            vec![("a", builtin("int")), ("m", text_map("int", "int"))],
        ),
        service("S", vec![("get", reference("Keyed"), reference("Keyed"))]),
    ];
    let (_, _, files) = generate("rust", rules);
    let dir = std::env::temp_dir().join(format!("csilgen-rust-shortest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for file in files.iter().filter(|f| f.path.ends_with(".rs")) {
        let path = if file.path == "lib.rs" {
            "mod.rs"
        } else {
            &file.path
        };
        std::fs::write(src.join(path), &file.content).unwrap();
    }
    std::fs::write(
        src.join("main.rs"),
        r#"#[allow(dead_code, unused_imports)]
#[path = "mod.rs"]
mod api;

fn main() {
    // {"a": 1, "m": {1: 2}} with the shortest heads, then with a long text key,
    // and with a long int key in the dynamic map.
    let good = [0xa2, 0x61, 0x61, 0x01, 0x61, 0x6d, 0xa1, 0x01, 0x02];
    let long_text = [0xa2, 0x78, 0x01, 0x61, 0x01, 0x61, 0x6d, 0xa1, 0x01, 0x02];
    let long_int = [0xa2, 0x61, 0x61, 0x01, 0x61, 0x6d, 0xa1, 0x18, 0x01, 0x02];
    assert!(api::decode_keyed(&good).is_ok());
    for bad in [&long_text[..], &long_int[..]] {
        let error = api::decode_keyed(bad).unwrap_err();
        assert_eq!(error.0, "csil cbor: map key is not in shortest form");
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"shortest-check\"\nedition = \"2024\"\nversion = \"0.0.0\"\n\n\
         [workspace]\n\n[dependencies]\n",
    )
    .unwrap();
    let run = std::process::Command::new("cargo")
        .args(["run", "--quiet"])
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "shortest-form check failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `base` repeated out to `len` characters, for names of a chosen length.
fn name_of(base: &str, len: usize) -> String {
    let words = ["Alpha", "Beta", "Gamma", "Delta", "Echo"];
    let mut out = base.to_string();
    let mut i = 0;
    while out.len() < len {
        out.push_str(words[i % words.len()]);
        i += 1;
    }
    out.truncate(len.max(base.len()));
    out
}

/// The same shapes as the formatting sweep, with every name `len` long: records,
/// enums, a named-number enum, constraints, maps, multi-arm replies, a mixed output
/// and a channel op.
fn sweep_rules(len: usize) -> Vec<CsilRule> {
    let rec = name_of("Rec", len);
    let err = name_of("Err", len);
    let en = name_of("En", len);
    let ne = name_of("Ne", len);
    let id = format!("{rec}Id");
    let field = name_of("f", len).to_lowercase();
    let op = name_of("op", len).to_lowercase();
    let numbered = |label: &str, n: i64| CsilTypeExpression::Constrained {
        base_type: Box::new(CsilTypeExpression::Literal(CsilLiteralValue::Text(
            label.to_string(),
        ))),
        constraints: vec![CsilControlOperator::EnumNumber(n)],
    };
    let constrained = |base: &str, op: CsilControlOperator| CsilTypeExpression::Constrained {
        base_type: Box::new(builtin(base)),
        constraints: vec![op],
    };
    let mut optional = entry(
        &format!("{field}_o"),
        CsilTypeExpression::Array {
            element_type: Box::new(reference(&rec)),
            occurrence: None,
        },
    );
    optional.occurrence = Some(CsilOccurrence::Optional);
    let mut record_rule = record(
        &rec,
        vec![
            (field.as_str(), builtin("text")),
            (&format!("{field}_m"), text_map("text", "int")),
            (&format!("{field}_e"), reference(&en)),
            (&format!("{field}_n"), reference(&ne)),
            (
                &format!("{field}_r"),
                constrained("text", CsilControlOperator::Regex("^a$".into())),
            ),
            (
                &format!("{field}_c"),
                constrained(
                    "int",
                    CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(0)),
                ),
            ),
        ],
    );
    if let CsilRuleType::GroupDef(group) = &mut record_rule.rule_type {
        group.entries.insert(1, optional);
    }
    let mut channel = service(
        &name_of("Svc", len),
        vec![
            (
                op.as_str(),
                reference(&rec),
                CsilTypeExpression::Choice(vec![reference(&rec), reference(&err)]),
            ),
            (
                &format!("{op}-b"),
                reference(&id),
                CsilTypeExpression::Array {
                    element_type: Box::new(reference(&rec)),
                    occurrence: None,
                },
            ),
            (
                &format!("{op}-c"),
                reference(&rec),
                CsilTypeExpression::Choice(vec![text_map("text", "int"), reference(&err)]),
            ),
            (
                &format!("{op}-d"),
                reference(&rec),
                CsilTypeExpression::Choice(vec![reference(&rec), reference(&err), reference(&en)]),
            ),
            (&format!("{op}-e"), reference(&rec), reference(&rec)),
        ],
    );
    if let CsilRuleType::ServiceDef(service) = &mut channel.rule_type {
        service.operations[4].direction = CsilServiceDirection::Bidirectional;
    }
    vec![
        alias(
            &ne,
            CsilTypeExpression::Choice(vec![
                numbered("one", 1),
                numbered(&format!("two-{field}"), 2),
            ]),
        ),
        literal_choice(&en, &["a", &format!("b{field}")]),
        record_rule,
        record(&err, vec![("message", builtin("text"))]),
        alias(&id, builtin("text")),
        channel,
    ]
}

/// Generated Rust passes `rustfmt --check` in the 2021 and 2024 style editions for
/// short names and for names up to 200 characters. Skipped without rustfmt.
#[test]
fn every_name_length_is_rustfmt_clean_in_both_editions() {
    let probe = std::process::Command::new("rustfmt")
        .arg("--version")
        .output();
    if probe.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: no rustfmt on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("csilgen-rust-sweep-{}", std::process::id()));
    // Each length below once found a layout rustfmt changed.
    for len in [
        8, 16, 21, 22, 25, 28, 29, 31, 32, 33, 34, 40, 43, 45, 49, 52, 57, 59, 61, 62, 64, 65, 66,
        67, 68, 69, 70, 73, 76, 79, 80, 82, 83, 85, 87, 91, 95, 100, 110, 120, 150, 175, 200,
    ] {
        for target in ["rust", "rust-client"] {
            let (_, _, files) = generate(target, sweep_rules(len));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let mut paths = Vec::new();
            for file in files.iter().filter(|f| f.path.ends_with(".rs")) {
                let path = dir.join(&file.path);
                std::fs::write(&path, &file.content).unwrap();
                paths.push(path);
            }
            for edition in ["2021", "2024"] {
                let run = std::process::Command::new("rustfmt")
                    .args(["--check", "--edition", edition])
                    .args(&paths)
                    .output()
                    .unwrap();
                assert!(
                    run.status.success(),
                    "{target}, names of {len}, edition {edition}:\n{}",
                    String::from_utf8_lossy(&run.stdout)
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Text enum values that are not identifiers get legal member names, and every
/// value round-trips through the codec as written.
#[test]
fn dotted_enum_values_round_trip() {
    let probe = std::process::Command::new("cargo")
        .arg("--version")
        .output();
    if probe.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: no cargo toolchain on PATH");
        return;
    }
    let values = ["calendars.read", "events.write", "a-b c", "2fa", "class"];
    let rules = vec![
        literal_choice("Scope", &values),
        record("Grant", vec![("scope", reference("Scope"))]),
        service(
            "Auth",
            vec![("grant", reference("Grant"), reference("Grant"))],
        ),
    ];
    let (_, all, files) = generate("rust", rules);
    for member in ["CalendarsRead,", "EventsWrite,", "ABC,", "V2fa,", "Class,"] {
        assert!(all.contains(&format!("    {member}\n")), "{member}\n{all}");
    }
    let dir = std::env::temp_dir().join(format!("csilgen-rust-enum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for file in files.iter().filter(|f| f.path.ends_with(".rs")) {
        let path = if file.path == "lib.rs" {
            "mod.rs"
        } else {
            &file.path
        };
        std::fs::write(src.join(path), &file.content).unwrap();
    }
    std::fs::write(
        src.join("main.rs"),
        r#"#[allow(dead_code, unused_imports)]
#[path = "mod.rs"]
mod api;
use api::Scope;

fn main() {
    let cases = [
        (Scope::CalendarsRead, "calendars.read"),
        (Scope::EventsWrite, "events.write"),
        (Scope::ABC, "a-b c"),
        (Scope::V2fa, "2fa"),
        (Scope::Class, "class"),
    ];
    for (scope, wire) in cases {
        let bytes = api::encode_scope(&scope).unwrap();
        let mut text = vec![0x60 | wire.len() as u8];
        text.extend_from_slice(wire.as_bytes());
        assert_eq!(bytes, text, "{wire}");
        assert_eq!(api::decode_scope(&bytes).unwrap(), scope);
        let grant = api::Grant { scope };
        let again = api::decode_grant(&api::encode_grant(&grant).unwrap()).unwrap();
        assert_eq!(again, grant);
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"enum-check\"\nedition = \"2024\"\nversion = \"0.0.0\"\n\n\
         [workspace]\n\n[dependencies]\n",
    )
    .unwrap();
    let run = std::process::Command::new("cargo")
        .args(["run", "--quiet"])
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "enum round trip failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
