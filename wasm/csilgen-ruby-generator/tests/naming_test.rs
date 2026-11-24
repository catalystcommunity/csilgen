//! Renames: a rule is renamed only when an emitted constant would clash with it,
//! a service clash keeps the declared service name on the wire, and names the
//! shared hoists synthesize go through the same checks. Each case also loads the
//! generated files in Ruby when a `ruby` is on PATH.

mod common;

use common::*;
use csilgen_common::*;
use csilgen_ruby_generator::run_generation;

fn unary(
    name: &str,
    input: CsilTypeExpression,
    output: CsilTypeExpression,
) -> CsilServiceOperation {
    op(name, input, output, CsilServiceDirection::Unidirectional)
}

fn typedef(name: &str, ty: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(ty),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn generate(spec: &CsilSpecSerialized, target: &str) -> WasmGeneratorOutput {
    let input = WasmGeneratorInput {
        csil_spec: spec.clone(),
        config: config(target),
        generator_metadata: csilgen_common::GeneratorMetadata {
            name: "ruby".into(),
            version: "0".into(),
            description: String::new(),
            target: "ruby".into(),
            capabilities: vec![],
            author: None,
            homepage: None,
        },
    };
    run_generation(&input).expect("generation succeeded")
}

fn content<'a>(out: &'a WasmGeneratorOutput, path: &str) -> &'a str {
    &out.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("expected {path}"))
        .content
}

/// Load every generated file in one Ruby process, so a constant clash (a module
/// that reopens a class, a reassigned constant) fails the test.
fn ruby_loads(out: &WasmGeneratorOutput, tag: &str) {
    if std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: no ruby on PATH");
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("csilgen-ruby-naming-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut requires = String::new();
    for f in &out.files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
        if let Some(stem) = f.path.strip_suffix(".rb") {
            requires.push_str(&format!("require_relative {stem:?}\n"));
        }
    }
    requires.push_str("puts \"ok\"\n");
    std::fs::write(dir.join("load.rb"), requires).unwrap();
    let run = std::process::Command::new("ruby")
        .arg("-W")
        .arg(dir.join("load.rb"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && !stderr.contains("warning:"),
        "generated Ruby did not load cleanly:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The first schema of docs/csilgen-requests/rust-false-op-codec-clash-renames.md:
/// an op's own request/response records.
fn rp_spec() -> CsilSpecSerialized {
    spec(vec![
        group_rule(
            "RpIssueAttestationRequest",
            vec![
                bare_entry("claim_type", builtin("text")),
                bare_entry("claim_value", builtin("bytes")),
            ],
        ),
        group_rule(
            "RpIssueAttestationResponse",
            vec![bare_entry("deposited", builtin("bool"))],
        ),
        service_rule(
            "Rp",
            vec![unary(
                "issue-attestation",
                reference("RpIssueAttestationRequest"),
                reference("RpIssueAttestationResponse"),
            )],
            None,
        ),
    ])
}

#[test]
fn op_boundary_records_keep_their_names() {
    for target in ["ruby", "ruby-client"] {
        let out = generate(&rp_spec(), target);
        assert!(out.warnings.is_empty(), "{target}: {:?}", out.warnings);
        let types = content(&out, "types.rb");
        assert_eq!(
            types
                .matches("RpIssueAttestationRequest = Data.define")
                .count(),
            1,
            "{types}"
        );
        assert!(!types.contains("RpIssueAttestationRequestType"));
        ruby_loads(&out, &format!("rp-{target}"));
    }
}

#[test]
fn rule_named_like_a_per_op_codec_is_not_renamed_in_ruby() {
    // The second schema of the request. Ruby emits no per-op constant, so the rule
    // that clashes with Rust's per-op codec clashes with nothing here.
    let s = spec(vec![
        typedef("MemberID", builtin("text")),
        group_rule(
            "MemberGetMemberRequest",
            vec![bare_entry("id", builtin("text"))],
        ),
        service_rule(
            "Member",
            vec![unary(
                "get-member",
                reference("MemberID"),
                reference("MemberID"),
            )],
            None,
        ),
    ]);
    let out = generate(&s, "ruby-client");
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert!(content(&out, "types.rb").contains("MemberGetMemberRequest = Data.define"));
    ruby_loads(&out, "member");
}

#[test]
fn derived_constants_are_reserved_only_for_the_surfaces_emitted() {
    let s = spec(vec![
        group_rule("MemberClient", vec![bare_entry("id", builtin("text"))]),
        group_rule("MemberRouter", vec![bare_entry("id", builtin("text"))]),
        service_rule(
            "Member",
            vec![unary(
                "get",
                reference("MemberClient"),
                reference("MemberRouter"),
            )],
            None,
        ),
    ]);
    // The client file declares `MemberClient`, so the record must move.
    let client = generate(&s, "ruby-client");
    assert_eq!(client.warnings.len(), 1, "{:?}", client.warnings);
    assert!(
        client.warnings[0]
            .message
            .contains("`MemberClient` is generated as `MemberClientType`")
    );
    assert!(content(&client, "types.rb").contains("MemberClientType = Data.define"));
    ruby_loads(&client, "client-surface");
    // No client and no router (the service has no channel op): nothing clashes.
    for target in ["ruby-server", "ruby-typesonly"] {
        let out = generate(&s, target);
        assert!(out.warnings.is_empty(), "{target}: {:?}", out.warnings);
        assert!(content(&out, "types.rb").contains("MemberClient = Data.define"));
        ruby_loads(&out, target);
    }
}

#[test]
fn clashing_service_identifiers_keep_their_wire_names() {
    let s = spec(vec![
        group_rule("Ping", vec![bare_entry("n", builtin("int"))]),
        service_rule(
            "UserSvc",
            vec![unary("ping", reference("Ping"), reference("Ping"))],
            None,
        ),
        service_rule(
            "user-svc",
            vec![unary("ping", reference("Ping"), reference("Ping"))],
            None,
        ),
    ]);
    let out = generate(&s, "ruby-client");
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(
        out.warnings[0]
            .message
            .starts_with("service `user-svc` is generated as")
    );
    let client = content(&out, "client.rb");
    assert!(client.contains("class UserSvcClient\n"), "{client}");
    assert!(client.contains("class UserSvc2Client\n"), "{client}");
    assert!(client.contains("@transport.call(\"UserSvc\", \"ping\", req.to_cbor)"));
    assert!(client.contains("@transport.call(\"user-svc\", \"ping\", req.to_cbor)"));
    ruby_loads(&out, "services");
}

#[test]
fn hoisted_names_are_checked_like_declared_rules() {
    // The hoister names the inline record `Rec_client`, whose constant `RecClient`
    // is the client class of `RecService`. The synthesized rule moves; `Rec` stays.
    let s = spec(vec![
        group_rule(
            "Rec",
            vec![bare_entry(
                "client",
                CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![bare_entry("a", builtin("int"))],
                }),
            )],
        ),
        service_rule(
            "RecService",
            vec![unary("put", reference("Rec"), reference("Rec"))],
            None,
        ),
    ]);
    let out = generate(&s, "ruby-client");
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(
        out.warnings[0]
            .message
            .contains("`Rec_client` is generated as `Rec_clientType`")
    );
    let types = content(&out, "types.rb");
    assert!(types.contains("Rec = Data.define(:client)"));
    assert!(types.contains("RecClientType = Data.define(:a)"));
    assert!(content(&out, "client.rb").contains("class RecClient\n"));
    ruby_loads(&out, "hoisted");
}

#[test]
fn mixed_op_output_is_one_named_union() {
    let s = spec(vec![
        group_rule("Holder", vec![bare_entry("n", builtin("int"))]),
        group_rule("Err", vec![bare_entry("message", builtin("text"))]),
        service_rule(
            "Lookup",
            vec![unary(
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
            None,
        ),
    ]);
    let out = generate(&s, "ruby");
    let codec = content(&out, "codec.rb");
    assert!(codec.contains("def enc_union_LookupReadCountsOutput(value)"));
    assert!(codec.contains("    when Err\n      [1, (value).csil_to_tree]"));
    let client = generate(&s, "ruby-client");
    assert!(content(&client, "client.rb").contains(
        "CsilCbor.dec_union_LookupReadCountsOutput(CsilCbor.decode(@transport.call(\"Lookup\", \"read-counts\", req.to_cbor)))"
    ));
    ruby_loads(&client, "op-union");
}
