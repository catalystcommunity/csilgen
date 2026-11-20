use std::{fs, process::Command};

#[test]
fn fail_on_warnings_uses_the_reported_severity() {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("api.csil");
    let quoted = r#"status = "busy" / "denied""#;
    let off = format!("options {{ lint_text_enums: \"off\" }}\n{quoted}");
    let error = format!("options {{ lint_text_enums: \"error\" }}\n{quoted}");
    for (source, strict, exit, diagnostic) in [
        (quoted, false, 0, "CBOR transmits the full label string."),
        (quoted, true, 1, "CBOR transmits the full label string."),
        ("BadName = int", true, 1, "snake_case"),
        (off.as_str(), true, 0, "0 warning(s)"),
        ("empty = {}", true, 0, "0 warning(s)"),
        (error.as_str(), false, 1, "1 error(s)"),
    ] {
        fs::write(&schema, source).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_csilgen"));
        command.arg("lint").arg(directory.path());
        if strict {
            command.arg("--fail-on-warnings");
        }
        let output = command.output().unwrap();
        let messages = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(exit), "{messages}");
        assert!(messages.contains(diagnostic), "{messages}");
    }
}
