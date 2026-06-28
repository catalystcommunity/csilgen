//! End-to-end surface/dispatch tests for the Ruby generator.

mod common;

use common::*;
use csilgen_common::*;

#[test]
fn frozen_header_on_every_file() {
    let s = spec(vec![
        group_rule("user", vec![bare_entry("name", builtin("text"))]),
        service_rule(
            "user_service",
            vec![op(
                "get-user",
                reference("user"),
                reference("user"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]);
    for target in ["ruby", "ruby-client"] {
        let files =
            generate_ruby_code_from_serialized(&s, &config(target)).expect("generation succeeded");
        assert!(!files.is_empty());
        for f in &files {
            assert!(
                f.content.starts_with("# frozen_string_literal: true\n"),
                "{} in target {target} must start with the frozen header",
                f.path
            );
        }
    }
}

#[test]
fn server_target_emits_types_and_server() {
    let s = spec(vec![
        group_rule("user", vec![bare_entry("name", builtin("text"))]),
        service_rule(
            "user_service",
            vec![op(
                "get-user",
                reference("user"),
                reference("user"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]);
    let p = paths(&s, "ruby");
    assert!(p.contains(&"types.rb".to_string()));
    assert!(p.contains(&"server.rb".to_string()));
    assert!(!p.contains(&"client.rb".to_string()));
}

#[test]
fn client_target_emits_types_and_client() {
    let s = spec(vec![
        group_rule("user", vec![bare_entry("name", builtin("text"))]),
        service_rule(
            "user_service",
            vec![op(
                "get-user",
                reference("user"),
                reference("user"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]);
    let p = paths(&s, "ruby-client");
    assert!(p.contains(&"types.rb".to_string()));
    assert!(p.contains(&"client.rb".to_string()));
    assert!(!p.contains(&"server.rb".to_string()));
}

#[test]
fn typesonly_target_emits_only_types() {
    let s = spec(vec![
        group_rule("user", vec![bare_entry("name", builtin("text"))]),
        service_rule(
            "user_service",
            vec![op(
                "get-user",
                reference("user"),
                reference("user"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]);
    let p = paths(&s, "ruby-typesonly");
    // The per-type CBOR codec rides alongside the types so a typesonly consumer gets
    // wire-ready value classes.
    assert_eq!(p, vec!["types.rb".to_string(), "codec.rb".to_string()]);
}

#[test]
fn ruby_server_alias_matches_ruby() {
    let s = spec(vec![service_rule(
        "user_service",
        vec![op(
            "get-user",
            reference("user"),
            reference("user"),
            CsilServiceDirection::Unidirectional,
        )],
        None,
    )]);
    assert_eq!(paths(&s, "ruby"), paths(&s, "ruby-server"));
}

#[test]
fn unknown_subtarget_is_an_error() {
    let s = spec(vec![group_rule(
        "user",
        vec![bare_entry("name", builtin("text"))],
    )]);
    assert!(generate_ruby_code_from_serialized(&s, &config("ruby-bogus")).is_err());
}
