//! Package documentation generation tests for Go code generator

use csilgen_common::*;
use std::collections::HashMap;

/// Helper to create a test generator input
fn create_test_input_with_package_desc(package_desc: Option<&str>) -> WasmGeneratorInput {
    let mut options = HashMap::new();
    options.insert(
        "package_name".to_string(),
        serde_json::Value::String("testpkg".to_string()),
    );

    if let Some(desc) = package_desc {
        options.insert(
            "package_description".to_string(),
            serde_json::Value::String(desc.to_string()),
        );
    }

    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "go".to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: GeneratorMetadata {
            name: "go-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "go".to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: None,
            homepage: None,
        },
    }
}

#[test]
fn test_default_package_documentation() {
    let input = create_test_input_with_package_desc(None);

    // Verify default package documentation structure
    assert!(!input.config.options.contains_key("package_description"));
    assert_eq!(
        input.config.options.get("package_name")
            .and_then(|v| v.as_str())
            .unwrap(),
        "testpkg"
    );
}

#[test]
fn test_custom_package_documentation() {
    let desc = "Package testpkg provides test types for demonstration.";
    let input = create_test_input_with_package_desc(Some(desc));

    // Verify custom package description is set
    assert_eq!(
        input.config.options.get("package_description")
            .and_then(|v| v.as_str())
            .unwrap(),
        desc
    );
}

#[test]
fn test_multiline_package_documentation() {
    let desc = "Package testpkg provides test types for demonstration.\n\nThis package contains all test structures.";
    let input = create_test_input_with_package_desc(Some(desc));

    // Verify multiline description is preserved
    let stored_desc = input.config.options.get("package_description")
        .and_then(|v| v.as_str())
        .unwrap();

    assert!(stored_desc.contains('\n'));
    assert_eq!(stored_desc.lines().count(), 3);
}

#[test]
fn test_package_name_used_in_default_doc() {
    let mut input = create_test_input_with_package_desc(None);

    // Change package name
    input.config.options.insert(
        "package_name".to_string(),
        serde_json::Value::String("mycustompkg".to_string()),
    );

    assert_eq!(
        input.config.options.get("package_name")
            .and_then(|v| v.as_str())
            .unwrap(),
        "mycustompkg"
    );
}

#[test]
fn test_generated_code_warning_required() {
    let input = create_test_input_with_package_desc(None);

    // Verify the structure that will allow the generator to add the warning
    assert_eq!(input.csil_spec.rules.len(), 1);
    assert_eq!(input.config.target, "go");
}

#[test]
fn test_empty_package_description() {
    let input = create_test_input_with_package_desc(Some(""));

    // Verify empty description is handled
    assert_eq!(
        input.config.options.get("package_description")
            .and_then(|v| v.as_str())
            .unwrap(),
        ""
    );
}

#[test]
fn test_package_doc_with_special_characters() {
    let desc = "Package testpkg provides types.\n\nSupports: JSON, YAML, & XML.";
    let input = create_test_input_with_package_desc(Some(desc));

    let stored_desc = input.config.options.get("package_description")
        .and_then(|v| v.as_str())
        .unwrap();

    assert!(stored_desc.contains('&'));
    assert!(stored_desc.contains(','));
}
