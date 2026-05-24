//! Rust code generator for CSIL specifications (WASM module)
//!
//! This generator produces idiomatic Rust code with serde serialization support,
//! service trait definitions, and proper handling of CSIL metadata.

use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilTypeExpression, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput, WasmGeneratorOutput,
    wasm_interface::*,
};
use std::collections::HashSet;

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "rust-code-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Rust struct/enum/service generator with serde support".to_string(),
        target: "rust".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/rust-generator".to_string()),
    };

    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(_) => return std::ptr::null(),
    };

    let bytes = metadata_json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
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
    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => return std::ptr::null_mut(),
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated_ptr.add(4), bytes.len());
            }

            allocated_ptr
        }
        Err(_code) => std::ptr::null_mut(),
    }
}

/// Process the generation request
fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = match std::str::from_utf8(input_slice) {
        Ok(s) => s,
        Err(_e) => return Err(error_codes::INVALID_INPUT),
    };

    let input: WasmGeneratorInput = match serde_json::from_str(input_str) {
        Ok(input) => input,
        Err(_e) => return Err(error_codes::SERIALIZATION_ERROR),
    };

    let mut generator = RustCodeGenerator::new(&input);
    let result = generator.generate();

    match result {
        Ok(files) => {
            let total_size = files.iter().map(|f| f.content.len()).sum();

            let stats = GenerationStats {
                files_generated: files.len(),
                total_size_bytes: total_size,
                services_count: input.csil_spec.service_count,
                fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
                generation_time_ms: 200,       // Mock generation time
                peak_memory_bytes: Some(4096), // Mock memory usage
            };

            let output = WasmGeneratorOutput {
                files,
                warnings: generator.warnings,
                stats,
            };

            Ok(output)
        }
        Err(_e) => Err(error_codes::GENERATION_ERROR),
    }
}

/// Rust code generator implementation
struct RustCodeGenerator<'a> {
    input: &'a WasmGeneratorInput,
    warnings: Vec<GeneratorWarning>,
    type_definitions: HashSet<String>,
}

impl<'a> RustCodeGenerator<'a> {
    fn new(input: &'a WasmGeneratorInput) -> Self {
        Self {
            input,
            warnings: Vec::new(),
            type_definitions: HashSet::new(),
        }
    }

    fn generate(&mut self) -> Result<Vec<GeneratedFile>, String> {
        let mut files = Vec::new();

        // Generate types.rs for structs and enums
        let types_content = self.generate_types()?;
        if !types_content.is_empty() {
            files.push(GeneratedFile {
                path: "types.rs".to_string(),
                content: types_content,
            });
        }

        // Generate service traits if services exist
        if self.input.csil_spec.service_count > 0 {
            let services_content = self.generate_services()?;
            files.push(GeneratedFile {
                path: "services.rs".to_string(),
                content: services_content,
            });
        }

        // Generate module root file to tie everything together
        let root_filename = self
            .input
            .config
            .options
            .get("module_root_filename")
            .and_then(|v| v.as_str())
            .unwrap_or("mod.rs")
            .to_string();
        let lib_content = self.generate_lib_file(&files)?;
        files.push(GeneratedFile {
            path: root_filename,
            content: lib_content,
        });

        Ok(files)
    }

    fn generate_types(&mut self) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("//! Generated types from CSIL specification\n\n");
        content.push_str("use serde::{Deserialize, Serialize};\n");

        if self.spec_has_bytes_fields() {
            content.push_str("use serde_bytes;\n");
        }

        content.push('\n');

        for rule in &self.input.csil_spec.rules {
            match &rule.rule_type {
                CsilRuleType::GroupDef(group) => {
                    let struct_code = self.generate_struct(&rule.name, group)?;
                    content.push_str(&struct_code);
                    content.push_str("\n\n");
                    self.type_definitions.insert(rule.name.clone());
                }
                CsilRuleType::TypeChoice(choices) => {
                    let enum_code = self.generate_enum(&rule.name, choices)?;
                    content.push_str(&enum_code);
                    content.push_str("\n\n");
                    self.type_definitions.insert(rule.name.clone());
                }
                CsilRuleType::TypeDef(type_expr) => {
                    let type_alias_code = self.generate_type_alias(&rule.name, type_expr)?;
                    content.push_str(&type_alias_code);
                    content.push_str("\n\n");
                    self.type_definitions.insert(rule.name.clone());
                }
                _ => {} // Services handled separately
            }
        }

        Ok(content)
    }

    fn generate_struct(
        &mut self,
        name: &str,
        group: &CsilGroupExpression,
    ) -> Result<String, String> {
        let mut content = String::new();
        let mut derive_attrs = vec!["Debug", "Clone", "Serialize", "Deserialize"];

        // Add struct documentation if any field has descriptions
        let has_descriptions = group.entries.iter().any(|e| {
            e.metadata
                .iter()
                .any(|m| matches!(m, CsilFieldMetadata::Description(_)))
        });

        if has_descriptions {
            content.push_str(&format!("/// {name}\n"));
        }

        // Check for PartialEq derive based on metadata
        if self.should_derive_partial_eq(group) {
            derive_attrs.push("PartialEq");
        }

        content.push_str(&format!("#[derive({})]\n", derive_attrs.join(", ")));
        content.push_str(&format!("pub struct {name} {{\n"));

        for entry in &group.entries {
            if let Some(field_name) = self.extract_field_name(&entry.key) {
                // Add field documentation
                for metadata in &entry.metadata {
                    if let CsilFieldMetadata::Description(desc) = metadata {
                        content.push_str(&format!("    /// {desc}\n"));
                    }
                }

                // Generate serde attributes based on metadata
                let serde_attrs = self.generate_serde_attributes(
                    &entry.metadata,
                    &entry.occurrence,
                    &entry.value_type,
                );
                if !serde_attrs.is_empty() {
                    content.push_str(&format!("    #[serde({})]\n", serde_attrs.join(", ")));
                }

                let rust_type = self.map_type_to_rust(&entry.value_type, &entry.occurrence)?;
                content.push_str(&format!("    pub {field_name}: {rust_type},\n"));
            }
        }

        content.push('}');
        Ok(content)
    }

    fn generate_enum(
        &mut self,
        name: &str,
        choices: &[CsilTypeExpression],
    ) -> Result<String, String> {
        let mut content = String::new();

        content.push_str(&format!("/// {name} enum variants\n"));
        content.push_str("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n");
        content.push_str("#[serde(untagged)]\n");
        content.push_str(&format!("pub enum {name} {{\n"));

        for (i, choice) in choices.iter().enumerate() {
            let variant_name = format!("Variant{i}");
            let rust_type = self.map_type_to_rust(choice, &None)?;
            content.push_str(&format!("    {variant_name}({rust_type}),\n"));
        }

        content.push('}');
        Ok(content)
    }

    fn generate_type_alias(
        &mut self,
        name: &str,
        type_expr: &CsilTypeExpression,
    ) -> Result<String, String> {
        match type_expr {
            CsilTypeExpression::Group(group) => self.generate_struct(name, group),
            CsilTypeExpression::Choice(choices) => self.generate_enum(name, choices),
            _ => {
                let rust_type = self.map_type_to_rust(type_expr, &None)?;
                Ok(format!("pub type {name} = {rust_type};"))
            }
        }
    }

    fn generate_services(&mut self) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("//! Generated service traits from CSIL specification\n\n");
        content.push_str("use super::types::*;\n\n");

        self.generate_service_error(&mut content);
        content.push('\n');

        if self.spec_has_channel_ops() {
            self.generate_codec_trait(&mut content);
            content.push('\n');
        }

        for rule in &self.input.csil_spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let trait_code = self.generate_service_trait(&rule.name, service)?;
                content.push_str(&trait_code);
                content.push_str("\n\n");

                if Self::service_has_channel_ops(service) {
                    content.push_str(&self.generate_service_router(&rule.name, service)?);
                    content.push('\n');
                    content.push_str(&self.generate_service_encoders(&rule.name, service)?);
                    content.push('\n');
                }
            }
        }

        Ok(content)
    }

    fn spec_has_channel_ops(&self) -> bool {
        self.input
            .csil_spec
            .rules
            .iter()
            .any(|r| match &r.rule_type {
                CsilRuleType::ServiceDef(def) => Self::service_has_channel_ops(def),
                _ => false,
            })
    }

    fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
        def.operations
            .iter()
            .any(|op| !matches!(op.direction, CsilServiceDirection::Unidirectional))
    }

    /// The codec abstraction the user supplies for the message-routing layer.
    /// Same shape across all language targets that emit a router/encoder pair:
    /// the generator never owns serialization or transport, only types and
    /// dispatch.
    fn generate_codec_trait(&self, code: &mut String) {
        code.push_str("/// User-supplied (de)serialization for channel messages. The generator\n");
        code.push_str("/// is codec-agnostic; the implementer wires this to CBOR, JSON, or\n");
        code.push_str("/// anything else its protocol expects.\n");
        code.push_str("pub trait Codec {\n");
        code.push_str("    fn encode<T: serde::Serialize>(&self, value: &T) -> Result<Vec<u8>, ServiceError>;\n");
        code.push_str("    fn decode<T: serde::de::DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, ServiceError>;\n");
        code.push_str("}\n");
    }

    fn generate_service_error(&self, code: &mut String) {
        code.push_str("#[derive(Debug, Clone)]\n");
        code.push_str("pub struct ServiceError {\n");
        code.push_str("    pub code: i32,\n");
        code.push_str("    pub message: String,\n");
        code.push_str("}\n\n");

        code.push_str("impl std::fmt::Display for ServiceError {\n");
        code.push_str("    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n");
        code.push_str("        write!(f, \"service error {}: {}\", self.code, self.message)\n");
        code.push_str("    }\n");
        code.push_str("}\n\n");

        code.push_str("impl std::error::Error for ServiceError {}\n");
    }

    fn generate_service_trait(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let mut content = String::new();

        content.push_str(&format!("/// {name} service trait\n"));
        content.push_str(&format!("pub trait {name} {{\n"));
        content.push_str("    type Context;\n");

        for operation in &service.operations {
            let op_name = self.to_snake_case(&operation.name);
            match operation.direction {
                CsilServiceDirection::Unidirectional => {
                    let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                    let output_type = self.map_type_to_rust(&operation.output_type, &None)?;
                    Self::write_op_doc(&mut content, operation, "request/response");
                    content.push_str(&format!(
                        "    fn {op_name}(&self, ctx: &Self::Context, input: {input_type}) -> Result<{output_type}, ServiceError>;\n",
                    ));
                }
                CsilServiceDirection::Bidirectional => {
                    // Server-side inbound: receive the client's pushed message.
                    // Outbound (Output) is encoded via the generated helper and
                    // pushed by the implementer's connection plumbing — the
                    // generator never owns the wire.
                    let input_type = self.map_type_to_rust(&operation.input_type, &None)?;
                    Self::write_op_doc(&mut content, operation, "channel inbound (bidirectional)");
                    content.push_str(&format!(
                        "    fn {op_name}(&self, ctx: &Self::Context, msg: {input_type}) -> Result<(), ServiceError>;\n",
                    ));
                }
                CsilServiceDirection::Reverse => {
                    // Reverse is server-pushed only: no inbound on the server
                    // side, just an outbound encoder emitted below.
                }
            }
        }

        content.push('}');
        Ok(content)
    }

    fn write_op_doc(content: &mut String, op: &CsilServiceOperation, fallback: &str) {
        if op.doc_comments.is_empty() {
            content.push_str(&format!("    /// {} ({fallback}).\n", op.name));
        } else {
            for line in &op.doc_comments {
                content.push_str(&format!("    /// {line}\n"));
            }
        }
    }

    /// For services with any `<->` op, emit `route_<service>_channel` that
    /// decodes inbound bytes (keyed by the wire method name) and dispatches
    /// to the trait method. Reverse ops never have an inbound route.
    fn generate_service_router(
        &mut self,
        service_name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let inbound_ops: Vec<&CsilServiceOperation> = service
            .operations
            .iter()
            .filter(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
            .collect();

        let mut content = String::new();
        let fn_name = format!("route_{}_channel", self.to_snake_case(service_name));
        content.push_str(&format!(
            "/// Decode one inbound channel frame for {service_name} and dispatch\n\
             /// to the matching trait method. The implementer feeds raw bytes\n\
             /// from its connection here; we never own the wire.\n\
             pub fn {fn_name}<H, C>(\n\
             \x20   handlers: &H,\n\
             \x20   ctx: &H::Context,\n\
             \x20   codec: &C,\n\
             \x20   method: &str,\n\
             \x20   bytes: &[u8],\n\
             ) -> Result<(), ServiceError>\n\
             where\n\
             \x20   H: {service_name},\n\
             \x20   C: Codec,\n\
             {{\n\
             \x20   match method {{\n"
        ));
        for op in &inbound_ops {
            let op_snake = self.to_snake_case(&op.name);
            let input_type = self.map_type_to_rust(&op.input_type, &None)?;
            let wire = Self::pascal_case(&op.name);
            content.push_str(&format!("        \"{wire}\" => {{\n"));
            content.push_str(&format!(
                "            let msg: {input_type} = codec.decode(bytes)?;\n"
            ));
            content.push_str(&format!("            handlers.{op_snake}(ctx, msg)\n"));
            content.push_str("        }\n");
        }
        content.push_str("        other => Err(ServiceError {\n");
        content.push_str("            code: 404,\n");
        content.push_str("            message: format!(\"unknown channel {other}\"),\n");
        content.push_str("        }),\n");
        content.push_str("    }\n");
        content.push_str("}\n");
        Ok(content)
    }

    /// For each `<->` and `<-` op, emit `encode_<service>_<op>` that returns
    /// `(method, bytes)` for the implementer to put on the wire. Unidirectional
    /// ops already have a return value from their trait method, so no encoder.
    fn generate_service_encoders(
        &mut self,
        service_name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let mut content = String::new();
        let svc_snake = self.to_snake_case(service_name);
        for op in &service.operations {
            if !matches!(
                op.direction,
                CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
            ) {
                continue;
            }
            let op_snake = self.to_snake_case(&op.name);
            let wire = Self::pascal_case(&op.name);
            let output_type = self.map_type_to_rust(&op.output_type, &None)?;
            let fn_name = format!("encode_{svc_snake}_{op_snake}");
            content.push_str(&format!(
                "/// Encode a `{wire}` message pushed from {service_name}'s server\n\
                 /// side; the implementer frames `(method, bytes)` onto its connection.\n\
                 pub fn {fn_name}<C: Codec>(codec: &C, msg: &{output_type}) -> Result<(String, Vec<u8>), ServiceError> {{\n\
                 \x20   Ok((\"{wire}\".to_string(), codec.encode(msg)?))\n\
                 }}\n"
            ));
        }
        Ok(content)
    }

    fn pascal_case(s: &str) -> String {
        let mut out = String::new();
        let mut cap = true;
        for ch in s.chars() {
            if ch == '-' || ch == '_' {
                cap = true;
            } else if cap {
                out.push(ch.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn generate_lib_file(&mut self, files: &[GeneratedFile]) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("//! Generated Rust code from CSIL specification\n\n");

        // Add module declarations
        if files.iter().any(|f| f.path == "types.rs") {
            content.push_str("pub mod types;\n");
            content.push_str("pub use types::*;\n\n");
        }

        if files.iter().any(|f| f.path == "services.rs") {
            content.push_str("pub mod services;\n");
            content.push_str("pub use services::*;\n\n");
        }

        Ok(content)
    }

    fn extract_field_name(&self, key: &Option<CsilGroupKey>) -> Option<String> {
        match key {
            Some(CsilGroupKey::Bare(name)) => Some(self.to_snake_case(name)),
            Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                Some(self.to_snake_case(name))
            }
            _ => None,
        }
    }

    fn map_type_to_rust(
        &mut self,
        type_expr: &CsilTypeExpression,
        occurrence: &Option<CsilOccurrence>,
    ) -> Result<String, String> {
        let base_type = match type_expr {
            CsilTypeExpression::Builtin(name) => self.map_builtin_type(name),
            CsilTypeExpression::Reference(name) => name.clone(),
            CsilTypeExpression::Array { element_type, .. } => {
                let element = self.map_type_to_rust(element_type, &None)?;
                format!("Vec<{element}>")
            }
            CsilTypeExpression::Map { key, value, .. } => {
                let key_type = self.map_type_to_rust(key, &None)?;
                let value_type = self.map_type_to_rust(value, &None)?;
                format!("std::collections::HashMap<{key_type}, {value_type}>")
            }
            CsilTypeExpression::Choice(choices) => {
                if choices.len() == 2
                    && choices
                        .iter()
                        .any(|c| matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Null)))
                {
                    // Handle optional types (T | null)
                    let non_null = choices.iter().find(|c| {
                        !matches!(c, CsilTypeExpression::Literal(CsilLiteralValue::Null))
                    });
                    if let Some(inner_type) = non_null {
                        let inner = self.map_type_to_rust(inner_type, &None)?;
                        format!("Option<{inner}>")
                    } else {
                        "serde_json::Value".to_string()
                    }
                } else {
                    "serde_json::Value".to_string() // General choice fallback
                }
            }
            CsilTypeExpression::Literal(literal) => match literal {
                CsilLiteralValue::Integer(_) => "i64".to_string(),
                CsilLiteralValue::Float(_) => "f64".to_string(),
                CsilLiteralValue::Text(_) => "String".to_string(),
                CsilLiteralValue::Bool(_) => "bool".to_string(),
                CsilLiteralValue::Bytes(_) => "Vec<u8>".to_string(),
                CsilLiteralValue::Null => "()".to_string(),
                CsilLiteralValue::Array(_) => "Vec<serde_json::Value>".to_string(),
            },
            _ => {
                self.warnings.push(GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: format!("Unsupported type expression: {type_expr:?}"),
                    location: None,
                    suggestion: Some("Consider using basic CDDL types".to_string()),
                });
                "serde_json::Value".to_string()
            }
        };

        // Apply occurrence modifiers
        let final_type = match occurrence {
            Some(CsilOccurrence::Optional) => format!("Option<{base_type}>"),
            _ => base_type,
        };

        Ok(final_type)
    }

    fn map_builtin_type(&mut self, name: &str) -> String {
        match name {
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "Vec<u8>".to_string(),
            "bool" => "bool".to_string(),
            "int" => "i64".to_string(),
            "uint" => "u64".to_string(),
            "float" | "float16" | "float32" | "float64" => "f64".to_string(),
            "null" => "()".to_string(),
            "any" => "serde_json::Value".to_string(),
            _ => {
                self.warnings.push(GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: format!("Unknown builtin type '{name}', using serde_json::Value"),
                    location: None,
                    suggestion: None,
                });
                "serde_json::Value".to_string()
            }
        }
    }

    fn generate_serde_attributes(
        &self,
        metadata: &[CsilFieldMetadata],
        occurrence: &Option<CsilOccurrence>,
        value_type: &CsilTypeExpression,
    ) -> Vec<String> {
        let mut attrs = Vec::new();

        for meta in metadata {
            match meta {
                CsilFieldMetadata::Visibility(visibility) => {
                    match visibility {
                        CsilFieldVisibility::SendOnly => {
                            attrs.push("skip_deserializing".to_string());
                        }
                        CsilFieldVisibility::ReceiveOnly => {
                            attrs.push("skip_serializing".to_string());
                        }
                        _ => {} // Bidirectional is default
                    }
                }
                CsilFieldMetadata::Custom { name, parameters } if name == "rust" => {
                    for param in parameters {
                        if let Some(param_name) = &param.name
                            && let CsilLiteralValue::Text(value) = &param.value
                        {
                            attrs.push(format!("{param_name} = \"{value}\""));
                        }
                    }
                }
                _ => {}
            }
        }

        if Self::is_bytes_type(value_type) {
            attrs.push("with = \"serde_bytes\"".to_string());
        }

        // Handle optional fields
        if matches!(occurrence, Some(CsilOccurrence::Optional)) {
            attrs.push("skip_serializing_if = \"Option::is_none\"".to_string());
        }

        attrs
    }

    fn is_bytes_type(type_expr: &CsilTypeExpression) -> bool {
        match type_expr {
            CsilTypeExpression::Builtin(name) => matches!(name.as_str(), "bytes" | "bstr"),
            CsilTypeExpression::Constrained { base_type, .. } => Self::is_bytes_type(base_type),
            _ => false,
        }
    }

    fn spec_has_bytes_fields(&self) -> bool {
        self.input.csil_spec.rules.iter().any(|rule| {
            if let CsilRuleType::GroupDef(group) = &rule.rule_type {
                group
                    .entries
                    .iter()
                    .any(|e| Self::is_bytes_type(&e.value_type))
            } else {
                false
            }
        })
    }

    fn should_derive_partial_eq(&self, _group: &CsilGroupExpression) -> bool {
        // For now, always derive PartialEq for structs
        true
    }

    fn to_snake_case(&self, s: &str) -> String {
        let mut result = String::new();

        for ch in s.chars() {
            if ch == '-' {
                result.push('_');
            } else if ch.is_ascii_uppercase() && !result.is_empty() {
                result.push('_');
                result.push(ch.to_ascii_lowercase());
            } else {
                result.push(ch.to_ascii_lowercase());
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::*;
    use std::collections::HashMap;

    fn create_test_input() -> WasmGeneratorInput {
        let metadata = GeneratorMetadata {
            name: "rust-code-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test Rust generator".to_string(),
            target: "rust".to_string(),
            capabilities: vec![
                GeneratorCapability::BasicTypes,
                GeneratorCapability::Services,
            ],
            author: None,
            homepage: None,
        };

        let config = GeneratorConfig {
            target: "rust".to_string(),
            output_dir: "/tmp/output".to_string(),
            options: HashMap::new(),
        };

        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Description(
                                "User's name".to_string(),
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("email".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::SendOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 2,
        };

        WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        }
    }

    #[test]
    fn test_struct_generation() {
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);

        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct User"));
        assert!(types_content.contains("pub name: String"));
        assert!(types_content.contains("pub email: Option<String>"));
        assert!(types_content.contains("#[serde(skip_deserializing"));
    }

    #[test]
    fn test_type_mapping() {
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);

        assert_eq!(generator.map_builtin_type("text"), "String");
        assert_eq!(generator.map_builtin_type("int"), "i64");
        assert_eq!(generator.map_builtin_type("bool"), "bool");
        assert_eq!(generator.map_builtin_type("bytes"), "Vec<u8>");
    }

    #[test]
    fn test_snake_case_conversion() {
        let input = create_test_input();
        let generator = RustCodeGenerator::new(&input);

        assert_eq!(generator.to_snake_case("CamelCase"), "camel_case");
        assert_eq!(generator.to_snake_case("HTTPResponse"), "h_t_t_p_response");
        assert_eq!(generator.to_snake_case("simple"), "simple");
        assert_eq!(generator.to_snake_case("create-entry"), "create_entry");
        assert_eq!(
            generator.to_snake_case("MyService-operation"),
            "my_service_operation"
        );
        assert_eq!(generator.to_snake_case("a--b"), "a__b");
    }

    #[test]
    fn test_service_generation_with_service() {
        let mut input = create_test_input();

        // Add a service to the spec
        input.csil_spec.rules.push(CsilRule {
            name: "UserService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "create_user".to_string(),
                    input_type: CsilTypeExpression::Reference("User".to_string()),
                    output_type: CsilTypeExpression::Reference("User".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 5,
                        column: 4,
                        offset: 100,
                    },
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 4,
                column: 1,
                offset: 80,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services_content = generator.generate_services().unwrap();

        assert!(services_content.contains("pub struct ServiceError {"));
        assert!(services_content.contains("pub code: i32"));
        assert!(services_content.contains("pub message: String"));
        assert!(services_content.contains("impl std::fmt::Display for ServiceError"));
        assert!(services_content.contains("impl std::error::Error for ServiceError"));
        assert!(services_content.contains("pub trait UserService"));
        assert!(services_content.contains("type Context;"));
        assert!(services_content.contains(
            "fn create_user(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));
    }

    #[test]
    fn test_service_with_hyphenated_operations() {
        let mut input = create_test_input();

        input.csil_spec.rules.push(CsilRule {
            name: "Guestbook".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    CsilServiceOperation {
                        name: "create-entry".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 2,
                            column: 4,
                            offset: 20,
                        },
                        doc_comments: Vec::new(),
                    },
                    CsilServiceOperation {
                        name: "list-entries".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 3,
                            column: 4,
                            offset: 40,
                        },
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services_content = generator.generate_services().unwrap();

        assert!(services_content.contains(
            "fn create_entry(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));
        assert!(!services_content.contains("fn create-entry("));
        assert!(services_content.contains(
            "fn list_entries(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));
        // Unidirectional ops get a generic (request/response) doc when the CSIL
        // has no `;;;` doc comments on the operation itself.
        assert!(services_content.contains("/// create-entry (request/response)."));
        assert!(services_content.contains("/// list-entries (request/response)."));
    }

    fn service_with_directions(
        name: &str,
        ops: &[(&str, &str, &str, CsilServiceDirection)],
    ) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: ops
                    .iter()
                    .map(|(n, i, o, d)| CsilServiceOperation {
                        name: n.to_string(),
                        input_type: CsilTypeExpression::Reference(i.to_string()),
                        output_type: CsilTypeExpression::Reference(o.to_string()),
                        direction: d.clone(),
                        position: CsilPosition {
                            line: 1,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                    })
                    .collect(),
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn bidirectional_op_emits_inbound_trait_method_router_and_outbound_encoder() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(service_with_directions(
            "Match",
            &[
                (
                    "list-events",
                    "User",
                    "User",
                    CsilServiceDirection::Unidirectional,
                ),
                ("play", "User", "User", CsilServiceDirection::Bidirectional),
            ],
        ));
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services = generator.generate_services().unwrap();

        // Codec trait emitted once at the top of the file.
        assert!(services.contains("pub trait Codec"), "codec trait expected");

        // Unidirectional kept as request/response.
        assert!(services.contains(
            "fn list_events(&self, ctx: &Self::Context, input: User) -> Result<User, ServiceError>"
        ));

        // Bidirectional is a fire-and-forget inbound handler (no return value).
        assert!(services.contains(
            "fn play(&self, ctx: &Self::Context, msg: User) -> Result<(), ServiceError>"
        ));

        // Router decodes the inbound bytes and dispatches by wire method name.
        assert!(services.contains("pub fn route_match_channel<H, C>"));
        assert!(services.contains("\"Play\" => {"));
        assert!(services.contains("handlers.play(ctx, msg)"));

        // Outbound encoder for the bidirectional op.
        assert!(services.contains(
            "pub fn encode_match_play<C: Codec>(codec: &C, msg: &User) -> Result<(String, Vec<u8>), ServiceError>"
        ));
        assert!(services.contains("(\"Play\".to_string(), codec.encode(msg)?)"));
    }

    #[test]
    fn reverse_op_emits_only_outbound_encoder_no_trait_method() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(service_with_directions(
            "Callbacks",
            &[("notify", "User", "User", CsilServiceDirection::Reverse)],
        ));
        input.csil_spec.service_count = 1;

        let mut generator = RustCodeGenerator::new(&input);
        let services = generator.generate_services().unwrap();

        // Reverse has no server-side inbound — no trait method at all.
        assert!(
            !services.contains("fn notify("),
            "reverse must not emit a trait method"
        );

        // Router exists but its match must NOT include a Notify arm.
        assert!(services.contains("pub fn route_callbacks_channel"));
        let router_start = services.find("pub fn route_callbacks_channel").unwrap();
        let router_block = &services[router_start..];
        assert!(!router_block.contains("\"Notify\" =>"));

        // The encoder for the reverse op (server pushes Output to the client).
        assert!(services.contains(
            "pub fn encode_callbacks_notify<C: Codec>(codec: &C, msg: &User) -> Result<(String, Vec<u8>), ServiceError>"
        ));
    }

    #[test]
    fn services_without_channel_ops_skip_codec_and_router() {
        // create_test_input has no service rules; create_test_input + add a
        // single unidirectional op should not pull in the channel scaffolding.
        let mut input = create_test_input();
        input.csil_spec.rules.push(service_with_directions(
            "Auth",
            &[(
                "login",
                "User",
                "User",
                CsilServiceDirection::Unidirectional,
            )],
        ));
        input.csil_spec.service_count = 1;

        let services = RustCodeGenerator::new(&input).generate_services().unwrap();
        assert!(!services.contains("pub trait Codec"));
        assert!(!services.contains("route_auth_channel"));
        assert!(!services.contains("encode_auth_login"));
    }

    #[test]
    fn test_module_root_filename_default() {
        let input = create_test_input();
        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();

        let root_file = files.iter().find(|f| f.path == "mod.rs");
        assert!(root_file.is_some());
    }

    #[test]
    fn test_module_root_filename_custom() {
        let mut input = create_test_input();
        input.config.options.insert(
            "module_root_filename".to_string(),
            serde_json::Value::String("lib.rs".to_string()),
        );

        let mut generator = RustCodeGenerator::new(&input);
        let files = generator.generate().unwrap();

        let root_file = files.iter().find(|f| f.path == "lib.rs");
        assert!(root_file.is_some());
        assert!(files.iter().all(|f| f.path != "mod.rs"));
    }

    #[test]
    fn test_full_generation_workflow() {
        let input = create_test_input();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.files.is_empty());
        assert_eq!(output.stats.fields_with_metadata_count, 2);

        // Check that types.rs and lib.rs are generated
        let type_file = output.files.iter().find(|f| f.path == "types.rs");
        assert!(type_file.is_some());

        let mod_file = output.files.iter().find(|f| f.path == "mod.rs");
        assert!(mod_file.is_some());
    }

    #[test]
    fn test_error_handling() {
        let result = process_generation(std::ptr::null(), 0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::INVALID_INPUT);

        let invalid_json = b"not json";
        let result = process_generation(invalid_json.as_ptr(), invalid_json.len());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::SERIALIZATION_ERROR);
    }

    #[test]
    fn test_memory_management() {
        let size = 1024;
        let ptr = allocate(size);
        assert!(!ptr.is_null());

        deallocate(ptr, size);
        // Test passes if no crash occurs
    }

    #[test]
    fn test_enum_from_typedef_wrapping_choice() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "CheckValue".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("int".to_string()),
                CsilTypeExpression::Builtin("float".to_string()),
            ])),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content.contains("pub enum CheckValue"),
            "Choice wrapped in TypeDef should generate an enum, not a type alias"
        );
        assert!(types_content.contains("Variant0(String)"));
        assert!(types_content.contains("Variant1(i64)"));
        assert!(types_content.contains("Variant2(f64)"));
        assert!(types_content.contains("#[serde(untagged)]"));
        assert!(
            !types_content.contains("pub type CheckValue = serde_json::Value"),
            "Should not fall back to serde_json::Value"
        );
    }

    #[test]
    fn test_struct_from_typedef_wrapping_group() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "CheckResult".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("result".to_string())),
                        value_type: CsilTypeExpression::Builtin("bool".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("entries".to_string())),
                        value_type: CsilTypeExpression::Reference("CheckEntries".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content.contains("pub struct CheckResult"),
            "Group wrapped in TypeDef should generate a struct, not a type alias"
        );
        assert!(types_content.contains("pub result: bool"));
        assert!(types_content.contains("pub entries: CheckEntries"));
        assert!(
            !types_content.contains("pub type CheckResult = serde_json::Value"),
            "Should not fall back to serde_json::Value"
        );
    }

    #[test]
    fn test_struct_with_optional_fields() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "HelloRequest".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("name".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct HelloRequest"));
        assert!(types_content.contains("pub name: Option<String>"));
        assert!(types_content.contains("skip_serializing_if = \"Option::is_none\""));
    }

    #[test]
    fn test_struct_with_receive_only_visibility() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![CsilRule {
            name: "GuestbookEntry".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("id".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::ReceiveOnly,
                        )],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("created_at".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::ReceiveOnly,
                        )],
                        doc_comments: Vec::new(),
                    },
                ],
            })),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(types_content.contains("pub struct GuestbookEntry"));
        assert!(types_content.contains("pub id: String"));
        assert!(types_content.contains("pub name: String"));
        assert!(types_content.contains("pub created_at: String"));
        // id and created_at should have skip_serializing, name should not
        let id_section = &types_content
            [types_content.find("pub id:").unwrap() - 80..types_content.find("pub id:").unwrap()];
        assert!(
            id_section.contains("skip_serializing"),
            "receive-only field 'id' should have skip_serializing"
        );
    }

    #[test]
    fn test_map_type_still_works() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "CheckEntries".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Reference("CheckValue".to_string())),
                occurrence: None,
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        assert!(
            types_content
                .contains("pub type CheckEntries = std::collections::HashMap<String, CheckValue>;")
        );
    }

    #[test]
    fn test_linkkeys_end_to_end() {
        let mut input = create_test_input();
        input.csil_spec.rules = vec![
            CsilRule {
                name: "CheckValue".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Builtin("text".to_string()),
                    CsilTypeExpression::Builtin("int".to_string()),
                    CsilTypeExpression::Builtin("float".to_string()),
                ])),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "CheckEntries".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                    key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                    value: Box::new(CsilTypeExpression::Reference("CheckValue".to_string())),
                    occurrence: None,
                }),
                position: CsilPosition {
                    line: 3,
                    column: 1,
                    offset: 30,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "CheckResult".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("result".to_string())),
                            value_type: CsilTypeExpression::Builtin("bool".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("entries".to_string())),
                            value_type: CsilTypeExpression::Reference("CheckEntries".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: CsilPosition {
                    line: 5,
                    column: 1,
                    offset: 60,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "HelloRequest".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                })),
                position: CsilPosition {
                    line: 10,
                    column: 1,
                    offset: 120,
                },
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "GuestbookEntry".to_string(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("id".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("created_at".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("updated_at".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::ReceiveOnly,
                            )],
                            doc_comments: Vec::new(),
                        },
                    ],
                })),
                position: CsilPosition {
                    line: 14,
                    column: 1,
                    offset: 160,
                },
                doc_comments: Vec::new(),
            },
        ];

        let mut generator = RustCodeGenerator::new(&input);
        let types_content = generator.generate_types().unwrap();

        // CheckValue is an enum with 3 variants
        assert!(types_content.contains("pub enum CheckValue"));
        assert!(types_content.contains("Variant0(String)"));
        assert!(types_content.contains("Variant1(i64)"));
        assert!(types_content.contains("Variant2(f64)"));

        // CheckEntries is a HashMap
        assert!(
            types_content
                .contains("pub type CheckEntries = std::collections::HashMap<String, CheckValue>")
        );

        // CheckResult is a struct
        assert!(types_content.contains("pub struct CheckResult"));
        assert!(types_content.contains("pub result: bool"));
        assert!(types_content.contains("pub entries: CheckEntries"));

        // HelloRequest has optional name
        assert!(types_content.contains("pub struct HelloRequest"));
        assert!(types_content.contains("pub name: Option<String>"));

        // GuestbookEntry is a struct with 4 fields
        assert!(types_content.contains("pub struct GuestbookEntry"));
        assert!(types_content.contains("pub id: String"));
        assert!(types_content.contains("pub name: String"));
        assert!(types_content.contains("pub created_at: String"));
        assert!(types_content.contains("pub updated_at: String"));

        // No serde_json::Value type aliases (except for 'any' typed fields)
        assert!(
            !types_content.contains("pub type CheckValue = serde_json::Value"),
            "CheckValue should be an enum"
        );
        assert!(
            !types_content.contains("pub type CheckResult = serde_json::Value"),
            "CheckResult should be a struct"
        );
        assert!(
            !types_content.contains("pub type HelloRequest = serde_json::Value"),
            "HelloRequest should be a struct"
        );
        assert!(
            !types_content.contains("pub type GuestbookEntry = serde_json::Value"),
            "GuestbookEntry should be a struct"
        );
    }
}
