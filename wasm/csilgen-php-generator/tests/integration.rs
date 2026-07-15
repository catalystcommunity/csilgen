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

fn entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: Vec::new(),
        doc_comments: Vec::new(),
    }
}

fn text_lit(value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(value.to_string()))
}

fn int_lit(value: i64) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Integer(value))
}

fn null_lit() -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Null)
}

fn bytes_lit(value: &[u8]) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Bytes(value.to_vec()))
}

fn choice_rule(name: &str, choices: Vec<CsilTypeExpression>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeChoice(choices),
        position: pos(),
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

fn list_of(element: CsilTypeExpression) -> CsilTypeExpression {
    CsilTypeExpression::Array {
        element_type: Box::new(element),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    }
}

fn map_of(key: CsilTypeExpression, value: CsilTypeExpression) -> CsilTypeExpression {
    CsilTypeExpression::Map {
        key: Box::new(key),
        value: Box::new(value),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    }
}

fn service_rule() -> CsilRule {
    CsilRule {
        name: "TaskService".to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: "create-task".to_string(),
                input_type: reference("Task"),
                output_type: reference("Task"),
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: Vec::new(),
                wire_id: None,
            }],
            wire_id: None,
        }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn spec(rules: Vec<CsilRule>) -> CsilSpecSerialized {
    let service_count = rules
        .iter()
        .filter(|rule| matches!(rule.rule_type, CsilRuleType::ServiceDef(_)))
        .count();
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count,
        fields_with_metadata_count: 0,
    }
}

fn file(files: &[GeneratedFile], path: &str) -> String {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("missing generated file {path}"))
        .content
        .clone()
}

#[test]
fn php_target_emits_php7_friendly_types_codec_client_and_server() {
    let s = spec(vec![
        group_rule(
            "Task",
            vec![
                entry("task-id", builtin("uint")),
                entry("title", builtin("text")),
            ],
        ),
        service_rule(),
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php")).expect("generation ok");
    let paths: Vec<_> = out.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["types.php", "codec.php", "server.php"]);

    let types = file(&out, "types.php");
    assert!(types.contains("namespace Csilgen\\Generated;"));
    assert!(types.contains("class Task"));
    assert!(types.contains("public $taskId;"));
    assert!(types.contains("public function __construct(array $values = array())"));

    let codec = file(&out, "codec.php");
    assert!(codec.contains("use Csilgen\\Transport\\CBOR;"));
    assert!(codec.contains("public static function encodeTask($value)"));
    assert!(codec.contains("$out['task-id'] = $field;"));

    let server = file(&out, "server.php");
    assert!(server.contains("interface TaskHandler"));
    assert!(server.contains("class TaskRouter"));
    assert!(server.contains("public function dispatch($op, $payload)"));
    // Router keys are the verbatim CSIL op names; the service is implied by
    // which router is invoked, so no pre-joined route strings survive.
    assert!(server.contains("case 'create-task':"));
    assert!(!server.contains("task-service/create-task"));
}

#[test]
fn client_seam_passes_verbatim_service_and_op_as_separate_arguments() {
    let s = spec(vec![
        group_rule("Task", vec![entry("title", builtin("text"))]),
        service_rule(),
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php-client")).unwrap();
    let client = file(&out, "client.php");

    // Wire contract (cbor-wire-contract.md "RPC call naming"): the seam gets the
    // CSIL service and op names exactly as written, as two arguments — never a
    // pre-joined or case-mangled route string.
    assert!(
        client.contains("$reply = $this->transport->call('TaskService', 'create-task', $payload);")
    );
    assert!(!client.contains("'task-service/create-task'"));
    assert!(!client.contains("'task-service'"));
    // Language-level identifiers keep their PHP casing; only wire strings are verbatim.
    assert!(client.contains("class TaskClient"));
    assert!(client.contains("public function createTask($request)"));
    // The docblock tells transport authors the seam shape.
    assert!(client.contains("call($service, $op, $payload)"));
}

#[test]
fn php_client_and_typesonly_subtargets_select_surface() {
    let s = spec(vec![
        group_rule("Task", vec![entry("title", builtin("text"))]),
        service_rule(),
    ]);

    let client = generate_php_code_from_serialized(&s, &config("php-client")).unwrap();
    assert!(client.iter().any(|f| f.path == "client.php"));
    assert!(!client.iter().any(|f| f.path == "server.php"));

    let types = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    assert!(types.iter().any(|f| f.path == "types.php"));
    assert!(types.iter().any(|f| f.path == "codec.php"));
    assert!(!types.iter().any(|f| f.path == "client.php"));
    assert!(!types.iter().any(|f| f.path == "server.php"));
}

#[test]
fn package_mode_emits_composer_layout() {
    let s = spec(vec![
        group_rule("Task", vec![entry("title", builtin("text"))]),
        service_rule(),
    ]);
    let mut cfg = config("php");
    cfg.options
        .insert("emit_packages".to_string(), serde_json::json!(["php"]));
    cfg.options.insert(
        "package_name".to_string(),
        serde_json::json!("acme/tasks-client"),
    );
    cfg.options
        .insert("package_version".to_string(), serde_json::json!("1.2.3"));
    cfg.options.insert(
        "php_namespace".to_string(),
        serde_json::json!("Acme\\Tasks\\Generated"),
    );

    let out = generate_php_code_from_serialized(&s, &cfg).expect("generation ok");
    assert!(out.iter().any(|f| f.path == "composer.json"));
    assert!(out.iter().any(|f| f.path == "src/types.php"));
    assert!(out.iter().any(|f| f.path == "src/client.php"));
    assert!(out.iter().any(|f| f.path == "src/server.php"));
    assert!(out.iter().any(|f| f.path == "genquickstart.md"));

    let composer = file(&out, "composer.json");
    assert!(composer.contains("\"name\": \"acme/tasks-client\""));
    assert!(composer.contains("\"php\": \">=7.2\""));
    assert!(composer.contains("\"csilgen/transport\": \"*\""));
    assert!(composer.contains("\"classmap\": [\"src/\"]"));
}

#[test]
fn codec_list_fields_reference_the_local_field_variable() {
    let s = spec(vec![
        group_rule("Item", vec![entry("label", builtin("text"))]),
        group_rule(
            "Bundle",
            vec![
                entry("tags", list_of(builtin("text"))),
                entry("items", list_of(reference("Item"))),
                CsilGroupEntry {
                    occurrence: Some(CsilOccurrence::Optional),
                    ..entry("aliases", list_of(builtin("text")))
                },
            ],
        ),
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(
        !codec.contains("$var"),
        "codec must never reference the undefined $var: {codec}"
    );
    assert!(codec.contains(
        "$out['tags'] = array_map(function ($item) { return $item; }, $field === null ? array() : $field);"
    ));
    assert!(codec.contains(
        "$out['items'] = array_map(function ($item) { return self::toCborItem($item); }, $field === null ? array() : $field);"
    ));
    // Optional list fields keep the null guard and still map over the local.
    assert!(codec.contains(
        "            $out['aliases'] = array_map(function ($item) { return $item; }, $field === null ? array() : $field);"
    ));
    assert!(codec.contains(
        "'items' => array_key_exists('items', $value) ? array_map(function ($item) { return self::fromCborItem($item); }, $value['items'] === null ? array() : $value['items']) : null,"
    ));
    assert!(codec.contains(
        "'tags' => array_key_exists('tags', $value) ? array_map(function ($item) { return $item; }, $value['tags'] === null ? array() : $value['tags']) : null,"
    ));
}

#[test]
fn codec_nested_lists_and_maps_of_lists_reference_enclosing_locals() {
    let s = spec(vec![group_rule(
        "Matrix",
        vec![
            entry("rows", list_of(list_of(builtin("uint")))),
            entry("index", map_of(builtin("text"), list_of(builtin("text")))),
        ],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(!codec.contains("$var"));
    // Nested list: the inner array_map iterates the enclosing closure's $item.
    assert!(codec.contains(
        "$out['rows'] = array_map(function ($item) { return array_map(function ($item) { return $item; }, $item === null ? array() : $item); }, $field === null ? array() : $field);"
    ));
    // Map of lists: the inner array_map iterates the enclosing foreach's $v.
    assert!(codec.contains(
        "$out[$k] = array_map(function ($item) { return $item; }, $v === null ? array() : $v);"
    ));
}

#[test]
fn alias_list_codec_maps_over_the_value_parameter() {
    let s = spec(vec![
        group_rule("Item", vec![entry("label", builtin("text"))]),
        CsilRule {
            name: "ItemList".to_string(),
            rule_type: CsilRuleType::TypeDef(list_of(reference("Item"))),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(!codec.contains("$var"));
    assert!(codec.contains(
        "return array_map(function ($item) { return self::toCborItem($item); }, $value === null ? array() : $value);"
    ));
    assert!(codec.contains(
        "return array_map(function ($item) { return self::fromCborItem($item); }, $value === null ? array() : $value);"
    ));
}

#[test]
fn unknown_php_subtarget_is_error() {
    let s = spec(vec![]);
    let err = generate_php_code_from_serialized(&s, &config("php-bogus")).unwrap_err();
    assert_eq!(err, wasm_interface::error_codes::GENERATION_ERROR);
}

#[test]
fn all_literal_choice_stays_bare_on_encode_and_validates_membership_on_decode() {
    let s = spec(vec![choice_rule(
        "Color",
        vec![text_lit("red"), text_lit("green"), text_lit("blue")],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // Encode: an already-valid literal is its own CBOR value (identity), no
    // wrapping array.
    assert!(codec.contains(
        "    public static function toCborColor($value)\n    {\n        return $value;\n    }\n\n"
    ));
    // Decode: validated against the declared member set.
    assert!(codec.contains("static $csilMembers = array('red', 'green', 'blue');"));
    assert!(codec.contains(
        "throw new CodecException('csil cbor: unknown Color value ' . var_export($value, true));"
    ));
    // Never the tagged-sum shape an actual union gets.
    assert!(!codec.contains("csilIdx"));
}

#[test]
fn mixed_union_choice_emits_tagged_sum_with_literal_first_precedence() {
    let s = spec(vec![choice_rule(
        "OrderStatus",
        vec![builtin("text"), text_lit("pending"), text_lit("confirmed")],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // Encode: text-typed literal arms and the general text arm share one
    // `is_string` dispatch clause; the literals are checked first by value and
    // keep their own declared index, the general arm is the fallback.
    assert!(codec.contains("public static function toCborOrderStatus($value)"));
    assert!(codec.contains("if (is_string($value)) {\n"));
    assert!(codec.contains(
        "if ($value === 'pending') {\n                return array(1, 'pending');\n            }"
    ));
    assert!(codec.contains("if ($value === 'confirmed') {\n                return array(2, 'confirmed');\n            }"));
    assert!(codec.contains("return array(0, $value);"));

    // Decode: dispatches by index; a literal arm validates equality against its
    // declared literal rather than trusting the payload.
    assert!(codec.contains(
        "if (!is_array($value) || count($value) !== 2) {\n            throw new CodecException('csil cbor: OrderStatus union expects a 2-element array');\n        }"
    ));
    assert!(codec.contains("if ($csilIdx === 0) {\n            return $csilVal;\n        }"));
    assert!(
        codec.contains("if ($csilIdx === 1) {\n            return self::expectLiteral($csilVal, 'pending');\n        }")
    );
    assert!(codec.contains(
        "throw new CodecException('csil cbor: unknown OrderStatus variant ' . var_export($csilIdx, true));"
    ));
}

#[test]
fn heterogeneous_union_dispatches_on_php_runtime_type() {
    let s = spec(vec![choice_rule(
        "IdOrName",
        vec![builtin("uint"), builtin("text")],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(
        codec.contains("if (is_int($value)) {\n            return array(0, $value);\n        }")
    );
    assert!(
        codec.contains("if (is_string($value)) {\n            return array(1, $value);\n        }")
    );
    assert!(codec.contains("if ($csilIdx === 0) {\n            return $csilVal;\n        }"));
    assert!(codec.contains("if ($csilIdx === 1) {\n            return $csilVal;\n        }"));
}

#[test]
fn same_dispatch_key_union_arms_pick_the_first_declared_arm_on_encode() {
    // `text` and `bytes` are both runtime PHP strings -- `php_union_dispatch_key`
    // can't tell them apart -- so both arms land in the SAME dispatch group. The
    // FIRST declared arm (index 0, `text`) must win on encode; before the fix, the
    // grouping loop unconditionally overwrote `general_idx` for every non-literal
    // arm sharing the key, so the LAST arm (`bytes`, index 1) won instead, which
    // silently corrupted every text value into a wire-encoded bytes payload.
    let s = spec(vec![choice_rule(
        "Blob",
        vec![builtin("text"), builtin("bytes")],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // Single check (`is_string`), single `return array(0, ...)` -- the `bytes`
    // arm (index 1) never gets its own reachable branch on encode.
    assert!(
        codec.contains("if (is_string($value)) {\n            return array(0, $value);\n        }")
    );
    assert!(!codec.contains("return array(1, "));
    // Decode still reconstructs both arms by their own declared index (identity for
    // a bare builtin arm, matching `heterogeneous_union_dispatches_on_php_runtime_type`).
    assert!(codec.contains("if ($csilIdx === 0) {\n            return $csilVal;\n        }"));
    assert!(codec.contains("if ($csilIdx === 1) {\n            return $csilVal;\n        }"));
}

#[test]
fn mixed_kind_literal_choice_with_a_null_arm_is_a_three_way_union() {
    // `"a" / 1 / null`: two literal arms of DIFFERENT kinds plus a `null` general
    // arm. A bare `null` choice arm always parses as `Builtin("null")` (never a
    // `Literal(Null)`), so it is a genuine non-literal "general" arm like any other
    // open builtin -- the whole choice fails `choice_is_enum`'s all-literal test on
    // that arm alone and is a proper 3-variant tagged-sum union, matching the
    // Python/TypeScript generators' empirically-observed behavior for the same CSIL
    // source (`"a" / 1 / null`): both emit `[0, "a"]` / `[1, 1]` / `[2, null]`, not
    // a "nullable scalar" shape.
    let s = spec(vec![choice_rule(
        "MixedLitNull",
        vec![text_lit("a"), int_lit(1), builtin("null")],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // The `null` general arm gets its OWN dispatch key (`=== null`), not lumped
    // into the `"a"` literal's `is_string` bucket -- an actual PHP `null` value
    // never satisfies `is_string`, so grouping them would make the null arm
    // unreachable on encode (the bug this test guards against).
    assert!(
        codec.contains("if (is_string($value)) {\n            return array(0, 'a');\n        }")
    );
    assert!(codec.contains("if (is_int($value)) {\n            return array(1, 1);\n        }"));
    assert!(
        codec.contains("if ($value === null) {\n            return array(2, $value);\n        }")
    );
    assert!(codec.contains("if ($csilIdx === 2) {\n            return $csilVal;\n        }"));
}

#[test]
fn all_bytes_literal_choice_is_an_enum_not_a_misrouted_union() {
    // Regression pinned by the migration to `csilgen_common::classify_choice`: the
    // prior local `choice_is_enum` additionally gated each arm on a `literal_kind`
    // helper covering only text/int/float/bool, so an all-bytes-literal choice
    // failed that gate and fell through to `choice_is_union` (which has no such
    // gate), misclassifying it as a tagged-sum union — `[index, value]` on the wire
    // — instead of the bare-literal enum the CSIL wire contract requires for EVERY
    // literal kind, bytes included (see `crates/csilgen-common/src/choice.rs`
    // module docs). `php_literal` already renders a bytes literal to valid PHP, so
    // nothing technically prevented the correct enum codec; only the stale gate did.
    let s = spec(vec![choice_rule(
        "BlobKind",
        vec![bytes_lit(b"\x01"), bytes_lit(b"\x02")],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // Encode: bare identity, no `[index, value]` wrapping array.
    assert!(codec.contains(
        "    public static function toCborBlobKind($value)\n    {\n        return $value;\n    }\n\n"
    ));
    // Decode: membership check against the declared literal set.
    assert!(codec.contains("static $csilMembers = array("));
    assert!(codec.contains(
        "throw new CodecException('csil cbor: unknown BlobKind value ' . var_export($value, true));"
    ));
    assert!(!codec.contains("csilIdx"));
}

#[test]
fn all_literal_choice_with_an_explicit_null_literal_arm_is_an_enum() {
    // `Literal(Null)` is only constructible directly via this generator's
    // `WasmGeneratorInput` API (never through the parser — a bare `null` written in
    // real CSIL source parses to `Builtin("null")`, a distinct, non-literal arm; see
    // `mixed_kind_literal_choice_with_a_null_arm_is_a_three_way_union` above for that
    // case). Per the shared contract an explicit `Literal(Null)` counts as a literal
    // like any other kind, so an ALL-literal choice built with one (every arm
    // literal) classifies as an enum — `choice_is_union`'s `has_null` guard only
    // ever fires when a `Literal(Null)` arm sits alongside a genuinely non-literal
    // arm (see `nullable_non_enum_choice_keeps_the_generic_passthrough_codec`).
    let s = spec(vec![choice_rule(
        "MaybeExplicitNull",
        vec![text_lit("a"), null_lit()],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(codec.contains(
        "    public static function toCborMaybeExplicitNull($value)\n    {\n        return $value;\n    }\n\n"
    ));
    assert!(codec.contains("static $csilMembers = array('a', null);"));
    assert!(!codec.contains("csilIdx"));
}

#[test]
fn record_arm_union_dispatches_via_instanceof_and_recurses_into_record_codecs() {
    let s = spec(vec![
        group_rule("Cat", vec![entry("lives", builtin("uint"))]),
        group_rule("Dog", vec![entry("breed", builtin("text"))]),
        choice_rule("Pet", vec![reference("Cat"), reference("Dog")]),
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(codec.contains(
        "if ($value instanceof Cat) {\n            return array(0, self::toCborCat($value));\n        }"
    ));
    assert!(codec.contains(
        "if ($value instanceof Dog) {\n            return array(1, self::toCborDog($value));\n        }"
    ));
    assert!(codec.contains(
        "if ($csilIdx === 0) {\n            return self::fromCborCat($csilVal);\n        }"
    ));
    assert!(codec.contains(
        "if ($csilIdx === 1) {\n            return self::fromCborDog($csilVal);\n        }"
    ));
}

#[test]
fn record_field_referencing_a_named_union_routes_through_its_codec() {
    let s = spec(vec![
        choice_rule("IdOrName", vec![builtin("uint"), builtin("text")]),
        group_rule("Ticket", vec![entry("who", reference("IdOrName"))]),
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(codec.contains("$out['who'] = self::toCborIdOrName($field);"));
    assert!(codec.contains(
        "'who' => array_key_exists('who', $value) ? self::fromCborIdOrName($value['who']) : null,"
    ));
}

#[test]
fn record_field_referencing_a_named_enum_validates_on_decode_but_stays_identity_on_encode() {
    let s = spec(vec![
        choice_rule("Color", vec![text_lit("red"), text_lit("green")]),
        group_rule("Swatch", vec![entry("color", reference("Color"))]),
    ]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // Encode stays identity inline (matches the reference Go/Python generators: an
    // already-valid literal is its own CBOR value, no dedicated encode call needed).
    assert!(codec.contains("$out['color'] = $field;"));
    // Decode routes through the enum's membership-validating fromCbor helper.
    assert!(codec.contains(
        "'color' => array_key_exists('color', $value) ? self::fromCborColor($value['color']) : null,"
    ));
}

#[test]
fn nullable_non_enum_choice_keeps_the_generic_passthrough_codec() {
    // `text / null` has a non-literal arm (excluded from enum) and a `null` arm
    // (excluded from union per the wire contract) -- neither codec shape applies,
    // so it must keep behaving exactly as it did before this change: identity.
    let s = spec(vec![choice_rule(
        "MaybeText",
        vec![builtin("text"), null_lit()],
    )]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    assert!(codec.contains(
        "    public static function toCborMaybeText($value)\n    {\n        return $value;\n    }\n\n"
    ));
    assert!(codec.contains(
        "    public static function fromCborMaybeText($value)\n    {\n        return $value;\n    }\n\n"
    ));
    assert!(!codec.contains("MaybeText union"));
    assert!(!codec.contains("unknown MaybeText value"));
}

#[test]
fn mixed_kind_literal_choice_is_a_bare_enum_not_a_union() {
    // A literal choice whose members are not all the same literal kind (text vs
    // int) is still an ALL-literal choice — the CSIL wire contract says the wire
    // value is the bare literal itself, self-discriminating by its own CBOR major
    // type, regardless of whether the declared literal kinds happen to match.
    // Matches the Go/PHP/Python/TypeScript generators' shared contract decision:
    // only a choice with at least one NON-literal arm is a tagged-sum union.
    let s = spec(vec![choice_rule("Mixed", vec![text_lit("a"), int_lit(1)])]);
    let out = generate_php_code_from_serialized(&s, &config("php-typesonly")).unwrap();
    let codec = file(&out, "codec.php");

    // Encode: an already-valid literal is its own CBOR value (identity), no
    // `[index, value]` wrapping array.
    assert!(codec.contains(
        "    public static function toCborMixed($value)\n    {\n        return $value;\n    }\n\n"
    ));
    // Decode: validated against the declared member set, spanning both literal
    // kinds in one membership list (PHP `===` distinguishes `1` (int) from `'1'`
    // (string) natively, so mixing kinds in one `csilMembers` array is safe).
    assert!(codec.contains("static $csilMembers = array('a', 1);"));
    assert!(codec.contains(
        "throw new CodecException('csil cbor: unknown Mixed value ' . var_export($value, true));"
    ));
    // Never the tagged-sum shape an actual union gets.
    assert!(!codec.contains("csilIdx"));
}
