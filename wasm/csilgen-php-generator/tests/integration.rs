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

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
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
    assert!(server.contains("case 'task-service/create-task':"));
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
fn unknown_php_subtarget_is_error() {
    let s = spec(vec![]);
    let err = generate_php_code_from_serialized(&s, &config("php-bogus")).unwrap_err();
    assert_eq!(err, wasm_interface::error_codes::GENERATION_ERROR);
}
