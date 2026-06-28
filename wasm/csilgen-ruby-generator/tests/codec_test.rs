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
