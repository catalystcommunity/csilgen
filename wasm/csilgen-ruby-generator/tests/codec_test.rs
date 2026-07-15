//! Round-trip the generated Ruby codec + typed client through a real `ruby`. The
//! generator emits `codec.rb` (a self-contained canonical-CBOR codec with per-class
//! `to_cbor`/`from_cbor`) and `client.rb` (typed methods over a dumb byte transport
//! seam); this test compiles nothing but runs the emitted Ruby to prove the wire form
//! round-trips. Skips cleanly when `ruby` is not on PATH so the suite stays portable.

mod common;

use common::*;
use csilgen_common::*;

/// A corndogs-shaped spec: Task (text uuid, text current_state, bytes payload, an
/// optional int priority, a map<text,int>, a list<text>), a SubmitTaskRequest holding
/// a nested Task plus a queue, a ServiceError, and a `submit-task` op whose output is
/// the `Task / ServiceError` union.
fn corndogs_spec() -> CsilSpecSerialized {
    let task = group_rule(
        "Task",
        vec![
            bare_entry("uuid", builtin("text")),
            bare_entry("current_state", builtin("text")),
            bare_entry("payload", builtin("bytes")),
            optional_entry("priority", builtin("int")),
            bare_entry(
                "labels",
                CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(builtin("int")),
                    occurrence: None,
                },
            ),
            bare_entry(
                "tags",
                CsilTypeExpression::Array {
                    element_type: Box::new(builtin("text")),
                    occurrence: None,
                },
            ),
            // A field typed as a named map alias (transparent `TypeDef` carrying a Map):
            // the regression stubbed these to nil, dropping their entries.
            bare_entry("counts", reference("StringInt64Map")),
            // A map-of-record alias: values must recurse into the ServiceError codec.
            bare_entry("errors", reference("ErrorMap")),
        ],
    );
    // Transparent aliases: a `Map` `TypeDef` has no codec of its own; the codec must
    // resolve a field referencing one to the underlying map and round-trip its entries.
    let string_int_map = type_def_rule(
        "StringInt64Map",
        CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(builtin("int")),
            occurrence: None,
        },
    );
    let error_map = type_def_rule(
        "ErrorMap",
        CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(reference("ServiceError")),
            occurrence: None,
        },
    );
    let req = group_rule(
        "SubmitTaskRequest",
        vec![
            bare_entry("task", reference("Task")),
            bare_entry("queue", builtin("text")),
        ],
    );
    let err = group_rule(
        "ServiceError",
        vec![
            bare_entry("code", builtin("int")),
            bare_entry("message", builtin("text")),
        ],
    );
    let svc = service_rule(
        "CorndogsService",
        vec![op(
            "submit-task",
            reference("SubmitTaskRequest"),
            CsilTypeExpression::Choice(vec![reference("Task"), reference("ServiceError")]),
            CsilServiceDirection::Unidirectional,
        )],
        None,
    );
    spec(vec![task, req, err, string_int_map, error_map, svc])
}

/// A transparent type-alias rule: `<name> = <ty>` carrying a non-group/non-choice type.
fn type_def_rule(name: &str, ty: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(ty),
        position: CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        },
        doc_comments: Vec::new(),
    }
}

#[test]
fn codec_round_trips_through_ruby() {
    let have = std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no ruby on PATH");
        return;
    }

    let s = corndogs_spec();
    let files = generate_ruby_code_from_serialized(&s, &config("ruby-client"))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-ruby-codec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.rb"), CODEC_DRIVER_RUBY).unwrap();

    let run = std::process::Command::new("ruby")
        .arg(dir.join("driver.rb"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ruby round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `OrderStatus = text / "pending" / "confirmed" / "processing" / "shipped" /
/// "delivered" / "cancelled" / "refunded"` (examples/real-world-api/e-commerce-api.csil
/// line 138), held by a single-field `Order` record: the mixed-union shape whose
/// literal arms used to be shadowed by the general `text` arm's catch-all `String`
/// guard (and whose `dec_union_OrderStatus` never validated a literal payload).
fn mixed_union_spec() -> CsilSpecSerialized {
    let order_status = type_def_rule(
        "OrderStatus",
        CsilTypeExpression::Choice(vec![
            builtin("text"),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("pending".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("confirmed".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("processing".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("shipped".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("delivered".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("cancelled".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("refunded".to_string())),
        ]),
    );
    let order = group_rule(
        "Order",
        vec![bare_entry("status", reference("OrderStatus"))],
    );
    spec(vec![order_status, order])
}

#[test]
fn mixed_union_round_trips_through_ruby() {
    let have = std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no ruby on PATH");
        return;
    }

    let s = mixed_union_spec();
    let files = generate_ruby_code_from_serialized(&s, &config("ruby-typesonly"))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-ruby-union-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.rb"), MIXED_UNION_DRIVER_RUBY).unwrap();

    let run = std::process::Command::new("ruby")
        .arg(dir.join("driver.rb"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ruby mixed-union round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Drives `CsilCbor.enc_union_OrderStatus`/`dec_union_OrderStatus` directly, plus the
/// `Order` record's byte round-trip, to prove: each literal wins its own declared
/// index over the general `text` arm, an unmatched string falls back to the general
/// arm's index 0, all 8 declared indices decode, and a literal-index payload that
/// does not equal the declared literal raises rather than being trusted verbatim.
const MIXED_UNION_DRIVER_RUBY: &str = r#"require_relative "codec"

# Declaration order: text=0, pending=1, confirmed=2, processing=3, shipped=4,
# delivered=5, cancelled=6, refunded=7.
literals = {
  "pending" => 1,
  "confirmed" => 2,
  "processing" => 3,
  "shipped" => 4,
  "delivered" => 5,
  "cancelled" => 6,
  "refunded" => 7
}

literals.each do |status, idx|
  raise "encode-#{status}" unless CsilCbor.enc_union_OrderStatus(status) == [idx, status]
  raise "decode-#{status}" unless CsilCbor.dec_union_OrderStatus([idx, status]) == status
end

# encode("pending") -> [1, "pending"]
raise "encode-pending" unless CsilCbor.enc_union_OrderStatus("pending") == [1, "pending"]

# encode("on-hold") -> [0, "on-hold"] (no matching literal falls back to the general
# `text` arm at its own declared index 0).
raise "encode-fallback" unless CsilCbor.enc_union_OrderStatus("on-hold") == [0, "on-hold"]

# All 8 declared indices decode.
raise "decode-general" unless CsilCbor.dec_union_OrderStatus([0, "on-hold"]) == "on-hold"

# decode([1, "confirmed"]) errors: index 1 is declared "pending", so a payload of
# "confirmed" at that index must be rejected rather than silently accepted.
begin
  CsilCbor.dec_union_OrderStatus([1, "confirmed"])
  raise "expected ArgumentError for mismatched literal payload"
rescue ArgumentError
  # expected
end

# Full record round-trip through real CBOR bytes for a literal and a fallback value.
order = Order.new(status: "pending")
back = Order.from_cbor(order.to_cbor)
raise "record-literal" unless back.status == "pending"

order2 = Order.new(status: "on-hold")
back2 = Order.from_cbor(order2.to_cbor)
raise "record-fallback" unless back2.status == "on-hold"

puts "ok"
"#;

/// Torture spec: a named all-literal enum (`Grade`) and a named mixed choice
/// (`Level`) whose *last* arm carries a trailing `.default` control operator --
/// the parser attaches it to that one arm (`Constrained { base_type: Literal, .. }`),
/// which used to fall out of literal classification entirely -- plus inline
/// (anonymous) choice fields at the record's own field position, as an array
/// element, as a map value, and as a tuple element, mirroring
/// `APIError.error_type` in examples/real-world-api/e-commerce-api.csil.
fn torture_spec() -> CsilSpecSerialized {
    let default_high = |lit: &str| CsilTypeExpression::Constrained {
        base_type: Box::new(CsilTypeExpression::Literal(CsilLiteralValue::Text(
            lit.to_string(),
        ))),
        constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
            "normal".to_string(),
        ))],
    };
    let lit = |s: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()));

    let grade = type_def_rule(
        "Grade",
        CsilTypeExpression::Choice(vec![lit("low"), default_high("high")]),
    );
    let level = type_def_rule(
        "Level",
        CsilTypeExpression::Choice(vec![builtin("text"), lit("low"), default_high("high")]),
    );
    let inline_status =
        CsilTypeExpression::Choice(vec![builtin("text"), lit("queued"), lit("shipped")]);
    let inline_priority = CsilTypeExpression::Choice(vec![lit("low"), lit("medium"), lit("high")]);
    let inline_color = CsilTypeExpression::Choice(vec![lit("red"), lit("green"), lit("blue")]);
    let inline_flag = CsilTypeExpression::Choice(vec![lit("on"), lit("off")]);
    let inline_yesno = CsilTypeExpression::Choice(vec![lit("yes"), lit("no")]);

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
fn constrained_arm_and_inline_choice_round_trip_through_ruby() {
    let have = std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no ruby on PATH");
        return;
    }

    let s = torture_spec();
    let files = generate_ruby_code_from_serialized(&s, &config("ruby-typesonly"))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-ruby-torture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.rb"), TORTURE_DRIVER_RUBY).unwrap();

    let run = std::process::Command::new("ruby")
        .arg("-W")
        .arg(dir.join("driver.rb"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ruby torture round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        !stderr.contains("warning:"),
        "ruby -W emitted a warning:\n{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const TORTURE_DRIVER_RUBY: &str = r#"require_relative "codec"

sample = Torture.new(
  status: "queued",
  priority: "medium",
  colors: ["red", "blue"],
  flags: { "a" => "on", "b" => "off" },
  pair: ["hi", "yes"],
  grade: "low",
  level: "low"
)

# Grade is a closed literal enum (with a `.default`-suffixed last arm): bare-text
# wire, not a tagged-sum array.
raise "grade-wire" unless sample.csil_to_tree["grade"] == "low"

back = Torture.csil_from_tree(sample.csil_to_tree)
raise "round-trip" unless back == sample

# Level's constrained last arm ("high" .default "normal") keeps its own declared
# index (2) with literal-equality validation, and the general `text` arm (index 0)
# stays reachable for values that aren't "low"/"high".
{ "low" => 1, "high" => 2, "other" => 0 }.each do |level, want_idx|
  v = Torture.new(**sample.to_h.merge(level: level))
  tree = v.csil_to_tree
  raise "level-idx-#{level}" unless tree["level"] == [want_idx, level]
  raise "level-round-trip-#{level}" unless Torture.csil_from_tree(tree).level == level
end

# Inline all-literal choices (priority / array element / map value / tuple
# element) ride the wire as the bare literal, same as a named enum.
tree = sample.csil_to_tree
raise "priority-wire" unless tree["priority"] == "medium"
raise "colors-wire" unless tree["colors"] == ["red", "blue"]
raise "flags-wire" unless tree["flags"] == { "a" => "on", "b" => "off" }
raise "pair-wire" unless tree["pair"] == ["hi", "yes"]

# Inline mixed choice field (status): literal arms win their own declared index
# over the general `text` arm.
raise "status-wire" unless tree["status"] == [1, "queued"]
other = Torture.new(**sample.to_h.merge(status: "backordered"))
raise "status-fallback" unless other.csil_to_tree["status"] == [0, "backordered"]

# Decode rejects a wrong-literal payload at a literal's declared index (Level).
bad = {
  "status" => [1, "queued"],
  "priority" => "low",
  "colors" => [],
  "flags" => {},
  "pair" => ["hi", "yes"],
  "grade" => "low",
  "level" => [2, "not-high"]
}
raised = begin
  Torture.csil_from_tree(bad)
  false
rescue ArgumentError
  true
end
raise "expected raise on bad level literal" unless raised

# Grade decode validates enum membership: an unknown literal must raise.
bad_grade = bad.merge("grade" => "unknown", "level" => [1, "low"])
raised_grade = begin
  Torture.csil_from_tree(bad_grade)
  false
rescue ArgumentError
  true
end
raise "expected raise on bad grade literal" unless raised_grade

# Inline all-literal choices reject an unknown literal on decode too.
bad_priority = bad.merge("grade" => "low", "priority" => "unknown", "level" => [1, "low"])
raised_priority = begin
  Torture.csil_from_tree(bad_priority)
  false
rescue ArgumentError
  true
end
raise "expected raise on bad priority literal" unless raised_priority

# Raw-bytes round trip.
back2 = Torture.from_cbor(sample.to_cbor)
raise "byte-round-trip" unless back2 == sample

puts "ok"
"#;

/// A named mixed-KIND literal choice (`Priority = "low" / 5 / "high"`): a text
/// literal and an integer literal in the same choice. Per the shared
/// `csilgen_common::classify_choice` contract, EVERY arm being a literal -- of any
/// kind, or a mix of kinds -- classifies as an `Enum` (bare wire value, membership
/// decode), never a `Union` (tagged-sum `[index, value]`). Pins the shared-machinery
/// migration: this crate's own `is_enum_choice`/`enum_literal_kind` never actually
/// required kind uniformity, but routing through `csilgen_common::all_literal` must
/// not regress that.
fn mixed_kind_enum_spec() -> CsilSpecSerialized {
    let priority = type_def_rule(
        "Priority",
        CsilTypeExpression::Choice(vec![
            CsilTypeExpression::Literal(CsilLiteralValue::Text("low".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Integer(5)),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("high".to_string())),
        ]),
    );
    let item = group_rule(
        "Item",
        vec![
            bare_entry("id", builtin("uint")),
            bare_entry("priority", reference("Priority")),
        ],
    );
    spec(vec![priority, item])
}

#[test]
fn mixed_kind_literal_choice_round_trips_through_ruby() {
    let have = std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no ruby on PATH");
        return;
    }

    let s = mixed_kind_enum_spec();
    let files = generate_ruby_code_from_serialized(&s, &config("ruby-typesonly"))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-ruby-mixed-kind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.rb"), MIXED_KIND_ENUM_DRIVER_RUBY).unwrap();

    let run = std::process::Command::new("ruby")
        .arg(dir.join("driver.rb"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ruby mixed-kind enum round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_KIND_ENUM_DRIVER_RUBY: &str = r#"require_relative "codec"

# Every declared literal of BOTH kinds (text and integer) rides the wire bare, proving
# the choice classified as an Enum, not a [index, value] tagged-sum Union.
[["low", "low"], [5, 5], ["high", "high"]].each do |value, want|
  item = Item.new(id: 1, priority: value)
  tree = item.csil_to_tree
  raise "wire-#{value}" unless tree["priority"] == want
  back = Item.csil_from_tree(tree)
  raise "roundtrip-#{value}" unless back.priority == want
end

# An out-of-vocabulary TEXT value (right runtime type, wrong value) must be rejected
# by decode -- a membership check, not a bare String/Integer type check that would let
# any string through.
bad_text = { "id" => 1, "priority" => "medium" }
raised_text = begin
  Item.csil_from_tree(bad_text)
  false
rescue ArgumentError
  true
end
raise "expected raise on out-of-vocabulary text" unless raised_text

# Likewise for an out-of-vocabulary INTEGER value.
bad_int = { "id" => 1, "priority" => 6 }
raised_int = begin
  Item.csil_from_tree(bad_int)
  false
rescue ArgumentError
  true
end
raise "expected raise on out-of-vocabulary integer" unless raised_int

puts "ok"
"#;

/// `Payload = text / "empty" / bytes`: a `.default`-free union where the literal
/// "empty" (guard `String`) shares its Ruby dispatch guard with TWO general arms --
/// `text` (index 0) and `bytes` (index 2), both of which also guard `String`. Encode
/// must fall back to the FIRST declared general arm (`text`, index 0) for a
/// non-"empty" string, not the last (`bytes`, index 2) -- `union_encode_body` used to
/// let the last non-literal arm in a shared-guard group overwrite `general_idx` on
/// every iteration, so this used to fall back to `bytes` instead. The two general
/// arms are wire-distinguishable: `bytes` forces the CBOR binary major type (2),
/// `text` the UTF-8 major type (3), so a plain string round-tripping with the wrong
/// major type is directly observable as a Ruby String encoding mismatch after decode.
fn shared_guard_two_general_arms_spec() -> CsilSpecSerialized {
    let payload = type_def_rule(
        "Payload",
        CsilTypeExpression::Choice(vec![
            builtin("text"),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("empty".to_string())),
            builtin("bytes"),
        ]),
    );
    let holder = group_rule(
        "PayloadHolder",
        vec![bare_entry("payload", reference("Payload"))],
    );
    spec(vec![payload, holder])
}

#[test]
fn shared_guard_first_general_arm_wins_through_ruby() {
    let have = std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no ruby on PATH");
        return;
    }

    let s = shared_guard_two_general_arms_spec();
    let files = generate_ruby_code_from_serialized(&s, &config("ruby-typesonly"))
        .expect("generation succeeded");

    let dir =
        std::env::temp_dir().join(format!("csilgen-ruby-shared-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.rb"), SHARED_GUARD_DRIVER_RUBY).unwrap();

    let run = std::process::Command::new("ruby")
        .arg(dir.join("driver.rb"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ruby shared-guard round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const SHARED_GUARD_DRIVER_RUBY: &str = r#"require_relative "codec"

# The literal "empty" keeps its own declared index (1) regardless of guard sharing.
raise "literal-index" unless CsilCbor.enc_union_Payload("empty") == [1, "empty"]

# A plain non-"empty" string must dispatch to the FIRST declared general arm sharing
# the guard (index 0, `text`), not the LAST (index 2, `bytes`).
enc = CsilCbor.enc_union_Payload("hello")
raise "encode-index" unless enc[0] == 0
raise "encode-value" unless enc[1] == "hello"
raise "encode-major-type" unless enc[1].encoding == Encoding::UTF_8

# The wire bytes themselves carry the `text` major type (3), not `bytes` (2) -- the
# observable proof that the FIRST arm, not the last, won the runtime-type dispatch.
wire = CsilCbor.encode(enc)
decoded = CsilCbor.decode(wire)
raise "wire-index" unless decoded[0] == 0
raise "wire-value" unless decoded[1] == "hello"
raise "wire-encoding" unless decoded[1].encoding == Encoding::UTF_8

back = CsilCbor.dec_union_Payload(decoded)
raise "decode-value" unless back == "hello"

# Full record round-trip: a plain string field survives as UTF-8 text, not binary.
holder = PayloadHolder.new(payload: "world")
restored = PayloadHolder.from_cbor(holder.to_cbor)
raise "record-value" unless restored.payload == "world"
raise "record-encoding" unless restored.payload.encoding == Encoding::UTF_8

puts "ok"
"#;

/// Drives the generated codec + client. `require_relative "client"` pulls in
/// `codec.rb` (which pulls in `types.rb`), so all three generated files are exercised.
const CODEC_DRIVER_RUBY: &str = r#"require_relative "client"

# A loopback transport: the dumb byte seam. It decodes the request, re-encodes its
# nested task as the response, so the typed client round-trips through real bytes.
class Loopback
  def call(_service, _op, req_bytes)
    SubmitTaskRequest.from_cbor(req_bytes).task.to_cbor
  end
end

payload = "\xde\xad\xbe".b
task = Task.new(
  uuid: "u-123",
  current_state: "PENDING",
  payload: payload,
  priority: 7,
  labels: { "a" => 1, "b" => 2 },
  tags: ["x", "y"],
  counts: { "hits" => 10, "miss" => 3 },
  errors: { "boom" => ServiceError.new(code: 500, message: "kaboom") }
)
req = SubmitTaskRequest.new(task: task, queue: "default")

# Direct codec round-trip through the nested record.
back = SubmitTaskRequest.from_cbor(req.to_cbor)
raise "uuid" unless back.task.uuid == "u-123"
raise "current_state" unless back.task.current_state == "PENDING"
raise "payload" unless back.task.payload == payload
raise "payload-binary" unless back.task.payload.encoding == Encoding::BINARY
raise "priority" unless back.task.priority == 7
raise "labels" unless back.task.labels == { "a" => 1, "b" => 2 }
raise "tags" unless back.task.tags == ["x", "y"]
raise "queue" unless back.queue == "default"
# Named map alias field: its entries must survive, not be stubbed to nil/empty.
raise "counts" unless back.task.counts == { "hits" => 10, "miss" => 3 }
# Map-of-record alias: values must rebuild as ServiceError instances.
raise "errors-keys" unless back.task.errors.keys == ["boom"]
raise "errors-code" unless back.task.errors["boom"].code == 500
raise "errors-message" unless back.task.errors["boom"].message == "kaboom"

# An absent optional must round-trip to nil (omitted on the wire, missing on decode).
bare_task = Task.new(uuid: "u", current_state: "S", payload: "".b, labels: {}, tags: [], counts: {}, errors: {})
raise "absent-optional-set" unless bare_task.priority.nil?
back2 = SubmitTaskRequest.from_cbor(SubmitTaskRequest.new(task: bare_task, queue: "q").to_cbor)
raise "absent-optional" unless back2.task.priority.nil?

# Typed client over the loopback byte seam.
result = CorndogsClient.new(Loopback.new).submit_task(req)
raise "client-uuid" unless result.uuid == "u-123"
raise "client-payload" unless result.payload == payload
raise "client-priority" unless result.priority == 7

puts "ok"
"#;

/// A linkkeys-shaped spec (docs/csilgen-requests/ruby-target-does-not-exist.md's
/// documented caveat): `Relation`, `ListRelationsRequest`, and `CheckPermissionRequest`
/// each carry a CSIL field literally named `object_id`, which collides with
/// `Object#object_id` on the generated `Data.define` instance.
fn linkkeys_spec() -> CsilSpecSerialized {
    let relation = group_rule(
        "Relation",
        vec![
            bare_entry("object_id", builtin("text")),
            bare_entry("relation", builtin("text")),
            bare_entry("subject_id", builtin("text")),
        ],
    );
    let list_relations_request = group_rule(
        "ListRelationsRequest",
        vec![bare_entry("object_id", builtin("text"))],
    );
    let check_permission_request = group_rule(
        "CheckPermissionRequest",
        vec![
            bare_entry("object_id", builtin("text")),
            bare_entry("permission", builtin("text")),
        ],
    );
    spec(vec![
        relation,
        list_relations_request,
        check_permission_request,
    ])
}

/// A CSIL field named `object_id` must not trigger Ruby 3.4's "redefining 'object_id'
/// may cause serious problems" warning, and `Object#object_id` must keep behaving as
/// the real object identity (an Integer) rather than being shadowed by the field. Runs
/// under `ruby -W` (not just `ruby`) so a definition-time warning fails the test, not
/// just a runtime error.
#[test]
fn reserved_member_object_id_round_trips_warning_free() {
    let have = std::process::Command::new("ruby")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no ruby on PATH");
        return;
    }

    let s = linkkeys_spec();
    let files = generate_ruby_code_from_serialized(&s, &config("ruby-client"))
        .expect("generation succeeded");

    // `ruby -c` must find every file syntactically clean, per the acceptance gate.
    let dir = std::env::temp_dir().join(format!("csilgen-ruby-reserved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
        let check = std::process::Command::new("ruby")
            .arg("-c")
            .arg(dir.join(&f.path))
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "ruby -c failed on {}:\n{}{}",
            f.path,
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
    }
    std::fs::write(dir.join("driver.rb"), RESERVED_MEMBER_DRIVER_RUBY).unwrap();

    let run = std::process::Command::new("ruby")
        .arg("-W")
        .arg(dir.join("driver.rb"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "ruby round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.trim().is_empty(),
        "expected zero warnings on stderr under `ruby -W`, got:\n{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Constructs a `Relation`, round-trips it through the generated codec, and asserts:
/// the Ruby member is the escaped `object_id_` (not `object_id`), `Object#object_id`
/// still returns the real object identity (an Integer), and the encoded CBOR map's
/// wire key is still the verbatim `"object_id"` string.
const RESERVED_MEMBER_DRIVER_RUBY: &str = r#"require_relative "codec"

rel = Relation.new(object_id_: "obj-42", relation: "viewer", subject_id: "user-7")

# Object#object_id is untouched: the real identity method, not the shadowed field.
raise "object_id-shadowed" unless rel.object_id.is_a?(Integer)
raise "object_id-not-identity" unless rel.object_id == rel.object_id
raise "object_id-field-value" unless rel.object_id_ == "obj-42"

bytes = rel.to_cbor
back = Relation.from_cbor(bytes)
raise "roundtrip-object_id" unless back.object_id_ == "obj-42"
raise "roundtrip-relation" unless back.relation == "viewer"
raise "roundtrip-subject_id" unless back.subject_id == "user-7"

# The wire form's CBOR map key is still literally "object_id", not the escaped member.
tree = CsilCbor.decode(bytes)
raise "wire-key-missing" unless tree.key?("object_id")
raise "wire-key-escaped" if tree.key?("object_id_")

# The other two linkkeys record shapes carrying the same collision.
lrr = ListRelationsRequest.new(object_id_: "obj-1")
back_lrr = ListRelationsRequest.from_cbor(lrr.to_cbor)
raise "list-relations-object_id" unless back_lrr.object_id_ == "obj-1"

cpr = CheckPermissionRequest.new(object_id_: "obj-9", permission: "read")
back_cpr = CheckPermissionRequest.from_cbor(cpr.to_cbor)
raise "check-permission-object_id" unless back_cpr.object_id_ == "obj-9"
raise "check-permission-permission" unless back_cpr.permission == "read"

puts "ok"
"#;
