use std::{fs, path::Path, process::Command};

fn check(command: &str, input: &Path, extra: &[&str], status: i32) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_csilgen"))
        .args([command, "--input"])
        .arg(input)
        .args(extra)
        .output()
        .unwrap();
    let messages = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(status), "{messages}");
    messages
}

#[test]
fn validate_rejects_a_misspelling_and_accepts_a_declared_name() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("api.csil");
    fs::write(&input, "user = tex").unwrap();
    for command in ["validate", "linted_validate"] {
        let message = check(command, &input, &[], 1);
        assert!(
            message.contains("Unknown type 'tex' in user. Did you mean 'text'?"),
            "{message}"
        );
    }
    fs::write(&input, "user = tex\ntex = text").unwrap();
    check("validate", &input, &[], 0);
}

#[test]
fn linted_validate_checks_reachable_source_files_with_their_own_options() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("api.csil");
    let common = dir.path().join("common.csil");
    fs::write(dir.path().join("unrelated.csil"), "bad = typo").unwrap();
    fs::write(&input, "include \"common.csil\" as shared\noptions { lint_text_enums: \"off\" }\nUser = shared.Record").unwrap();
    for (policy, exit) in [("error", 1), ("warn", 0), ("off", 0)] {
        fs::write(&common, format!("options {{ lint_text_enums: \"{policy}\" }}\nRecord = {{ current: Status }}\nStatus = \"busy\" / \"denied\"")).unwrap();
        check("validate", &input, &[], 0);
        let output = check("linted-validate", &input, &[], exit);
        assert_eq!(
            output.contains("CBOR transmits the full label string."),
            policy != "off"
        );
        if policy == "warn" {
            check("linted_validate", &input, &["--fail-on-warnings"], 1);
        }
    }
}

#[test]
fn naming_policies_are_independent_and_local_to_the_source_file() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("api.csil");
    let shared = dir.path().join("shared.csil");
    fs::write(&entry, "include \"shared.csil\"\noptions { lint_rule_names: \"off\" }\nservice Api { hello: bad_name -> Root }\nRoot = text").unwrap();
    let declarations = "bad_name = { userName: text }\nservice Other { getUser: bad_name -> text }";
    fs::write(&shared, declarations).unwrap();
    let output = check("linted_validate", &entry, &["--fail-on-warnings"], 1);
    for diagnostic in [
        "PascalCase",
        "Field 'userName'",
        "Operation 'getUser'",
        "3 warning(s)",
    ] {
        assert!(output.contains(diagnostic), "{output}");
    }
    fs::write(&shared, format!("options {{ lint_rule_names: \"off\", lint_field_names: \"off\", lint_operation_names: \"error\" }}\n{declarations}")).unwrap();
    let output = check("linted_validate", &entry, &[], 1);
    assert!(output.contains("Operation 'getUser'"), "{output}");
    assert!(output.contains("1 error(s), 0 warning(s)"), "{output}");
}
