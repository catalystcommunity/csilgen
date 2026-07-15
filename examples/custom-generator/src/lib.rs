//! Example CSIL generator: emits a Markdown summary of a spec's types and services.
//!
//! This is the reference example for docs/generator-plugin-contract.md's
//! "Minimal walkthrough" section. It deliberately produces documentation
//! rather than a runtime codec or client/server bindings — the plugin
//! contract (Tier 0) puts no constraint on what a generator emits beyond
//! `(path, content)` pairs, and this crate exists to make that concrete: a
//! generator can be a docs tool, a lint report, project scaffolding, anything
//! expressible that way. `wasm/csilgen-json-generator` and
//! `wasm/csilgen-openapi-generator` are the in-repo precedent for the same
//! point; this crate is the minimal illustration of it, structured the same
//! way as `wasm/csilgen-noop-generator` (the ABI reference implementation)
//! but processing enough of the AST to be genuinely useful output rather than
//! a fixed string.
//!
//! Target name: `mdsummary` (crate `csilgen-mdsummary-generator` builds to
//! `csilgen_mdsummary_generator.wasm`, so `--target mdsummary` resolves to
//! this generator once it's built and discoverable — see README.md).

use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupExpression, CsilGroupKey, CsilRuleType,
    CsilServiceDirection, CsilTypeExpression, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
};

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "mdsummary-example-generator".to_string(),
        version: "1.0.0".to_string(),
        description:
            "Example generator: emits a Markdown summary of a CSIL spec's types and services"
                .to_string(),
        target: "mdsummary".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
        ],
        author: Some("csilgen".to_string()),
        homepage: Some(
            "https://github.com/catalystcommunity/csilgen/tree/main/examples/custom-generator"
                .to_string(),
        ),
    };

    serialize_and_return_ptr(&metadata)
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf); // Prevent deallocation; caller owns the lifetime by convention, not by type.
    ptr
}

/// Memory deallocation (WASM export)
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Main generator function (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    match process_generation(input_ptr, input_len) {
        Ok(output) => serialize_and_return_ptr(&output),
        Err(_code) => std::ptr::null_mut(),
    }
}

/// Serialize a value to JSON and return a length-prefixed buffer obtained
/// through this module's own `allocate` — the calling convention documented
/// in docs/generator-plugin-contract.md §2: `generate`/`get_metadata` output
/// is always this module's own allocation, never the host's.
fn serialize_and_return_ptr<T: serde::Serialize>(value: &T) -> *mut u8 {
    let json = match serde_json::to_string(value) {
        Ok(json) => json,
        Err(_) => return std::ptr::null_mut(),
    };

    let bytes = json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    unsafe {
        std::ptr::write(ptr as *mut u32, bytes.len() as u32);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Process a generation request: parse `WasmGeneratorInput` out of the raw
/// memory the host wrote, build the Markdown summary, and return
/// `WasmGeneratorOutput`.
fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }
    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    // The host writes the input directly into our memory without calling our
    // `allocate` (docs/generator-plugin-contract.md §2) — we only reconstruct
    // a slice over it here, we don't own or free it.
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let (markdown, warnings) = build_summary(&input);

    let stats = GenerationStats {
        files_generated: 1,
        total_size_bytes: markdown.len(),
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };

    Ok(WasmGeneratorOutput {
        files: vec![GeneratedFile {
            path: "SUMMARY.md".to_string(),
            content: markdown,
        }],
        warnings,
        stats,
    })
}

/// Build the Markdown summary and any advisory warnings.
fn build_summary(input: &WasmGeneratorInput) -> (String, Vec<GeneratorWarning>) {
    let mut out = String::new();
    let mut warnings = Vec::new();

    out.push_str("# CSIL Specification Summary\n\n");
    out.push_str(&format!(
        "Generated by `{}` v{} for target `{}`.\n\n",
        input.generator_metadata.name, input.generator_metadata.version, input.config.target
    ));

    if input.csil_spec.rules.is_empty() {
        warnings.push(GeneratorWarning {
            level: WarningLevel::Info,
            message: "CSIL spec has no rules".to_string(),
            location: None,
            suggestion: Some("Add type, group, or service definitions".to_string()),
        });
    }

    out.push_str("## Types\n\n");
    let mut wrote_a_type = false;
    for rule in &input.csil_spec.rules {
        let Some(section) = format_type_rule(rule) else {
            continue;
        };
        wrote_a_type = true;
        out.push_str(&section);
    }
    if !wrote_a_type {
        out.push_str("*No type definitions.*\n\n");
    }

    out.push_str("## Services\n\n");
    let mut wrote_a_service = false;
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        wrote_a_service = true;
        out.push_str(&format!("### {}\n\n", rule.name));
        for doc_line in &rule.doc_comments {
            out.push_str(&format!("> {doc_line}\n"));
        }
        if !rule.doc_comments.is_empty() {
            out.push('\n');
        }

        if service.operations.is_empty() {
            warnings.push(GeneratorWarning {
                level: WarningLevel::Warning,
                message: format!("Service '{}' has no operations", rule.name),
                location: Some(csilgen_common::SourceLocation {
                    line: rule.position.line,
                    column: rule.position.column,
                    offset: rule.position.offset,
                    context: Some(rule.name.clone()),
                }),
                suggestion: Some("Add at least one operation".to_string()),
            });
        }

        out.push_str("| Operation | Input | Output | Direction |\n");
        out.push_str("|-----------|-------|--------|-----------|\n");
        for op in &service.operations {
            let arrow = match op.direction {
                CsilServiceDirection::Unidirectional => "->",
                CsilServiceDirection::Bidirectional => "<->",
                CsilServiceDirection::Reverse => "<-",
            };
            out.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` |\n",
                op.name,
                format_type_expression(&op.input_type),
                format_type_expression(&op.output_type),
                arrow
            ));
        }
        out.push('\n');
    }
    if !wrote_a_service {
        out.push_str("*No service definitions.*\n\n");
    }

    (out, warnings)
}

/// Render one non-service rule as a Markdown section, or `None` for
/// `ServiceDef` rules (handled separately by `build_summary`).
fn format_type_rule(rule: &csilgen_common::CsilRule) -> Option<String> {
    if matches!(rule.rule_type, CsilRuleType::ServiceDef(_)) {
        return None;
    }

    let mut section = format!("### {}\n\n", rule.name);
    for doc_line in &rule.doc_comments {
        section.push_str(&format!("> {doc_line}\n"));
    }
    if !rule.doc_comments.is_empty() {
        section.push('\n');
    }

    match &rule.rule_type {
        CsilRuleType::TypeDef(type_expr) => {
            section.push_str(&format!(
                "`{} = {}`\n\n",
                rule.name,
                format_type_expression(type_expr)
            ));
        }
        CsilRuleType::GroupDef(group) => {
            section.push_str(&format_group_table(group));
        }
        CsilRuleType::TypeChoice(choices) => {
            let parts: Vec<String> = choices.iter().map(format_type_expression).collect();
            section.push_str(&format!("Choice of: {}\n\n", parts.join(" / ")));
        }
        CsilRuleType::GroupChoice(choices) => {
            section.push_str(&format!(
                "Group choice with {} alternatives.\n\n",
                choices.len()
            ));
        }
        CsilRuleType::ServiceDef(_) => unreachable!("filtered out above"),
    }
    Some(section)
}

/// Render a group's fields as a Markdown table, showing the field's visibility
/// annotation (if any) — the field-metadata pattern the noop generator
/// deliberately skips but every real generator has to handle.
fn format_group_table(group: &CsilGroupExpression) -> String {
    let mut table = String::new();
    table.push_str("| Field | Type | Visibility | Description |\n");
    table.push_str("|-------|------|------------|-------------|\n");

    for entry in &group.entries {
        let field_name = match &entry.key {
            Some(CsilGroupKey::Bare(name)) => name.clone(),
            Some(CsilGroupKey::Type(expr)) => format_type_expression(expr),
            Some(CsilGroupKey::Literal(literal)) => format!("{literal:?}"),
            None => "<unnamed>".to_string(),
        };

        let visibility = entry
            .metadata
            .iter()
            .find_map(|m| match m {
                CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly) => Some("send-only"),
                CsilFieldMetadata::Visibility(CsilFieldVisibility::ReceiveOnly) => {
                    Some("receive-only")
                }
                CsilFieldMetadata::Visibility(CsilFieldVisibility::Bidirectional) => {
                    Some("bidirectional")
                }
                _ => None,
            })
            .unwrap_or("bidirectional");

        let description = entry
            .metadata
            .iter()
            .find_map(|m| match m {
                CsilFieldMetadata::Description(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "*none*".to_string());

        table.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            field_name,
            format_type_expression(&entry.value_type),
            visibility,
            description
        ));
    }
    table.push('\n');
    table
}

/// Compact, human-readable rendering of a type expression. This is
/// deliberately not a code-generation type mapper (contrast
/// `wasm/csilgen-*-generator`, which map every variant to a target-language
/// type) — a docs generator only needs something readable, not compilable.
fn format_type_expression(expr: &CsilTypeExpression) -> String {
    match expr {
        CsilTypeExpression::Builtin(name) => name.clone(),
        CsilTypeExpression::Reference(name) => name.clone(),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("[{}]", format_type_expression(element_type))
        }
        CsilTypeExpression::Map { key, value, .. } => format!(
            "map<{}, {}>",
            format_type_expression(key),
            format_type_expression(value)
        ),
        CsilTypeExpression::Group(group) => format!("{{{} fields}}", group.entries.len()),
        CsilTypeExpression::Tuple(group) => format!("({} elements)", group.entries.len()),
        CsilTypeExpression::Choice(choices) => choices
            .iter()
            .map(format_type_expression)
            .collect::<Vec<_>>()
            .join(" / "),
        CsilTypeExpression::Range {
            start,
            end,
            inclusive,
        } => {
            let op = if *inclusive { "..=" } else { ".." };
            format!(
                "{}{op}{}",
                start.map_or_else(String::new, |s| s.to_string()),
                end.map_or_else(String::new, |e| e.to_string())
            )
        }
        CsilTypeExpression::Socket(name) => format!("${name}"),
        CsilTypeExpression::Plug(name) => format!("$${name}"),
        CsilTypeExpression::Literal(literal) => format!("{literal:?}"),
        CsilTypeExpression::Constrained { base_type, .. } => {
            format!("{} (constrained)", format_type_expression(base_type))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilGroupEntry, CsilOccurrence, CsilPosition, CsilRule, CsilServiceDefinition,
        CsilServiceOperation, CsilSpecSerialized, GeneratorConfig,
    };
    use std::collections::HashMap;

    fn sample_input() -> WasmGeneratorInput {
        let user_group = CsilGroupExpression {
            entries: vec![
                CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("id".to_string())),
                    value_type: CsilTypeExpression::Builtin("uint".to_string()),
                    occurrence: None,
                    metadata: vec![
                        CsilFieldMetadata::Visibility(CsilFieldVisibility::ReceiveOnly),
                        CsilFieldMetadata::Description("Unique user identifier".to_string()),
                    ],
                    doc_comments: Vec::new(),
                },
                CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("name".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                    doc_comments: Vec::new(),
                },
            ],
        };

        let rules = vec![
            CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(user_group),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec!["A logged-in user.".to_string()],
            },
            CsilRule {
                name: "UserAPI".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "get-user".to_string(),
                        input_type: CsilTypeExpression::Builtin("uint".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 10,
                            column: 1,
                            offset: 100,
                        },
                        doc_comments: Vec::new(),
                        wire_id: None,
                    }],
                    wire_id: None,
                }),
                position: CsilPosition {
                    line: 9,
                    column: 1,
                    offset: 90,
                },
                doc_comments: Vec::new(),
            },
        ];

        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules,
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 1,
            },
            config: GeneratorConfig {
                target: "mdsummary".to_string(),
                output_dir: "/tmp/out".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "mdsummary-example-generator".to_string(),
                version: "1.0.0".to_string(),
                description: "test".to_string(),
                target: "mdsummary".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    #[test]
    fn test_build_summary_includes_type_and_service_sections() {
        let input = sample_input();
        let (markdown, warnings) = build_summary(&input);

        assert!(markdown.contains("### User"));
        assert!(markdown.contains("A logged-in user."));
        assert!(markdown.contains("receive-only"));
        assert!(markdown.contains("Unique user identifier"));
        assert!(markdown.contains("### UserAPI"));
        assert!(markdown.contains("`get-user`"));
        assert!(markdown.contains("`uint`"));
        assert!(markdown.contains("`User`"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_build_summary_warns_on_empty_service() {
        let mut input = sample_input();
        if let CsilRuleType::ServiceDef(service) = &mut input.csil_spec.rules[1].rule_type {
            service.operations.clear();
        }

        let (_markdown, warnings) = build_summary(&input);
        assert!(warnings.iter().any(|w| w.message.contains("no operations")));
    }

    #[test]
    fn test_build_summary_empty_spec() {
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "mdsummary".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "test".to_string(),
                target: "mdsummary".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        };

        let (markdown, warnings) = build_summary(&input);
        assert!(markdown.contains("*No type definitions.*"));
        assert!(markdown.contains("*No service definitions.*"));
        assert!(warnings.iter().any(|w| w.message.contains("no rules")));
    }

    #[test]
    fn test_format_type_expression_variants() {
        assert_eq!(
            format_type_expression(&CsilTypeExpression::Builtin("text".to_string())),
            "text"
        );
        assert_eq!(
            format_type_expression(&CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            }),
            "[int]"
        );
        assert_eq!(
            format_type_expression(&CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            }),
            "map<text, int>"
        );
    }

    #[test]
    fn test_process_generation_end_to_end_via_raw_pointers() {
        let input = sample_input();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "SUMMARY.md");
        assert!(
            output.files[0]
                .content
                .contains("# CSIL Specification Summary")
        );
        assert_eq!(output.stats.services_count, 1);
    }

    #[test]
    fn test_process_generation_invalid_input() {
        let result = process_generation(std::ptr::null(), 0);
        assert_eq!(result.unwrap_err(), error_codes::INVALID_INPUT);
    }

    #[test]
    fn test_process_generation_invalid_json() {
        let invalid = b"not json";
        let result = process_generation(invalid.as_ptr(), invalid.len());
        assert_eq!(result.unwrap_err(), error_codes::SERIALIZATION_ERROR);
    }

    #[test]
    fn test_memory_allocation_deallocation() {
        let ptr = allocate(256);
        assert!(!ptr.is_null());
        deallocate(ptr, 256);
    }
}
