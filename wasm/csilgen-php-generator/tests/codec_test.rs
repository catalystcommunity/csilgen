//! Round-trip the generated PHP codec through a real `php`. The generator emits
//! `types.php` (the value classes) and `codec.php` (a self-contained CBOR codec
//! keyed off `Csilgen\Transport\CBOR`, the hand-maintained transport library's
//! canonical-CBOR value tree); this test writes both alongside a copy of the
//! transport's `CBOR.php` and runs a driver script through the real interpreter to
//! prove the wire form round-trips. Skips cleanly when `php` is not on PATH (it is
//! not a `catalyst-tools`-managed binary -- see docs on `~/.config/catalyst-tools`)
//! so the suite stays portable.

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

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

fn text_lit(value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(value.to_string()))
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

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn type_def_rule(name: &str, ty: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(ty),
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

/// Torture spec: a named all-literal enum (`Grade`) and a named mixed choice
/// (`Level`) whose *last* arm carries a trailing `.default` control operator -- the
/// parser attaches it to that one arm (`Constrained { base_type: Literal, .. }`),
/// which used to fall out of literal classification entirely -- plus inline
/// (anonymous) choice fields at the record's own field position, as an array
/// element, as a map value, and as a tuple element, mirroring `APIError.error_type`
/// in examples/real-world-api/e-commerce-api.csil.
fn torture_spec() -> CsilSpecSerialized {
    let default_high = |lit: &str| CsilTypeExpression::Constrained {
        base_type: Box::new(CsilTypeExpression::Literal(CsilLiteralValue::Text(
            lit.to_string(),
        ))),
        constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
            "normal".to_string(),
        ))],
    };

    let grade = type_def_rule(
        "Grade",
        CsilTypeExpression::Choice(vec![text_lit("low"), default_high("high")]),
    );
    let level = type_def_rule(
        "Level",
        CsilTypeExpression::Choice(vec![builtin("text"), text_lit("low"), default_high("high")]),
    );
    let inline_status = CsilTypeExpression::Choice(vec![
        builtin("text"),
        text_lit("queued"),
        text_lit("shipped"),
    ]);
    let inline_priority =
        CsilTypeExpression::Choice(vec![text_lit("low"), text_lit("medium"), text_lit("high")]);
    let inline_color =
        CsilTypeExpression::Choice(vec![text_lit("red"), text_lit("green"), text_lit("blue")]);
    let inline_flag = CsilTypeExpression::Choice(vec![text_lit("on"), text_lit("off")]);
    let inline_yesno = CsilTypeExpression::Choice(vec![text_lit("yes"), text_lit("no")]);

    let torture = group_rule(
        "Torture",
        vec![
            bare_entry("status", inline_status),
            bare_entry("priority", inline_priority),
            bare_entry(
                "colors",
                CsilTypeExpression::Array {
                    element_type: Box::new(inline_color),
                    occurrence: None,
                },
            ),
            bare_entry(
                "flags",
                CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(inline_flag),
                    occurrence: None,
                },
            ),
            bare_entry(
                "pair",
                CsilTypeExpression::Tuple(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: None,
                            value_type: builtin("text"),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: vec![],
                        },
                        CsilGroupEntry {
                            key: None,
                            value_type: inline_yesno,
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: vec![],
                        },
                    ],
                }),
            ),
            bare_entry("grade", reference("Grade")),
            bare_entry("level", reference("Level")),
        ],
    );
    spec(vec![grade, level, torture])
}

#[test]
fn constrained_arm_and_inline_choice_round_trip_through_php() {
    if !have_php() {
        eprintln!("skipping: no php on PATH");
        return;
    }

    let s = torture_spec();
    let files = generate_php_code_from_serialized(&s, &config("php-typesonly"))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-php-torture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::copy(transport_cbor_php(), dir.join("CBOR.php"))
        .expect("copy transport CBOR.php into the driver dir");
    std::fs::write(dir.join("driver.php"), TORTURE_DRIVER_PHP).unwrap();

    let run = std::process::Command::new("php")
        .arg("-l")
        .arg(dir.join("types.php"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "php -l types.php failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let run = std::process::Command::new("php")
        .arg("-l")
        .arg(dir.join("codec.php"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "php -l codec.php failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let run = std::process::Command::new("php")
        .arg(dir.join("driver.php"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "php torture round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const TORTURE_DRIVER_PHP: &str = r#"<?php
require_once __DIR__ . '/CBOR.php';
require_once __DIR__ . '/types.php';
require_once __DIR__ . '/codec.php';

use Csilgen\Generated\Codec;
use Csilgen\Generated\CodecException;

function assert_eq($got, $want, $label) {
    if ($got !== $want) {
        fwrite(STDERR, "$label: got " . var_export($got, true) . ", want " . var_export($want, true) . "\n");
        exit(1);
    }
}

$sample = array(
    'status' => 'queued',
    'priority' => 'medium',
    'colors' => array('red', 'blue'),
    'flags' => array('a' => 'on', 'b' => 'off'),
    'pair' => array('hi', 'yes'),
    'grade' => 'low',
    'level' => 'low',
);

// Grade is a closed literal enum (with a `.default`-suffixed last arm): bare-text
// wire, not a tagged-sum array.
$tree = Codec::toCborTorture($sample);
assert_eq($tree['grade'], 'low', 'grade-wire');

$back = Codec::fromCborTorture($tree);
assert_eq($back->grade, 'low', 'grade-round-trip');
assert_eq($back->status, 'queued', 'status-round-trip');
assert_eq($back->priority, 'medium', 'priority-round-trip');
assert_eq($back->colors, array('red', 'blue'), 'colors-round-trip');
assert_eq($back->flags, array('a' => 'on', 'b' => 'off'), 'flags-round-trip');
assert_eq($back->pair, array('hi', 'yes'), 'pair-round-trip');
assert_eq($back->level, 'low', 'level-round-trip');

// Level's constrained last arm ("high" .default "normal") keeps its own declared
// index (2) with literal-equality validation, and the general `text` arm (index 0)
// stays reachable for values that aren't "low"/"high".
$wantIdx = array('low' => 1, 'high' => 2, 'other' => 0);
foreach ($wantIdx as $level => $idx) {
    $v = $sample;
    $v['level'] = $level;
    $tree = Codec::toCborTorture($v);
    assert_eq($tree['level'], array($idx, $level), "level-idx-$level");
    $back = Codec::fromCborTorture($tree);
    assert_eq($back->level, $level, "level-round-trip-$level");
}

// Inline mixed choice field (status): literal arms win their own declared index
// over the general `text` arm.
$tree = Codec::toCborTorture($sample);
assert_eq($tree['status'], array(1, 'queued'), 'status-wire');
$other = $sample;
$other['status'] = 'backordered';
$otherTree = Codec::toCborTorture($other);
assert_eq($otherTree['status'], array(0, 'backordered'), 'status-fallback');

// Decode rejects a wrong-literal payload at a literal's declared index (Level).
$bad = array(
    'status' => array(1, 'queued'),
    'priority' => 'low',
    'colors' => array(),
    'flags' => array(),
    'pair' => array('hi', 'yes'),
    'grade' => 'low',
    'level' => array(2, 'not-high'),
);
$raised = false;
try {
    Codec::fromCborTorture($bad);
} catch (CodecException $e) {
    $raised = true;
}
if (!$raised) {
    fwrite(STDERR, "expected CodecException on bad level literal\n");
    exit(1);
}

// Grade decode validates enum membership: an unknown literal must raise.
$badGrade = $bad;
$badGrade['grade'] = 'unknown';
$badGrade['level'] = array(1, 'low');
$raisedGrade = false;
try {
    Codec::fromCborTorture($badGrade);
} catch (CodecException $e) {
    $raisedGrade = true;
}
if (!$raisedGrade) {
    fwrite(STDERR, "expected CodecException on bad grade literal\n");
    exit(1);
}

// Inline all-literal choices reject an unknown literal on decode too.
$badPriority = $bad;
$badPriority['grade'] = 'low';
$badPriority['priority'] = 'unknown';
$badPriority['level'] = array(1, 'low');
$raisedPriority = false;
try {
    Codec::fromCborTorture($badPriority);
} catch (CodecException $e) {
    $raisedPriority = true;
}
if (!$raisedPriority) {
    fwrite(STDERR, "expected CodecException on bad priority literal\n");
    exit(1);
}

// Raw-bytes round trip.
$bytes = Codec::encodeTorture($sample);
$back2 = Codec::decodeTorture($bytes);
assert_eq($back2->grade, 'low', 'byte-round-trip-grade');
assert_eq($back2->level, 'low', 'byte-round-trip-level');
assert_eq($back2->status, 'queued', 'byte-round-trip-status');

echo "ok\n";
"#;
