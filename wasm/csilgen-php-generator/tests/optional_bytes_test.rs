//! Optional-`bytes` presence through a real `php`.
//!
//! Per `docs/cbor-wire-contract.md` ("Optional fields"), `? payload: bytes` carries
//! three distinct states and each must survive an encode/decode round trip:
//!
//!   - **absent** — the `payload` key is omitted from the CBOR map entirely;
//!   - **present-and-empty** — the key is present with a zero-length byte string (`0x40`);
//!   - **present-and-non-empty** — the key is present with the bytes.
//!
//! A consumer uses that distinction to mean "leave the stored value alone" (absent)
//! versus "replace it with nothing" (present-empty), so a codec that gates on
//! truthiness rather than presence silently collapses two of the three states.
//!
//! PHP is deliberately covered here rather than in `tests/interop`: the interop matrix
//! has no PHP harness, so this is the only place the emitted PHP codec is *executed*
//! against these three states. Skips cleanly when `php` is not on PATH.

use csilgen_common::*;
use csilgen_php_generator::generate_php_code_from_serialized;
use std::collections::HashMap;

fn config(target: &str) -> GeneratorConfig {
    GeneratorConfig {
        target: target.to_string(),
        output_dir: "/tmp".to_string(),
        options: HashMap::new(),
    }
}

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
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
        metadata: Vec::new(),
        doc_comments: Vec::new(),
    }
}

fn optional_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: Some(CsilOccurrence::Optional),
        metadata: Vec::new(),
        doc_comments: Vec::new(),
    }
}

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn spec(rules: Vec<CsilRule>) -> CsilSpecSerialized {
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    }
}

/// Path to the hand-maintained transport's `CBOR.php`, relative to this crate.
fn transport_cbor_php() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/php/src/CBOR.php")
}

fn have_php() -> bool {
    std::process::Command::new("php")
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn optional_bytes_three_states_round_trip_through_php() {
    if !have_php() {
        eprintln!("skipping optional_bytes_three_states_round_trip_through_php: no php on PATH");
        return;
    }

    let s = spec(vec![group_rule(
        "UpdateRequest",
        vec![
            bare_entry("id", builtin("text")),
            optional_entry("payload", builtin("bytes")),
        ],
    )]);
    let files = generate_php_code_from_serialized(&s, &config("php-typesonly"))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-php-optbytes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::copy(transport_cbor_php(), dir.join("CBOR.php"))
        .expect("copy transport CBOR.php into the driver dir");
    std::fs::write(dir.join("driver.php"), OPT_BYTES_DRIVER_PHP).unwrap();

    let run = std::process::Command::new("php")
        .arg(dir.join("driver.php"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "php optional-bytes round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The driver asserts the exact wire bytes for each state, then asserts the decode
/// puts the value back in the same state. The hex is the whole point: it is what a
/// peer in any other language sees.
const OPT_BYTES_DRIVER_PHP: &str = r#"<?php
require_once __DIR__ . '/CBOR.php';
require_once __DIR__ . '/types.php';
require_once __DIR__ . '/codec.php';

use Csilgen\Generated\Codec;

function assert_eq($got, $want, $label) {
    if ($got !== $want) {
        fwrite(STDERR, "$label: got " . var_export($got, true) . ", want " . var_export($want, true) . "\n");
        exit(1);
    }
}

// 1. Absent: the `payload` key is omitted, so the map has one entry (`id`).
$absent = Codec::encodeUpdateRequest(array('id' => 'x'));
assert_eq(bin2hex($absent), 'a16269646178', 'absent wire form');

// 2. Present and empty: the key is there with a zero-length byte string (0x40).
$empty = Codec::encodeUpdateRequest(array('id' => 'x', 'payload' => ''));
assert_eq(bin2hex($empty), 'a26269646178677061796c6f616440', 'present-empty wire form');

// 3. Present and non-empty: the key carries the bytes verbatim.
$full = Codec::encodeUpdateRequest(array('id' => 'x', 'payload' => "\x01\x02\xf0\xff"));
assert_eq(bin2hex($full), 'a26269646178677061796c6f6164440102f0ff', 'present-non-empty wire form');

// Absent and present-empty must not encode alike -- that is the collapse this guards.
if ($absent === $empty) {
    fwrite(STDERR, "absent and present-empty encoded identically\n");
    exit(1);
}

// Decode must put each value back in the state it was sent in. A present empty byte
// string must NOT come back as absent.
$d = Codec::decodeUpdateRequest($absent);
assert_eq($d->payload, null, 'absent decodes to null');

$d = Codec::decodeUpdateRequest($empty);
assert_eq($d->payload, '', 'present-empty decodes to an empty string, not null');
if ($d->payload === null) {
    fwrite(STDERR, "present-empty decoded to absent\n");
    exit(1);
}

$d = Codec::decodeUpdateRequest($full);
assert_eq(bin2hex($d->payload), '0102f0ff', 'present-non-empty decodes to the bytes');

echo "ok\n";
"#;
