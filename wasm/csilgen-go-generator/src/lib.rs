//! Go Code Generator for CSIL
//!
//! This example generator demonstrates how to create a fully functional
//! CSIL generator that produces Go code with struct definitions and service interfaces.

use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilTypeExpression,
    CsilValidationConstraint, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
};
use std::collections::HashMap;

/// Generate Go code from CSIL specifications
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "go-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Go code generator with service support".to_string(),
        target: "go".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/go-generator".to_string()),
    };

    serialize_and_return_ptr(&metadata)
}

#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let result = match deserialize_input(input_ptr, input_len) {
        Ok(input) => process_generation(input),
        Err(error_code) => return create_error_result(error_code),
    };

    match result {
        Ok(output) => serialize_and_return_ptr(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn deserialize_input(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorInput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;

    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

fn process_generation(input: WasmGeneratorInput) -> Result<WasmGeneratorOutput, i32> {
    let config = GoConfig::from_options(&input.config.options);
    let mut warnings = Vec::new();
    let mut files = Vec::new();

    // Helper to build output path with optional subdirectory
    let make_path = |filename: &str| -> String {
        if config.output_subdir.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", config.output_subdir, filename)
        }
    };

    // Generate types file
    if let Some(types_content) = generate_types(&input, &config, &mut warnings)? {
        files.push(GeneratedFile {
            path: make_path("types.gen.go"),
            content: types_content,
        });
    }

    // Generate services file if there are services
    if input.csil_spec.service_count > 0
        && let Some(services_content) = generate_services(&input, &config, &mut warnings)?
    {
        files.push(GeneratedFile {
            path: make_path("services.gen.go"),
            content: services_content,
        });
    }

    // Generate validation file if there are constraints
    if input.csil_spec.fields_with_metadata_count > 0
        && let Some(validation_content) = generate_validation(&input, &config, &mut warnings)?
    {
        files.push(GeneratedFile {
            path: make_path("validation.gen.go"),
            content: validation_content,
        });
    }

    // Generate constructors file if there are types with defaults
    if config.generate_constructors
        && let Some(constructors_content) = generate_constructors(&input, &config, &mut warnings)?
    {
        files.push(GeneratedFile {
            path: make_path("constructors.gen.go"),
            content: constructors_content,
        });
    }

    let total_size: usize = files.iter().map(|f| f.content.len()).sum();

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: total_size,
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 100, // Mock generation time for WASM
        peak_memory_bytes: Some(estimate_memory_usage()),
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings,
        stats,
    })
}

#[derive(Debug)]
struct GoConfig {
    package_name: String,
    output_subdir: String,
    use_json_tags: bool,
    use_yaml_tags: bool,
    generate_validation: bool,
    generate_constructors: bool,
    indent_style: String,
    go_imports: Vec<String>,
}

impl GoConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Self {
        let go_package = options.get("go_package").and_then(|v| v.as_str());

        // Extract package name from go_package option (last path component)
        let package_name = if let Some(pkg) = go_package {
            pkg.split('/').next_back().unwrap_or("api").to_string()
        } else {
            options
                .get("package_name")
                .and_then(|v| v.as_str())
                .unwrap_or("api")
                .to_string()
        };

        // Optionally derive output subdirectory from go_module and go_package.
        // If go_module is provided, strip it from go_package to get the relative path.
        // e.g., go_module="github.com/foo/bar", go_package="github.com/foo/bar/v1/internal/config"
        // -> output_subdir="v1/internal/config"
        // If go_module is NOT provided, output_subdir remains empty (files go to --output dir).
        let output_subdir = options
            .get("go_module")
            .and_then(|v| v.as_str())
            .and_then(|module| {
                go_package.and_then(|pkg| {
                    pkg.strip_prefix(module)
                        .map(|s| s.trim_start_matches('/').to_string())
                })
            })
            .unwrap_or_default();

        // Parse go_imports as array of strings
        let go_imports = options
            .get("go_imports")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Self {
            package_name,
            output_subdir,
            use_json_tags: options
                .get("use_json_tags")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            use_yaml_tags: options
                .get("use_yaml_tags")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            generate_validation: options
                .get("generate_validation")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            generate_constructors: options
                .get("generate_constructors")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            indent_style: "\t".to_string(), // Go convention is tabs
            go_imports,
        }
    }
}

fn generate_types(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();

    // Package-level documentation
    let package_description = input
        .config
        .options
        .get("package_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !package_description.is_empty() {
        // Add custom package description
        for line in package_description.lines() {
            content.push_str(&format!("// {}\n", line));
        }
        content.push_str("//\n");
    } else {
        // Default package comment
        content.push_str(&format!(
            "// Package {} contains generated types.\n",
            config.package_name
        ));
        content.push_str("//\n");
    }

    // Generated code warning
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");

    // Package declaration
    content.push_str(&format!("package {}\n\n", config.package_name));

    // Add imports if configured
    if !config.go_imports.is_empty() {
        content.push_str("import (\n");
        for import_path in &config.go_imports {
            content.push_str(&format!("{}\"{}\"", config.indent_style, import_path));
            content.push('\n');
        }
        content.push_str(")\n\n");
    }

    // Generate type definitions
    let mut has_types = false;
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                has_types = true;
                content.push_str(&format!(
                    "// {} represents a structured data type\n",
                    rule.name
                ));
                content.push_str(&format!("type {} struct {{\n", rule.name));

                for entry in &group.entries {
                    if let Some(key) = &entry.key {
                        let field_name = go_field_name_from_key_with_metadata(key, &entry.metadata);
                        // Check for @go_type override first, otherwise map CSIL type
                        let go_type = get_go_type_override(&entry.metadata).unwrap_or_else(|| {
                            map_csil_type_to_go(&entry.value_type, &entry.occurrence)
                        });

                        // Add field documentation
                        if let Some(description) = get_field_description(&entry.metadata) {
                            content
                                .push_str(&format!("{}// {}\n", config.indent_style, description));
                        }

                        content.push_str(&format!(
                            "{}{} {}",
                            config.indent_style, field_name, go_type
                        ));

                        // Add struct tags
                        let mut tag_parts = Vec::new();

                        // Add JSON tags if enabled
                        if config.use_json_tags {
                            let json_name = go_json_name_from_key(key);

                            // Add omitempty for optional fields
                            if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                tag_parts.push(format!("json:\"{},omitempty\"", json_name));
                            } else {
                                tag_parts.push(format!("json:\"{}\"", json_name));
                            }

                            // Check field visibility
                            let visibility = get_field_visibility(&entry.metadata);
                            match visibility {
                                CsilFieldVisibility::SendOnly => {
                                    tag_parts.push("json:\"-\" # send-only".to_string());
                                    warnings.push(GeneratorWarning {
                                        level: WarningLevel::Info,
                                        message: format!("Field '{}' marked as send-only, consider separate request/response types", field_name),
                                        location: None,
                                        suggestion: Some("Create separate request and response structs for better type safety".to_string()),
                                    });
                                }
                                CsilFieldVisibility::ReceiveOnly => {
                                    tag_parts.push("# receive-only".to_string());
                                }
                                _ => {}
                            }
                        }

                        // Add YAML tags if enabled
                        if config.use_yaml_tags {
                            let yaml_name = go_json_name_from_key(key);

                            // Check if this is a map type that should be inlined
                            // Map types with occurrence indicator should use inline
                            let is_inline_map =
                                matches!(&entry.value_type, CsilTypeExpression::Map { .. });

                            if is_inline_map {
                                tag_parts.push("yaml:\",inline\"".to_string());
                            } else if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                tag_parts.push(format!("yaml:\"{},omitempty\"", yaml_name));
                            } else {
                                tag_parts.push(format!("yaml:\"{}\"", yaml_name));
                            }
                        }

                        if !tag_parts.is_empty() {
                            content.push_str(&format!(" `{}`", tag_parts.join(" ")));
                        }

                        content.push('\n');
                    }
                }

                content.push_str("}\n\n");
            }
            CsilRuleType::TypeDef(type_expr) => {
                has_types = true;

                // Special case: if TypeDef contains a Group expression, expand it as a struct
                if let CsilTypeExpression::Group(group) = type_expr {
                    content.push_str(&format!(
                        "// {} represents a structured data type\n",
                        rule.name
                    ));
                    content.push_str(&format!("type {} struct {{\n", rule.name));

                    for entry in &group.entries {
                        if let Some(key) = &entry.key {
                            let field_name =
                                go_field_name_from_key_with_metadata(key, &entry.metadata);
                            // Check for @go_type override first, otherwise map CSIL type
                            let go_type =
                                get_go_type_override(&entry.metadata).unwrap_or_else(|| {
                                    map_csil_type_to_go(&entry.value_type, &entry.occurrence)
                                });

                            if let Some(description) = get_field_description(&entry.metadata) {
                                content.push_str(&format!(
                                    "{}// {}\n",
                                    config.indent_style, description
                                ));
                            }

                            content.push_str(&format!(
                                "{}{} {}",
                                config.indent_style, field_name, go_type
                            ));

                            let mut tag_parts = Vec::new();

                            if config.use_json_tags {
                                let json_name = go_json_name_from_key(key);
                                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                                    tag_parts.push(format!("json:\"{},omitempty\"", json_name));
                                } else {
                                    tag_parts.push(format!("json:\"{}\"", json_name));
                                }
                            }

                            if config.use_yaml_tags {
                                let yaml_name = go_json_name_from_key(key);
                                let is_inline_map =
                                    matches!(&entry.value_type, CsilTypeExpression::Map { .. });

                                if is_inline_map {
                                    tag_parts.push("yaml:\",inline\"".to_string());
                                } else if matches!(entry.occurrence, Some(CsilOccurrence::Optional))
                                {
                                    tag_parts.push(format!("yaml:\"{},omitempty\"", yaml_name));
                                } else {
                                    tag_parts.push(format!("yaml:\"{}\"", yaml_name));
                                }
                            }

                            if !tag_parts.is_empty() {
                                content.push_str(&format!(" `{}`", tag_parts.join(" ")));
                            }

                            content.push('\n');
                        }
                    }

                    content.push_str("}\n\n");
                } else {
                    // Regular type alias
                    let go_type = map_csil_type_to_go(type_expr, &None);
                    content.push_str(&format!("// {} is a type alias\n", rule.name));
                    content.push_str(&format!("type {} {}\n\n", rule.name, go_type));
                }
            }
            _ => {} // Services handled separately
        }
    }

    if has_types {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

fn generate_services(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();

    content.push_str(&format!(
        "// Package {} contains generated service interfaces.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");
    content.push_str(&format!("package {}\n\n", config.package_name));

    let needs_channel = spec_has_channel_ops(input);

    content.push_str("import (\n");
    content.push_str(&format!("{}\"context\"\n", config.indent_style));
    if needs_channel {
        // fmt.Errorf for the router's unknown-method case.
        content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    }
    content.push_str(")\n\n");

    if needs_channel {
        // Same shape across all generators: the codec is consumer-supplied so
        // the runtime never owns serialization.
        content.push_str(
            "// Codec is the consumer-supplied (de)serialization layer for channel\n\
             // messages. The generator is codec-agnostic; the implementer wires this\n\
             // to CBOR, JSON, or anything else its protocol expects.\n\
             type Codec interface {\n",
        );
        content.push_str(&format!(
            "{}Encode(value any) ([]byte, error)\n",
            config.indent_style
        ));
        content.push_str(&format!(
            "{}Decode(data []byte, out any) error\n",
            config.indent_style
        ));
        content.push_str("}\n\n");
    }

    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            emit_service_interface(&mut content, &rule.name, service, config);

            if service_has_channel_ops(service) {
                emit_channel_router(&mut content, &rule.name, service, config);
                emit_channel_encoders(&mut content, &rule.name, service, config);
            }
        }
    }

    Ok(Some(content))
}

fn spec_has_channel_ops(input: &WasmGeneratorInput) -> bool {
    input.csil_spec.rules.iter().any(|r| match &r.rule_type {
        CsilRuleType::ServiceDef(def) => service_has_channel_ops(def),
        _ => false,
    })
}

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
}

fn emit_service_interface(
    content: &mut String,
    name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    content.push_str(&format!("// {name} defines the service interface\n"));
    content.push_str(&format!("type {name} interface {{\n"));

    for operation in &service.operations {
        let method_name = go_method_name(&operation.name);
        match operation.direction {
            CsilServiceDirection::Unidirectional => {
                let input_type = map_csil_type_to_go(&operation.input_type, &None);
                let output_type = map_csil_type_to_go(&operation.output_type, &None);
                content.push_str(&format!(
                    "{}{}(ctx context.Context, req {}) ({}, error)\n",
                    config.indent_style, method_name, input_type, output_type
                ));
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the implementer's plumbing pulls a
                // frame off the wire, hands it to Route<Service>Channel, which
                // decodes and dispatches here.
                let input_type = map_csil_type_to_go(&operation.input_type, &None);
                content.push_str(&format!(
                    "{}{}(ctx context.Context, msg {}) error\n",
                    config.indent_style, method_name, input_type
                ));
            }
            CsilServiceDirection::Reverse => {
                // Server pushes only; no inbound method on the server side.
            }
        }
    }

    content.push_str("}\n\n");
}

fn emit_channel_router(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    config: &GoConfig,
) {
    let route_fn = format!("Route{service_name}Channel");
    content.push_str(&format!(
        "// {route_fn} decodes one inbound channel frame and dispatches to the\n\
         // matching {service_name} method. The implementer feeds bytes from its\n\
         // connection here; the generator never owns the wire.\n"
    ));
    content.push_str(&format!(
        "func {route_fn}(handlers {service_name}, ctx context.Context, codec Codec, method string, data []byte) error {{\n"
    ));
    content.push_str(&format!("{}switch method {{\n", config.indent_style));
    for operation in &service.operations {
        if !matches!(operation.direction, CsilServiceDirection::Bidirectional) {
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let input_type = map_csil_type_to_go(&operation.input_type, &None);
        content.push_str(&format!("{}case \"{method_name}\":\n", config.indent_style));
        content.push_str(&format!(
            "{}{}var msg {input_type}\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}if err := codec.Decode(data, &msg); err != nil {{\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}{}return err\n",
            config.indent_style, config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}}}\n",
            config.indent_style, config.indent_style
        ));
        content.push_str(&format!(
            "{}{}return handlers.{method_name}(ctx, msg)\n",
            config.indent_style, config.indent_style
        ));
    }
    content.push_str(&format!("{}default:\n", config.indent_style));
    content.push_str(&format!(
        "{}{}return fmt.Errorf(\"unknown channel method %q\", method)\n",
        config.indent_style, config.indent_style
    ));
    content.push_str(&format!("{}}}\n", config.indent_style));
    content.push_str("}\n\n");
}

fn emit_channel_encoders(
    content: &mut String,
    service_name: &str,
    service: &CsilServiceDefinition,
    _config: &GoConfig,
) {
    for operation in &service.operations {
        if !matches!(
            operation.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let method_name = go_method_name(&operation.name);
        let output_type = map_csil_type_to_go(&operation.output_type, &None);
        let fn_name = format!("Encode{service_name}{method_name}");
        content.push_str(&format!(
            "// {fn_name} encodes a `{method_name}` message the server pushes to a peer;\n\
             // the implementer frames (method, bytes) onto its connection.\n"
        ));
        content.push_str(&format!(
            "func {fn_name}(codec Codec, msg {output_type}) (string, []byte, error) {{\n"
        ));
        content.push_str("\tdata, err := codec.Encode(msg)\n");
        content.push_str("\tif err != nil {\n");
        content.push_str("\t\treturn \"\", nil, err\n");
        content.push_str("\t}\n");
        content.push_str(&format!("\treturn \"{method_name}\", data, nil\n"));
        content.push_str("}\n\n");
    }
}

fn generate_validation(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    if !config.generate_validation {
        return Ok(None);
    }

    let mut content = String::new();

    // Package-level documentation
    content.push_str(&format!(
        "// Package {} contains generated validation functions.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");

    // Package declaration
    content.push_str(&format!("package {}\n\n", config.package_name));

    // Imports
    content.push_str("import (\n");
    content.push_str(&format!("{}\"errors\"\n", config.indent_style));
    content.push_str(&format!("{}\"fmt\"\n", config.indent_style));
    content.push_str(")\n\n");

    // Generate validation functions for types with constraints
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::GroupDef(group) = &rule.rule_type {
            let has_validation = group.entries.iter().any(|entry| {
                entry
                    .metadata
                    .iter()
                    .any(|meta| matches!(meta, CsilFieldMetadata::Constraint(_)))
            });

            if has_validation {
                content.push_str(&format!(
                    "// Validate{} validates the {} struct\n",
                    rule.name, rule.name
                ));
                content.push_str(&format!("func (v *{}) Validate() error {{\n", rule.name));

                for entry in &group.entries {
                    if let Some(key) = &entry.key {
                        let field_name = go_field_name_from_key_with_metadata(key, &entry.metadata);

                        for metadata in &entry.metadata {
                            if let CsilFieldMetadata::Constraint(constraint) = metadata {
                                match constraint {
                                    CsilValidationConstraint::MinLength(min_len) => {
                                        content.push_str(&format!(
                                            "{}if len(v.{}) < {} {{\n",
                                            config.indent_style, field_name, min_len
                                        ));
                                        let unit = if *min_len == 1 {
                                            "character"
                                        } else {
                                            "characters"
                                        };
                                        content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must have at least {} {}\")\n",
                                            config.indent_style, config.indent_style, field_name, min_len, unit));
                                        content.push_str(&format!("{}}}\n", config.indent_style));
                                    }
                                    CsilValidationConstraint::MaxLength(max_len) => {
                                        content.push_str(&format!(
                                            "{}if len(v.{}) > {} {{\n",
                                            config.indent_style, field_name, max_len
                                        ));
                                        let unit = if *max_len == 1 {
                                            "character"
                                        } else {
                                            "characters"
                                        };
                                        content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must have at most {} {}\")\n",
                                            config.indent_style, config.indent_style, field_name, max_len, unit));
                                        content.push_str(&format!("{}}}\n", config.indent_style));
                                    }
                                    CsilValidationConstraint::MinItems(min_items) => {
                                        content.push_str(&format!(
                                            "{}if len(v.{}) < {} {{\n",
                                            config.indent_style, field_name, min_items
                                        ));
                                        let unit = if *min_items == 1 { "item" } else { "items" };
                                        content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must have at least {} {}\")\n",
                                            config.indent_style, config.indent_style, field_name, min_items, unit));
                                        content.push_str(&format!("{}}}\n", config.indent_style));
                                    }
                                    CsilValidationConstraint::MaxItems(max_items) => {
                                        content.push_str(&format!(
                                            "{}if len(v.{}) > {} {{\n",
                                            config.indent_style, field_name, max_items
                                        ));
                                        let unit = if *max_items == 1 { "item" } else { "items" };
                                        content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must have at most {} {}\")\n",
                                            config.indent_style, config.indent_style, field_name, max_items, unit));
                                        content.push_str(&format!("{}}}\n", config.indent_style));
                                    }
                                    CsilValidationConstraint::MinValue(min_val) => {
                                        let value_str = literal_value_to_go_string(min_val);
                                        content.push_str(&format!(
                                            "{}if v.{} < {} {{\n",
                                            config.indent_style, field_name, value_str
                                        ));
                                        content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must be at least {}\")\n",
                                            config.indent_style, config.indent_style, field_name, value_str));
                                        content.push_str(&format!("{}}}\n", config.indent_style));
                                    }
                                    CsilValidationConstraint::MaxValue(max_val) => {
                                        let value_str = literal_value_to_go_string(max_val);
                                        content.push_str(&format!(
                                            "{}if v.{} > {} {{\n",
                                            config.indent_style, field_name, value_str
                                        ));
                                        content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must be at most {}\")\n",
                                            config.indent_style, config.indent_style, field_name, value_str));
                                        content.push_str(&format!("{}}}\n", config.indent_style));
                                    }
                                    CsilValidationConstraint::Custom { name, value } => {
                                        if name == "regex"
                                            && let CsilLiteralValue::Text(pattern) = value
                                        {
                                            content.push_str(&format!(
                                                "{}matched, _ := regexp.MatchString(`{}`, v.{})\n",
                                                config.indent_style, pattern, field_name
                                            ));
                                            content.push_str(&format!(
                                                "{}if !matched {{\n",
                                                config.indent_style
                                            ));
                                            content.push_str(&format!("{}{}return fmt.Errorf(\"field '{}' must match pattern '{}'\")\n",
                                                config.indent_style, config.indent_style, field_name, pattern));
                                            content
                                                .push_str(&format!("{}}}\n", config.indent_style));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                content.push_str(&format!("{}return nil\n", config.indent_style));
                content.push_str("}\n\n");
            }
        }
    }

    if content.len()
        > format!("package {}\n\n", config.package_name).len()
            + "import (\n\t\"errors\"\n\t\"fmt\"\n)\n\n".len()
    {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

fn generate_constructors(
    input: &WasmGeneratorInput,
    config: &GoConfig,
    _warnings: &mut Vec<GeneratorWarning>,
) -> Result<Option<String>, i32> {
    let mut content = String::new();
    let mut has_constructors = false;

    // Package-level documentation
    content.push_str(&format!(
        "// Package {} contains generated constructor functions.\n",
        config.package_name
    ));
    content.push_str("//\n");
    content.push_str("// Code generated by csilgen; DO NOT EDIT.\n");

    // Package declaration
    content.push_str(&format!("package {}\n\n", config.package_name));

    // Generate constructor functions for types with default values
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::GroupDef(group) = &rule.rule_type {
            // Check if this type has any fields with default values
            let fields_with_defaults: Vec<_> = group
                .entries
                .iter()
                .filter_map(|entry| {
                    let key = entry.key.as_ref()?;
                    for metadata in &entry.metadata {
                        if let CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom {
                            name,
                            value,
                        }) = metadata
                            && name == "default"
                        {
                            return Some((
                                key,
                                value,
                                &entry.value_type,
                                &entry.occurrence,
                                &entry.metadata,
                            ));
                        }
                    }
                    None
                })
                .collect();

            if !fields_with_defaults.is_empty() {
                has_constructors = true;

                // Generate godoc comment with default values listed
                content.push_str(&format!(
                    "// New{} creates a {} with default values:\n",
                    rule.name, rule.name
                ));
                for (key, value, _, _, _) in &fields_with_defaults {
                    let field_name = go_json_name_from_key(key);
                    let value_str = literal_value_to_go_string(value);
                    content.push_str(&format!("//   - {}: {}\n", field_name, value_str));
                }
                content.push_str(&format!("func New{}() *{} {{\n", rule.name, rule.name));
                content.push_str(&format!(
                    "{}return &{} {{\n",
                    config.indent_style, rule.name
                ));

                // Set default values for each field
                for (key, value, value_type, occurrence, metadata) in &fields_with_defaults {
                    let field_name = go_field_name_from_key_with_metadata(key, metadata);
                    let go_value = literal_value_to_go_value(value, value_type, occurrence);
                    content.push_str(&format!(
                        "{}{}{}: {},\n",
                        config.indent_style, config.indent_style, field_name, go_value
                    ));
                }

                content.push_str(&format!("{}}}\n", config.indent_style));
                content.push_str("}\n\n");
            }
        }
    }

    if has_constructors {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

fn map_csil_type_to_go(
    type_expr: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
) -> String {
    let base_type = match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" => "int64",
            "uint" => "uint64",
            "float" => "float64",
            "text" => "string",
            "bytes" => "[]byte",
            "bool" => "bool",
            "nil" | "null" => "interface{}",
            _ => name,
        },
        CsilTypeExpression::Reference(name) => name,
        CsilTypeExpression::Array { element_type, .. } => {
            let element = map_csil_type_to_go(element_type, &None);
            return format!("[]{}", element);
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let key_type = map_csil_type_to_go(key, &None);
            let value_type = map_csil_type_to_go(value, &None);
            return format!("map[{}]{}", key_type, value_type);
        }
        CsilTypeExpression::Constrained { base_type, .. } => {
            // Unwrap constrained types and map the base type
            // Constraints like .size, .default, .regex are validation rules, not Go types
            return map_csil_type_to_go(base_type, occurrence);
        }
        _ => "interface{}", // Fallback for complex types
    };

    // Handle occurrence
    match occurrence {
        Some(CsilOccurrence::Optional) => format!("*{}", base_type),
        _ => base_type.to_string(),
    }
}

fn go_field_name_from_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => {
            // Convert to PascalCase for Go public fields
            pascal_case(name)
        }
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => pascal_case(name),
        _ => "Field".to_string(),
    }
}

fn go_field_name_from_key_with_metadata(
    key: &CsilGroupKey,
    metadata: &[CsilFieldMetadata],
) -> String {
    // Check for go_name custom metadata
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "go_name"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(go_name) = &param.value
        {
            return go_name.clone();
        }
    }

    // Fall back to default naming
    go_field_name_from_key(key)
}

fn get_go_type_override(metadata: &[CsilFieldMetadata]) -> Option<String> {
    for meta in metadata {
        if let CsilFieldMetadata::Custom { name, parameters } = meta
            && name == "go_type"
            && let Some(param) = parameters.first()
            && let CsilLiteralValue::Text(go_type) = &param.value
        {
            return Some(go_type.clone());
        }
    }
    None
}

fn go_json_name_from_key(key: &CsilGroupKey) -> String {
    match key {
        CsilGroupKey::Bare(name) => name.clone(),
        CsilGroupKey::Literal(CsilLiteralValue::Text(name)) => name.clone(),
        _ => "field".to_string(),
    }
}

fn go_method_name(name: &str) -> String {
    pascal_case(name)
}

fn pascal_case(s: &str) -> String {
    s.split(&['_', '-'][..])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

fn get_field_visibility(metadata: &[CsilFieldMetadata]) -> CsilFieldVisibility {
    for meta in metadata {
        if let CsilFieldMetadata::Visibility(vis) = meta {
            return vis.clone();
        }
    }
    CsilFieldVisibility::Bidirectional
}

fn get_field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.as_str())
        } else {
            None
        }
    })
}

fn literal_value_to_go_string(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{}\"", s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "[]byte{}".to_string(),
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(literal_value_to_go_string).collect();
            format!("[{}]", formatted.join(", "))
        }
    }
}

fn literal_value_to_go_value(
    value: &CsilLiteralValue,
    _value_type: &CsilTypeExpression,
    occurrence: &Option<CsilOccurrence>,
) -> String {
    let base_value = match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => format!("\"{}\"", s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "nil".to_string(),
        CsilLiteralValue::Bytes(_) => "[]byte{}".to_string(),
        CsilLiteralValue::Array(elements) => {
            let formatted: Vec<String> = elements.iter().map(literal_value_to_go_string).collect();
            format!("[]interface{{}}{{{}}}", formatted.join(", "))
        }
    };

    // For optional fields, we need to create a pointer to the value
    match occurrence {
        Some(CsilOccurrence::Optional) => match value {
            CsilLiteralValue::Integer(i) => {
                format!("func() *int64 {{ v := int64({}); return &v }}()", i)
            }
            CsilLiteralValue::Float(f) => {
                format!("func() *float64 {{ v := float64({}); return &v }}()", f)
            }
            CsilLiteralValue::Text(s) => {
                format!("func() *string {{ v := \"{}\"; return &v }}()", s)
            }
            CsilLiteralValue::Bool(b) => format!("func() *bool {{ v := {}; return &v }}()", b),
            _ => "nil".to_string(),
        },
        _ => base_value,
    }
}

fn estimate_memory_usage() -> usize {
    // Simple memory usage estimate
    4096 // 4KB estimate
}

fn serialize_and_return_ptr<T: serde::Serialize>(data: &T) -> *mut u8 {
    let serialized = match serde_json::to_string(data) {
        Ok(json) => json,
        Err(_) => return std::ptr::null_mut(),
    };

    let bytes = serialized.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

fn create_error_result(error_code: i32) -> *mut u8 {
    let error_output = WasmGeneratorOutput {
        files: vec![],
        warnings: vec![GeneratorWarning {
            level: WarningLevel::Warning,
            message: format!("Generator failed with error code: {}", error_code),
            location: None,
            suggestion: None,
        }],
        stats: GenerationStats::default(),
    };

    serialize_and_return_ptr(&error_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilPosition, CsilRule, CsilServiceDefinition, CsilServiceOperation, CsilSpecSerialized,
        GeneratorConfig,
    };
    use std::collections::HashMap;

    #[test]
    fn test_pascal_case() {
        assert_eq!(pascal_case("user_name"), "UserName");
        assert_eq!(pascal_case("api-key"), "ApiKey");
        assert_eq!(pascal_case("simple"), "Simple");
        assert_eq!(pascal_case("openbao_installed"), "OpenbaoInstalled");
        assert_eq!(pascal_case("dns_zones_created"), "DnsZonesCreated");
        assert_eq!(pascal_case("k8s_installed"), "K8sInstalled");
    }

    #[test]
    fn test_go_type_mapping() {
        assert_eq!(
            map_csil_type_to_go(&CsilTypeExpression::Builtin("text".to_string()), &None),
            "string"
        );
        assert_eq!(
            map_csil_type_to_go(&CsilTypeExpression::Builtin("int".to_string()), &None),
            "int64"
        );
        assert_eq!(
            map_csil_type_to_go(&CsilTypeExpression::Reference("User".to_string()), &None),
            "User"
        );
    }

    #[test]
    fn test_optional_types() {
        let optional = Some(CsilOccurrence::Optional);
        assert_eq!(
            map_csil_type_to_go(&CsilTypeExpression::Builtin("text".to_string()), &optional),
            "*string"
        );
    }

    fn input_with_service(name: &str, ops: Vec<CsilServiceOperation>) -> WasmGeneratorInput {
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![CsilRule {
                    name: name.to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition { operations: ops }),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
                source_content: None,
                service_count: 1,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "go".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "go".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                target: "go".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        }
    }

    fn make_op(
        name: &str,
        input: &str,
        output: &str,
        direction: CsilServiceDirection,
    ) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: CsilTypeExpression::Reference(input.to_string()),
            output_type: CsilTypeExpression::Reference(output.to_string()),
            direction,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn bidirectional_op_emits_inbound_method_router_and_outbound_encoder() {
        let input = input_with_service(
            "Match",
            vec![
                make_op(
                    "list-events",
                    "User",
                    "User",
                    CsilServiceDirection::Unidirectional,
                ),
                make_op("play", "User", "User", CsilServiceDirection::Bidirectional),
            ],
        );
        let config = GoConfig::from_options(&input.config.options);
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Codec interface emitted once.
        assert!(services.contains("type Codec interface"));

        // Unidirectional stays request/response.
        assert!(services.contains("ListEvents(ctx context.Context, req User) (User, error)"));
        // Bidirectional is a fire-and-forget inbound (no Send/Recv stream).
        assert!(services.contains("Play(ctx context.Context, msg User) error"));
        // The old Stream interface MUST NOT be emitted.
        assert!(!services.contains("PlayStream interface"));
        assert!(!services.contains("Send(User) error"));
        assert!(!services.contains("Recv() (User, error)"));

        // Router dispatches by wire method name.
        assert!(services.contains("func RouteMatchChannel(handlers Match, ctx context.Context, codec Codec, method string, data []byte) error"));
        assert!(services.contains("case \"Play\":"));
        assert!(services.contains("return handlers.Play(ctx, msg)"));

        // Outbound encoder for the bidi op.
        assert!(
            services
                .contains("func EncodeMatchPlay(codec Codec, msg User) (string, []byte, error)")
        );
        assert!(services.contains("return \"Play\", data, nil"));
    }

    #[test]
    fn reverse_op_emits_only_outbound_encoder_no_inbound_method_or_on_callback() {
        let input = input_with_service(
            "Callbacks",
            vec![make_op(
                "notify",
                "User",
                "User",
                CsilServiceDirection::Reverse,
            )],
        );
        let config = GoConfig::from_options(&input.config.options);
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        // Reverse has no server-side inbound method or On<M> callback.
        assert!(!services.contains("Notify(ctx context.Context"));
        assert!(!services.contains("OnNotify"));

        // Router exists but has no Notify case (no inbound to dispatch).
        let router_start = services.find("func RouteCallbacksChannel").unwrap();
        let router_block = &services[router_start..];
        assert!(!router_block.contains("case \"Notify\":"));

        // The server-pushed encoder is present.
        assert!(
            services.contains(
                "func EncodeCallbacksNotify(codec Codec, msg User) (string, []byte, error)"
            )
        );
    }

    #[test]
    fn unary_only_service_skips_codec_and_router() {
        let input = input_with_service(
            "Auth",
            vec![make_op(
                "login",
                "User",
                "User",
                CsilServiceDirection::Unidirectional,
            )],
        );
        let config = GoConfig::from_options(&input.config.options);
        let services = super::generate_services(&input, &config, &mut Vec::new())
            .unwrap()
            .expect("services emitted");

        assert!(!services.contains("type Codec interface"));
        assert!(!services.contains("RouteAuthChannel"));
        assert!(!services.contains("EncodeAuthLogin"));
        // "fmt" should not be imported when no router exists.
        assert!(!services.contains("\"fmt\""));
    }
}
